//! What each backup component holds and how it is read from, checked against
//! and written back to Reader's stores. Every component is a bag of logical
//! records `kind\0identity -> JSON`, keyed by semantic identity (never by a sync
//! version), so two exports of the same data are equal and a merge compares
//! records, not bytes.
//!
//! Profile components belong to one profile; device components to this device.
//! Records of kind `series` and `chapter` inside a state component are support
//! records: the cached titles and chapters that the component's own records
//! refer to. They are inserted when missing and never replace or delete the
//! cache already on the device.
use super::archive::Payload;
use reader_domain::{
    Bookmark, Category, Chapter, ChapterId, HistoryEntry, LibraryEntry, ReaderPreferences,
    ReaderState, ReadingProgress, Series, SeriesId,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// A profile's durable rows, keyed by logical key (without `profile:{id}:`).
pub(crate) type Rows = BTreeMap<String, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    Profile,
    Device,
}
pub(crate) struct Def {
    pub id: &'static str,
    pub label: &'static str,
    pub scope: Scope,
    /// Selected by the "Everything" preset. Large files are opt-in.
    pub everything: bool,
}
const fn def(id: &'static str, label: &'static str, scope: Scope, everything: bool) -> Def {
    Def {
        id,
        label,
        scope,
        everything,
    }
}
pub(crate) const COMPONENTS: &[Def] = &[
    def("library", "Library", Scope::Profile, true),
    def("categories", "Categories", Scope::Profile, true),
    def("progress", "Reading progress", Scope::Profile, true),
    def("history", "History", Scope::Profile, true),
    def("bookmarks", "Bookmarks", Scope::Profile, true),
    def("settings", "Reader and app settings", Scope::Profile, true),
    def("title_settings", "Per-title settings", Scope::Profile, true),
    def("addons", "Addon settings", Scope::Profile, true),
    def("addon_secrets", "Addon credentials", Scope::Profile, true),
    def(
        "sync_overrides",
        "Per-device sync overrides",
        Scope::Profile,
        true,
    ),
    def("packages", "Addon packages", Scope::Device, true),
    def(
        "device_settings",
        "This device's settings",
        Scope::Device,
        true,
    ),
];
pub(crate) fn definition(name: &str) -> Option<&'static Def> {
    COMPONENTS.iter().find(|def| def.id == name)
}
pub(crate) fn supported(name: &str) -> bool {
    definition(name).is_some()
}
pub(crate) const MAX_VALUE_BYTES: usize = 1024 * 1024;

pub(crate) fn key(kind: &str, id: &str) -> String {
    format!("{kind}\0{id}")
}
pub(crate) fn split(key: &str) -> Option<(&str, &str)> {
    key.split_once('\0')
}
pub(crate) fn empty() -> Payload {
    Payload {
        version: 1,
        records: BTreeMap::new(),
    }
}
fn json_key(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).expect("identity serializes")
}
/// Category names match by this form, as Reader's own duplicate check does.
pub(crate) fn category_key(name: &str) -> String {
    name.trim().to_lowercase()
}
// ---------------------------------------------------------------- state parts

/// The records of a state component, decoded.
#[derive(Default)]
pub(crate) struct Parts {
    pub series: Vec<Series>,
    pub chapters: Vec<Chapter>,
    /// Library entries with their categories as [`category_key`] names.
    pub library: Vec<(LibraryEntry, Vec<String>)>,
    pub categories: Vec<(String, String)>,
    pub progress: Vec<ReadingProgress>,
    pub history: Vec<HistoryEntry>,
    pub bookmarks: Vec<Bookmark>,
}

fn kinds(name: &str) -> &'static [&'static str] {
    match name {
        "library" => &["entry", "series", "chapter"],
        "categories" => &["category"],
        "progress" => &["progress", "series", "chapter"],
        "history" => &["history", "series", "chapter"],
        "bookmarks" => &["bookmark", "series", "chapter"],
        _ => &[],
    }
}
fn bookmark_identity(bookmark: &Bookmark) -> String {
    format!(
        "{}\0{}\0{}",
        json_key(&bookmark.chapter),
        bookmark.anchor.page_id,
        bookmark.anchor.offset_millionths
    )
}

