//! Turning an opened archive and the user's choices into an exact, reviewable
//! plan: for every target the records before and after, the counts, and the
//! rows each store will receive. Nothing here writes; `apply.rs` commits a plan
//! that is still valid for the revisions it was built against.
use super::{
    Snapshot,
    archive::{self, Archive, DEVICE, Payload},
    components::{self, Rows, key, split},
    merge::{self, Counts},
};
use crate::{content_presentation as presentation, services::Services};
use nova_reader_ui::backend_text as text;
use reader_domain::{Category, ReaderState, SeriesId};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// An archive that has been read and validated.
pub struct Opened {
    pub archive: Archive,
    /// The file the archive was read from, which restored chapter files stream
    /// from. Empty for an undo.
    pub path: std::path::PathBuf,
    /// Delete the file when this is dropped: a copy made for this import.
    pub temporary: bool,
    /// A metadata backup from before archives existed, wrapped as one profile.
    pub legacy: bool,
}

impl Drop for Opened {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Open a backup file of either format. Nothing is applied.
pub fn open(path: &std::path::Path) -> Result<Opened, String> {
    if archive::is_zip(path)? {
        Ok(Opened {
            archive: archive::decode_file(path)?,
            path: path.to_owned(),
            temporary: false,
            legacy: false,
        })
    } else {
        super::legacy::open(path)
    }
}

/// How one archive entry (a profile, or the device) is restored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub included: bool,
    /// The entry's `archive_profile_id` in the archive.
    pub source: String,
    /// The profile it is restored into (`DEVICE` for the device entry).
    pub target: String,
    pub name: String,
    /// The target is a new profile that the restore creates.
    pub create: bool,
    pub components: BTreeSet<String>,
    pub replace: bool,
    pub use_backup: BTreeSet<String>,
}

pub(crate) fn present(opened: &Opened, source: &str) -> BTreeSet<String> {
    opened
        .archive
        .payloads
        .keys()
        .filter(|(id, _)| id == source)
        .map(|(_, name)| name.clone())
        .collect()
}

/// Starting choices: a profile with the id of a profile on this device is
/// restored into it (the Default profile of a fresh device, or the same profile
/// of the same account), anything else becomes a new profile.
pub fn default_choices(opened: &Opened) -> Result<Vec<Choice>, String> {
    let live = crate::profiles::list()?;
    let mut choices = Vec::new();
    for profile in &opened.archive.manifest.profiles {
        let components: BTreeSet<String> = present(opened, &profile.archive_profile_id)
            .into_iter()
            .filter(|name| {
                // Large file sets are opt-in on import as they are on export.
                profile.archive_profile_id != DEVICE
                    || components::definition(name).is_some_and(|def| def.everything)
            })
            .collect();
        if profile.archive_profile_id == DEVICE {
            choices.push(Choice {
                included: true,
                source: DEVICE.into(),
                target: DEVICE.into(),
                name: profile.name.clone(),
                create: false,
                components,
                replace: false,
                use_backup: BTreeSet::new(),
            });
            continue;
        }
        let existing = live
            .iter()
            .find(|candidate| candidate.id == profile.source_profile_id);
        choices.push(Choice {
            included: true,
            source: profile.archive_profile_id.clone(),
            target: match existing {
                Some(existing) => existing.id.clone(),
                None => crate::profiles::allocate_id()?,
            },
            name: existing.map_or_else(|| profile.name.clone(), |p| p.name.clone()),
            create: existing.is_none(),
            components,
            replace: opened.legacy,
            use_backup: BTreeSet::new(),
        });
    }
    Ok(choices)
}

#[derive(Clone, Debug)]
pub(crate) struct PreviewRow {
    pub profile: String,
    pub component: String,
    pub counts: Counts,
    pub notes: Vec<String>,
}

