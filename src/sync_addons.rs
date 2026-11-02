//! Desired addon state and bounded, verified peer package transfer. Packages
//! remain disabled until this device grants their permissions.
use crate::services::Services;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

static SERIAL: Mutex<()> = Mutex::new(());
static RECONCILE: Mutex<()> = Mutex::new(());
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub id: String,
    pub version: String,
    pub sha256: String,
    pub enabled: bool,
    pub disabled_catalogs: BTreeSet<String>,
    pub settings: serde_json::Value,
}
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub pending: usize,
    pub failed: usize,
    pub confirmation_required: usize,
}
pub fn domain() -> String {
    nova_sync::profile_domain("reader", "reader_addons")
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Descriptor {
    fn valid(&self, key: &str) -> bool {
        self.id == key
            && valid_id(key)
            && valid_hash(&self.sha256)
            && !self.version.is_empty()
            && self.version.len() <= 128
            && !self.version.chars().any(char::is_control)
            && self.disabled_catalogs.len() <= 1024
            && self.disabled_catalogs.iter().all(|id| {
                id.len() <= 256
                    && !id.chars().any(char::is_control)
                    && (id == key || id.starts_with(&format!("{key}.")))
            })
            && self.settings.as_object().is_some_and(|map| map.len() <= 64)
            && serde_json::to_vec(&self.settings).is_ok_and(|v| v.len() <= 64 * 1024)
    }
}
pub fn validate_record(key: &str, record: &nova_sync::Record) -> bool {
    if !valid_id(key) {
        return false;
    }
    record.is_deleted()
        || record
            .value
            .as_ref()
            .and_then(|raw| serde_json::from_str::<Descriptor>(raw).ok())
            .is_some_and(|d| d.valid(key))
}
fn snapshot(services: &Services) -> Result<Vec<(String, String, u64)>, String> {
    services
        .sources
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|source| {
            let (sha256, _) = services
                .sources
                .archive(&source.id)
                .map_err(|e| e.to_string())?;
            let row = Descriptor {
                disabled_catalogs: crate::source_catalogs::disabled(&source.id),
                settings: services
                    .sources
                    .settings(&source.id)
                    .map_err(|e| e.to_string())?,
                id: source.id,
                version: source.version,
                sha256,
                enabled: source.enabled,
            };
            Ok((
                row.id.clone(),
                serde_json::to_string(&row).map_err(|e| e.to_string())?,
                0,
            ))
        })
        .collect()
}
struct Blobs(reader_sources::SourceRegistry);
impl nova_sync::PackageBlobStore for Blobs {
    fn load(&self, hash: &str) -> nova_sync::Result<Option<Vec<u8>>> {
        if !valid_hash(hash) {
            return Ok(None);
        }
        for source in self.0.list()? {
            let (actual, bytes) = self.0.archive(&source.id)?;
            if actual == hash {
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    }
}
// Baselines describe what this device actually materialized, including local
// permission denials. Outbound values describe only changes to that baseline.
fn merge_intent(
    before: &Descriptor,
    actual: &Descriptor,
    shared: &Descriptor,
) -> Result<Descriptor, String> {
    let mut next = shared.clone();
    let package_changed = (before.sha256.as_str(), before.version.as_str())
        != (actual.sha256.as_str(), actual.version.as_str());
    if package_changed {
        next.sha256 = actual.sha256.clone();
        next.version = actual.version.clone();
    }
    // An archive update can revoke this device's execution grant because its
    // permissions expanded. That automatic denial is not an account opt-out.
    if before.enabled != actual.enabled && (!package_changed || actual.enabled) {
        next.enabled = actual.enabled;
    }
    for id in before
        .disabled_catalogs
        .symmetric_difference(&actual.disabled_catalogs)
    {
        if actual.disabled_catalogs.contains(id) {
            next.disabled_catalogs.insert(id.clone());
        } else {
            next.disabled_catalogs.remove(id);
        }
    }
    let prior = before
        .settings
        .as_object()
        .ok_or("invalid addon materialization baseline")?;
    let current = actual
        .settings
        .as_object()
        .ok_or("invalid addon settings snapshot")?;
    let values = next
        .settings
        .as_object_mut()
        .ok_or("invalid shared addon settings")?;
    let keys: BTreeSet<_> = prior.keys().chain(current.keys()).collect();
    for key in keys {
        if prior.get(key) != current.get(key) {
            if let Some(value) = current.get(key) {
                values.insert(key.clone(), value.clone());
            } else {
                values.remove(key);
            }
        }
    }
    Ok(next)
}
fn intent_rows(
    store: &nova_sync::Store,
    actual: &[(String, String, u64)],
) -> Result<Vec<(String, String, u64)>, String> {
    let baseline: BTreeMap<String, String> = store
        .extra_value(&format!("sync:baseline:{}", domain()))
        .map_err(|e| e.to_string())?
        .map(|raw| serde_json::from_str(&raw))
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    actual
        .iter()
        .map(|(key, raw, at)| {
            let value = if let (Some(before), Some(shared)) = (
                baseline.get(key),
                store
                    .record(&domain(), key)
                    .and_then(|r| r.value.as_deref()),
            ) {
                let before: Descriptor = serde_json::from_str(before).map_err(|e| e.to_string())?;
                let local: Descriptor = serde_json::from_str(raw).map_err(|e| e.to_string())?;
                let shared: Descriptor = serde_json::from_str(shared).map_err(|e| e.to_string())?;
                serde_json::to_string(&merge_intent(&before, &local, &shared)?)
                    .map_err(|e| e.to_string())?
            } else {
                raw.clone()
            };
            Ok((key.clone(), value, *at))
        })
        .collect()
}

fn commit_locked(
    rows: &[(String, String, u64)],
    projection: bool,
    seed: bool,
) -> Result<(), String> {
    nova_sync::flush_persistence().map_err(|e| e.to_string())?;
    let owner = nova_sync::local_store().map_err(|e| e.to_string())?;
    let dev = nova_sync::local_device().map_err(|e| e.to_string())?;
    let mut guard = owner.lock().unwrap_or_else(|e| e.into_inner());
    let mut next = guard.clone();
    let outbound = if !projection && !seed {
        intent_rows(&next, rows)?
    } else {
        rows.to_vec()
    };
    let changed =
        nova_sync::prepare_snapshot(&mut next, dev, &domain(), &outbound, None, projection, seed)
            .map_err(|e| e.to_string())?;
    // Shared values can contain a grant or disjoint remote settings that this
    // device has not materialized. Never mistake those for its local baseline.
    let materialized: BTreeMap<_, _> = rows
        .iter()
        .map(|(key, value, _)| (key.clone(), value.clone()))
        .collect();
    next.queue_extra(
        &format!("sync:baseline:{}", domain()),
        Some(serde_json::to_string(&materialized).map_err(|e| e.to_string())?),
    );
    next.save().map_err(|e| e.to_string())?;
    *guard = next;
    drop(guard);
    if changed && !projection && !seed {
        nova_sync::request_local_sync();
    }
    Ok(())
}

pub fn initialize(services: &Services) -> Result<(), String> {
    nova_sync::configure_package_store(Arc::new(Blobs(services.sources.clone())));
    let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    commit_locked(&snapshot(services)?, false, true)
}
/// Serialize explicit filesystem/settings intent with remote materialization,
/// and publish its durable snapshot before a reconciler can acknowledge it.
/// The closure must not call `publish` or nest another `local_change`.
pub fn local_change<T>(
    services: &Services,
    mutate: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let value = mutate()?;
    commit_locked(&snapshot(services)?, false, false)?;
    Ok(value)
}

/// Snapshot publication without a mutation. Mutating callers use `local_change`.
pub fn publish(services: &Services) -> Result<(), String> {
    let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    commit_locked(&snapshot(services)?, false, false)
}

/// Worker-only: package bytes use the authenticated package ALPN, never records.
/// The lease/controller owns cancellation, and disabled sync performs no fetch.
pub fn reconcile(services: &Services, cancel: Arc<AtomicBool>) -> Result<Status, String> {
    let Ok(_worker) = RECONCILE.try_lock() else {
        return Ok(Status::default());
    };
    let mut status = Status::default();
    if !nova_sync::read_settings().enabled || cancel.load(Ordering::Acquire) {
        return Ok(status);
    }
    // An explicit local edit owns both the registry mutation and its outgoing
    // snapshot. Skip this opportunistic pass until that intent is published.
    let Ok(initial_gate) = SERIAL.try_lock() else {
        return Ok(status);
    };
    let owner = nova_sync::local_store().map_err(|e| e.to_string())?;
    let (desired, deleted, basis) = {
        let store = owner.lock().unwrap_or_else(|e| e.into_inner());
        let desired = store
            .records(&domain())
            .into_iter()
            .map(|(key, raw)| {
                let row: Descriptor = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                if !row.valid(&key) {
                    return Err("invalid addon sync descriptor".into());
                }
                Ok((key, row))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let deleted = services
            .sources
            .list()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|s| {
                store
                    .record(&domain(), &s.id)
                    .is_some_and(|r| r.is_deleted())
            })
            .map(|s| s.id)
            .collect::<Vec<_>>();
        (desired, deleted, store.digest().get(&domain()).cloned())
    };
    drop(initial_gate);
    for id in deleted {
        let _mutation = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        if cancel.load(Ordering::Acquire) {
            return Ok(status);
        }
        if !owner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(&domain(), &id)
            .is_some_and(|r| r.is_deleted())
        {
            continue;
        }
        services.sources.remove(&id).map_err(|e| e.to_string())?;
    }
    let mut completed = true;
    for row in desired.values() {
        if cancel.load(Ordering::Acquire) || !nova_sync::read_settings().enabled {
            completed = false;
            break;
        }
        let previous_archive = services.sources.archive(&row.id).ok().map(|(hash, _)| hash);
        let present = previous_archive.as_deref() == Some(row.sha256.as_str());
        if !present {
            status.pending += 1;
            let Some(engine) = nova_sync::engine() else {
                continue;
            };
            let receipt = engine
                .fetch_package(&row.sha256, cancel.clone())
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    install_receipt(services, row, &bytes, &cancel, previous_archive.as_deref())
                });
            if receipt.is_err() {
                status.failed += 1;
                continue;
            }
            status.pending -= 1;
        }
        let _mutation = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let current = owner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(&domain(), &row.id)
            .and_then(|record| record.value.as_deref())
            .and_then(|raw| serde_json::from_str::<Descriptor>(raw).ok());
        if current.as_ref() != Some(row) {
            continue;
        }
        // Never let a peer grant execution permission on this device. Existing
        // grants survive a compatible update; registry clears expanded domain,
        // HTTP POST and exact cookie origin/name grants.
        if !row.enabled {
            services
                .sources
                .set_enabled(&row.id, false)
                .map_err(|e| e.to_string())?;
        }
        let enabled = services
            .sources
            .list()
            .map_err(|e| e.to_string())?
            .iter()
            .any(|s| s.id == row.id && s.enabled);
        if row.enabled && !enabled {
            status.confirmation_required += 1;
        }
        let schema = services
            .sources
            .settings_schema(&row.id)
            .map_err(|e| e.to_string())?;
        let patch = row
            .settings
            .as_object()
            .unwrap()
            .iter()
            .filter(|(key, _)| {
                schema.iter().any(|field| {
                    &field.key == *key && field.kind != reader_sources::SettingKind::Secret
                })
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if services
            .sources
            .set_settings(&row.id, serde_json::Value::Object(patch))
            .is_err()
        {
            status.failed += 1;
        }
        let current = crate::source_catalogs::disabled(&row.id);
        for id in current.union(&row.disabled_catalogs) {
            crate::source_catalogs::set_enabled(&row.id, id, !row.disabled_catalogs.contains(id))?;
        }
    }
    // Acknowledge the actual local materialization, including device-local
    // permission denials; this cannot echo a remote desired grant as an edit.
    let _mutation = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    commit_locked(&snapshot(services)?, true, false)?;
    if completed
        && status.pending == 0
        && status.failed == 0
        && let Some(basis) = basis
    {
        acknowledge_projection(&owner, &basis)?;
    }
    Ok(status)
}
fn acknowledge_projection(
    owner: &Arc<Mutex<nova_sync::Store>>,
    basis: &BTreeMap<String, nova_sync::Version>,
) -> Result<(), String> {
    let mut guard = owner.lock().unwrap_or_else(|e| e.into_inner());
    let mut next = guard.clone();
    // Store checks the current digest against the ORIGINAL input snapshot.
    // A merge during package transfer remains pending for a subsequent pass.
    next.mark_projected(&domain(), basis)
        .map_err(|e| e.to_string())?;
    *guard = next;
    Ok(())
}

fn install_receipt(
    services: &Services,
    row: &Descriptor,
    bytes: &[u8],
    cancel: &AtomicBool,
    previous_archive: Option<&str>,
) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) || !nova_sync::read_settings().enabled {
        return Err("package transfer cancelled".into());
    }
    verify_receipt(row, bytes)?;
    let _mutation = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if cancel.load(Ordering::Acquire) || !nova_sync::read_settings().enabled {
        return Err("package transfer cancelled".into());
    }
    let owner = nova_sync::local_store().map_err(|e| e.to_string())?;
    let current = owner
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .record(&domain(), &row.id)
        .and_then(|record| record.value.as_deref())
        .and_then(|raw| serde_json::from_str::<Descriptor>(raw).ok());
    if current.as_ref() != Some(row) {
        return Err("addon desired state changed during transfer".into());
    }
    let current_archive = services.sources.archive(&row.id).ok().map(|(hash, _)| hash);
    if current_archive.as_deref() != previous_archive {
        return Err("local addon changed during transfer".into());
    }
    let source = services
        .sources
        .install_zip(bytes)
        .map_err(|e| e.to_string())?;
    services
        .domain
        .update(|state| {
            state.sources.retain(|saved| saved.addon_id != source.id);
            state.sources.push(reader_domain::SourceDescriptor {
                id: source.id.clone(),
                addon_id: source.id.clone(),
                name: source.name.clone(),
                language: source.language.clone(),
                version: source.version.clone(),
            });
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    commit_locked(&snapshot(services)?, true, false)?;
    Ok(())
}

fn verify_receipt(row: &Descriptor, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > nova_sync::MAX_PACKAGE_BYTES
        || format!("{:x}", Sha256::digest(bytes)) != row.sha256
    {
        return Err("package digest mismatch".into());
    }
    let (source, hash) =
        reader_sources::SourceRegistry::inspect_archive(bytes).map_err(|e| e.to_string())?;
    if source.id != row.id || source.version != row.version || hash != row.sha256 {
        return Err("package descriptor mismatch".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    fn package(contract: &str) -> Vec<u8> {
        let manifest = serde_json::json!({"formatVersion":2,"contract":contract,"api":"^1.0","id":"fixture","name":"Fixture","version":"1.0.0","language":"en","entry":"main.js","permissions":{"domains":[]},"capabilities":["browse","search","filters","details","chapters","pages"]});
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("main.js", b"globalThis.source = {};".to_vec()),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
    #[test]
    fn field_intent_preserves_unmaterialized_account_values() {
        let before = Descriptor {
            id: "fixture".into(),
            version: "1.0.0".into(),
            sha256: "a".repeat(64),
            enabled: false,
            disabled_catalogs: BTreeSet::from(["fixture.old".into()]),
            settings: serde_json::json!({"a":"before","b":"before"}),
        };
        let mut actual = before.clone();
        actual.settings["a"] = serde_json::json!("local");
        actual.disabled_catalogs = BTreeSet::from(["fixture.local".into()]);
        let mut shared = before.clone();
        shared.enabled = true;
        shared.version = "2.0.0".into();
        shared.sha256 = "b".repeat(64);
        shared.settings["b"] = serde_json::json!("remote");
        shared.disabled_catalogs.insert("fixture.remote".into());
        let merged = merge_intent(&before, &actual, &shared).unwrap();
        assert!(merged.enabled);
        assert_eq!(merged.version, "2.0.0");
        assert_eq!(merged.sha256, "b".repeat(64));
        assert_eq!(
            merged.settings,
            serde_json::json!({"a":"local","b":"remote"})
        );
        assert_eq!(
            merged.disabled_catalogs,
            BTreeSet::from(["fixture.local".into(), "fixture.remote".into()])
        );
        let mut granted = before.clone();
        granted.enabled = true;
        assert!(
            !merge_intent(&granted, &actual, &shared).unwrap().enabled,
            "an actual local disable remains an account edit"
        );
        let mut expanded = actual.clone();
        expanded.version = "3.0.0".into();
        expanded.sha256 = "c".repeat(64);
        assert!(
            merge_intent(&granted, &expanded, &shared).unwrap().enabled,
            "a permission-expanding update does not opt other devices out"
        );
    }

    #[test]
    fn cookie_permission_expansion_revokes_locally_without_publishing_an_account_disable() {
        let directory = tempfile::tempdir().unwrap();
        let registry = reader_sources::SourceRegistry::open(directory.path()).unwrap();
        let make_package = |version: &str, names: Vec<&str>| {
            let manifest = serde_json::json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"fixture","name":"Fixture","version":version,"language":"en","entry":"main.js","permissions":{"domains":["example.com"],"cookies":[{"origin":"https://example.com","names":names}]}});
            let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for (name, bytes) in [
                ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
                ("main.js", b"void 0;".to_vec()),
            ] {
                zip.start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(&bytes).unwrap();
            }
            zip.finish().unwrap().into_inner()
        };
        registry
            .install_zip(&make_package("1.0.0", vec!["mature"]))
            .unwrap();
        registry.set_enabled("fixture", true).unwrap();
        let before = Descriptor {
            id: "fixture".into(),
            version: "1.0.0".into(),
            sha256: registry.archive("fixture").unwrap().0,
            enabled: true,
            disabled_catalogs: BTreeSet::new(),
            settings: serde_json::json!({}),
        };
        let expanded = registry
            .install_zip(&make_package("1.1.0", vec!["mature", "other"]))
            .unwrap();
        assert!(!expanded.enabled);
        let mut actual = before.clone();
        actual.version = expanded.version;
        actual.sha256 = registry.archive("fixture").unwrap().0;
        actual.enabled = expanded.enabled;
        let outbound = merge_intent(&before, &actual, &before).unwrap();
        assert!(
            outbound.enabled,
            "local permission denial must not disable another device's approved addon"
        );
        assert_eq!(outbound.version, "1.1.0");
        assert!(
            registry.source("fixture").is_err(),
            "a remote desired grant cannot activate new cookie authority"
        );
    }

    #[test]
    fn stale_addon_projection_does_not_ack_new_records() {
        let mut records = nova_sync::Store::default();
        let mut descriptor = Descriptor {
            id: "fixture".into(),
            version: "1.0.0".into(),
            sha256: "a".repeat(64),
            enabled: false,
            disabled_catalogs: BTreeSet::new(),
            settings: serde_json::json!({"label":"old"}),
        };
        assert!(records.set(
            &domain(),
            "fixture",
            Some(serde_json::to_string(&descriptor).unwrap()),
            0,
            1
        ));
        records.mark_pending(&domain());
        let original_basis = records.digest().get(&domain()).unwrap().clone();
        descriptor.settings = serde_json::json!({"label":"arrived during transfer"});
        assert!(records.set(
            &domain(),
            "fixture",
            Some(serde_json::to_string(&descriptor).unwrap()),
            0,
            2
        ));
        let owner = Arc::new(Mutex::new(records));
        acknowledge_projection(&owner, &original_basis).unwrap();
        assert!(owner.lock().unwrap().pending_domains().contains(&domain()));
    }

    #[test]
    fn receipts_require_hash_identity_version_and_reader_contract() {
        let bytes = package("nova-reader1");
        let mut row = Descriptor {
            id: "fixture".into(),
            version: "1.0.0".into(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            enabled: true,
            disabled_catalogs: BTreeSet::new(),
            settings: serde_json::json!({}),
        };
        verify_receipt(&row, &bytes).unwrap();
        row.id = "other".into();
        assert!(verify_receipt(&row, &bytes).is_err());
        row.id = "fixture".into();
        row.version = "2.0.0".into();
        assert!(verify_receipt(&row, &bytes).is_err());
        row.version = "1.0.0".into();
        let mut changed = bytes.clone();
        changed[0] ^= 1;
        assert!(verify_receipt(&row, &changed).is_err());
        let wrong = package("nova-provider1");
        row.sha256 = format!("{:x}", Sha256::digest(&wrong));
        assert!(verify_receipt(&row, &wrong).is_err());
    }
}