/// Decode and cross-check the records of a state component.
pub(crate) fn parse_state(name: &str, payload: &Payload) -> Result<Parts, String> {
    let invalid = |what: &str| format!("Invalid {name} record: {what}");
    let allowed = kinds(name);
    let mut parts = Parts::default();
    for (record, value) in &payload.records {
        let (kind, id) = split(record).ok_or_else(|| invalid("identity"))?;
        if !allowed.contains(&kind) {
            return Err(invalid("unexpected kind"));
        }
        macro_rules! typed {
            ($ty:ty) => {
                serde_json::from_value::<$ty>(value.clone()).map_err(|_| invalid(kind))?
            };
        }
        match kind {
            "series" => {
                let series = typed!(Series);
                if json_key(&series.id) != id {
                    return Err(invalid("series identity"));
                }
                parts.series.push(series);
            }
            "chapter" => {
                let chapter = typed!(Chapter);
                if json_key(&chapter.id) != id {
                    return Err(invalid("chapter identity"));
                }
                parts.chapters.push(chapter);
            }
            "entry" => {
                let mut value = value.clone();
                let names: Vec<String> = value
                    .get("categories")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|_| invalid("categories"))?
                    .unwrap_or_default();
                value["categories"] = json!([]);
                let entry: LibraryEntry =
                    serde_json::from_value(value).map_err(|_| invalid("entry"))?;
                if json_key(&entry.series) != id
                    || names.iter().any(|name| name != &category_key(name))
                {
                    return Err(invalid("entry identity"));
                }
                parts.library.push((entry, names));
            }
            "category" => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Row {
                    name: String,
                }
                let row = typed!(Row);
                if category_key(&row.name) != id || row.name.trim().is_empty() {
                    return Err(invalid("category identity"));
                }
                parts.categories.push((id.to_owned(), row.name));
            }
            "progress" => {
                let progress = typed!(ReadingProgress);
                if json_key(&progress.chapter) != id {
                    return Err(invalid("progress identity"));
                }
                parts.progress.push(progress);
            }
            "history" => {
                let history = typed!(HistoryEntry);
                if json_key(&history.chapter) != id {
                    return Err(invalid("history identity"));
                }
                parts.history.push(history);
            }
            "bookmark" => {
                let bookmark = typed!(Bookmark);
                if bookmark_identity(&bookmark) != id {
                    return Err(invalid("bookmark identity"));
                }
                parts.bookmarks.push(bookmark);
            }
            _ => return Err(invalid("kind")),
        }
    }
    Ok(parts)
}

/// Check the decoded records as a whole: the same invariants a stored state
/// must satisfy, so an archive cannot smuggle in a dangling reference.
fn validate_state(name: &str, payload: &Payload) -> Result<(), String> {
    let parts = parse_state(name, payload)?;
    let mut state = ReaderState {
        series: parts.series,
        chapters: parts.chapters,
        progress: parts.progress,
        history: parts.history,
        bookmarks: parts.bookmarks,
        ..ReaderState::default()
    };
    let names: BTreeSet<&String> = parts
        .library
        .iter()
        .flat_map(|(_, names)| names)
        .chain(parts.categories.iter().map(|(key, _)| key))
        .collect();
    state.categories = names
        .into_iter()
        .map(|name| Category {
            id: name.clone(),
            name: name.clone(),
        })
        .collect();
    state.library = parts
        .library
        .into_iter()
        .map(|(mut entry, names)| {
            entry.categories = names.into_iter().collect();
            entry
        })
        .collect();
    // The library of a component that has no chapters of its own is valid with
    // series alone; progress-like components carry every chapter they name.
    state.validate().map_err(|e| e.to_string())
}