/// Everything a profile target receives.
#[derive(Clone, Default)]
pub(crate) struct TargetPlan {
    pub before: BTreeMap<String, Payload>,
    pub after: BTreeMap<String, Payload>,
    /// The state the profile ends with, with the one it started from.
    pub state: Option<(ReaderState, ReaderState)>,
    /// Profile rows by logical key; `None` removes the row.
    pub kv: BTreeMap<String, Option<String>>,
    /// Per-title presentation, which also publishes an account record.
    pub presentation: BTreeMap<SeriesId, Option<presentation::Settings>>,
    /// The synced settings documents this restore writes.
    pub synced_documents: BTreeSet<String>,
    pub addon_settings: BTreeMap<String, Map<String, Value>>,
    pub addon_secrets: BTreeMap<String, Map<String, Value>>,
    /// Addons whose credentials a Replace clears.
    pub addon_secrets_cleared: BTreeSet<String>,
}
#[derive(Clone, Default)]
pub(crate) struct DevicePlan {
    pub before: BTreeMap<String, Payload>,
    pub after: BTreeMap<String, Payload>,
    pub kv: BTreeMap<String, Option<String>>,
    pub open_last_profile: Option<bool>,
    /// Packages to install: (id, sha256).
    pub packages: Vec<(String, String)>,
    /// Documents of the selected profile's synced settings that change.
    pub synced_documents: BTreeSet<String>,
    pub files: super::files::Plan,
}

pub struct Preview {
    pub opened: Arc<Opened>,
    pub choices: Vec<Choice>,
    pub(crate) rows: Vec<PreviewRow>,
    pub(crate) plans: BTreeMap<String, TargetPlan>,
    pub(crate) device: Option<DevicePlan>,
    pub(crate) revisions: BTreeMap<String, String>,
    pub(crate) device_revision: Option<String>,
    /// Lines that concern the whole restore, not one component.
    pub notes: Vec<String>,
}
impl Preview {
    /// A readable line for each previewed component.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = self.notes.clone();
        for row in &self.rows {
            let c = &row.counts;
            let label = components::definition(&row.component)
                .map_or(row.component.as_str(), |def| def.label);
            let mut line = format!(
                "{} · {label}: {} in backup, {} new, {} identical, {} conflicting, {} updated, {} kept, {} deleted",
                row.profile,
                c.archived,
                c.added,
                c.identical,
                c.conflicts,
                c.updated,
                c.preserved,
                c.deleted
            );
            if !c.examples.is_empty() {
                line.push_str(&format!(" (e.g. {})", c.examples.join("; ")));
            }
            for note in &row.notes {
                line.push_str(&format!("\n{note}"));
            }
            lines.push(line);
        }
        lines
    }
}

fn read_json(raw: Option<&String>) -> Option<Value> {
    raw.and_then(|raw| serde_json::from_str(raw).ok())
}

/// Synced settings documents: restoring one publishes its changed fields.
fn synced(logical: &str) -> bool {
    matches!(
        logical,
        "reader:show-adult"
            | "reader:reader-settings"
            | "reader:appearance"
            | "reader:captures"
            | "reader:image-storage"
    ) || logical.starts_with("reader:reader-settings-series:")
        || logical.starts_with("reader:mode-choice:")
}

/// Rows a restored `kv\0key` record set implies against the profile's rows:
/// everything wanted is written when it differs, and in Replace mode every
/// local row of the component that the backup lacks is removed.
fn kv_edits(
    after: &Payload,
    rows: &Rows,
    universe: impl Fn(&str) -> bool,
    transform: impl Fn(&str, &Value) -> Value,
    out: &mut BTreeMap<String, Option<String>>,
    documents: &mut BTreeSet<String>,
) {
    let mut wanted = BTreeSet::new();
    for (record, value) in &after.records {
        let Some(("kv", logical)) = split(record) else {
            continue;
        };
        wanted.insert(logical.to_owned());
        let value = transform(logical, value);
        if read_json(rows.get(logical)).as_ref() != Some(&value) {
            out.insert(logical.to_owned(), Some(value.to_string()));
            if synced(logical) {
                documents.insert(logical.to_owned());
            }
        }
    }
    for logical in rows.keys().filter(|logical| universe(logical)) {
        if !wanted.contains(logical) {
            out.insert(logical.clone(), None);
            if synced(logical) {
                documents.insert(logical.clone());
            }
        }
    }
}

