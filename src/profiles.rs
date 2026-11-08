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
use nova_sync::profiles::directory::{self, Directory, Host};
use reader_domain::profile as scope;
use serde::{Deserialize, Serialize};
use std::sync::{LazyLock, RwLock};

pub use directory::{
    DEFAULT_NAME, DELETIONS_DOMAIN, DOMAIN, Profile, validate_record, validated_name,
};
pub use scope::{DEFAULT_ID, DEVICE_KEY, valid_id};

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

/// What Reader adds to the shared directory's sequences: the source registry's
/// per-profile settings, which live outside the record store.
struct ReaderDirectory;
impl Host for ReaderDirectory {
    type Guard = ();

    fn begin_write(&self, _id: &str) {}
    /// Never remove the data of the profile on screen; switching away comes first.
    fn keep_local_data(&self, id: &str) -> bool {
        id == scope::selected()
    }
    fn remove_local_data(&self, id: &str) -> Result<(), String> {
        remove_registry_settings(id)
    }
    fn deleted(&self, id: &str) {
        // The deletion is durable already; a failure here is retried by the
        // cleanup that every `list` runs for a retired profile.
        let _ = remove_registry_settings(id);
    }
    #[cfg(test)]
    fn checkpoint(&self, stage: directory::Stage) -> Result<(), String> {
        deletion_checkpoint(match stage {
            directory::Stage::AfterTombstone => DeleteFailure::AfterTombstone,
            directory::Stage::BeforeScan => DeleteFailure::BeforeScan,
        })
    }
}
static DIRECTORY: LazyLock<Directory<ReaderDirectory>> =
    LazyLock::new(|| Directory::new(ReaderDirectory, DEFAULT_ID));

