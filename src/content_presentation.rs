//! Per-entry presentation (the user's Default/Reader/Gallery choice and the
//! addon's last hint), independent of strict domain state. It is synced per
//! entry, so a collection the addon marks as a gallery opens as one on every
//! paired device without refetching details from its source.
//!
//! The account record lives in its own domain, never in `reader_domain::Series`
//! (that struct rejects unknown fields, so older peers would refuse the series).
//! Local KV remains the read model: the projection writes it through the sync
//! store transaction and refreshes `SNAPSHOTS`, and writers commit KV and the
//! account record together. Lock order is always `SNAPSHOTS` then the sync
//! store; projection touches `SNAPSHOTS` only after releasing the store.
use reader_domain::SeriesId;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const PREFIX: &str = "reader:content-presentation:";
const SCHEMA: u32 = 1;
const MAX_RECORD_BYTES: usize = 64 * 1024;
static SNAPSHOTS: Mutex<BTreeMap<String, Settings>> = Mutex::new(BTreeMap::new());
/// Set once Reader's sync owner exists. Before that (tests, tools that never
/// initialize sync) writes stay plain KV and create no account identity.
static SYNC: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserPreference {
    #[default]
    Default,
    Reader,
    Gallery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    Reader,
    Gallery,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub user: UserPreference,
    pub last_known_addon_hint: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u32,
    settings: Settings,
}

/// Cache entries are keyed by the physical key, so a profile's entries can never
/// answer for another profile even if a stale worker publishes after a switch.
fn cache_key(series: &SeriesId) -> String {
    reader_domain::profile::key(&key(series))
}

