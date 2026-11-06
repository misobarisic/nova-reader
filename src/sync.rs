//! Reader's per-identity materialization on the shared, socket-free sync owner.
//! The domain lock is always acquired before the record lock. Network callbacks
//! must enter through `apply_remote`, never hold the record lock while doing so.
use reader_domain::{Bookmark, ChapterId, DomainStore, ReaderState, UpdateKind};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    sync::{Arc, LazyLock},
};

pub const PROFILE: &str = "reader";
const KINDS: &[&str] = &[
    "series",
    "chapters",
    "library",
    "categories",
    "memberships",
    "resume",
    "read_actions",
    "history",
    "bookmarks",
];
type Rows = BTreeMap<String, Vec<(String, String, u64)>>;

pub fn domain(kind: &str) -> String {
    nova_sync::profile_domain(PROFILE, &format!("reader_{kind}"))
}
/// Domains whose materialization and receipts this adapter owns.
pub fn handles_projection_domain(name: &str) -> bool {
    KINDS.iter().any(|kind| domain(kind) == name)
        || name == crate::sync_settings::domain()
        || name == crate::content_presentation::domain()
}
fn key(value: &impl Serialize) -> nova_sync::Result<String> {
    Ok(serde_json::to_string(value)?)
}
fn bookmark_key(bookmark: &Bookmark) -> nova_sync::Result<String> {
    // A note edit keeps identity; moving the immutable anchor is delete + add.
    key(&(
        &bookmark.chapter,
        &bookmark.anchor.page_id,
        bookmark.anchor.offset_millionths,
    ))
}
#[derive(Serialize, Deserialize)]
struct Membership {
    series: reader_domain::SeriesId,
    category: String,
}
#[derive(Serialize, Deserialize)]
struct ReadAction {
    chapter: ChapterId,
    completed: bool,
    at: i64,
}

fn snapshots(state: &ReaderState) -> nova_sync::Result<Rows> {
    let mut rows: Rows = KINDS
        .iter()
        .map(|kind| (kind.to_string(), Vec::new()))
        .collect();
    macro_rules! push {
        ($kind:literal, $id:expr, $value:expr) => {
            rows.get_mut($kind).unwrap().push((
                key($id)?,
                serde_json::to_value($value)?.to_string(),
                0,
            ));
        };
    }
    for series in &state.series {
        push!("series", &series.id, series);
    }
    for chapter in &state.chapters {
        push!("chapters", &chapter.id, chapter);
    }
    for entry in &state.library {
        let mut value = entry.clone();
        value.categories.clear();
        push!("library", &entry.series, &value);
        for category in &entry.categories {
            push!(
                "memberships",
                &(&entry.series, category),
                &Membership {
                    series: entry.series.clone(),
                    category: category.clone()
                }
            );
        }
    }
    for category in &state.categories {
        push!("categories", &category.id, category);
    }
    for progress in &state.progress {
        push!("resume", &progress.chapter, progress);
    }
    for history in &state.history {
        push!("history", &history.chapter, history);
    }
    for bookmark in &state.bookmarks {
        rows.get_mut("bookmarks").unwrap().push((
            bookmark_key(bookmark)?,
            serde_json::to_value(bookmark)?.to_string(),
            0,
        ));
    }
    // Legacy backups may contain duplicate history or bookmark anchors. Their
    // deterministic migration keeps the last value, matching set_progress.
    for values in rows.values_mut() {
        let unique: BTreeMap<_, _> = values
            .drain(..)
            .map(|(id, value, timestamp)| (id, (value, timestamp)))
            .collect();
        values.extend(
            unique
                .into_iter()
                .map(|(id, (value, timestamp))| (id, value, timestamp)),
        );
    }
    Ok(rows)
}

/// Initialize after storage, before engine setup; this never binds an endpoint.
pub fn initialize(store: &DomainStore) -> nova_sync::Result<()> {
    nova_sync::configure_protocol(nova_sync::ProtocolConfig::reader())?;
    nova_sync::configure_record_policy(READER_POLICY.clone())?;
    nova_sync::local_store()?;
    crate::content_presentation::install_sync();
    reader_domain::install_persistence_hook(Arc::new(|before, state, kind| {
        persist(before, state, kind)
            .map_err(|e| reader_domain::Error::Invalid(format!("sync persistence: {e:#}")))
    }));
    store.update_with_kind(UpdateKind::Seed, |_| Ok(()))?;
    apply_remote(store)
}

