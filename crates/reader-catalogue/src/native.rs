use crate::{CatalogueError, EXPANDED_LIMIT, MAX_RECORDS, Result, invalid};
use ed25519_dalek::{Signature, VerifyingKey};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeCatalogue {
    pub format_version: u32,
    pub repository: String,
    /// Monotonic publisher sequence, independent of package versions.
    pub sequence: u64,
    pub packages: Vec<NativePackage>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativePackage {
    pub id: String,
    pub name: String,
    pub version: Version,
    pub api: VersionReq,
    pub zip_url: String,
    pub sha256: String,
    pub sources: Vec<NativeSource>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeSource {
    pub id: String,
    pub name: String,
    pub language: String,
    pub nsfw: bool,
    pub capabilities: Vec<String>,
}

/// Trust is configured explicitly by the caller, never imported from an index.
/// Persist these keys and sequence floors in app-owned storage before fetching.
#[derive(Default)]
pub struct TrustStore {
    keys: BTreeMap<String, (String, VerifyingKey)>,
    minimum_sequences: BTreeMap<String, u64>,
}
impl TrustStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn trust_key(&mut self, repository: &str, id: &str, public_key: [u8; 32]) -> Result<()> {
        if repository.is_empty() || id.is_empty() || id.len() > 128 || self.keys.contains_key(id) {
            return Err(invalid(
                "key ID empty, too long, or already trusted; rotate explicitly",
            ));
        }
        let key = VerifyingKey::from_bytes(&public_key).map_err(invalid)?;
        if key.is_weak() {
            return Err(invalid("weak signing key"));
        }
        self.keys.insert(id.into(), (repository.into(), key));
        Ok(())
    }
    /// Rotation requires the application's explicit trust decision. Catalogue
    /// payloads cannot replace their own verifier or add additional trusted keys.
    pub fn remove_key(&mut self, id: &str) -> bool {
        self.keys.remove(id).is_some()
    }
    pub fn advance_sequence(&mut self, repository: &str, sequence: u64) {
        self.minimum_sequences
            .entry(repository.into())
            .and_modify(|v| *v = (*v).max(sequence))
            .or_insert(sequence);
    }
}

pub fn verify_native_catalogue(
    bytes: &[u8],
    key_id: &str,
    signature_hex: &str,
    trust: &TrustStore,
) -> Result<NativeCatalogue> {
    if bytes.len() > EXPANDED_LIMIT {
        return Err(CatalogueError::Limit);
    }
    let (trusted_repository, key) = trust
        .keys
        .get(key_id)
        .ok_or_else(|| CatalogueError::UnknownKey(key_id.into()))?;
    if signature_hex.len() != 128 {
        return Err(CatalogueError::Signature);
    }
    let signature = hex::decode(signature_hex).map_err(|_| CatalogueError::Signature)?;
    let signature = Signature::from_slice(&signature).map_err(|_| CatalogueError::Signature)?;
    key.verify_strict(bytes, &signature)
        .map_err(|_| CatalogueError::Signature)?;
    let catalogue: NativeCatalogue = serde_json::from_slice(bytes).map_err(invalid)?;
    if &catalogue.repository != trusted_repository {
        return Err(invalid("signing key belongs to a different repository"));
    }
    validate_native(&catalogue)?;
    if catalogue.sequence
        < trust
            .minimum_sequences
            .get(&catalogue.repository)
            .copied()
            .unwrap_or(0)
    {
        return Err(invalid("catalogue sequence rollback"));
    }
    Ok(catalogue)
}

pub(crate) fn https_url(raw: &str) -> Result<Url> {
    let mut url = Url::parse(raw).map_err(invalid)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid(
            "repository and artifact URLs must use HTTPS without credentials",
        ));
    }
    url.set_fragment(None);
    Ok(url)
}
fn validate_native(catalogue: &NativeCatalogue) -> Result<()> {
    if catalogue.format_version != 1 || catalogue.repository.is_empty() {
        return Err(invalid(
            "unsupported native catalogue version or empty repository identity",
        ));
    }
    if catalogue.packages.len() > MAX_RECORDS {
        return Err(CatalogueError::Limit);
    }
    let mut packages = BTreeSet::new();
    let mut source_count = 0;
    for package in &catalogue.packages {
        if package.id.is_empty() || package.id.len() > 128 || !packages.insert(&package.id) {
            return Err(invalid("empty, long or duplicate native addon ID"));
        }
        https_url(&package.zip_url)?;
        if package.sha256.len() != 64 || hex::decode(&package.sha256).is_err() {
            return Err(invalid("invalid native ZIP SHA-256"));
        }
        source_count += package.sources.len();
        if source_count > MAX_RECORDS {
            return Err(CatalogueError::Limit);
        }
        if package.sources.is_empty() {
            return Err(invalid("native addon has no sources"));
        }
        let mut sources = BTreeSet::new();
        for source in &package.sources {
            if source.id.is_empty() || !sources.insert(&source.id) || source.capabilities.len() > 32
            {
                return Err(invalid(
                    "empty/duplicate source ID or capability count limit",
                ));
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NativeState {
    Available,
    Installed,
    UpdateAvailable,
    Incompatible { reason: String },
}
pub fn native_state(
    package: &NativePackage,
    installed: Option<&Version>,
    api: &Version,
) -> NativeState {
    if !package.api.matches(api) {
        return NativeState::Incompatible {
            reason: format!("Requires reader API {}", package.api),
        };
    }
    const OPERATIONS: &[&str] = &[
        "capabilities",
        "browse",
        "search",
        "filters",
        "details",
        "chapters",
        "pages",
        "resolvePage",
    ];
    if let Some(capability) = package
        .sources
        .iter()
        .flat_map(|s| &s.capabilities)
        .find(|c| !OPERATIONS.contains(&c.as_str()))
    {
        return NativeState::Incompatible {
            reason: format!("Unsupported capability {capability}"),
        };
    }
    match installed {
        Some(version) if version < &package.version => NativeState::UpdateAvailable,
        Some(_) => NativeState::Installed,
        None => NativeState::Available,
    }
}
