//! Account profile directory and the device's local selection.
//!
//! A profile owns the person-specific parts of Reader: library, progress,
//! history, reader/appearance settings and per-title choices. The directory is
//! synced in the account domains `profiles` (id → `{"name":…}`) and
//! `profile_deletions` (id → `"1"`, a permanent retirement that Kit enforces by
//! refusing every later write to `profile:{id}:*`). The selection is device-local
//! (`profiles:device:v1`) and never synced; every paired device synchronizes every
//! profile, whether or not it is selected.
//!
//! The first profile has the id `reader`: Reader's record domains were already
//! `profile:reader:reader_*`, so existing data and 0.1.35 peers need no migration.
//! Storage key scoping, the whitelist and the active pointer live in
//! `reader_domain::profile`; this module owns identity and lifecycle.
use reader_domain::profile as scope;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    sync::{LazyLock, RwLock},
};

pub use scope::{DEFAULT_ID, DEVICE_KEY, valid_id};
pub const DOMAIN: &str = "profiles";
pub const DELETIONS_DOMAIN: &str = "profile_deletions";
/// Shown for the untouched default profile, translated when rendered.
pub const DEFAULT_NAME: &str = "Default";
const MAX_NAME_CHARS: usize = 64;
const MAX_RECORD_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicePreferences {
    #[serde(default = "default_id")]
    pub active_profile: String,
    #[serde(default = "default_true")]
    pub open_last_profile_on_startup: bool,
}
fn default_id() -> String {
    DEFAULT_ID.into()
}
fn default_true() -> bool {
    true
}
impl Default for DevicePreferences {
    fn default() -> Self {
        Self {
            active_profile: default_id(),
            open_last_profile_on_startup: true,
        }
    }
}

/// Retired IDs known to this process. Lock-free with respect to the record owner
/// because projection predicates run while that owner is locked.
static RETIRED: LazyLock<RwLock<HashSet<String>>> = LazyLock::new(Default::default);
/// Retired IDs whose local data has already been removed this run.
static CLEANED: LazyLock<RwLock<HashSet<String>>> = LazyLock::new(Default::default);

pub fn is_retired(id: &str) -> bool {
    RETIRED
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .contains(id)
}
fn retire_locally(id: &str) {
    RETIRED
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id.to_owned());
}

/// Registry profile for a Reader profile. The first profile keeps the registry's
/// original `default` identity, so installed addons keep their settings and
/// session partitions without moving files.
pub fn registry_profile(id: &str) -> String {
    if id == DEFAULT_ID {
        "default".into()
    } else {
        id.into()
    }
}

/// Run `work` on a new thread that remains pinned to the profile that started it.
/// A profile switch while the worker runs cannot redirect its storage, state or
/// record writes into the replacement profile.
pub fn spawn<F>(work: F) -> std::thread::JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    let profile = scope::active();
    std::thread::spawn(move || scope::pinned(&profile, work))
}

pub fn validated_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err("Enter a profile name of up to 64 characters.".into());
    }
    Ok(name.into())
}

/// Record policy for the two account domains.
pub fn validate_record(domain: &str, key: &str, record: &nova_sync::Record) -> bool {
    if !valid_id(key) {
        return false;
    }
    let Some(raw) = record.value.as_deref() else {
        // Profile removal tombstones its directory row. A retirement marker is
        // permanent: deleting it would let a deleted ID come back.
        return domain == DOMAIN && record.is_deleted();
    };
    if raw.len() > MAX_RECORD_BYTES {
        return false;
    }
    if domain == DELETIONS_DOMAIN {
        return raw == "1";
    }
    #[derive(Deserialize)]
    struct Row {
        name: String,
    }
    // Unknown fields from newer peers (for example a future policy) are kept.
    serde_json::from_str::<Row>(raw).is_ok_and(|row| validated_name(&row.name).is_ok())
}

fn store() -> Result<std::sync::Arc<std::sync::Mutex<nova_sync::Store>>, String> {
    nova_sync::local_store().map_err(|e| e.to_string())
}

