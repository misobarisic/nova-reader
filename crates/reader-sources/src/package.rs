use crate::{Result, Source, SourceError, network::NetworkTransport};
use nova_storage::{
    fs::atomic_write,
    zip::{self as bounded, Limits, PathRule, Policy, printable_path},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Cursor, Read},
    path::Path,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};
const MAX_ARCHIVE: usize = 8 * 1024 * 1024;
const MAX_EXPANDED: usize = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 162;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub format_version: u32,
    pub contract: String,
    pub api: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub language: String,
    #[serde(default)]
    pub nsfw: bool,
    pub entry: String,
    #[serde(default)]
    pub scripts: Vec<String>,
    #[serde(default)]
    pub assets: Vec<String>,
    /// Optional PNG path declared in assets; never a remote URL.
    #[serde(default)]
    pub icon: Option<String>,
    pub permissions: Permissions,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub configuration: Vec<crate::SettingField>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Permissions {
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub http_post: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cookies: Vec<CookiePermission>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CookiePermission {
    pub origin: String,
    pub names: Vec<String>,
}
impl Permissions {
    pub(crate) fn expands(&self, old: &Self) -> bool {
        (self.http_post && !old.http_post)
            || self.domains.iter().any(|d| {
                !old.domains.iter().any(|old| {
                    d.eq_ignore_ascii_case(old)
                        || d.to_ascii_lowercase()
                            .ends_with(&format!(".{}", old.to_ascii_lowercase()))
                })
            })
            || self.cookies.iter().any(|cookie| {
                cookie.names.iter().any(|name| {
                    !old.cookies
                        .iter()
                        .any(|prior| prior.origin == cookie.origin && prior.names.contains(name))
                })
            })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceDescriptor {
    pub id: String,
    pub name: String,
    pub version: String,
    pub language: String,
    pub nsfw: bool,
    pub enabled: bool,
    pub capabilities: Vec<String>,
}
impl SourceDescriptor {
    pub(crate) fn new(m: &Manifest, enabled: bool) -> Self {
        Self {
            id: m.id.clone(),
            name: m.name.clone(),
            version: m.version.clone(),
            language: m.language.clone(),
            nsfw: m.nsfw,
            enabled,
            capabilities: m.capabilities.clone(),
        }
    }
}
/// A package's configuration for one profile, secrets included ([`SourceRegistry::export_settings`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportedSettings {
    pub values: serde_json::Map<String, serde_json::Value>,
    pub secrets: BTreeMap<String, String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Installed {
    digest: String,
    enabled: bool,
}
#[derive(Default, Clone, Serialize, Deserialize)]
struct State {
    installed: BTreeMap<String, Installed>,
    versions: BTreeMap<String, String>,
}
struct Inner {
    root: std::path::PathBuf,
    _lock: fs::File,
    state: State,
    active: BTreeMap<String, Arc<AtomicBool>>,
    sessions: BTreeMap<(String, String), Arc<NetworkTransport>>,
    settings: BTreeMap<(String, String), Arc<Mutex<serde_json::Value>>>,
}
#[derive(Clone)]
pub struct SourceRegistry {
    inner: Arc<Mutex<Inner>>,
    /// Shared by every clone, so one switch retargets all holders of the registry.
    profile: Arc<RwLock<String>>,
}
fn invalid(s: impl Into<String>) -> SourceError {
    SourceError::Package(s.into())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn validate_manifest(m: &Manifest) -> Result<BTreeSet<String>> {
    crate::settings::validate_schema(&m.configuration)?;
    if m.format_version != 2 || m.contract != "nova-reader1" {
        return Err(invalid("expected format 2 nova-reader1 contract"));
    }
    let version = semver::Version::parse(&m.version)
        .map_err(|_| invalid("invalid addon semantic version"))?;
    let _ = version;
    let api = semver::VersionReq::parse(&m.api).map_err(|_| invalid("invalid API range"))?;
    if !api.matches(&semver::Version::new(1, 0, 0)) {
        return Err(invalid("unsupported reader API range"));
    }
    if m.id.is_empty()
        || m.id.len() > 64
        || !m
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || m.name.is_empty()
        || m.name.len() > 128
        || m.language.is_empty()
        || m.language.len() > 16
    {
        return Err(invalid("invalid source identity"));
    }
    if m.scripts.len() > 32
        || m.assets.len() > 128
        || m.permissions.domains.len() > 32
        || m.capabilities.len() > 16
    {
        return Err(invalid("manifest collection limit"));
    }
    for domain in &m.permissions.domains {
        if domain.len() > 253
            || domain.is_empty()
            || domain.split('.').any(|label| {
                label.is_empty()
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            || domain.parse::<std::net::IpAddr>().is_ok()
            || !domain.contains('.')
        {
            return Err(invalid("domains must be explicit DNS names"));
        }
    }
    if m.permissions.cookies.len() > 16 {
        return Err(invalid("cookie permission limit"));
    }
    let mut origins = BTreeSet::new();
    let mut cookie_count = 0;
    for cookie in &m.permissions.cookies {
        crate::network::checked_cookie_origin(&cookie.origin, &m.permissions)
            .map_err(|_| invalid("invalid cookie permission origin"))?;
        cookie_count += cookie.names.len();
        let mut names = BTreeSet::new();
        if !origins.insert(&cookie.origin)
            || cookie.names.is_empty()
            || cookie_count > 32
            || cookie
                .names
                .iter()
                .any(|name| !crate::network::valid_cookie_name(name) || !names.insert(name))
        {
            return Err(invalid("invalid or excessive cookie names"));
        }
    }
    for cap in &m.capabilities {
        if ![
            "capabilities",
            "browse",
            "search",
            "filters",
            "details",
            "chapters",
            "pages",
            "resolvePage",
        ]
        .contains(&cap.as_str())
        {
            return Err(invalid("unsupported reader operation"));
        }
    }
    let mut paths = BTreeSet::from(["manifest.json".into()]);
    for path in std::iter::once(&m.entry)
        .chain(m.scripts.iter())
        .chain(m.assets.iter())
    {
        if !printable_path(path) || !paths.insert(path.clone()) {
            return Err(invalid("unsafe or duplicate declared file"));
        }
    }
    for script in std::iter::once(&m.entry).chain(m.scripts.iter()) {
        if !script.ends_with(".js") {
            return Err(invalid("scripts must be JavaScript"));
        }
    }
    if let Some(icon) = &m.icon
        && (!m.assets.contains(icon) || !icon.ends_with(".png"))
    {
        return Err(invalid("icon must be a declared PNG asset"));
    }
    Ok(paths)
}
fn archive_error(error: bounded::Error) -> SourceError {
    use bounded::Error::*;
    invalid(match error {
        Archive(_) => "invalid ZIP",
        TruncatedDirectory | TooManyEntries => "invalid or oversized ZIP directory",
        DuplicateEntry => "duplicate ZIP entry",
        UnsafePath | NotRegular | Directory | Encrypted => {
            "unsafe, duplicate or non-file ZIP entry"
        }
        EntryTooLarge | ExpandedTooLarge => "expanded archive limit",
        SizeMismatch | Missing | Io(_) => "unreadable ZIP entry",
    })
}
pub(crate) fn validate(bytes: &[u8]) -> Result<(Manifest, String)> {
    if bytes.len() > MAX_ARCHIVE {
        return Err(invalid("archive exceeds 8 MiB"));
    }
    // The zip crate indexes by name and silently coalesces duplicate central
    // records; the bounded reader counts the raw directory before trusting it.
    let mut zip = bounded::Archive::open(
        Cursor::new(bytes),
        Limits {
            max_entries: MAX_ENTRIES,
            max_entry_bytes: MAX_EXPANDED as u64,
            max_expanded_bytes: MAX_EXPANDED as u64,
        },
        Policy {
            paths: PathRule::Printable,
            directories: false,
            regular_files_only: true,
        },
    )
    .map_err(archive_error)?;
    let mut files = BTreeMap::new();
    for index in 0..zip.entries().len() {
        let name = zip.entries()[index].name.clone();
        let data = zip
            .read(index, MAX_EXPANDED as u64)
            .map_err(archive_error)?;
        files.insert(name, data);
    }
    let m: Manifest = serde_json::from_slice(
        files
            .get("manifest.json")
            .ok_or_else(|| invalid("missing manifest"))?,
    )
    .map_err(|e| invalid(format!("invalid manifest: {e}")))?;
    let expected = validate_manifest(&m)?;
    if expected != files.keys().cloned().collect() {
        return Err(invalid("missing or unlisted file"));
    }
    if let Some(icon) = &m.icon {
        let data = &files[icon];
        if data.len() > 256 * 1024
            || data.len() < 24
            || &data[..8] != b"\x89PNG\r\n\x1a\n"
            || &data[12..16] != b"IHDR"
            || [16, 20].iter().any(|&start| {
                let side = u32::from_be_bytes(data[start..start + 4].try_into().unwrap());
                side == 0 || side > 512
            })
        {
            return Err(invalid("icon exceeds PNG size/dimension limits"));
        }
    }
    let mut script = String::new();
    for path in m.scripts.iter().chain(std::iter::once(&m.entry)) {
        let data = files.get(path).unwrap();
        if script.len() + data.len() + 2 > 2 * 1024 * 1024 {
            return Err(invalid("JavaScript exceeds 2 MiB"));
        }
        script.push_str(std::str::from_utf8(data).map_err(|_| invalid("script is not UTF-8"))?);
        script.push_str("\n;\n");
    }
    Ok((m, script))
}
impl SourceRegistry {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(root.as_ref().join("packages"))?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.as_ref().join("registry.lock"))?;
        lock.try_lock()
            .map_err(|_| invalid("source registry is already open in another instance"))?;
        let path = root.as_ref().join("registry.json");
        let state = if path.exists() {
            serde_json::from_slice::<State>(&fs::read(path)?)
                .map_err(|_| invalid("corrupt registry"))?
        } else {
            State::default()
        };
        let mut active = BTreeMap::new();
        for (id, row) in &state.installed {
            if row.digest.len() != 64 || !row.digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid("invalid registry digest"));
            }
            let data = fs::read(
                root.as_ref()
                    .join("packages")
                    .join(format!("{}.zip", row.digest)),
            )?;
            if digest(&data) != row.digest {
                return Err(invalid("installed package digest mismatch"));
            }
            let (m, _) = validate(&data)?;
            if m.id != *id {
                return Err(invalid("installed identity mismatch"));
            }
            active.insert(id.clone(), Arc::new(AtomicBool::new(row.enabled)));
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                root: root.as_ref().into(),
                _lock: lock,
                state,
                active,
                sessions: BTreeMap::new(),
                settings: BTreeMap::new(),
            })),
            profile: Arc::new(RwLock::new("default".into())),
        })
    }
    /// Session state is in memory and isolated by profile and source; archives remain app-wide.
    pub fn with_profile(&self, profile: impl Into<String>) -> Self {
        Self {
            inner: self.inner.clone(),
            profile: Arc::new(RwLock::new(profile.into())),
        }
    }
    /// Retarget every clone of this registry. Sources already obtained keep the
    /// settings of the profile they were opened for.
    pub fn set_profile(&self, profile: impl Into<String>) {
        *self.profile.write().unwrap_or_else(|e| e.into_inner()) = profile.into();
    }
    fn profile(&self) -> String {
        self.profile
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Delete the persisted settings of a removed profile. Installed packages
    /// are app-wide and stay.
    pub fn remove_profile_settings(&self, profile: &str) -> Result<()> {
        let inner = self.inner.lock().unwrap();
        let directory = crate::settings::path(&inner.root, profile, "x")
            .parent()
            .map(std::path::Path::to_path_buf);
        drop(inner);
        match directory {
            Some(directory) => match fs::remove_dir_all(directory) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            },
            None => Ok(()),
        }
    }
    pub fn install_zip(&self, bytes: &[u8]) -> Result<SourceDescriptor> {
        let (m, script) = validate(bytes)?;
        crate::runtime::validate_script(&script)?;
        let hash = digest(bytes);
        let mut inner = self.inner.lock().unwrap();
        let mut state = inner.state.clone();
        let key = format!("{}@{}", m.id, m.version);
        if state
            .versions
            .get(&key)
            .is_some_and(|previous| previous != &hash)
        {
            return Err(invalid("changed bytes at unchanged source ID/version"));
        }
        let mut enabled = state.installed.get(&m.id).is_some_and(|row| row.enabled);
        if let Some(previous) = state.installed.get(&m.id) {
            let old_bytes = fs::read(
                inner
                    .root
                    .join("packages")
                    .join(format!("{}.zip", previous.digest)),
            )?;
            let (old, _) = validate(&old_bytes)?;
            if m.permissions.expands(&old.permissions) {
                enabled = false;
            }
        }
        // Installation is explicit; a newly installed source is enabled only by a separate user action.
        let package_path = inner.root.join("packages").join(format!("{hash}.zip"));
        atomic_write(&package_path, bytes)?;
        state.versions.insert(key, hash.clone());
        state.installed.insert(
            m.id.clone(),
            Installed {
                digest: hash,
                enabled,
            },
        );
        atomic_write(
            &inner.root.join("registry.json"),
            &serde_json::to_vec(&state).map_err(|e| invalid(e.to_string()))?,
        )?;
        if let Some(old) = inner
            .active
            .insert(m.id.clone(), Arc::new(AtomicBool::new(enabled)))
        {
            old.store(false, Ordering::Release)
        }
        inner.sessions.retain(|(_, id), _| id != &m.id);
        inner.settings.retain(|(_, id), _| id != &m.id);
        inner.state = state;
        Ok(SourceDescriptor::new(&m, enabled))
    }
    pub fn list(&self) -> Result<Vec<SourceDescriptor>> {
        let ids: Vec<_> = self
            .inner
            .lock()
            .unwrap()
            .state
            .installed
            .keys()
            .cloned()
            .collect();
        ids.iter()
            .map(|id| self.source_unchecked(id).map(|s| s.descriptor()))
            .collect()
    }
    /// Validate an archive without installing or granting its permissions.
    pub fn inspect_archive(bytes: &[u8]) -> Result<(SourceDescriptor, String)> {
        let (manifest, script) = validate(bytes)?;
        crate::runtime::validate_script(&script)?;
        Ok((SourceDescriptor::new(&manifest, false), digest(bytes)))
    }
    /// Only currently installed, verified archives can be served to paired peers.
    pub fn archive(&self, id: &str) -> Result<(String, Vec<u8>)> {
        let source = self.source_unchecked(id)?;
        let bytes = source.archive.as_ref().clone();
        Ok((digest(&bytes), bytes))
    }
    /// Read verified package decoration even for disabled addons. No script or
    /// network runs, and package updates/removal cannot supply stale bytes.
    pub fn icon(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let source = self.source_unchecked(id)?;
        source
            .manifest
            .icon
            .as_deref()
            .map(|path| asset(&source.archive, &source.manifest, path))
            .transpose()
    }
    fn source_unchecked(&self, id: &str) -> Result<Source> {
        let profile = self.profile();
        let mut inner = self.inner.lock().unwrap();
        let row = inner
            .state
            .installed
            .get(id)
            .ok_or(SourceError::Unavailable)?;
        let bytes = fs::read(
            inner
                .root
                .join("packages")
                .join(format!("{}.zip", row.digest)),
        )?;
        if digest(&bytes) != row.digest {
            return Err(invalid("installed package digest mismatch"));
        }
        let (m, script) = validate(&bytes)?;
        let active = inner.active.get(id).unwrap().clone();
        let cache_partition = serde_json::to_string(&(inner.root.to_string_lossy(), &profile, id))
            .expect("source profile identity serializes");
        let transport = inner
            .sessions
            .entry((profile.clone(), id.into()))
            .or_insert_with(|| {
                Arc::new(NetworkTransport::new_scoped(
                    m.permissions.clone(),
                    cache_partition,
                ))
            })
            .clone();
        let key = (profile.clone(), id.to_owned());
        if !inner.settings.contains_key(&key) {
            let values = crate::settings::load(
                &crate::settings::path(&inner.root, &profile, id),
                &m.configuration,
            )?;
            inner
                .settings
                .insert(key.clone(), Arc::new(Mutex::new(values)));
        }
        let settings = inner.settings.get(&key).unwrap().clone();
        Ok(Source {
            settings,
            manifest: m,
            script: Arc::new(script),
            archive: Arc::new(bytes),
            transport,
            active,
        })
    }
    pub fn settings_schema(&self, id: &str) -> Result<Vec<crate::SettingField>> {
        Ok(self.source_unchecked(id)?.manifest.configuration)
    }
    /// A safe editor snapshot: secret keys are omitted, including empty secrets.
    pub fn settings(&self, id: &str) -> Result<serde_json::Value> {
        let source = self.source_unchecked(id)?;
        let mut values = source.settings.lock().unwrap().clone();
        for field in &source.manifest.configuration {
            if field.kind == crate::SettingKind::Secret {
                values.as_object_mut().unwrap().remove(&field.key);
            }
        }
        Ok(values)
    }
    /// The profile's complete configuration for a full backup: public values and
    /// every non-empty secret in plaintext. Nothing else reads secrets back out.
    pub fn export_settings(&self, id: &str) -> Result<ExportedSettings> {
        let source = self.source_unchecked(id)?;
        let values = source.settings.lock().unwrap();
        let mut exported = ExportedSettings::default();
        for field in &source.manifest.configuration {
            let Some(value) = values.get(&field.key) else {
                continue;
            };
            if field.kind == crate::SettingKind::Secret {
                if let Some(secret) = value.as_str().filter(|secret| !secret.is_empty()) {
                    exported
                        .secrets
                        .insert(field.key.clone(), secret.to_owned());
                }
            } else {
                exported.values.insert(field.key.clone(), value.clone());
            }
        }
        Ok(exported)
    }
    /// Apply configuration from a backup. Unlike [`SourceRegistry::set_settings`]
    /// it is lenient: a key the installed package no longer declares, or a value
    /// that its schema rejects, is skipped and counted, and the rest applies.
    pub fn import_settings(
        &self,
        id: &str,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<usize> {
        let fields = self.settings_schema(id)?;
        let mut patch = serde_json::Map::new();
        let mut skipped = 0;
        for (key, value) in values {
            match fields.iter().find(|field| &field.key == key) {
                Some(field) if crate::settings::valid_value(field, value) => {
                    patch.insert(key.clone(), value.clone());
                }
                _ => skipped += 1,
            }
        }
        if !patch.is_empty() {
            self.set_settings(id, serde_json::Value::Object(patch))?;
        }
        Ok(skipped)
    }
    pub fn settings_secrets(&self, id: &str) -> Result<BTreeMap<String, bool>> {
        let source = self.source_unchecked(id)?;
        let values = source.settings.lock().unwrap();
        Ok(source
            .manifest
            .configuration
            .iter()
            .filter(|f| f.kind == crate::SettingKind::Secret)
            .map(|f| {
                (
                    f.key.clone(),
                    values
                        .get(&f.key)
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|s| !s.is_empty()),
                )
            })
            .collect())
    }
    /// Partial updates retain omitted values. An explicit empty secret clears it.
    pub fn set_settings(&self, id: &str, patch: serde_json::Value) -> Result<()> {
        let profile = self.profile();
        let source = self.source_unchecked(id)?;
        let inner = self.inner.lock().unwrap();
        if inner
            .active
            .get(id)
            .is_none_or(|active| !Arc::ptr_eq(active, &source.active))
        {
            return Err(SourceError::Unavailable);
        }
        let mut current = source.settings.lock().unwrap();
        let merged = crate::settings::merge(&current, &patch, &source.manifest.configuration)?;
        crate::settings::persist(
            &crate::settings::path(&inner.root, &profile, id),
            &merged,
            &source.manifest.configuration,
        )?;
        if *current != merged {
            // Freeze running invocations on their old jar; new settings must not
            // inherit cookies set from an earlier settings snapshot.
            if let Some(session) = inner.sessions.get(&(profile.clone(), id.to_owned())) {
                session.reset_session();
            }
            *current = merged;
        }
        Ok(())
    }
    pub fn source(&self, id: &str) -> Result<Source> {
        let source = self.source_unchecked(id)?;
        if !source.active.load(Ordering::Acquire) {
            return Err(SourceError::Unavailable);
        }
        Ok(source)
    }
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let mut state = inner.state.clone();
        state
            .installed
            .get_mut(id)
            .ok_or(SourceError::Unavailable)?
            .enabled = enabled;
        atomic_write(
            &inner.root.join("registry.json"),
            &serde_json::to_vec(&state).unwrap(),
        )?;
        inner.state = state;
        if let Some(previous) = inner
            .active
            .insert(id.into(), Arc::new(AtomicBool::new(enabled)))
        {
            previous.store(false, Ordering::Release)
        }
        inner.sessions.retain(|(_, source), _| source != id);
        Ok(())
    }
    pub fn disable(&self, id: &str) -> Result<()> {
        self.set_enabled(id, false)
    }
    pub fn remove(&self, id: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let mut state = inner.state.clone();
        state.installed.remove(id).ok_or(SourceError::Unavailable)?;
        atomic_write(
            &inner.root.join("registry.json"),
            &serde_json::to_vec(&state).unwrap(),
        )?;
        inner.state = state;
        if let Some(active) = inner.active.remove(id) {
            active.store(false, Ordering::Release)
        }
        inner.sessions.retain(|(_, source), _| source != id);
        Ok(())
    }
}

pub(crate) fn asset(bytes: &[u8], manifest: &Manifest, path: &str) -> Result<Vec<u8>> {
    if !manifest.assets.iter().any(|p| p == path) {
        return Err(SourceError::NotFound);
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| invalid("invalid ZIP"))?;
    let mut file = zip.by_name(path).map_err(|_| SourceError::NotFound)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    Ok(data)
}
