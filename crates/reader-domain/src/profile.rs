//! Per-profile storage namespaces.
//!
//! Storage cannot be reopened (one directory per process), so switching profiles
//! re-keys instead: personal keys live at `profile:{id}:{logical key}` and the
//! active profile is a process-wide pointer. Only an explicit whitelist is
//! personal; everything else (image limits, caches, `sync:*`, addon packages) is
//! device-global and passes through unchanged.
//!
//! Scoping is inactive until `DomainStore::open` enables it, so code and tests
//! that use bare `nova_storage` keep the legacy layout.
use crate::{Error, Result};
use std::sync::{
    RwLock,
    atomic::{AtomicBool, Ordering},
};

/// The first profile. Reader's synced record domains were already
/// `profile:reader:reader_*`, so existing data and older peers need no migration.
pub const DEFAULT_ID: &str = "reader";
/// Device-local selection; never synced.
pub const DEVICE_KEY: &str = "profiles:device:v1";

static ENABLED: AtomicBool = AtomicBool::new(false);
static ACTIVE: RwLock<String> = RwLock::new(String::new());
thread_local! {
    // A worker pins the profile that launched it, so a switch that happens while
    // it runs cannot redirect its later reads or writes into another profile.
    static PINNED: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Personal exact keys. Pinned by a test: moving a key between this list and the
/// device-global set changes whose data an upgrade or a profile switch exposes.
pub const SCOPED_EXACT_KEYS: &[&str] = &[
    "reader:state",
    "reader:show-adult",
    "reader:reader-settings",
    "reader:source-browser",
    "reader:library-navigation:v1",
    "reader:last-update",
    "reader:appearance",
    "reader:sync-settings-baseline",
    "reader:sync-settings-overrides",
    // Stores per-profile category IDs.
    "reader:download-cleanup",
];
/// Personal key families (one row per series or per addon package).
pub const SCOPED_PREFIXES: &[&str] = &[
    "reader:chapter-order:",
    "reader:reader-settings-series:",
    "reader:mode-choice:",
    "reader:content-presentation:",
    "reader:disabled-catalogs:",
];

pub fn scoped_key(key: &str) -> bool {
    SCOPED_EXACT_KEYS.contains(&key) || SCOPED_PREFIXES.iter().any(|p| key.starts_with(p))
}
/// IDs appear in sync domain names (`profile:{id}:...`) and paths, so they are
/// restricted to `[A-Za-z0-9_-]{1,64}`; `:` would make domains ambiguous.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
pub fn key_for(profile: &str, key: &str) -> String {
    format!("profile:{profile}:{key}")
}
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}
/// The profile this thread works for: its pinned launch profile, otherwise the
/// device-wide selection.
pub fn active() -> String {
    PINNED
        .with(|pinned| pinned.borrow().clone())
        .unwrap_or_else(selected)
}
/// The device-wide selection, ignoring any thread pin.
pub fn selected() -> String {
    let active = ACTIVE.read().unwrap_or_else(|e| e.into_inner());
    if active.is_empty() {
        DEFAULT_ID.into()
    } else {
        active.clone()
    }
}
/// Run `f` with this thread pinned to `profile`.
pub fn pinned<R>(profile: &str, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PINNED.with(|pinned| *pinned.borrow_mut() = self.0.take());
        }
    }
    let previous = PINNED.with(|pinned| pinned.replace(Some(profile.to_owned())));
    let _restore = Restore(previous);
    f()
}
/// Map a logical key for the active profile. Device-global keys are unchanged.
pub fn key(logical: &str) -> String {
    key_in(&active(), logical)
}
/// Map a logical key for an explicit profile (inactive-profile projection,
/// deletion, the captured owner of a background writer).
pub fn key_in(profile: &str, logical: &str) -> String {
    if is_enabled() && scoped_key(logical) {
        key_for(profile, logical)
    } else {
        logical.into()
    }
}
/// Enable scoping with `initial` active, once. Later calls keep the selection.
pub(crate) fn enable(initial: &str) {
    if is_enabled() {
        return;
    }
    let mut active = ACTIVE.write().unwrap_or_else(|e| e.into_inner());
    *active = if valid_id(initial) {
        initial.into()
    } else {
        DEFAULT_ID.into()
    };
    ENABLED.store(true, Ordering::Release);
}
/// Change the active profile. Callers hold `DomainStore`'s serial lock.
pub(crate) fn set_active(id: &str) -> Result<()> {
    if !valid_id(id) {
        return Err(Error::Invalid("invalid profile id".into()));
    }
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = id.into();
    Ok(())
}
/// The selection recorded in device-local `profiles:device:v1`, if usable.
pub fn stored_selection() -> Option<String> {
    let raw = nova_storage::try_get_str(DEVICE_KEY).ok()??;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let id = value.get("active_profile")?.as_str()?;
    valid_id(id).then(|| id.to_owned())
}