/// Live profiles, default first. Also refreshes the retirement set and removes
/// local data of profiles that were deleted here or on a peer.
pub fn list() -> Result<Vec<Profile>, String> {
    let owner = store()?;
    let guard = owner.lock().unwrap_or_else(|e| e.into_inner());
    let deleted: HashSet<String> = guard
        .records(DELETIONS_DOMAIN)
        .into_iter()
        .map(|(id, _)| id)
        .filter(|id| valid_id(id))
        .collect();
    RETIRED
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .extend(deleted.iter().cloned());
    #[derive(Deserialize)]
    struct Row {
        name: String,
    }
    let mut profiles = Vec::new();
    for (id, raw) in guard.records(DOMAIN) {
        if !valid_id(&id) || deleted.contains(&id) {
            continue;
        }
        if let Ok(row) = serde_json::from_str::<Row>(&raw) {
            profiles.push(Profile { id, name: row.name });
        }
    }
    drop(guard);
    profiles.sort_by(|a, b| {
        (b.id == DEFAULT_ID)
            .cmp(&(a.id == DEFAULT_ID))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    cleanup_retired(deleted)?;
    Ok(profiles)
}

/// Tombstone any live child record a stale peer delivered after retirement and
/// remove this device's copy of the profile's data. Idempotent.
fn cleanup_retired(ids: HashSet<String>) -> Result<(), String> {
    for id in ids {
        let owner = store()?;
        let device = nova_sync::local_device().map_err(|e| e.to_string())?;
        let mut guard = owner.lock().unwrap_or_else(|e| e.into_inner());
        let prefix = format!("profile:{id}:");
        let mut changed = false;
        for domain in guard.domains() {
            if domain.starts_with(&prefix) {
                for (key, _) in guard.records(&domain) {
                    changed |= guard.set(&domain, &key, None, 0, device);
                }
            }
        }
        if changed {
            guard.save().map_err(|e| e.to_string())?;
        }
        drop(guard);
        if changed {
            nova_sync::request_local_sync();
        }
        if CLEANED
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&id)
        {
            continue;
        }
        // Never remove the data of the profile on screen; switching away comes first.
        if id == scope::selected() {
            continue;
        }
        let mut edits = Vec::new();
        for prefix in [
            format!("profile:{id}:"),
            format!("sync:baseline:profile:{id}:"),
        ] {
            for (key, _) in nova_storage::try_scan_prefix(&prefix).map_err(|e| e.to_string())? {
                edits.push((key, None));
            }
        }
        nova_storage::try_write_batch(&edits).map_err(|e| e.to_string())?;
        if let Some(registry) = REGISTRY.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
            registry
                .remove_profile_settings(&registry_profile(&id))
                .map_err(|e| e.to_string())?;
        }
        CLEANED
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id);
    }
    Ok(())
}

/// The source registry whose per-profile settings follow the selection.
static REGISTRY: LazyLock<RwLock<Option<reader_sources::SourceRegistry>>> =
    LazyLock::new(Default::default);
pub fn install_registry(registry: reader_sources::SourceRegistry) {
    *REGISTRY.write().unwrap_or_else(|e| e.into_inner()) = Some(registry);
}
/// Point the registry at the selected profile.
pub fn apply_registry_profile() {
    if let Some(registry) = REGISTRY.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        registry.set_profile(registry_profile(&scope::selected()));
    }
}

fn write_profile(id: &str, name: &str) -> Result<(), String> {
    let owner = store()?;
    let device = nova_sync::local_device().map_err(|e| e.to_string())?;
    let mut guard = owner.lock().unwrap_or_else(|e| e.into_inner());
    if guard
        .record(DELETIONS_DOMAIN, id)
        .is_some_and(|record| !record.is_deleted())
    {
        return Err("This profile was deleted.".into());
    }
    guard.set(
        DOMAIN,
        id,
        Some(serde_json::json!({"name": name}).to_string()),
        0,
        device,
    );
    guard.save().map_err(|e| e.to_string())?;
    drop(guard);
    nova_sync::request_local_sync();
    Ok(())
}

/// `p{device}_{sequence}`: the device ID is stable per account member and the
/// durable local sequence never repeats, so concurrent devices never collide and
/// a retired ID is never handed out again.
fn allocate_id() -> Result<String, String> {
    let device = nova_sync::local_device().map_err(|e| e.to_string())?;
    let sequence =
        nova_storage::try_allocate_local_sequence("profiles", 1).map_err(|e| e.to_string())?;
    let id = format!("p{device:x}_{sequence:x}");
    debug_assert!(valid_id(&id));
    if is_retired(&id) {
        return Err("profile id was retired".into());
    }
    Ok(id)
}

pub fn create(name: &str) -> Result<Profile, String> {
    let name = validated_name(name)?;
    let id = allocate_id()?;
    write_profile(&id, &name)?;
    Ok(Profile { id, name })
}