fn remove_registry_settings(id: &str) -> Result<(), String> {
    match REGISTRY.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(registry) => registry
            .remove_profile_settings(&registry_profile(id))
            .map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

pub fn is_retired(id: &str) -> bool {
    DIRECTORY.is_retired(id)
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

fn store() -> Result<std::sync::Arc<std::sync::Mutex<nova_sync::Store>>, String> {
    nova_sync::local_store().map_err(|e| e.to_string())
}

/// Live profiles, default first. Also refreshes the retirement set and removes
/// local data of profiles that were deleted here or on a peer.
pub fn list() -> Result<Vec<Profile>, String> {
    let mut profiles = DIRECTORY.list()?;
    profiles.sort_by(|a, b| {
        (b.id == DEFAULT_ID)
            .cmp(&(a.id == DEFAULT_ID))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(profiles)
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

pub fn create(name: &str) -> Result<Profile, String> {
    DIRECTORY.create(name)
}

/// A fresh profile ID, in the form [`create`] hands out (`p{device}_{sequence}`),
/// for a transaction that writes the directory row itself (a restore).
pub fn allocate_id() -> Result<String, String> {
    let device = nova_sync::local_device().map_err(|e| e.to_string())?;
    let sequence =
        nova_storage::try_allocate_local_sequence("profiles", 1).map_err(|e| e.to_string())?;
    Ok(format!("p{device:x}_{sequence:x}"))
}

pub fn rename(id: &str, name: &str) -> Result<(), String> {
    let name = validated_name(name)?;
    if !list()?.iter().any(|profile| profile.id == id) {
        return Err("This profile no longer exists.".into());
    }
    DIRECTORY.write(id, &name)
}

// Failure injection stays local to the isolated test's calling thread.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteFailure {
    AfterTombstone,
    BeforeScan,
}
#[cfg(test)]
thread_local! {
    static DELETE_FAILURE: std::cell::Cell<Option<DeleteFailure>> = const { std::cell::Cell::new(None) };
}
#[cfg(test)]
pub(crate) fn with_delete_failure<R>(failure: DeleteFailure, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<DeleteFailure>);
    impl Drop for Restore {
        fn drop(&mut self) {
            DELETE_FAILURE.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(DELETE_FAILURE.with(|slot| slot.replace(Some(failure))));
    f()
}
#[cfg(test)]
fn deletion_checkpoint(point: DeleteFailure) -> Result<(), String> {
    if DELETE_FAILURE.with(|slot| slot.get() == Some(point)) {
        Err("Injected profile deletion failure".into())
    } else {
        Ok(())
    }
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
    DIRECTORY.retire(id)
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
            let changed = DIRECTORY.seed_default(&mut guard, device);
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
    fn registry_keeps_the_original_identity_for_the_first_profile() {
        assert_eq!(registry_profile(DEFAULT_ID), "default");
        assert_eq!(registry_profile("p1_2"), "p1_2");
    }

    /// Storage and the sync owner are per-process, so this scenario re-executes
    /// itself in a child that owns a fresh directory.
    fn isolated(name: &str, body: impl FnOnce(&std::path::Path)) {
        const CHILD: &str = "READER_PROFILES_UNIT_CHILD";
        if let Some(directory) = std::env::var_os(CHILD) {
            body(std::path::Path::new(&directory));
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env(CHILD, directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name} failed\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn everything(
        owner: &nova_sync::Store,
        id: &str,
    ) -> (Vec<(String, Vec<(String, String)>)>, Vec<(String, String)>) {
        let mut records: Vec<_> = owner
            .domains()
            .into_iter()
            .map(|domain| {
                let rows = owner.records(&domain);
                (domain, rows)
            })
            .collect();
        records.sort();
        let mut durable = nova_storage::try_scan_prefix(&format!("profile:{id}:")).unwrap();
        durable.extend(
            nova_storage::try_scan_prefix(&format!("sync:baseline:profile:{id}:")).unwrap(),
        );
        (records, durable)
    }

    #[test]
    fn a_failed_delete_changes_nothing_and_a_completed_one_removes_everything() {
        isolated(
            "profiles::tests::a_failed_delete_changes_nothing_and_a_completed_one_removes_everything",
            |directory| {
                let _services = crate::services::Services::open(directory).unwrap();
                let second = create("Second").unwrap();
                let id = second.id;
                let device = nova_sync::local_device().unwrap();
                {
                    let owner = store().unwrap();
                    let mut guard = owner.lock().unwrap();
                    assert!(guard.set(
                        &format!("profile:{id}:fixture_rows"),
                        "a",
                        Some("{}".into()),
                        0,
                        device,
                    ));
                    guard.queue_extra(
                        &format!("sync:baseline:profile:{id}:fixture_rows"),
                        Some("{}".into()),
                    );
                    guard.save().unwrap();
                }
                nova_storage::try_set_str(&format!("profile:{id}:fixture_settings"), "{}").unwrap();
                let before = everything(&store().unwrap().lock().unwrap(), &id);
                let before_disk = everything(&nova_sync::Store::try_load().unwrap(), &id);
                assert!(!before.1.is_empty());

                for failure in [DeleteFailure::AfterTombstone, DeleteFailure::BeforeScan] {
                    let result = with_delete_failure(failure, || delete(&id));
                    assert!(result.is_err());
                    assert_eq!(
                        everything(&store().unwrap().lock().unwrap(), &id),
                        before,
                        "live state after a failure"
                    );
                    assert_eq!(
                        everything(&nova_sync::Store::try_load().unwrap(), &id),
                        before_disk,
                        "durable state after a failure"
                    );
                    assert!(!is_retired(&id));
                    assert!(list().unwrap().iter().any(|profile| profile.id == id));
                }

                delete(&id).unwrap();
                assert!(is_retired(&id));
                for owner in [
                    store().unwrap().lock().unwrap().clone(),
                    nova_sync::Store::try_load().unwrap(),
                ] {
                    assert!(owner.record(DOMAIN, &id).unwrap().is_deleted());
                    assert!(!owner.record(DELETIONS_DOMAIN, &id).unwrap().is_deleted());
                    assert!(
                        owner
                            .record(&format!("profile:{id}:fixture_rows"), "a")
                            .unwrap()
                            .is_deleted()
                    );
                }
                assert!(
                    nova_storage::try_scan_prefix(&format!("profile:{id}:"))
                        .unwrap()
                        .is_empty()
                        && nova_storage::try_scan_prefix(&format!("sync:baseline:profile:{id}:"))
                            .unwrap()
                            .is_empty()
                );
                assert!(list().unwrap().iter().all(|profile| profile.id != id));
            },
        );
    }
}
