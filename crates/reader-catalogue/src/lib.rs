//! Catalogue discovery never executes APK/JAR files or installs native ZIPs.
//! Detached signatures authenticate exact catalogue bytes against explicit trust.
mod fetch;
mod native;
pub mod proto;
pub use fetch::{CatalogueClient, FetchResponse, HttpTransport, Transport};
pub use native::{
    NativeCatalogue, NativePackage, NativeSource, NativeState, TrustStore, native_state,
    verify_native_catalogue,
};

use flate2::read::MultiGzDecoder;
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

pub const COMPRESSED_LIMIT: usize = 8 * 1024 * 1024;
pub const EXPANDED_LIMIT: usize = 32 * 1024 * 1024;
pub const MAX_RECORDS: usize = 20_000;
pub const MAX_DEPTH: usize = 4;
pub const KEIYOUSHI_INDEX: &str =
    "https://raw.githubusercontent.com/keiyoushi/extensions/repo/index.pb";

#[derive(Debug, Error)]
pub enum CatalogueError {
    #[error("catalogue size or record limit exceeded")]
    Limit,
    #[error("invalid catalogue: {0}")]
    Invalid(String),
    #[error("repository fetch: {0}")]
    Network(String),
    #[error("repository link cycle or recursion limit")]
    Recursion,
    #[error("unknown trusted signing key: {0}")]
    UnknownKey(String),
    #[error("catalogue signature verification failed")]
    Signature,
}
pub type Result<T> = std::result::Result<T, CatalogueError>;
fn invalid(message: impl ToString) -> CatalogueError {
    CatalogueError::Invalid(message.to_string())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpstreamSource {
    /// Decimal text preserves all signed 64-bit IDs through JSON/JavaScript.
    pub id: String,
    pub name: String,
    pub language: String,
    pub home_url: String,
    pub mirror_urls: Vec<String>,
    pub message: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpstreamExtension {
    pub name: String,
    pub package_name: String,
    pub version_code: String,
    pub version_name: String,
    pub extension_lib: String,
    /// Original enum integer: unknown future warning values remain visible.
    pub content_warning: i32,
    pub apk_url: String,
    pub jar_url: String,
    pub icon_url: String,
    pub sources: Vec<UpstreamSource>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpstreamCatalogue {
    pub repository: String,
    pub name: String,
    pub badge_label: String,
    /// Upstream APK signing metadata does not authenticate a Nova ZIP.
    pub upstream_signing_key: String,
    pub website: String,
    pub discord: Option<String>,
    pub extensions: Vec<UpstreamExtension>,
}

pub fn decompress_gzip(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() > COMPRESSED_LIMIT {
        return Err(CatalogueError::Limit);
    }
    let expanded = nova_net::read_capped(MultiGzDecoder::new(bytes), EXPANDED_LIMIT as u64)
        .map_err(|e| match e {
            nova_net::ReadError::TooLarge => CatalogueError::Limit,
            nova_net::ReadError::Io(e) => invalid(e),
        })?;
    Ok(expanded)
}

pub fn decode_keiyoushi_gzip(repository: &str, bytes: &[u8]) -> Result<UpstreamCatalogue> {
    let expanded = decompress_gzip(bytes)?;
    proto::guard(&expanded, true)?;
    let index = proto::Index::decode(expanded.as_slice()).map_err(invalid)?;
    let list = match index.extensions.as_ref() {
        Some(proto::Extensions::Embedded(list)) => list.clone(),
        Some(proto::Extensions::Url(_)) => {
            return Err(invalid("linked extension list requires CatalogueClient"));
        }
        None => return Err(invalid("index has no extension list")),
    };
    convert(repository, index, list)
}

fn convert(
    repository: &str,
    index: proto::Index,
    list: proto::ExtensionList,
) -> Result<UpstreamCatalogue> {
    if list.extensions.len() > MAX_RECORDS {
        return Err(CatalogueError::Limit);
    }
    let mut seen = BTreeMap::new();
    let mut count = 0;
    let mut extensions = Vec::with_capacity(list.extensions.len());
    for extension in list.extensions {
        if extension.package_name.is_empty()
            || seen.insert(extension.package_name.clone(), ()).is_some()
        {
            return Err(invalid("empty or duplicate upstream package"));
        }
        count += extension.sources.len();
        if count > MAX_RECORDS {
            return Err(CatalogueError::Limit);
        }
        let mut source_ids = BTreeMap::new();
        let sources = extension
            .sources
            .into_iter()
            .map(|s| {
                if source_ids.insert(s.id, ()).is_some() {
                    return Err(invalid("duplicate upstream source ID"));
                }
                Ok(UpstreamSource {
                    id: s.id.to_string(),
                    name: s.name,
                    language: s.language,
                    home_url: s.home_url,
                    mirror_urls: s.mirror_urls,
                    message: s.message,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let resources = extension.resources.unwrap_or_default();
        extensions.push(UpstreamExtension {
            name: extension.name,
            package_name: extension.package_name,
            version_code: extension.version_code.to_string(),
            version_name: extension.version_name,
            extension_lib: extension.extension_lib,
            content_warning: extension.content_warning,
            apk_url: resources.apk_url,
            jar_url: resources.jar_url,
            icon_url: resources.icon_url,
            sources,
        });
    }
    extensions.sort_by(|a, b| a.package_name.cmp(&b.package_name));
    let contact = index.contact.unwrap_or_default();
    Ok(UpstreamCatalogue {
        repository: repository.into(),
        name: index.name,
        badge_label: index.badge_label,
        upstream_signing_key: index.signing_key,
        website: contact.website,
        discord: contact.discord,
        extensions,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortMapping {
    pub repository: String,
    pub package_name: String,
    pub source_id: String,
    pub native_addon_id: String,
    pub native_source_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpstreamState {
    NotYetPorted { reason: String },
    PortAvailable { addon_id: String, source_id: String },
    InstalledNativePort { addon_id: String, source_id: String },
}
/// A mapping is actionable only when a verified native catalogue supplies it;
/// merely knowing an upstream ID never implies executable compatibility.
pub fn upstream_state(
    repository: &str,
    package: &str,
    source: &str,
    mappings: &[PortMapping],
    available: &NativeCatalogue,
    installed: &BTreeMap<String, String>,
) -> UpstreamState {
    let mapping = mappings
        .iter()
        .find(|m| m.repository == repository && m.package_name == package && m.source_id == source);
    if let Some(m) = mapping {
        let exists = available.packages.iter().any(|p| {
            p.id == m.native_addon_id && p.sources.iter().any(|s| s.id == m.native_source_id)
        });
        if exists {
            if installed.contains_key(&m.native_addon_id) {
                return UpstreamState::InstalledNativePort {
                    addon_id: m.native_addon_id.clone(),
                    source_id: m.native_source_id.clone(),
                };
            }
            return UpstreamState::PortAvailable {
                addon_id: m.native_addon_id.clone(),
                source_id: m.native_source_id.clone(),
            };
        }
    }
    UpstreamState::NotYetPorted { reason: "This APK/JAR source cannot run in Nova Reader; install an available native JavaScript port.".into() }
}

/// These exact upstream IDs were read from the live index and match the
/// reference addons' explicitly implemented language variants.
pub fn reference_port_mappings() -> Vec<PortMapping> {
    serde_json::from_str(include_str!("../fixtures/port-mappings.json"))
        .expect("the checked-in mapping fixture must be valid")
}

#[cfg(test)]
mod tests;