/// Call only after draining queued reading checkpoints. No remote projection is
/// echoed as a local edit, and state/baselines/records commit in one transaction.
pub fn apply_remote(store: &DomainStore) -> nova_sync::Result<()> {
    nova_sync::flush_persistence()?;
    store.update_with_kind(UpdateKind::Projection, |_| Ok(()))?;
    Ok(())
}

fn persist(
    before: &ReaderState,
    state: &mut ReaderState,
    kind: UpdateKind,
) -> nova_sync::Result<()> {
    let owner = nova_sync::local_store()?;
    let dev = nova_sync::local_device()?;
    let mut guard = owner.lock().unwrap_or_else(|e| e.into_inner());
    // Failed writes must not expose partially prepared records to the engine.
    let mut records = guard.clone();
    let projection = kind == UpdateKind::Projection;
    if kind == UpdateKind::Checkpoint {
        for progress in &mut state.progress {
            if let Some(prior) = before
                .progress
                .iter()
                .find(|prior| prior.chapter == progress.chapter)
                && prior.last_read_at > progress.last_read_at
            {
                *progress = prior.clone();
            }
        }
        for history in &mut state.history {
            if let Some(prior) = before
                .history
                .iter()
                .find(|prior| prior.chapter == history.chapter)
                && prior.read_at > history.read_at
            {
                *history = prior.clone();
            }
        }
    }
    if projection {
        project(&records, state)?;
    }
    if kind == UpdateKind::ManualProgress {
        for progress in &state.progress {
            let prior = before
                .progress
                .iter()
                .find(|p| p.chapter == progress.chapter);
            if prior != Some(progress) {
                let action = ReadAction {
                    chapter: progress.chapter.clone(),
                    completed: progress.completed,
                    at: progress.last_read_at,
                };
                let identity = key(&action.chapter)?;
                let mut value = serde_json::to_value(&action)?.to_string();
                if let Some(old) = records
                    .record(&domain("read_actions"), &identity)
                    .and_then(|record| record.value.as_deref())
                {
                    value = preserve_fields(old, &value)?;
                }
                records.set(&domain("read_actions"), &identity, Some(value), 0, dev);
            }
        }
    }
    if !projection {
        for progress in &before.progress {
            if !state.progress.iter().any(|p| p.chapter == progress.chapter) {
                records.set(
                    &domain("read_actions"),
                    &key(&progress.chapter)?,
                    None,
                    0,
                    dev,
                );
            }
        }
        resolve_actions(&records, state)?;
        if kind == UpdateKind::Checkpoint {
            // A queued old checkpoint must not undo an explicit history clear.
            // Fresh reading activity after the deletion may create history again.
            state.history.retain(|entry| {
                let Ok(identity) = key(&entry.chapter) else {
                    return false;
                };
                !records
                    .record(&domain("history"), &identity)
                    .is_some_and(|record| {
                        record.is_deleted() && entry.read_at <= (record.version.ts / 1000) as i64
                    })
            });
        }
    }
    let mut rows = snapshots(state)?;
    for (kind_name, values) in &mut rows {
        for (identity, value, _) in values {
            if let Some(old) = records
                .record(&domain(kind_name), identity)
                .and_then(|record| record.value.as_deref())
            {
                *value = preserve_fields(old, value)?;
            }
        }
    }
    let mut changed = false;
    let mut presentation = Vec::new();
    if projection {
        crate::sync_settings::apply_remote(&mut records)?;
        presentation = crate::content_presentation::apply_remote(&mut records)?;
    } else if kind == UpdateKind::Seed {
        changed |= crate::sync_settings::prepare(&mut records, dev, false, true)?;
        changed |= crate::content_presentation::seed(&mut records, dev)?;
    }
    for kind_name in KINDS {
        if *kind_name == "read_actions" {
            continue;
        }
        changed |= nova_sync::prepare_snapshot(
            &mut records,
            dev,
            &domain(kind_name),
            &rows[*kind_name],
            None,
            projection,
            kind == UpdateKind::Seed,
        )?;
    }
    records.queue_extra(reader_domain::STATE_KEY, Some(state.storage_json()?));
    records.save()?;
    *guard = records;
    if projection {
        let digest = guard.digest();
        for owned in [
            crate::sync_settings::domain(),
            crate::content_presentation::domain(),
        ] {
            if let Some(basis) = digest.get(&owned) {
                guard.mark_projected(&owned, basis)?;
            }
        }
        for kind_name in KINDS {
            let name = domain(kind_name);
            if let Some(basis) = digest.get(&name) {
                guard.mark_projected(&name, basis)?;
            }
        }
    }
    drop(guard);
    crate::content_presentation::publish_projection(presentation);
    if changed || kind == UpdateKind::ManualProgress {
        nova_sync::request_local_sync();
    }
    Ok(())
}

