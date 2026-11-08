//! Full-device export and import: one `.nova-reader-backup` archive holds the
//! chosen profiles (library, progress, settings, addon configuration) and the
//! device's own state (addon packages, device settings, optionally downloads),
//! and an import maps the profiles onto this device, previews every change and
//! applies it as one durable transaction that can be undone.
//!
//! The container (ZIP, manifest, checksums, limits) is the kit's `nova-backup`;
//! `archive.rs` binds it to Reader's components (`components.rs`). `export`
//! below writes an archive, `apply.rs` restores one. See `docs/backups.md`.
use crate::services::Services;
use components::{ProfileCapture, Rows};
use reader_domain::ReaderState;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub(crate) mod archive;
pub(crate) mod components;

/// The file extension of an archive, without the dot.
pub const EXTENSION: &str = "nova-reader-backup";
/// The device preference row; only `open_last_profile_on_startup` is exported.
pub(crate) const DEVICE_PREFERENCES_KEY: &str = reader_domain::profile::DEVICE_KEY;

/// What an export contains: components per profile, and device components.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub profiles: BTreeMap<String, BTreeSet<String>>,
    pub device: BTreeSet<String>,
}
impl Selection {
    /// Every live profile with every component, and the device's own state.
    /// Downloaded and imported chapter files are large, so they are opt-in.
    pub fn everything(profiles: &[crate::profiles::Profile], files: Files) -> Self {
        let mut selection = Self::default();
        for def in components::COMPONENTS {
            match def.scope {
                components::Scope::Profile => {
                    for profile in profiles {
                        selection
                            .profiles
                            .entry(profile.id.clone())
                            .or_default()
                            .insert(def.id.to_owned());
                    }
                }
                components::Scope::Device if def.everything => {
                    selection.device.insert(def.id.to_owned());
                }
                components::Scope::Device => {}
            }
        }
        for (wanted, name) in [
            (files.downloads, "downloads"),
            (files.local, "local_imports"),
        ] {
            if wanted && components::definition(name).is_some() {
                selection.device.insert(name.to_owned());
            }
        }
        selection
    }
}
/// Which large file sets an export or import includes. Both default to off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Files {
    pub downloads: bool,
    pub local: bool,
}

/// A consistent read of the stores an export or a preview depends on.
pub(crate) struct Snapshot {
    pub profiles: Vec<crate::profiles::Profile>,
    pub states: BTreeMap<String, ReaderState>,
    pub rows: BTreeMap<String, Rows>,
    pub device_rows: Rows,
    pub revisions: BTreeMap<String, String>,
}

const DEVICE_ROW_PREFIXES: &[&str] = &[
    "reader:image-storage",
    "reader:image-limits",
    "reader:captures",
    components::COMPRESSION_PREFIX,
    DEVICE_PREFERENCES_KEY,
];

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Hash of a profile's durable rows and its sync records: a preview is valid
/// only while this is unchanged.
pub(crate) fn revision(id: &str, rows: &Rows, store: &nova_sync::Store) -> String {
    let digest = store
        .digest()
        .into_iter()
        .filter(|(domain, _)| {
            nova_sync::parse_profile_domain(domain).is_some_and(|(owner, _)| owner == id)
                || domain == crate::profiles::DOMAIN
                || domain == crate::profiles::DELETIONS_DOMAIN
        })
        .collect::<BTreeMap<_, _>>();
    archive::digest(
        serde_json::to_string(&(rows, digest))
            .expect("revision serializes")
            .as_bytes(),
    )
}

/// Strip `profile:{id}:` from the rows of one profile.
pub(crate) fn profile_rows(id: &str, all: &BTreeMap<String, String>) -> Rows {
    let prefix = format!("profile:{id}:");
    all.range(prefix.clone()..)
        .take_while(|(key, _)| key.starts_with(&prefix))
        .map(|(key, value)| (key[prefix.len()..].to_owned(), value.clone()))
        .collect()
}