pub fn rename(id: &str, name: &str) -> Result<(), String> {
    let name = validated_name(name)?;
    if !list()?.iter().any(|profile| profile.id == id) {
        return Err("This profile no longer exists.".into());
    }
    write_profile(id, &name)
}

/// Remove `id`: retire it permanently, tombstone its records and delete this
/// device's copy. The caller switches away from it first and keeps one profile.
pub fn delete(id: &str) -> Result<(), String> {
    if id == scope::selected() {
        return Err("Switch to another profile before deleting this one.".into());
    }
    let profiles = list()?;
    if !profiles.iter().any(|profile| profile.id == id) {
        return Err("This profile no longer exists.".into());
    }
    if profiles.len() < 2 {
        return Err("At least one profile must remain.".into());
    }
    let owner = store()?;
    let device = nova_sync::local_device().map_err(|e| e.to_string())?;
    {
        let mut guard = owner.lock().unwrap_or_else(|e| e.into_inner());
        // The marker first: from here Kit refuses any later live write to the
        // profile, including delayed workers and stale peers.
        guard.set(DELETIONS_DOMAIN, id, Some("1".into()), 0, device);
        guard.set(DOMAIN, id, None, 0, device);
        guard.save().map_err(|e| e.to_string())?;
    }
    retire_locally(id);
    list()?;
    nova_sync::request_local_sync();
    Ok(())
}

fn neutral_default(store: &mut nova_sync::Store, device: u64) -> bool {
    if store.record(DOMAIN, DEFAULT_ID).is_some()
        || store
            .record(DELETIONS_DOMAIN, DEFAULT_ID)
            .is_some_and(|record| !record.is_deleted())
    {
        return false;
    }
    // The default is scaffolding, never an explicit rename: its neutral version
    // loses to any real edit, so a device joining later cannot overwrite a name
    // another device chose.
    store.apply(
        DOMAIN,
        DEFAULT_ID,
        nova_sync::Record {
            value: Some(serde_json::json!({"name": DEFAULT_NAME}).to_string()),
            version: nova_sync::Version::new(0, 0, device, false),
        },
    )
}