/// Forget cached entries (profile switch); the next load reads the new profile's KV.
pub fn reset() {
    SNAPSHOTS.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

fn key(series: &SeriesId) -> String {
    format!(
        "{PREFIX}{}",
        serde_json::to_string(series).expect("SeriesId serializes")
    )
}

fn read(series: &SeriesId) -> Result<Settings, String> {
    let Some(raw) =
        reader_domain::profile::kv::try_get_str(&key(series)).map_err(|e| e.to_string())?
    else {
        return Ok(Settings::default());
    };
    match serde_json::from_str::<Record>(&raw) {
        Ok(record) if record.schema_version == 1 => Ok(record.settings),
        _ => {
            eprintln!("Nova Reader: invalid content presentation record; using Default");
            Ok(Settings::default())
        }
    }
}

fn local_raw(settings: &Settings) -> Result<String, String> {
    serde_json::to_string(&Record {
        schema_version: SCHEMA,
        settings: settings.clone(),
    })
    .map_err(|e| e.to_string())
}

fn write(series: &SeriesId, settings: &Settings) -> Result<(), String> {
    let raw = local_raw(settings)?;
    if !SYNC.load(Ordering::Acquire) {
        return reader_domain::profile::kv::try_set_str(&key(series), &raw)
            .map_err(|e| e.to_string());
    }
    publish(series, settings, raw).map_err(|e| e.to_string())
}

// --- Account record -------------------------------------------------------

pub fn domain() -> String {
    crate::sync::domain("presentation")
}

/// Installed by `sync::initialize`, before the first seed or projection.
pub(crate) fn install_sync() {
    SYNC.store(true, Ordering::Release);
}

/// Wire value: `{"schema_version":1,"user":"default|reader|gallery","hint":"gallery"|"reader"|null}`.
/// Unknown fields from newer peers are tolerated and preserved on edits.
#[derive(Serialize, Deserialize)]
struct Wire {
    schema_version: u32,
    user: WireUser,
    hint: Option<String>,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WireUser {
    Default,
    Reader,
    Gallery,
}
impl From<UserPreference> for WireUser {
    fn from(user: UserPreference) -> Self {
        match user {
            UserPreference::Default => Self::Default,
            UserPreference::Reader => Self::Reader,
            UserPreference::Gallery => Self::Gallery,
        }
    }
}
impl From<WireUser> for UserPreference {
    fn from(user: WireUser) -> Self {
        match user {
            WireUser::Default => Self::Default,
            WireUser::Reader => Self::Reader,
            WireUser::Gallery => Self::Gallery,
        }
    }
}
fn known_hint(hint: &str) -> bool {
    matches!(hint, "gallery" | "reader")
}
fn wire_value(settings: &Settings) -> Result<String, serde_json::Error> {
    serde_json::to_string(&Wire {
        schema_version: SCHEMA,
        user: settings.user.into(),
        hint: settings.last_known_addon_hint.clone(),
    })
}
/// Defaults carry no authority: they are what every device assumes already.
fn is_default(settings: &Settings) -> bool {
    settings.user == UserPreference::Default && settings.last_known_addon_hint.is_none()
}
fn valid_series_key(raw: &str) -> bool {
    serde_json::from_str::<SeriesId>(raw).is_ok_and(|id| {
        let text = |v: &str| !v.trim().is_empty() && v.len() <= 8192 && !v.contains('\0');
        text(&id.source_id) && text(&id.local_id)
    })
}
/// Decode a version-1 value. Newer schema versions return `None`: they are
/// retained and relayed, but this version cannot apply them.
fn decode(raw: &str) -> Option<Settings> {
    let wire: Wire = serde_json::from_str(raw).ok()?;
    (wire.schema_version == SCHEMA && wire.hint.as_deref().is_none_or(known_hint)).then(|| {
        Settings {
            user: wire.user.into(),
            last_known_addon_hint: wire.hint,
        }
    })
}
/// Record policy for the presentation domain.
pub(crate) fn validate_record(record_key: &str, record: &nova_sync::Record) -> bool {
    if !valid_series_key(record_key) {
        return false;
    }
    let Some(raw) = record.value.as_deref() else {
        return record.is_deleted();
    };
    if raw.len() > MAX_RECORD_BYTES {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    match value.get("schema_version").and_then(|v| v.as_u64()) {
        Some(version) if version == u64::from(SCHEMA) => decode(raw).is_some(),
        // A future schema stays opaque to this version but must still be an
        // object, so it can be stored and relayed to peers that understand it.
        Some(version) if version > u64::from(SCHEMA) => value.is_object(),
        _ => false,
    }
}

/// Commit KV and the account record in one transaction.
fn publish(series: &SeriesId, settings: &Settings, raw: String) -> nova_sync::Result<()> {
    let owner = nova_sync::local_store()?;
    let dev = nova_sync::local_device()?;
    let mut guard = owner.lock().unwrap_or_else(|e| e.into_inner());
    let mut records = guard.clone();
    let domain = domain();
    let identity = key_json(series);
    let prior = records
        .record(&domain, &identity)
        .and_then(|record| record.value.clone());
    let mut changed = false;
    if prior.is_some() || !is_default(settings) {
        let mut value = wire_value(settings)?;
        if let Some(old) = &prior {
            value = crate::sync::preserve_fields(old, &value)?;
        }
        changed = records.set(&domain, &identity, Some(value), 0, dev);
    }
    records.queue_extra(&cache_key(series), Some(raw));
    records.save()?;
    *guard = records;
    drop(guard);
    if changed {
        nova_sync::request_local_sync();
    }
    Ok(())
}
fn key_json(series: &SeriesId) -> String {
    serde_json::to_string(series).expect("SeriesId serializes")
}

/// Publish choices that predate sync once. An existing account record always
/// wins, and untouched defaults gain no authority.
pub(crate) fn seed(store: &mut nova_sync::Store, dev: u64) -> nova_sync::Result<bool> {
    let domain = domain();
    let mut changed = false;
    for (storage, raw) in reader_domain::profile::kv::try_scan_prefix(PREFIX)? {
        let Some(identity) = storage.strip_prefix(PREFIX) else {
            continue;
        };
        let Some(settings) = serde_json::from_str::<Record>(&raw)
            .ok()
            .filter(|record| record.schema_version == SCHEMA)
            .map(|record| record.settings)
        else {
            continue;
        };
        if is_default(&settings)
            || !valid_series_key(identity)
            || store.record(&domain, identity).is_some()
            || settings
                .last_known_addon_hint
                .as_deref()
                .is_some_and(|hint| !known_hint(hint))
        {
            continue;
        }
        changed |= store.set(&domain, identity, Some(wire_value(&settings)?), 0, dev);
    }
    Ok(changed)
}

/// Stage received choices into local KV inside the caller's store transaction.
/// Returns what to publish to the in-memory cache once the transaction commits.
pub(crate) fn apply_remote(
    store: &mut nova_sync::Store,
) -> nova_sync::Result<Vec<(String, Settings)>> {
    let domain = domain();
    let existing: BTreeMap<String, String> = reader_domain::profile::kv::try_scan_prefix(PREFIX)?
        .into_iter()
        .collect();
    let mut changed = Vec::new();
    for (identity, raw) in store.records(&domain) {
        let Some(settings) = decode(&raw) else {
            continue;
        };
        let storage = format!("{PREFIX}{identity}");
        let current = existing
            .get(&storage)
            .and_then(|raw| serde_json::from_str::<Record>(raw).ok())
            .filter(|record| record.schema_version == SCHEMA)
            .map(|record| record.settings);
        if current.as_ref() == Some(&settings) {
            continue;
        }
        let physical = reader_domain::profile::key(&storage);
        store.queue_extra(
            &physical,
            Some(local_raw(&settings).map_err(std::io::Error::other)?),
        );
        changed.push((physical, settings));
    }
    Ok(changed)
}

/// Replace cached entries after `apply_remote`'s transaction committed.
pub(crate) fn publish_projection(changed: Vec<(String, Settings)>) {
    if changed.is_empty() {
        return;
    }
    let mut snapshots = SNAPSHOTS.lock().unwrap_or_else(|e| e.into_inner());
    for (storage, settings) in changed {
        snapshots.insert(storage, settings);
    }
}

impl Settings {
    /// Cold reads and mutations share one lock, so a stale load cannot replace
    /// a successfully published write. Hot card projections use memory only.
    pub fn load(series: &SeriesId) -> Result<Self, String> {
        let mut snapshots = SNAPSHOTS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(settings) = snapshots.get(&cache_key(series)) {
            return Ok(settings.clone());
        }
        let settings = read(series)?;
        snapshots.insert(cache_key(series), settings.clone());
        Ok(settings)
    }

    pub fn set_user(series: &SeriesId, user: UserPreference) -> Result<Self, String> {
        let mut snapshots = SNAPSHOTS.lock().unwrap_or_else(|e| e.into_inner());
        let mut settings = match snapshots.get(&cache_key(series)) {
            Some(settings) => settings.clone(),
            None => read(series)?,
        };
        settings.user = user;
        write(series, &settings)?;
        snapshots.insert(cache_key(series), settings.clone());
        Ok(settings)
    }

    /// A missing hint withdraws the suggestion; never changes the user's choice.
    /// Only a full details response may withdraw; listings use `record_listing_hint`.
    pub fn record_hint(series: &SeriesId, hint: Option<&str>) -> Result<Self, String> {
        let mut snapshots = SNAPSHOTS.lock().unwrap_or_else(|e| e.into_inner());
        let mut settings = match snapshots.get(&cache_key(series)) {
            Some(settings) => settings.clone(),
            None => read(series)?,
        };
        // Persist only known bounded tokens, never arbitrary addon payloads.
        let hint = hint.filter(|hint| known_hint(hint));
        if settings.last_known_addon_hint.as_deref() != hint {
            settings.last_known_addon_hint = hint.map(str::to_owned);
            write(series, &settings)?;
        }
        snapshots.insert(cache_key(series), settings.clone());
        Ok(settings)
    }

    /// Browse/search rows only add information: many catalogues omit the
    /// optional field from listings, and that silence must not erase a hint
    /// learned from a full details response or synced from another device.
    pub fn record_listing_hint(
        series: &SeriesId,
        hint: Option<&str>,
    ) -> Result<Option<Self>, String> {
        match hint.filter(|hint| known_hint(hint)) {
            Some(hint) => Self::record_hint(series, Some(hint)).map(Some),
            None => Ok(None),
        }
    }

    pub fn effective(&self) -> Presentation {
        match self.user {
            UserPreference::Gallery => Presentation::Gallery,
            UserPreference::Reader => Presentation::Reader,
            UserPreference::Default if self.last_known_addon_hint.as_deref() == Some("gallery") => {
                Presentation::Gallery
            }
            UserPreference::Default => Presentation::Reader,
        }
    }
}