impl Snapshot {
    /// Let queued writes land and remote records materialize, then read the
    /// states and rows of `ids` and the device rows together, under the domain
    /// lock and the record owner (in their documented order).
    pub fn take(services: &Services, ids: &[String]) -> Result<Self, String> {
        nova_sync::flush_persistence().map_err(|e| e.to_string())?;
        crate::sync::apply_remote(&services.domain).map_err(|e| e.to_string())?;
        let profiles = crate::profiles::list()?;
        let invalid = |message: String| reader_domain::Error::Invalid(message);
        services
            .domain
            .exclusive(|domain| {
                let handle = nova_sync::local_store().map_err(|e| invalid(e.to_string()))?;
                let store = handle.lock().unwrap_or_else(|e| e.into_inner());
                let mut prefixes: Vec<String> =
                    ids.iter().map(|id| format!("profile:{id}:")).collect();
                prefixes.extend(DEVICE_ROW_PREFIXES.iter().map(|p| (*p).to_owned()));
                let all: BTreeMap<String, String> = nova_storage::try_scan_prefixes(&prefixes)?
                    .into_iter()
                    .collect();
                let mut snapshot = Self {
                    profiles: profiles.clone(),
                    states: BTreeMap::new(),
                    rows: BTreeMap::new(),
                    device_rows: all
                        .iter()
                        .filter(|(key, _)| !key.starts_with("profile:"))
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                    revisions: BTreeMap::new(),
                };
                for id in ids {
                    let rows = profile_rows(id, &all);
                    snapshot
                        .revisions
                        .insert(id.clone(), revision(id, &rows, &store));
                    snapshot.states.insert(id.clone(), domain.read(id)?);
                    snapshot.rows.insert(id.clone(), rows);
                }
                Ok(snapshot)
            })
            .map_err(|e| e.to_string())
    }
}

/// What an export wrote.
#[derive(Clone, Debug)]
pub struct Exported {
    pub path: PathBuf,
    pub profiles: usize,
    pub bytes: u64,
}