pub fn device_preferences() -> DevicePreferences {
    match nova_storage::try_get_str(DEVICE_KEY) {
        Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_default(),
        _ => DevicePreferences::default(),
    }
}
pub fn write_device_preferences(preferences: &DevicePreferences) -> Result<(), String> {
    nova_storage::try_set_str(
        DEVICE_KEY,
        &serde_json::to_string(preferences).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
pub fn set_open_last_profile(enabled: bool) -> Result<(), String> {
    let mut preferences = device_preferences();
    preferences.open_last_profile_on_startup = enabled;
    write_device_preferences(&preferences)
}

/// Seed the default profile (if the directory is empty) and settle the selection.
/// Returns the preferences after correcting a selection that no longer exists.
pub fn initialize() -> Result<DevicePreferences, String> {
    let mut preferences = device_preferences();
    if list()?.is_empty() {
        let owner = store()?;
        let device = nova_sync::local_device().map_err(|e| e.to_string())?;
        let seeded = {
            let mut guard = owner.lock().unwrap_or_else(|e| e.into_inner());
            let changed = neutral_default(&mut guard, device);
            if changed {
                guard.save().map_err(|e| e.to_string())?;
                // Our own seed is not news: leave nothing pending for a UI refresh.
                if let Some(basis) = guard.digest().get(DOMAIN).cloned() {
                    guard
                        .mark_projected(DOMAIN, &basis)
                        .map_err(|e| e.to_string())?;
                }
            }
            changed
        };
        if seeded {
            nova_sync::request_local_sync();
        }
        if list()?.is_empty() {
            // Peers deleted every profile (including the default) while offline.
            // A retired ID is never reused, so the replacement is a new identity.
            create(DEFAULT_NAME)?;
        }
    }
    let profiles = list()?;
    let selected = profiles
        .iter()
        .find(|profile| profile.id == preferences.active_profile)
        .or_else(|| profiles.first())
        .map(|profile| profile.id.clone())
        .ok_or("No profile is available.")?;
    preferences.active_profile = selected;
    write_device_preferences(&preferences)?;
    Ok(preferences)
}

/// Make `id` the selected profile for the whole process: state, personal
/// settings, entry presentation and registry settings. Nothing changes if the
/// selection cannot be persisted. UI models are the caller's concern.
pub fn activate(domain: &reader_domain::DomainStore, id: &str) -> Result<(), String> {
    if !list()?.iter().any(|profile| profile.id == id) {
        return Err("This profile no longer exists.".into());
    }
    // Persisting first means a failed write leaves the old profile fully intact.
    write_selection(id)?;
    domain.set_profile(id).map_err(|e| e.to_string())?;
    apply_registry_profile();
    crate::content_presentation::reset();
    if let Err(error) = crate::reader_settings::init() {
        eprintln!("Nova Reader: could not read reader settings for profile {id}: {error}");
    }
    // A first activation has no baselines yet; seeding keeps defaults from
    // gaining LWW authority, and publishes nothing that already exists.
    domain
        .update_with_kind(reader_domain::UpdateKind::Seed, |_| Ok(()))
        .map_err(|e| e.to_string())
}

/// Persist the selection. Called last in a switch: if this fails the caller keeps
/// the previous profile and reports the error.
pub fn write_selection(id: &str) -> Result<(), String> {
    let mut preferences = device_preferences();
    preferences.active_profile = id.to_owned();
    write_device_preferences(&preferences)
}

/// Avatar colors derive from a stable hash of the ID so every device agrees
/// without storing or syncing anything.
pub fn avatar(id: &str) -> (u32, u32) {
    // FNV-1a, 32 bit.
    let hash = id.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    AVATARS[(hash as usize) % AVATARS.len()]
}
/// (background, foreground) as 0xRRGGBB. Each pair keeps a contrast ratio of at
/// least 4.5:1, which `avatar_pairs_are_readable` checks.
pub const AVATARS: [(u32, u32); 8] = [
    (0x1f4e79, 0xffffff),
    (0x7a2e2e, 0xffffff),
    (0x2e6b3a, 0xffffff),
    (0x6a3d8f, 0xffffff),
    (0x8a4b00, 0xffffff),
    (0x0f6b6b, 0xffffff),
    (0x4a4a8a, 0xffffff),
    (0x7a1f5c, 0xffffff),
];
pub fn initial(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(rgb: u32) -> f64 {
        let channel = |shift: u32| {
            let value = f64::from((rgb >> shift) & 0xff) / 255.;
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    #[test]
    fn avatar_pairs_are_readable() {
        for (background, foreground) in AVATARS {
            let (a, b) = (luminance(background), luminance(foreground));
            let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
            assert!(
                ratio >= 4.5,
                "{background:06x} on {foreground:06x}: {ratio:.2}"
            );
        }
        assert_eq!(avatar("p1_1"), avatar("p1_1"));
    }

    #[test]
    fn names_are_trimmed_bounded_and_printable() {
        assert_eq!(validated_name("  Mina  ").unwrap(), "Mina");
        assert_eq!(validated_name(&"é".repeat(64)).unwrap().chars().count(), 64);
        for bad in ["", "   ", "tab\there", "new\nline", &"x".repeat(65)] {
            assert!(validated_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(initial(" mina"), "M");
        assert_eq!(initial(""), "?");
    }

    #[test]
    fn registry_keeps_the_original_identity_for_the_first_profile() {
        assert_eq!(registry_profile(DEFAULT_ID), "default");
        assert_eq!(registry_profile("p1_2"), "p1_2");
    }

    #[test]
    fn directory_records_are_validated() {
        let live = |value: &str| nova_sync::Record {
            value: Some(value.into()),
            version: nova_sync::Version::new(1, 0, 1, false),
        };
        let tombstone = nova_sync::Record {
            value: None,
            version: nova_sync::Version::new(1, 0, 1, true),
        };
        assert!(validate_record(DOMAIN, "p1_2", &live(r#"{"name":"Mina"}"#)));
        assert!(validate_record(
            DOMAIN,
            "p1_2",
            &live(r#"{"name":"Mina","future":{"a":1}}"#)
        ));
        assert!(validate_record(DOMAIN, "p1_2", &tombstone));
        assert!(!validate_record(DOMAIN, "a:b", &live(r#"{"name":"Mina"}"#)));
        assert!(!validate_record(DOMAIN, "p1_2", &live(r#"{"name":""}"#)));
        assert!(!validate_record(DOMAIN, "p1_2", &live("not json")));
        assert!(validate_record(DELETIONS_DOMAIN, "p1_2", &live("1")));
        assert!(!validate_record(DELETIONS_DOMAIN, "p1_2", &live("0")));
        assert!(!validate_record(DELETIONS_DOMAIN, "p1_2", &tombstone));
    }
}