fn put_support(payload: &mut Payload, state: &ReaderState, chapters: &BTreeSet<&ChapterId>) {
    let mut series = BTreeSet::new();
    for chapter in state.chapters.iter().filter(|c| chapters.contains(&c.id)) {
        series.insert(&chapter.id.series);
        payload.records.insert(
            key("chapter", &json_key(&chapter.id)),
            serde_json::to_value(chapter).expect("chapter serializes"),
        );
    }
    for series in state.series.iter().filter(|s| series.contains(&s.id)) {
        payload.records.insert(
            key("series", &json_key(&series.id)),
            serde_json::to_value(series).expect("series serializes"),
        );
    }
}

pub(crate) fn capture_state(name: &str, state: &ReaderState) -> Result<Payload, String> {
    let mut payload = empty();
    let mut chapters = BTreeSet::new();
    match name {
        "library" => {
            let names: BTreeMap<&String, String> = state
                .categories
                .iter()
                .map(|c| (&c.id, category_key(&c.name)))
                .collect();
            let mut series = BTreeSet::new();
            for entry in &state.library {
                let mut value = serde_json::to_value(entry).map_err(|e| e.to_string())?;
                let mut categories: Vec<&String> = entry
                    .categories
                    .iter()
                    .filter_map(|id| names.get(id))
                    .collect();
                categories.sort();
                categories.dedup();
                value["categories"] = json!(categories);
                payload
                    .records
                    .insert(key("entry", &json_key(&entry.series)), value);
                series.insert(&entry.series);
            }
            // A title's chapter list is cache, but it is what makes a restored
            // library usable before each title is opened again.
            chapters.extend(
                state
                    .chapters
                    .iter()
                    .filter(|c| series.contains(&c.id.series))
                    .map(|c| &c.id),
            );
            put_support(&mut payload, state, &chapters);
            for title in state.series.iter().filter(|s| series.contains(&s.id)) {
                payload.records.insert(
                    key("series", &json_key(&title.id)),
                    serde_json::to_value(title).map_err(|e| e.to_string())?,
                );
            }
        }
        "categories" => {
            for category in &state.categories {
                payload
                    .records
                    .entry(key("category", &category_key(&category.name)))
                    .or_insert_with(|| json!({"name": category.name.trim()}));
            }
        }
        "progress" => {
            for progress in &state.progress {
                chapters.insert(&progress.chapter);
                payload.records.insert(
                    key("progress", &json_key(&progress.chapter)),
                    serde_json::to_value(progress).map_err(|e| e.to_string())?,
                );
            }
            put_support(&mut payload, state, &chapters);
        }
        "history" => {
            for history in &state.history {
                chapters.insert(&history.chapter);
                payload.records.insert(
                    key("history", &json_key(&history.chapter)),
                    serde_json::to_value(history).map_err(|e| e.to_string())?,
                );
            }
            put_support(&mut payload, state, &chapters);
        }
        "bookmarks" => {
            for bookmark in &state.bookmarks {
                chapters.insert(&bookmark.chapter);
                payload.records.insert(
                    key("bookmark", &bookmark_identity(bookmark)),
                    serde_json::to_value(bookmark).map_err(|e| e.to_string())?,
                );
            }
            put_support(&mut payload, state, &chapters);
        }
        _ => return Err("Unknown state component".into()),
    }
    Ok(payload)
}

// ------------------------------------------------------------- settings rows

/// Documents of a profile that the `settings` component holds as they are
/// saved, apart from `reader:download-cleanup`, whose category IDs are carried
/// as names. `reader:library-navigation:v1` is the last Library tab and scroll
/// position of this session: it is not a setting and is not exported.
pub(crate) const SETTINGS_KEYS: &[&str] = &[
    "reader:show-adult",
    "reader:reader-settings",
    "reader:appearance",
    "reader:source-browser",
    "reader:download-cleanup",
    "reader:last-update",
];
const TITLE_PREFIXES: &[&str] = &[
    "reader:chapter-order:",
    "reader:reader-settings-series:",
    "reader:mode-choice:",
    "reader:content-presentation:",
];
pub(crate) const PRESENTATION_PREFIX: &str = "reader:content-presentation:";
pub(crate) const OVERRIDES_KEY: &str = "reader:sync-settings-overrides";
pub(crate) const DISABLED_PREFIX: &str = "reader:disabled-catalogs:";
const DEVICE_KEYS: &[&str] = &[
    "reader:image-storage",
    "reader:image-limits",
    "reader:captures",
];
pub(crate) const COMPRESSION_PREFIX: &str = "reader:source-compression:";

