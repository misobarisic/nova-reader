//! Profile-owned source configuration. Secrets never enter public descriptors or
//! sync. A full-device backup carries them in plaintext, on purpose, and says so.
use crate::{Result, SourceError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};
const MAX_SETTINGS: usize = 64 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    Text,
    Bool,
    Select,
    Secret,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingField {
    pub key: String,
    pub name: String,
    pub kind: SettingKind,
    pub default: Value,
    #[serde(default)]
    pub options: Vec<String>,
}
fn invalid() -> SourceError {
    SourceError::Package("invalid source settings schema or values".into())
}
pub(crate) fn valid_value(field: &SettingField, value: &Value) -> bool {
    match field.kind {
        SettingKind::Bool => value.is_boolean(),
        SettingKind::Text | SettingKind::Secret => value
            .as_str()
            .is_some_and(|v| v.len() <= 4096 && !v.contains('\0')),
        SettingKind::Select => value
            .as_str()
            .is_some_and(|v| field.options.iter().any(|o| o == v)),
    }
}
pub(crate) fn validate_schema(fields: &[SettingField]) -> Result<()> {
    if fields.len() > 64 || serde_json::to_vec(fields).map_err(|_| invalid())?.len() > MAX_SETTINGS
    {
        return Err(invalid());
    }
    let mut keys = BTreeSet::new();
    for field in fields {
        if field.key.is_empty()
            || field.key.len() > 64
            || !field
                .key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || !keys.insert(&field.key)
            || field.name.trim().is_empty()
            || field.name.len() > 128
            || field.name.chars().any(char::is_control)
            || field.options.len() > 128
        {
            return Err(invalid());
        }
        let mut options = BTreeSet::new();
        if field
            .options
            .iter()
            .any(|o| o.len() > 256 || o.chars().any(char::is_control) || !options.insert(o))
            || (!matches!(field.kind, SettingKind::Select) && !field.options.is_empty())
            || !valid_value(field, &field.default)
            || (field.kind == SettingKind::Secret && field.default != Value::String(String::new()))
        {
            return Err(invalid());
        }
    }
    Ok(())
}
pub(crate) fn path(root: &Path, profile: &str, id: &str) -> PathBuf {
    root.join("settings")
        .join(format!("{:x}", Sha256::digest(profile.as_bytes())))
        .join(format!("{id}.json"))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSettings {
    version: u32,
    values: Map<String, Value>,
    kinds: std::collections::BTreeMap<String, SettingKind>,
}
pub(crate) fn load(path: &Path, fields: &[SettingField]) -> Result<Value> {
    let persisted = if path.exists() {
        let file = std::fs::File::open(path)?;
        if file.metadata()?.len() > MAX_SETTINGS as u64 {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_SETTINGS {
            return Err(invalid());
        }
        let stored = serde_json::from_slice::<StoredSettings>(&bytes).map_err(|_| invalid())?;
        if stored.version != 1 {
            return Err(invalid());
        }
        Some(stored)
    } else {
        None
    };
    // Preserve compatible values across schema upgrades, including secrecy type.
    // A previous secret must never become public when a field changes to text.
    let values = fields
        .iter()
        .map(|f| {
            let value = persisted
                .as_ref()
                .filter(|p| p.kinds.get(&f.key) == Some(&f.kind))
                .and_then(|p| p.values.get(&f.key))
                .filter(|v| valid_value(f, v))
                .unwrap_or(&f.default);
            (f.key.clone(), value.clone())
        })
        .collect();
    let result = Value::Object(values);
    if serde_json::to_vec(&result).map_err(|_| invalid())?.len() > MAX_SETTINGS {
        return Err(invalid());
    }
    Ok(result)
}
pub(crate) fn merge(current: &Value, patch: &Value, fields: &[SettingField]) -> Result<Value> {
    let patch = patch.as_object().ok_or_else(invalid)?;
    if patch.len() > 64 || serde_json::to_vec(patch).map_err(|_| invalid())?.len() > MAX_SETTINGS {
        return Err(invalid());
    }
    let mut values = current.as_object().cloned().ok_or_else(invalid)?;
    for (key, value) in patch {
        let field = fields.iter().find(|f| &f.key == key).ok_or_else(invalid)?;
        if !valid_value(field, value) {
            return Err(invalid());
        }
        values.insert(key.clone(), value.clone());
    }
    let values = Value::Object(values);
    if serde_json::to_vec(&values).map_err(|_| invalid())?.len() > MAX_SETTINGS {
        return Err(invalid());
    }
    Ok(values)
}
pub(crate) fn persist(path: &Path, value: &Value, fields: &[SettingField]) -> Result<()> {
    let parent = path.parent().ok_or_else(invalid)?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let stored = StoredSettings {
        version: 1,
        values: value.as_object().cloned().ok_or_else(invalid)?,
        kinds: fields
            .iter()
            .map(|f| (f.key.clone(), f.kind.clone()))
            .collect(),
    };
    let bytes = serde_json::to_vec(&stored).map_err(|_| invalid())?;
    if bytes.len() > MAX_SETTINGS {
        return Err(invalid());
    }
    nova_storage::fs::atomic_write(path, &bytes)?;
    Ok(())
}