fn plan_profile(
    services: &Services,
    opened: &Opened,
    snapshot: &Snapshot,
    choice: &Choice,
    rows_out: &mut Vec<PreviewRow>,
) -> Result<TargetPlan, String> {
    let (state, rows) = if choice.create {
        (ReaderState::default(), Rows::new())
    } else {
        (
            snapshot
                .states
                .get(&choice.target)
                .ok_or("The target profile changed; rebuild the preview")?
                .clone(),
            snapshot.rows[&choice.target].clone(),
        )
    };
    let registry = services
        .sources
        .with_profile(crate::profiles::registry_profile(&choice.target));
    let capture = components::ProfileCapture {
        state: &state,
        rows: &rows,
        registry: &registry,
    };
    let mut plan = TargetPlan::default();
    let installed: BTreeSet<String> = services
        .sources
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|source| source.id)
        .collect();
    let incoming: BTreeSet<String> = opened
        .archive
        .payloads
        .get(&(DEVICE.to_owned(), "packages".to_owned()))
        .map(|payload| {
            payload
                .records
                .keys()
                .filter_map(|record| split(record).map(|(_, id)| id.to_owned()))
                .collect()
        })
        .unwrap_or_default();
    for name in &choice.components {
        let backup = opened
            .archive
            .payloads
            .get(&(choice.source.clone(), name.clone()))
            .ok_or("A selected backup component is missing")?;
        let local = if choice.create {
            components::empty()
        } else {
            components::capture_profile(name, &capture)?
        };
        let use_backup = choice.use_backup.contains(name);
        let after = match opened
            .archive
            .undo_after
            .as_ref()
            .and_then(|after| after.get(&(choice.target.clone(), name.clone())))
        {
            Some(imported) => merge::undo_merge(&local, backup, imported, use_backup),
            None => merge::merge(name, &local, backup, choice.replace, use_backup),
        };
        let counts = merge::count(name, &local, backup, &after);
        let mut notes = Vec::new();
        if counts.skipped > 0 {
            notes.push(format!(
                "{}: {}",
                text::tr("Skipped unsupported values"),
                counts.skipped
            ));
        }
        if matches!(name.as_str(), "addons" | "addon_secrets") {
            for record in after.records.keys() {
                if let Some((_, id)) = split(record)
                    && !installed.contains(id)
                    && !incoming.contains(id)
                {
                    notes.push(format!(
                        "{id}: {}",
                        text::tr("Reinstall needed; settings are skipped")
                    ));
                }
            }
        }
        rows_out.push(PreviewRow {
            profile: choice.name.clone(),
            component: name.clone(),
            counts,
            notes,
        });
        plan.before.insert(name.clone(), local);
        plan.after.insert(name.clone(), after);
    }

    // The state and the rows that follow its categories.
    let state_names = ["library", "categories", "progress", "history", "bookmarks"];
    let touches_state = choice
        .components
        .iter()
        .any(|name| state_names.contains(&name.as_str()) || name == "settings");
    let mut categories: Vec<Category> = state.categories.clone();
    if touches_state {
        let materialized = components::materialize_state(&state, &plan.after, || {
            Ok(format!(
                "category-{:016x}-{}",
                nova_sync::local_device().map_err(|e| e.to_string())?,
                nova_storage::try_allocate_local_sequence("reader:category", 1)
                    .map_err(|e| e.to_string())?
            ))
        })?;
        if materialized.omitted_categories > 0
            && let Some(row) = rows_out
                .iter_mut()
                .rev()
                .find(|row| row.component == "library" && row.profile == choice.name)
        {
            row.notes.push(format!(
                "{}: {}",
                text::tr("Category assignments omitted because their categories are unavailable"),
                materialized.omitted_categories
            ));
        }
        categories = materialized.state.categories.clone();
        plan.state = Some((state.clone(), materialized.state));
    }
    plan_rows(&mut plan, &rows, &categories, choice);
    Ok(plan)
}