/// The SeriesId at the start of `suffix` (a per-title key's tail).
fn leading_series(suffix: &str) -> Option<SeriesId> {
    let mut stream = serde_json::Deserializer::from_str(suffix).into_iter::<SeriesId>();
    let id = stream.next()?.ok()?;
    (stream.byte_offset() == suffix.len()).then_some(id)
}
pub(crate) fn title_key_family(logical: &str) -> Option<&'static str> {
    let prefix = TITLE_PREFIXES.iter().find(|p| logical.starts_with(**p))?;
    leading_series(&logical[prefix.len()..]).map(|_| *prefix)
}
fn json_row(raw: &str) -> Option<Value> {
    (raw.len() <= MAX_VALUE_BYTES)
        .then(|| serde_json::from_str(raw).ok())
        .flatten()
}

/// `reader:download-cleanup` with excluded category IDs replaced by names.
fn cleanup_by_name(value: &Value, categories: &[Category]) -> Value {
    let mut value = value.clone();
    if let Some(ids) = value.get("excluded_categories").and_then(Value::as_array) {
        let mut names: Vec<String> = ids
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|id| categories.iter().find(|c| c.id == id))
            .map(|c| category_key(&c.name))
            .collect();
        names.sort();
        names.dedup();
        if let Some(object) = value.as_object_mut() {
            object.remove("excluded_categories");
            object.insert("excluded_category_names".into(), json!(names));
        }
    }
    value
}
/// The inverse of [`cleanup_by_name`] against a profile's categories.
pub(crate) fn cleanup_by_id(value: &Value, categories: &[Category]) -> Value {
    let mut value = value.clone();
    if let Some(names) = value
        .get("excluded_category_names")
        .and_then(Value::as_array)
    {
        let ids: BTreeSet<&str> = names
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|name| {
                categories
                    .iter()
                    .find(|c| category_key(&c.name) == name)
                    .map(|c| c.id.as_str())
            })
            .collect();
        if let Some(object) = value.as_object_mut() {
            object.remove("excluded_category_names");
            object.insert("excluded_categories".into(), json!(ids));
        }
    }
    value
}

pub(crate) struct ProfileCapture<'a> {
    pub state: &'a ReaderState,
    pub rows: &'a Rows,
    pub registry: &'a reader_sources::SourceRegistry,
}

fn capture_settings(cx: &ProfileCapture) -> Payload {
    let mut payload = empty();
    for logical in SETTINGS_KEYS {
        let Some(value) = cx.rows.get(*logical).and_then(|raw| json_row(raw)) else {
            continue;
        };
        let value = if *logical == "reader:download-cleanup" {
            cleanup_by_name(&value, &cx.state.categories)
        } else {
            value
        };
        payload.records.insert(key("kv", logical), value);
    }
    payload.records.insert(
        key("preferences", "state"),
        serde_json::to_value(&cx.state.preferences).expect("preferences serialize"),
    );
    payload
}
fn capture_title_settings(cx: &ProfileCapture) -> Payload {
    let mut payload = empty();
    for (logical, raw) in cx.rows {
        if title_key_family(logical).is_some()
            && let Some(value) = json_row(raw)
        {
            payload.records.insert(key("kv", logical), value);
        }
    }
    payload
}
fn capture_overrides(cx: &ProfileCapture) -> Payload {
    let mut payload = empty();
    if let Some(Value::Object(map)) = cx.rows.get(OVERRIDES_KEY).and_then(|raw| json_row(raw)) {
        for (field, value) in map {
            payload.records.insert(key("override", &field), value);
        }
    }
    payload
}
fn capture_addons(cx: &ProfileCapture) -> Result<Payload, String> {
    let mut payload = empty();
    for package in cx.registry.list().map_err(|e| e.to_string())? {
        let exported = cx
            .registry
            .export_settings(&package.id)
            .map_err(|e| e.to_string())?;
        let disabled: BTreeSet<String> = cx
            .rows
            .get(&format!("{DISABLED_PREFIX}{}", json_key(&package.id)))
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();
        payload.records.insert(
            key("addon", &package.id),
            json!({"settings": exported.values, "disabled_catalogs": disabled}),
        );
    }
    Ok(payload)
}
fn capture_secrets(cx: &ProfileCapture) -> Result<Payload, String> {
    let mut payload = empty();
    for package in cx.registry.list().map_err(|e| e.to_string())? {
        let exported = cx
            .registry
            .export_settings(&package.id)
            .map_err(|e| e.to_string())?;
        if !exported.secrets.is_empty() {
            payload
                .records
                .insert(key("secret", &package.id), json!(exported.secrets));
        }
    }
    Ok(payload)
}