pub(crate) fn preserve_fields(old: &str, new: &str) -> nova_sync::Result<String> {
    fn merge(old: serde_json::Value, new: serde_json::Value) -> serde_json::Value {
        match (old, new) {
            (serde_json::Value::Object(mut old), serde_json::Value::Object(new)) => {
                for (key, value) in new {
                    let value = match old.remove(&key) {
                        Some(prior) => merge(prior, value),
                        None => value,
                    };
                    old.insert(key, value);
                }
                serde_json::Value::Object(old)
            }
            (_, new) => new,
        }
    }
    Ok(serde_json::to_string(&merge(
        serde_json::from_str(old)?,
        serde_json::from_str(new)?,
    ))?)
}

fn values<T: DeserializeOwned>(store: &nova_sync::Store, kind: &str) -> nova_sync::Result<Vec<T>> {
    store
        .records(&domain(kind))
        .into_iter()
        .map(|(_, raw)| decode_value(kind, &raw))
        .collect()
}
fn resolve_actions(store: &nova_sync::Store, state: &mut ReaderState) -> nova_sync::Result<()> {
    for action in values::<ReadAction>(store, "read_actions")? {
        if let Some(progress) = state
            .progress
            .iter_mut()
            .find(|p| p.chapter == action.chapter)
            && action.at >= progress.last_read_at
        {
            progress.completed = action.completed;
        }
    }
    Ok(())
}
fn project(store: &nova_sync::Store, state: &mut ReaderState) -> nova_sync::Result<()> {
    // Metadata remains a cache: removing membership never deletes cached series,
    // and a metadata refresh cannot recreate membership.
    for value in values::<reader_domain::Series>(store, "series")? {
        state.series.retain(|s| s.id != value.id);
        state.series.push(value);
    }
    for value in values::<reader_domain::Chapter>(store, "chapters")? {
        state.chapters.retain(|s| s.id != value.id);
        state.chapters.push(value);
    }
    state.categories = values(store, "categories")?;
    state.library = values(store, "library")?;
    for entry in &mut state.library {
        entry.categories.clear();
    }
    for value in values::<Membership>(store, "memberships")? {
        if state.categories.iter().any(|c| c.id == value.category)
            && let Some(entry) = state.library.iter_mut().find(|e| e.series == value.series)
        {
            entry.categories.insert(value.category);
        }
    }
    state.progress = values(store, "resume")?;
    state.history = values(store, "history")?;
    state.bookmarks = values(store, "bookmarks")?;
    resolve_actions(store, state)?;
    state.validate()?;
    Ok(())
}

// Unknown future object fields remain in Store records while typed projections
// consume only fields understood by this version.
fn decode_value<T: DeserializeOwned>(kind: &str, raw: &str) -> nova_sync::Result<T> {
    let mut value: serde_json::Value = serde_json::from_str(raw)?;
    let allowed: &[&str] = match kind {
        "series" => &[
            "publication_status",
            "id",
            "title",
            "description",
            "authors",
            "genres",
            "cover_url",
            "canonical_url",
        ],
        "chapters" => &[
            "id",
            "title",
            "label",
            "number",
            "volume",
            "source_order",
            "language",
            "scanlator",
            "uploaded_at",
        ],
        "library" => &[
            "reading_status",
            "series",
            "categories",
            "preferences",
            "bookmarked",
            "added_at",
        ],
        "categories" => &["id", "name"],
        "memberships" => &["series", "category"],
        "resume" => &["chapter", "anchor", "mode", "last_read_at", "completed"],
        "read_actions" => &["chapter", "completed", "at"],
        "history" => &["chapter", "read_at"],
        "bookmarks" => &["chapter", "anchor", "note"],
        _ => &[],
    };
    if let Some(object) = value.as_object_mut() {
        object.retain(|key, _| allowed.contains(&key.as_str()));
        if let Some(anchor) = object.get_mut("anchor").and_then(|v| v.as_object_mut()) {
            anchor.retain(|key, _| {
                ["page_id", "page_index", "offset_millionths"].contains(&key.as_str())
            });
        }
        if let Some(preferences) = object
            .get_mut("preferences")
            .and_then(|v| v.as_object_mut())
        {
            preferences
                .retain(|key, _| ["mode", "fit_width", "gap_pixels"].contains(&key.as_str()));
        }
    }
    Ok(serde_json::from_value(value)?)
}