/// Rows, presentation and addon settings that follow from the merged payloads.
fn plan_rows(plan: &mut TargetPlan, rows: &Rows, categories: &[Category], choice: &Choice) {
    let after = plan.after.clone();
    let mut documents = BTreeSet::new();
    for (name, payload) in &after {
        match name.as_str() {
            "settings" => kv_edits(
                payload,
                rows,
                |logical| components::SETTINGS_KEYS.contains(&logical),
                |logical, value| {
                    if logical == "reader:download-cleanup" {
                        components::cleanup_by_id(value, categories)
                    } else {
                        value.clone()
                    }
                },
                &mut plan.kv,
                &mut documents,
            ),
            "title_settings" => {
                let mut ordinary = BTreeMap::new();
                let mut wanted_titles = BTreeSet::new();
                for (record, value) in &payload.records {
                    let Some(("kv", logical)) = split(record) else {
                        continue;
                    };
                    if let Some(suffix) = logical.strip_prefix(components::PRESENTATION_PREFIX)
                        && let Ok(series) = serde_json::from_str::<SeriesId>(suffix)
                    {
                        let settings = presentation::settings_of(value);
                        let local = rows
                            .get(logical)
                            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                            .and_then(|value| presentation::settings_of(&value));
                        wanted_titles.insert(series.clone());
                        if settings != local {
                            plan.presentation.insert(series, settings);
                        }
                    } else {
                        ordinary.insert(record.clone(), value.clone());
                    }
                }
                for logical in rows.keys() {
                    if let Some(suffix) = logical.strip_prefix(components::PRESENTATION_PREFIX)
                        && let Ok(series) = serde_json::from_str::<SeriesId>(suffix)
                        && !wanted_titles.contains(&series)
                    {
                        plan.presentation.insert(series, None);
                    }
                }
                kv_edits(
                    &Payload {
                        version: 1,
                        records: ordinary,
                    },
                    rows,
                    |logical| {
                        components::title_key_family(logical)
                            .is_some_and(|family| family != components::PRESENTATION_PREFIX)
                    },
                    |_, value| value.clone(),
                    &mut plan.kv,
                    &mut documents,
                );
            }
            "sync_overrides" => {
                let map: Map<String, Value> = payload
                    .records
                    .iter()
                    .filter_map(|(record, value)| {
                        split(record)
                            .filter(|(kind, _)| *kind == "override")
                            .map(|(_, field)| (field.to_owned(), value.clone()))
                    })
                    .collect();
                let local = read_json(rows.get(components::OVERRIDES_KEY));
                if map.is_empty() {
                    if rows.contains_key(components::OVERRIDES_KEY) {
                        plan.kv.insert(components::OVERRIDES_KEY.into(), None);
                    }
                } else if local.as_ref() != Some(&Value::Object(map.clone())) {
                    plan.kv.insert(
                        components::OVERRIDES_KEY.into(),
                        Some(Value::Object(map).to_string()),
                    );
                }
            }
            "addons" => {
                let mut wanted = BTreeSet::new();
                for (record, value) in &payload.records {
                    let Some(("addon", id)) = split(record) else {
                        continue;
                    };
                    let row = format!(
                        "{}{}",
                        components::DISABLED_PREFIX,
                        serde_json::to_string(id).unwrap()
                    );
                    wanted.insert(row.clone());
                    let disabled = value["disabled_catalogs"].clone();
                    let empty = disabled.as_array().is_some_and(Vec::is_empty);
                    if read_json(rows.get(&row)).unwrap_or(Value::Array(Vec::new())) != disabled {
                        plan.kv.insert(
                            row,
                            if empty {
                                None
                            } else {
                                Some(disabled.to_string())
                            },
                        );
                    }
                    plan.addon_settings
                        .insert(id.to_owned(), components::object(&value["settings"]));
                }
                for row in rows
                    .keys()
                    .filter(|row| row.starts_with(components::DISABLED_PREFIX))
                {
                    if !wanted.contains(row) && choice.replace {
                        plan.kv.insert(row.clone(), None);
                    }
                }
            }
            "addon_secrets" => {
                for (record, value) in &payload.records {
                    if let Some(("secret", id)) = split(record) {
                        plan.addon_secrets
                            .insert(id.to_owned(), components::object(value));
                    }
                }
                if choice.replace
                    && let Some(local) = plan.before.get("addon_secrets")
                {
                    for record in local.records.keys() {
                        if let Some((_, id)) = split(record)
                            && !payload.records.contains_key(record)
                        {
                            plan.addon_secrets_cleared.insert(id.to_owned());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    plan.synced_documents = documents;
}

fn plan_device(
    services: &Services,
    opened: &Opened,
    snapshot: &Snapshot,
    choice: &Choice,
    rows_out: &mut Vec<PreviewRow>,
) -> Result<DevicePlan, String> {
    let capture = components::DeviceCapture {
        rows: &snapshot.device_rows,
        registry: &services.sources,
        downloads: &services.downloads,
    };
    let mut plan = DevicePlan::default();
    for name in &choice.components {
        let backup = opened
            .archive
            .payloads
            .get(&(choice.source.clone(), name.clone()))
            .ok_or("A selected backup component is missing")?;
        let local = components::capture_device(name, &capture)?;
        let use_backup = choice.use_backup.contains(name);
        let after = match opened
            .archive
            .undo_after
            .as_ref()
            .and_then(|after| after.get(&(choice.target.clone(), name.clone())))
        {
            Some(imported) => merge::undo_merge(&local, backup, imported, use_backup),
            None => merge::merge(name, &local, backup, choice.replace, use_backup),
        };
        let counts = merge::count(name, &local, backup, &after);
        let mut row_notes = Vec::new();
        match name.as_str() {
            "packages" => {
                for (record, value) in &after.records {
                    if local.records.contains_key(record) {
                        if local.records[record] != *value {
                            row_notes.push(format!(
                                "{}: {}",
                                value["id"].as_str().unwrap_or(""),
                                text::tr("Kept the version installed here")
                            ));
                        }
                        continue;
                    }
                    let id = value["id"].as_str().unwrap_or_default().to_owned();
                    let hash = value["sha256"].as_str().unwrap_or_default().to_owned();
                    if opened.archive.packages.contains_key(&hash) {
                        if value["enabled"] == Value::Bool(true) {
                            row_notes.push(format!(
                                "{id}: {}",
                                text::tr("Installed disabled; enable it in Browse")
                            ));
                        }
                        plan.packages.push((id, hash));
                    }
                }
            }
            "device_settings" => {
                let mut documents = BTreeSet::new();
                kv_edits(
                    &after,
                    &snapshot.device_rows,
                    |logical| {
                        logical.starts_with(components::COMPRESSION_PREFIX)
                            || matches!(
                                logical,
                                "reader:image-storage" | "reader:image-limits" | "reader:captures"
                            )
                    },
                    |_, value| value.clone(),
                    &mut plan.kv,
                    &mut documents,
                );
                plan.open_last_profile = after
                    .records
                    .get(&key("device", "open_last_profile_on_startup"))
                    .and_then(Value::as_bool);
                if plan.open_last_profile
                    == Some(crate::profiles::device_preferences().open_last_profile_on_startup)
                {
                    plan.open_last_profile = None;
                }
                // The portable subset of these documents is also part of the
                // selected profile's synced settings.
                plan.synced_documents = documents;
            }
            "downloads" | "local_imports" => {
                super::files::plan(name, &local, &after, &mut plan.files, &mut row_notes)
            }
            _ => {}
        }
        rows_out.push(PreviewRow {
            profile: choice.name.clone(),
            component: name.clone(),
            counts,
            notes: row_notes,
        });
        plan.before.insert(name.clone(), local);
        plan.after.insert(name.clone(), after);
    }
    Ok(plan)
}

/// Build the preview of `choices` against the stores as they are now.
pub fn preview(
    services: &Services,
    opened: Arc<Opened>,
    choices: Vec<Choice>,
) -> Result<Preview, String> {
    let choices: Vec<Choice> = choices
        .into_iter()
        .filter(|choice| choice.included && !choice.components.is_empty())
        .collect();
    if choices.is_empty() {
        return Err("Choose at least one profile or the device".into());
    }
    let (mut targets, mut sources) = (BTreeSet::new(), BTreeSet::new());
    for choice in &choices {
        if !targets.insert(choice.target.clone()) || !sources.insert(choice.source.clone()) {
            return Err("Choose one distinct target for each backup profile".into());
        }
        if choice.source == DEVICE {
            if choice.target != DEVICE || choice.create {
                return Err("The device entry restores to this device".into());
            }
        } else {
            if choice.target == DEVICE || !crate::profiles::valid_id(&choice.target) {
                return Err("Invalid target profile".into());
            }
            if choice.name.trim().is_empty()
                || choice.name.chars().count() > 64
                || choice.name.chars().any(char::is_control)
            {
                return Err(text::tr("Enter a profile name of up to 64 characters."));
            }
        }
    }
    let ids: Vec<String> = choices
        .iter()
        .filter(|choice| choice.source != DEVICE && !choice.create)
        .map(|choice| choice.target.clone())
        .collect();
    let snapshot = Snapshot::take(services, &ids)?;
    let mut preview = Preview {
        opened: opened.clone(),
        choices: choices.clone(),
        rows: Vec::new(),
        plans: BTreeMap::new(),
        device: None,
        revisions: BTreeMap::new(),
        device_revision: None,
        notes: Vec::new(),
    };
    {
        let handle = nova_sync::local_store().map_err(|e| e.to_string())?;
        let store = handle.lock().unwrap_or_else(|e| e.into_inner());
        for choice in choices.iter().filter(|c| c.source != DEVICE) {
            check_owner(&choice.target, &store, choice.create)?;
            let revision = match snapshot.revisions.get(&choice.target) {
                Some(revision) => revision.clone(),
                None => super::revision(&choice.target, &Rows::new(), &store),
            };
            preview.revisions.insert(choice.target.clone(), revision);
        }
    }
    for choice in &choices {
        if choice.source == DEVICE {
            let plan = plan_device(services, &opened, &snapshot, choice, &mut preview.rows)?;
            preview.device_revision =
                Some(super::device_revision(services, &snapshot.device_rows)?);
            preview.device = Some(plan);
        } else {
            let plan = plan_profile(services, &opened, &snapshot, choice, &mut preview.rows)?;
            preview.plans.insert(choice.target.clone(), plan);
        }
    }
    Ok(preview)
}

/// A retired profile is never restored into, and a created one must not exist.
pub(crate) fn check_owner(id: &str, store: &nova_sync::Store, create: bool) -> Result<(), String> {
    if !crate::profiles::valid_id(id)
        || crate::profiles::is_retired(id)
        || store
            .record(crate::profiles::DELETIONS_DOMAIN, id)
            .is_some_and(|row| !row.is_deleted())
    {
        return Err(text::tr("The target profile was deleted"));
    }
    let exists = store
        .record(crate::profiles::DOMAIN, id)
        .is_some_and(|row| !row.is_deleted());
    if exists == create {
        return Err(text::tr("The target profile changed; rebuild the preview"));
    }
    Ok(())
}