pub(crate) fn capture_profile(name: &str, cx: &ProfileCapture) -> Result<Payload, String> {
    match name {
        "library" | "categories" | "progress" | "history" | "bookmarks" => {
            capture_state(name, cx.state)
        }
        "settings" => Ok(capture_settings(cx)),
        "title_settings" => Ok(capture_title_settings(cx)),
        "sync_overrides" => Ok(capture_overrides(cx)),
        "addons" => capture_addons(cx),
        "addon_secrets" => capture_secrets(cx),
        _ => Err("Unknown profile component".into()),
    }
}

// --------------------------------------------------------------------- device

pub(crate) struct DeviceCapture<'a> {
    pub rows: &'a Rows,
    pub registry: &'a reader_sources::SourceRegistry,
}
pub(crate) fn capture_device(name: &str, cx: &DeviceCapture) -> Result<Payload, String> {
    let mut payload = empty();
    match name {
        "device_settings" => {
            for (logical, raw) in cx.rows {
                if (DEVICE_KEYS.contains(&logical.as_str())
                    || logical.starts_with(COMPRESSION_PREFIX))
                    && let Some(value) = json_row(raw)
                {
                    payload.records.insert(key("kv", logical), value);
                }
            }
            if let Some(raw) = cx.rows.get(super::DEVICE_PREFERENCES_KEY)
                && let Ok(preferences) = serde_json::from_str::<Value>(raw)
                && let Some(open) = preferences.get("open_last_profile_on_startup")
                && open.is_boolean()
            {
                payload
                    .records
                    .insert(key("device", "open_last_profile_on_startup"), open.clone());
            }
        }
        "packages" => {
            for package in cx.registry.list().map_err(|e| e.to_string())? {
                let (sha256, _) = cx
                    .registry
                    .archive(&package.id)
                    .map_err(|e| e.to_string())?;
                payload.records.insert(
                    key("package", &package.id),
                    json!({
                        "id": package.id,
                        "name": package.name,
                        "version": package.version,
                        "sha256": sha256,
                        "enabled": package.enabled,
                    }),
                );
            }
        }
        _ => return Err("Unknown device component".into()),
    }
    Ok(payload)
}

// ----------------------------------------------------------------- validation

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_addon_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn sized(value: &Value) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= MAX_VALUE_BYTES)
}