/// Move every legacy unprefixed personal key to `profile:reader:<key>` in one
/// transaction. A destination that already exists wins and the legacy row is left
/// alone, so the move is idempotent, crash-safe, and never discards data.
pub fn migrate_legacy() -> Result<bool> {
    let mut legacy = Vec::new();
    for key in SCOPED_EXACT_KEYS {
        if let Some(value) = nova_storage::try_get_str(key)? {
            legacy.push(((*key).to_owned(), value));
        }
    }
    let prefixes: Vec<String> = SCOPED_PREFIXES.iter().map(|p| (*p).into()).collect();
    legacy.extend(nova_storage::try_scan_prefixes(&prefixes)?);
    let mut edits = Vec::new();
    for (key, value) in legacy {
        let target = key_for(DEFAULT_ID, &key);
        if nova_storage::try_get_str(&target)?.is_some() {
            continue;
        }
        edits.push((target, Some(value)));
        edits.push((key, None));
    }
    if edits.is_empty() {
        return Ok(false);
    }
    nova_storage::try_write_batch(&edits)?;
    Ok(true)
}

/// `nova_storage` with personal keys mapped to the active profile.
pub mod kv {
    use super::{active, key, key_in};
    use nova_storage::Error;

    pub fn try_get_str(logical: &str) -> Result<Option<String>, Error> {
        nova_storage::try_get_str(&key(logical))
    }
    pub fn get_str(logical: &str) -> Option<String> {
        nova_storage::get_str(&key(logical))
    }
    pub fn try_set_str(logical: &str, value: &str) -> Result<(), Error> {
        nova_storage::try_set_str(&key(logical), value)
    }
    pub fn set_str(logical: &str, value: &str) {
        nova_storage::set_str(&key(logical), value)
    }
    pub fn try_set_cached_str(logical: &str, value: &str) -> Result<(), Error> {
        nova_storage::try_set_cached_str(&key(logical), value)
    }
    pub fn remove(logical: &str) {
        nova_storage::remove(&key(logical))
    }
    pub fn try_write_batch(entries: &[(String, Option<String>)]) -> Result<(), Error> {
        let mapped: Vec<_> = entries.iter().map(|(k, v)| (key(k), v.clone())).collect();
        nova_storage::try_write_batch(&mapped)
    }
    /// Scan a logical prefix; results come back with logical keys.
    pub fn try_scan_prefix(prefix: &str) -> Result<Vec<(String, String)>, Error> {
        try_scan_prefixes(&[prefix.to_owned()])
    }
    pub fn try_scan_prefixes(prefixes: &[String]) -> Result<Vec<(String, String)>, Error> {
        let profile = active();
        let mapped: Vec<String> = prefixes.iter().map(|p| key_in(&profile, p)).collect();
        let strip = format!("profile:{profile}:");
        Ok(nova_storage::try_scan_prefixes(&mapped)?
            .into_iter()
            .map(|(k, v)| {
                let logical = k
                    .strip_prefix(&strip)
                    .filter(|logical| super::scoped_key(logical))
                    .map_or(k.clone(), str::to_owned);
                (logical, v)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins which keys belong to a person and which to the device. Changing this
    /// list silently changes what a profile switch exposes or hides.
    #[test]
    fn personal_and_device_global_keys_are_pinned() {
        for personal in [
            "reader:state",
            "reader:show-adult",
            "reader:chapter-order:{\"source_id\":\"a\",\"local_id\":\"b\"}",
            "reader:reader-settings",
            "reader:reader-settings-series:{}",
            "reader:mode-choice:{}",
            "reader:content-presentation:{}",
            "reader:source-browser",
            "reader:library-navigation:v1",
            "reader:last-update",
            "reader:disabled-catalogs:\"package\"",
            "reader:appearance",
            "reader:sync-settings-baseline",
            "reader:sync-settings-overrides",
            "reader:download-cleanup",
        ] {
            assert!(scoped_key(personal), "{personal} must be personal");
        }
        for global in [
            "reader:image-storage",
            "reader:image-limits",
            "reader:captures",
            "reader:source-compression:\"package\"",
            "reader:metadata-refresh:{}",
            "reader:cache-gc:artwork:v1",
            "reader:installed-sources:v1",
            "sync:settings",
            "sync:baseline:profile:reader:reader_resume",
            "profile:reader:reader:state",
            "profiles:device:v1",
            "reader:state-extra",
            "reader:reader-settings-extra",
        ] {
            assert!(!scoped_key(global), "{global} must stay device-global");
        }
        assert_eq!(SCOPED_EXACT_KEYS.len() + SCOPED_PREFIXES.len(), 15);
    }

    #[test]
    fn profile_ids_are_domain_and_path_safe() {
        for good in ["reader", "p1f_2a", "A-b_9", &"x".repeat(64)] {
            assert!(valid_id(good), "{good}");
        }
        for bad in ["", "a:b", "a/b", "a b", "..", "é", &"x".repeat(65)] {
            assert!(!valid_id(bad), "{bad}");
        }
    }
}