/// Write the selected data to `destination` as one archive. The file is staged
/// beside the destination, flushed to disk and renamed over it, so a failure
/// never leaves a partial archive where a good one was.
pub fn export(
    services: &Services,
    selection: &Selection,
    destination: &Path,
) -> Result<Exported, String> {
    use std::io::{BufWriter, Write};
    if selection.profiles.values().all(BTreeSet::is_empty) && selection.device.is_empty() {
        return Err("Choose something to export".into());
    }
    let ids: Vec<String> = selection.profiles.keys().cloned().collect();
    let snapshot = Snapshot::take(services, &ids)?;
    let mut manifest = archive::Manifest {
        format: archive::FORMAT.into(),
        version: 1,
        created_at: crate::services::now().max(0) as u64,
        app_version: env!("CARGO_PKG_VERSION").into(),
        archive_id: format!(
            "{}-{}",
            now_ms(),
            nova_storage::try_allocate_local_sequence("backup", 1).map_err(|e| e.to_string())?
        ),
        profiles: Vec::new(),
        packages: Vec::new(),
    };
    let mut payloads = BTreeMap::new();
    let describe = |archive_id: &str, names: &BTreeSet<String>| -> Vec<archive::Component> {
        names
            .iter()
            .map(|name| archive::Component {
                name: name.clone(),
                schema_version: 1,
                path: format!("profiles/{archive_id}/{name}.json"),
                item_count: 0,
                sha256: String::new(),
            })
            .collect()
    };
    for (index, (id, names)) in selection.profiles.iter().enumerate() {
        if names.is_empty() {
            continue;
        }
        let profile = snapshot
            .profiles
            .iter()
            .find(|profile| &profile.id == id)
            .ok_or("Profile changed during export; retry")?;
        let registry = services
            .sources
            .with_profile(crate::profiles::registry_profile(id));
        let cx = ProfileCapture {
            state: &snapshot.states[id],
            rows: &snapshot.rows[id],
            registry: &registry,
        };
        let archive_id = format!("p{index}");
        for name in names {
            match components::definition(name) {
                Some(def) if def.scope == components::Scope::Profile => {}
                _ => return Err(format!("Unknown profile component: {name}")),
            }
            payloads.insert(
                (archive_id.clone(), name.clone()),
                components::capture_profile(name, &cx)?,
            );
        }
        manifest.profiles.push(archive::Profile {
            components: describe(&archive_id, names),
            archive_profile_id: archive_id,
            source_profile_id: id.clone(),
            name: profile.name.clone(),
        });
    }
    let mut packages = BTreeMap::new();
    if !selection.device.is_empty() {
        let cx = components::DeviceCapture {
            rows: &snapshot.device_rows,
            registry: &services.sources,
        };
        for name in &selection.device {
            match components::definition(name) {
                Some(def) if def.scope == components::Scope::Device => {}
                _ => return Err(format!("Unknown device component: {name}")),
            }
            let payload = components::capture_device(name, &cx)?;
            if name == "packages" {
                for value in payload.records.values() {
                    let hash = value["sha256"].as_str().ok_or("Invalid package record")?;
                    let id = value["id"].as_str().ok_or("Invalid package record")?;
                    let (actual, bytes) =
                        services.sources.archive(id).map_err(|e| e.to_string())?;
                    if actual != hash {
                        return Err("An addon changed during export; retry".into());
                    }
                    manifest.packages.push(archive::Package {
                        id: id.to_owned(),
                        version: value["version"].as_str().unwrap_or_default().to_owned(),
                        path: format!("packages/{hash}.zip"),
                        sha256: hash.to_owned(),
                    });
                    packages.insert(hash.to_owned(), bytes);
                }
            }
            payloads.insert((archive::DEVICE.to_owned(), name.clone()), payload);
        }
        manifest.profiles.push(archive::Profile {
            components: describe(archive::DEVICE, &selection.device),
            archive_profile_id: archive::DEVICE.into(),
            source_profile_id: String::new(),
            name: "This device".into(),
        });
    }

    let directory = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staged = tempfile::Builder::new()
        .prefix(".nova-reader-backup-")
        .suffix(".tmp")
        .tempfile_in(directory)
        .map_err(|e| e.to_string())?;
    let out = BufWriter::new(staged.reopen().map_err(|e| e.to_string())?);
    let mut writer = archive::Writer::new(out);
    let manifest = writer.write(manifest, &payloads, &packages)?;
    let mut out = writer.finish(&manifest)?;
    out.flush().map_err(|e| e.to_string())?;
    let file = out.into_inner().map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    let bytes = file.metadata().map_err(|e| e.to_string())?.len();
    drop(file);
    staged
        .persist(destination)
        .map_err(|e| e.error.to_string())?;
    Ok(Exported {
        path: destination.to_owned(),
        profiles: manifest
            .profiles
            .iter()
            .filter(|profile| profile.archive_profile_id != archive::DEVICE)
            .count(),
        bytes,
    })
}

/// A readable summary of an archive file after full validation. Nothing is applied.
pub fn describe(path: &Path) -> Result<String, String> {
    if !archive::is_zip(path)? {
        return Err("This is not a Nova Reader backup archive".into());
    }
    let decoded = archive::decode_file(path)?;
    let mut lines = vec![format!(
        "Nova Reader backup {} (app {}, created {})",
        decoded.manifest.archive_id, decoded.manifest.app_version, decoded.manifest.created_at
    )];
    for profile in &decoded.manifest.profiles {
        lines.push(format!("{}:", profile.name));
        for component in &profile.components {
            let label = components::definition(&component.name)
                .map_or(component.name.as_str(), |def| def.label);
            lines.push(format!("  {label}: {} records", component.item_count));
        }
    }
    for package in &decoded.manifest.packages {
        lines.push(format!("Package {} {}", package.id, package.version));
    }
    if !decoded.unsupported.is_empty() {
        lines.push(format!("Unsupported: {}", decoded.unsupported.join(", ")));
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests;