static READER_POLICY: LazyLock<Arc<dyn nova_sync::RecordPolicy>> =
    LazyLock::new(|| Arc::new(ReaderPolicy));
struct ReaderPolicy;
impl nova_sync::RecordPolicy for ReaderPolicy {
    fn validate(&self, name: &str, record_key: &str, record: &nova_sync::Record) -> bool {
        if name == crate::sync_settings::domain() {
            // Retain newer schemas for the account inspector. Settings projection
            // applies only supported portable fields to this device.
            return record.is_deleted()
                || record.value.as_deref().is_some_and(|raw| {
                    raw.len() <= 1024 * 1024
                        && serde_json::from_str::<serde_json::Value>(raw).is_ok()
                });
        }
        if name == crate::sync_addons::domain() {
            return crate::sync_addons::validate_record(record_key, record);
        }
        if name == crate::content_presentation::domain() {
            return crate::content_presentation::validate_record(record_key, record);
        }
        let Some(kind) = KINDS.iter().find(|kind| domain(kind) == name) else {
            return true;
        };
        if record.is_deleted() {
            return valid_record_key(kind, record_key);
        }
        let Some(raw) = record.value.as_deref() else {
            return false;
        };
        validate_record(kind, record_key, raw).unwrap_or(false)
    }
}
fn valid_record_key(kind: &str, raw: &str) -> bool {
    match kind {
        "series" | "library" => {
            serde_json::from_str::<reader_domain::SeriesId>(raw).is_ok_and(|id| valid_series(&id))
        }
        "chapters" | "resume" | "read_actions" | "history" => {
            serde_json::from_str::<ChapterId>(raw).is_ok_and(|id| valid_chapter(&id))
        }
        "categories" => serde_json::from_str::<String>(raw).is_ok_and(|id| valid_text(&id)),
        "memberships" => serde_json::from_str::<(reader_domain::SeriesId, String)>(raw)
            .is_ok_and(|(id, category)| valid_series(&id) && valid_text(&category)),
        "bookmarks" => {
            serde_json::from_str::<(ChapterId, String, u32)>(raw).is_ok_and(|(id, page, offset)| {
                valid_chapter(&id) && valid_text(&page) && offset <= 1_000_000
            })
        }
        _ => false,
    }
}
fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 8192 && !value.contains('\0')
}
fn valid_series(id: &reader_domain::SeriesId) -> bool {
    valid_text(&id.source_id) && valid_text(&id.local_id)
}
fn valid_decimal(value: &str) -> bool {
    let value = value.strip_prefix('-').unwrap_or(value);
    let mut parts = value.split('.');
    let first = parts.next().unwrap_or("");
    !first.is_empty()
        && first.bytes().all(|b| b.is_ascii_digit())
        && parts
            .next()
            .is_none_or(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        && parts.next().is_none()
}
fn valid_chapter(id: &ChapterId) -> bool {
    valid_series(&id.series) && valid_text(&id.local_id)
}
fn valid_anchor(anchor: &reader_domain::ResumeAnchor) -> bool {
    valid_text(&anchor.page_id) && anchor.offset_millionths <= 1_000_000
}
fn validate_record(kind: &str, record_key: &str, raw: &str) -> nova_sync::Result<bool> {
    let (identity, valid) = match kind {
        "series" => {
            let v: reader_domain::Series = decode_value(kind, raw)?;
            (key(&v.id)?, valid_series(&v.id) && valid_text(&v.title))
        }
        "chapters" => {
            let v: reader_domain::Chapter = decode_value(kind, raw)?;
            (
                key(&v.id)?,
                valid_chapter(&v.id)
                    && v.number.as_deref().is_none_or(valid_decimal)
                    && v.volume.as_deref().is_none_or(valid_decimal),
            )
        }
        "library" => {
            let v: reader_domain::LibraryEntry = decode_value(kind, raw)?;
            (
                key(&v.series)?,
                valid_series(&v.series) && v.categories.is_empty(),
            )
        }
        "categories" => {
            let v: reader_domain::Category = decode_value(kind, raw)?;
            (key(&v.id)?, valid_text(&v.id) && valid_text(&v.name))
        }
        "memberships" => {
            let v: Membership = decode_value(kind, raw)?;
            (
                key(&(&v.series, &v.category))?,
                valid_series(&v.series) && valid_text(&v.category),
            )
        }
        "resume" => {
            let v: reader_domain::ReadingProgress = decode_value(kind, raw)?;
            (
                key(&v.chapter)?,
                valid_chapter(&v.chapter) && valid_anchor(&v.anchor),
            )
        }
        "read_actions" => {
            let v: ReadAction = decode_value(kind, raw)?;
            (key(&v.chapter)?, valid_chapter(&v.chapter))
        }
        "history" => {
            let v: reader_domain::HistoryEntry = decode_value(kind, raw)?;
            (key(&v.chapter)?, valid_chapter(&v.chapter))
        }
        "bookmarks" => {
            let v: Bookmark = decode_value(kind, raw)?;
            (
                bookmark_key(&v)?,
                valid_chapter(&v.chapter) && valid_anchor(&v.anchor),
            )
        }
        _ => return Ok(false),
    };
    Ok(valid && identity == record_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_domains_are_owned_and_profile_isolated() {
        assert!(handles_projection_domain(&domain("resume")));
        assert!(handles_projection_domain(&crate::sync_settings::domain()));
        assert!(!handles_projection_domain("peers"));
        assert!(!handles_projection_domain(&domain("future")));
        assert!(!handles_projection_domain(&crate::sync_addons::domain()));
        assert!(!handles_projection_domain(&nova_sync::profile_domain(
            "another",
            "reader_resume"
        )));
    }
    #[test]
    fn legacy_bookmark_key_is_source_scoped_and_note_independent() {
        let mut bookmark = Bookmark {
            chapter: ChapterId::new(reader_domain::SeriesId::new("a", "b:c"), "chapter"),
            anchor: reader_domain::ResumeAnchor::new("page", 2, 0.25).unwrap(),
            note: "before".into(),
        };
        let original = bookmark_key(&bookmark).unwrap();
        bookmark.note = "edited".into();
        assert_eq!(bookmark_key(&bookmark).unwrap(), original);
        bookmark.chapter.series = reader_domain::SeriesId::new("a:b", "c");
        assert_ne!(bookmark_key(&bookmark).unwrap(), original);
    }
    #[test]
    fn updates_preserve_future_nested_fields() {
        let old = r#"{"anchor":{"page_id":"page","future":42},"future":{"enabled":true}}"#;
        let new = r#"{"anchor":{"page_id":"new"}}"#;
        let value: serde_json::Value =
            serde_json::from_str(&preserve_fields(old, new).unwrap()).unwrap();
        assert_eq!(value["anchor"]["page_id"], "new");
        assert_eq!(value["anchor"]["future"], 42);
        assert_eq!(value["future"]["enabled"], true);
    }
    #[test]
    fn legacy_duplicate_bookmark_anchors_migrate_deterministically() {
        let first = Bookmark {
            chapter: ChapterId::new(reader_domain::SeriesId::new("source", "series"), "chapter"),
            anchor: reader_domain::ResumeAnchor::new("page", 0, 0.5).unwrap(),
            note: "first".into(),
        };
        let mut last = first.clone();
        last.note = "last".into();
        let state = ReaderState {
            bookmarks: vec![first, last],
            ..ReaderState::default()
        };
        let rows = snapshots(&state).unwrap();
        assert_eq!(rows["bookmarks"].len(), 1);
        let value: Bookmark = serde_json::from_str(&rows["bookmarks"][0].1).unwrap();
        assert_eq!(value.note, "last");
    }
    #[test]
    fn record_validation_checks_identity_and_preserves_future_fields() {
        let category = reader_domain::Category {
            id: "stable".into(),
            name: "Renamed".into(),
        };
        let mut raw = serde_json::to_value(&category).unwrap();
        raw["future"] = serde_json::json!({"enabled":true});
        assert!(
            validate_record("categories", &key(&category.id).unwrap(), &raw.to_string()).unwrap()
        );
        assert!(!validate_record("categories", "\"other\"", &raw.to_string()).unwrap());
        let restored: reader_domain::Category =
            decode_value("categories", &raw.to_string()).unwrap();
        assert_eq!(restored, category);
    }
}