/// Whether `value` is an acceptable saved form of the document at `logical`.
fn valid_document(logical: &str, value: &Value) -> bool {
    if !sized(value) {
        return false;
    }
    match logical {
        "reader:show-adult"
        | "reader:reader-settings"
        | "reader:appearance"
        | "reader:image-storage"
        | "reader:captures" => crate::sync_settings::valid_stored(logical, value),
        "reader:download-cleanup" => {
            value.is_object()
                && serde_json::from_value::<crate::download_cleanup::Settings>(cleanup_by_id(
                    value,
                    &[],
                ))
                .is_ok()
        }
        "reader:image-limits" => {
            serde_json::from_value::<crate::image_limits::Settings>(value.clone())
                .is_ok_and(|s| s.validate().is_ok())
        }
        "reader:source-browser" => value.is_object(),
        "reader:last-update" => value.is_number() || value.is_string(),
        _ if logical.starts_with(COMPRESSION_PREFIX) => serde_json::from_value::<
            Option<crate::source_compression::CompressionOverride>,
        >(value.clone())
        .is_ok_and(|v| v.is_none_or(|v| v.valid())),
        _ if logical.starts_with("reader:chapter-order:") => value.is_boolean(),
        _ if logical.starts_with("reader:reader-settings-series:")
            || logical.starts_with("reader:mode-choice:") =>
        {
            crate::sync_settings::valid_stored(logical, value)
        }
        _ if logical.starts_with(PRESENTATION_PREFIX) => {
            crate::content_presentation::valid_stored(value)
        }
        _ => false,
    }
}

fn each_record(
    payload: &Payload,
    mut check: impl FnMut(&str, &str, &Value) -> bool,
) -> Result<(), String> {
    for (record, value) in &payload.records {
        let Some((kind, id)) = split(record) else {
            return Err("Invalid record identity".into());
        };
        if !check(kind, id, value) {
            return Err(format!("Invalid {kind} record"));
        }
    }
    Ok(())
}

pub(crate) fn validate(name: &str, payload: &Payload) -> Result<(), String> {
    match name {
        "library" | "categories" | "progress" | "history" | "bookmarks" => {
            validate_state(name, payload)
        }
        "settings" => each_record(payload, |kind, id, value| match kind {
            "kv" => SETTINGS_KEYS.contains(&id) && valid_document(id, value),
            "preferences" => {
                id == "state" && serde_json::from_value::<ReaderPreferences>(value.clone()).is_ok()
            }
            _ => false,
        }),
        "title_settings" => each_record(payload, |kind, id, value| {
            kind == "kv" && title_key_family(id).is_some() && valid_document(id, value)
        }),
        "sync_overrides" => each_record(payload, |kind, id, value| {
            kind == "override" && !id.is_empty() && id.len() <= 512 && sized(value)
        }),
        "addons" => each_record(payload, |kind, id, value| {
            kind == "addon"
                && valid_addon_id(id)
                && value.get("settings").is_some_and(Value::is_object)
                && value
                    .get("disabled_catalogs")
                    .and_then(Value::as_array)
                    .is_some_and(|ids| {
                        ids.len() <= 1024
                            && ids.iter().all(|c| {
                                c.as_str().is_some_and(|c| {
                                    c.len() <= 256 && (c == id || c.starts_with(&format!("{id}.")))
                                })
                            })
                    })
                && sized(value)
        }),
        "addon_secrets" => each_record(payload, |kind, id, value| {
            kind == "secret"
                && valid_addon_id(id)
                && value.as_object().is_some_and(|map| {
                    map.len() <= 64
                        && map.values().all(|v| {
                            v.as_str()
                                .is_some_and(|s| s.len() <= 4096 && !s.contains('\0'))
                        })
                })
        }),
        "device_settings" => each_record(payload, |kind, id, value| match kind {
            "kv" => {
                (DEVICE_KEYS.contains(&id) || id.starts_with(COMPRESSION_PREFIX))
                    && valid_document(id, value)
            }
            "device" => id == "open_last_profile_on_startup" && value.is_boolean(),
            _ => false,
        }),
        "packages" => each_record(payload, |kind, id, value| {
            kind == "package"
                && value["id"].as_str() == Some(id)
                && valid_addon_id(id)
                && value["version"]
                    .as_str()
                    .is_some_and(|v| !v.is_empty() && v.len() <= 128)
                && value["sha256"].as_str().is_some_and(|h| valid_hex(h, 64))
                && value["enabled"].is_boolean()
        }),
        _ => Err("Unknown component".into()),
    }
}
