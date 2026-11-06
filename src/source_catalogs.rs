//! Host-owned catalogue visibility, separate from addon-defined settings.
use std::collections::BTreeSet;

fn key(package: &str) -> String {
    format!(
        "reader:disabled-catalogs:{}",
        serde_json::to_string(package).expect("string serializes")
    )
}
pub fn disabled(package: &str) -> BTreeSet<String> {
    reader_domain::profile::kv::try_get_str(&key(package))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}
pub fn set_enabled(package: &str, catalogue: &str, enabled: bool) -> Result<(), String> {
    if catalogue != package && !catalogue.starts_with(&format!("{package}.")) {
        return Err("Catalogue belongs to another addon".into());
    }
    let mut disabled = disabled(package);
    if enabled {
        disabled.remove(catalogue);
    } else {
        disabled.insert(catalogue.to_owned());
    }
    let raw = serde_json::to_string(&disabled).map_err(|error| error.to_string())?;
    reader_domain::profile::kv::try_set_str(&key(package), &raw).map_err(|error| error.to_string())
}
