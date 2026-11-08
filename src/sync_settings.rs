//! Field-level account settings with durable, device-local override baselines.
//! Only the explicit portable schema is published. Remote projections write KV
//! through the sync store transaction and never pass through the local publisher.
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, LazyLock, Mutex},
};

type Fields = BTreeMap<String, Value>;
type Publisher = Arc<dyn Fn() + Send + Sync>;
type Reapply = Arc<dyn Fn(&crate::AppWindow) + Send + Sync>;
static REAPPLY: LazyLock<Mutex<Option<Reapply>>> = LazyLock::new(|| Mutex::new(None));
/// The app owns active-reader geometry, decoded-image invalidation and platform
/// presentation. Reapply those after the preference caches have been refreshed.
pub fn install_reapply(reapply: impl Fn(&crate::AppWindow) + Send + Sync + 'static) {
    *REAPPLY.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(reapply));
}
const BASELINE: &str = "reader:sync-settings-baseline";
const OVERRIDES: &str = "reader:sync-settings-overrides";
static PUBLISHER: LazyLock<Mutex<Option<Publisher>>> = LazyLock::new(|| Mutex::new(None));

pub fn domain() -> String {
    crate::sync::domain("settings")
}
/// Physical KV key for a logical settings document: personal documents live in
/// the active profile's namespace, device-global ones (captures, image storage)
/// are shared by every profile.
fn kv(key: &str) -> String {
    reader_domain::profile::key(key)
}
pub fn install_publisher(publisher: impl Fn() + Send + Sync + 'static) {
    *PUBLISHER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(publisher));
}
/// Merge the editor's changed fields into the latest durable preferences. A
/// worker may already have projected remote values while Slint still displays
/// its previous snapshot, so replacing an editor's whole document loses edits.
pub fn persist_preferences<T>(key: &str, previous: &T, next: &T) -> Result<T, String>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Clone,
{
    let previous = serde_json::to_value(previous).map_err(|e| e.to_string())?;
    let next_value = serde_json::to_value(next).map_err(|e| e.to_string())?;
    if template(key).is_none() {
        let json = serde_json::to_string(next).map_err(|e| e.to_string())?;
        reader_domain::profile::kv::try_set_str(key, &json).map_err(|e| e.to_string())?;
        return Ok(next.clone());
    }
    let publisher = PUBLISHER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let effective = if publisher.is_some() {
        let owner = nova_sync::local_store().map_err(|e| e.to_string())?;
        let dev = nova_sync::local_device().map_err(|e| e.to_string())?;
        let mut store = owner.lock().unwrap_or_else(|e| e.into_inner());
        let before = store.clone();
        let result = (|| -> nova_sync::Result<T> {
            // New series have no baseline yet. Their untouched None/Default
            // values are inheritance, not edits with account authority.
            if series_field(key).is_some() {
                let prior = documents(&store)?
                    .remove(key)
                    .unwrap_or_else(|| previous.clone());
                let mut baseline = read_map(&store, BASELINE)?;
                for (field, value) in fields(&BTreeMap::from([(key.into(), prior)])) {
                    baseline.entry(field).or_insert(value);
                }
                store.queue_extra(&kv(BASELINE), Some(serde_json::to_string(&baseline)?));
            }
            let merged = merge_editor(&mut store, key, &previous, &next_value)?;
            let effective = serde_json::from_value(merged)?;
            let mut documents = documents(&store)?;
            documents.insert(key.into(), serde_json::to_value(&effective)?);
            prepare_documents(&mut store, dev, false, false, documents)?;
            store.save()?;
            Ok(effective)
        })();
        match result {
            Ok(effective) => effective,
            Err(error) => {
                *store = before;
                return Err(error.to_string());
            }
        }
    } else {
        // Before sync is installed ordinary preference tests and startup still
        // use fallible KV writes. No account records or identity are created.
        let raw = reader_domain::profile::kv::try_get_str(key).map_err(|e| e.to_string())?;
        let mut current = raw
            .and_then(|raw| {
                let mut document: Value = serde_json::from_str(&raw).ok()?;
                document = unpack_document(key, document);
                let typed: T = serde_json::from_value(document).ok()?;
                serde_json::to_value(typed)
                    .ok()
                    .and_then(|value| normalize_document(key, value).ok())
                    .filter(|value| validate(key, value))
            })
            .unwrap_or_else(|| previous.clone());
        merge_changes("", &previous, &next_value, &mut current);
        if !validate(key, &current) {
            return Err("invalid merged Reader preferences".into());
        }
        let effective = serde_json::from_value(current.clone()).map_err(|e| e.to_string())?;
        let document = stored_document(key, &current);
        reader_domain::profile::kv::try_set_str(key, &document.to_string())
            .map_err(|e| e.to_string())?;
        effective
    };
    if let Some(publish) = publisher {
        publish();
    }
    Ok(effective)
}
fn normalize_document(key: &str, value: Value) -> serde_json::Result<Value> {
    if key == "reader:image-storage" {
        serde_json::to_value(
            serde_json::from_value::<crate::image_storage::ImageStoragePreferences>(value)?
                .normalized(),
        )
    } else {
        Ok(value)
    }
}
fn merge_changes(pointer: &str, previous: &Value, next: &Value, current: &mut Value) {
    if previous == next {
        return;
    }
    if pointer != "/default_mode"
        && let (Some(previous), Some(next), Some(current)) = (
            previous.as_object(),
            next.as_object(),
            current.as_object_mut(),
        )
    {
        for (key, value) in next {
            if let Some(old) = previous.get(key) {
                let slot = current.entry(key.clone()).or_insert_with(|| old.clone());
                merge_changes(&format!("{pointer}/{key}"), old, value, slot);
            } else {
                current.insert(key.clone(), value.clone());
            }
        }
    } else {
        *current = next.clone();
    }
}
fn stored_document(key: &str, value: &Value) -> Value {
    if key.starts_with(MODE_PREFIX) {
        serde_json::json!({"schema_version":1,"choice":value})
    } else if key == "reader:reader-settings" || key.starts_with(SERIES_PREFIX) {
        serde_json::json!({"schema_version":1,"preferences":value})
    } else {
        value.clone()
    }
}
fn merge_editor(
    store: &mut nova_sync::Store,
    key: &str,
    previous: &Value,
    next: &Value,
) -> nova_sync::Result<Value> {
    let mut current = documents(store)?
        .remove(key)
        .or_else(|| template(key))
        .ok_or_else(|| std::io::Error::other("unknown settings document"))?;
    merge_changes("", previous, next, &mut current);
    if !validate(key, &current) {
        return Err(std::io::Error::other("invalid merged Reader preferences").into());
    }
    store.queue_extra(
        &kv(key),
        Some(serde_json::to_string(&stored_document(key, &current))?),
    );
    Ok(current)
}

const SERIES_PREFIX: &str = "reader:reader-settings-series:";
const MODE_PREFIX: &str = "reader:mode-choice:";

/// Parse the serialized SeriesId, not '/' delimiters: source-local IDs can
/// contain slashes, quotes and punctuation. Only a complete ID is a KV key.
fn series_field(key: &str) -> Option<(&str, &str)> {
    let prefix = [SERIES_PREFIX, MODE_PREFIX]
        .into_iter()
        .find(|p| key.starts_with(p))?;
    let suffix = &key[prefix.len()..];
    let mut stream =
        serde_json::Deserializer::from_str(suffix).into_iter::<reader_domain::SeriesId>();
    stream.next()?.ok()?;
    let end = prefix.len() + stream.byte_offset();
    Some((&key[..end], &key[end..]))
}
fn template(key: &str) -> Option<Value> {
    if let Some(value) = templates().remove(key) {
        return Some(value);
    }
    let (storage, pointer) = series_field(key)?;
    if !pointer.is_empty() {
        return None;
    }
    Some(if storage.starts_with(SERIES_PREFIX) {
        serde_json::to_value(crate::reader_settings::SeriesOverrides::default()).unwrap()
    } else {
        serde_json::to_value(crate::reader_modes::ModeChoice::Default).unwrap()
    })
}
fn unpack_document(key: &str, value: Value) -> Value {
    if key.starts_with(MODE_PREFIX) {
        value.get("choice").cloned().unwrap_or(value)
    } else if key == "reader:reader-settings" || key.starts_with(SERIES_PREFIX) {
        value.get("preferences").cloned().unwrap_or(value)
    } else {
        value
    }
}
fn document_pointer<'a>(storage: &str, key: &'a str) -> Option<&'a str> {
    key.strip_prefix(storage)
        .filter(|pointer| pointer.is_empty() || pointer.starts_with('/'))
}
fn templates() -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            "reader:appearance".into(),
            serde_json::to_value(crate::appearance::Preferences::default()).unwrap(),
        ),
        (
            "reader:captures".into(),
            serde_json::to_value(crate::capture_settings::CapturePreferences::default()).unwrap(),
        ),
        (
            "reader:image-storage".into(),
            serde_json::to_value(crate::image_storage::ImageStoragePreferences::default()).unwrap(),
        ),
        (
            "reader:reader-settings".into(),
            serde_json::to_value(crate::reader_settings::ReaderSettings::default()).unwrap(),
        ),
        ("reader:show-adult".into(), Value::Bool(false)),
    ])
}
fn portable(storage: &str, pointer: &str) -> bool {
    match storage {
        "reader:appearance" | "reader:show-adult" => true,
        "reader:captures" => !matches!(
            pointer,
            "/resolution" | "/conflict" | "/save_source" | "/convert_saves"
        ),
        "reader:image-storage" => {
            pointer.starts_with("/cache/") || pointer.starts_with("/downloads/")
        }
        "reader:reader-settings" => !matches!(
            pointer,
            "/rotation"
                | "/fullscreen"
                | "/cutout"
                | "/keep_awake"
                | "/custom_brightness"
                | "/brightness"
                | "/page_flash"
                | "/navigator_modes"
                | "/navigator_left"
                | "/navigator_height"
                | "/desktop_webtoon_width"
        ),
        _ if storage.starts_with(SERIES_PREFIX) => {
            pointer.starts_with("/paged/") || pointer.starts_with("/webtoon/")
        }
        _ if storage.starts_with(MODE_PREFIX) => pointer.is_empty(),
        _ => false,
    }
}
fn flatten(storage: &str, pointer: &str, value: &Value, out: &mut Fields) {
    if pointer != "/default_mode"
        && !storage.starts_with(MODE_PREFIX)
        && let Some(object) = value.as_object()
    {
        for (key, value) in object {
            flatten(storage, &format!("{pointer}/{key}"), value, out);
        }
    } else if portable(storage, pointer) {
        out.insert(format!("{storage}{pointer}"), value.clone());
    }
}
fn validate(storage: &str, value: &Value) -> bool {
    match storage {
        "reader:appearance" => {
            serde_json::from_value::<crate::appearance::Preferences>(value.clone())
                .is_ok_and(|v| v.valid())
        }
        "reader:captures" => {
            serde_json::from_value::<crate::capture_settings::CapturePreferences>(value.clone())
                .is_ok_and(|v| v.valid())
        }
        "reader:image-storage" => {
            serde_json::from_value::<crate::image_storage::ImageStoragePreferences>(value.clone())
                .is_ok_and(|v| v.valid())
        }
        "reader:reader-settings" => {
            serde_json::from_value::<crate::reader_settings::ReaderSettings>(value.clone())
                .is_ok_and(|v| v.validate().is_ok())
        }
        "reader:show-adult" => value.is_boolean(),
        _ if storage.starts_with(SERIES_PREFIX) => {
            serde_json::from_value::<crate::reader_settings::SeriesOverrides>(value.clone())
                .is_ok_and(|v| v.validate().is_ok())
        }
        _ if storage.starts_with(MODE_PREFIX) => {
            serde_json::from_value::<crate::reader_modes::ModeChoice>(value.clone()).is_ok()
        }
        _ => false,
    }
}
/// `value` through its module's typed schema, so fields added since it was
/// saved take their defaults.
fn typed(key: &str, value: Value) -> serde_json::Result<Value> {
    match key {
        "reader:appearance" => serde_json::to_value(serde_json::from_value::<
            crate::appearance::Preferences,
        >(value)?),
        "reader:captures" => serde_json::to_value(serde_json::from_value::<
            crate::capture_settings::CapturePreferences,
        >(value)?),
        "reader:image-storage" => serde_json::to_value(serde_json::from_value::<
            crate::image_storage::ImageStoragePreferences,
        >(value)?),
        "reader:reader-settings" => serde_json::to_value(serde_json::from_value::<
            crate::reader_settings::ReaderSettings,
        >(value)?),
        _ if key.starts_with(SERIES_PREFIX) => serde_json::to_value(serde_json::from_value::<
            crate::reader_settings::SeriesOverrides,
        >(value)?),
        _ => Ok(value),
    }
}
fn documents(store: &nova_sync::Store) -> nova_sync::Result<BTreeMap<String, Value>> {
    let mut documents = templates();
    // Include legacy local decisions and remote identities not opened here yet.
    for (key, _) in
        reader_domain::profile::kv::try_scan_prefixes(&[SERIES_PREFIX.into(), MODE_PREFIX.into()])?
    {
        // A removal queued in this transaction is not a document any more.
        if store.extra_value(&kv(&key))?.is_none() {
            continue;
        }
        if let Some(value) = template(&key) {
            documents.insert(key, value);
        }
    }
    // Documents written in this transaction are not durable yet: a restore
    // queues per-title rows and prepares their records before it saves.
    let strip = format!("profile:{}:", reader_domain::profile::active());
    for prefix in [SERIES_PREFIX, MODE_PREFIX] {
        for physical in store.queued_extra_keys_with_prefix(&kv(prefix)) {
            let logical = physical.strip_prefix(&strip).unwrap_or(&physical);
            if store.extra_value(&physical)?.is_some()
                && let Some(value) = template(logical)
            {
                documents.insert(logical.to_owned(), value);
            }
        }
    }
    for (key, _) in store.records(&domain()) {
        if let Some((storage, _)) = series_field(&key)
            && let Some(value) = template(storage)
        {
            documents.insert(storage.into(), value);
        }
    }
    for (key, template) in &mut documents {
        if let Some(raw) = store.extra_value(&kv(key))? {
            let value = unpack_document(key, serde_json::from_str(&raw)?);
            // Deserialize and reserialize through each module's defaults so
            // preferences introduced after the last local edit participate.
            let normalized = typed(key, value)?;
            let normalized = normalize_document(key, normalized)?;
            if !validate(key, &normalized) {
                return Err(std::io::Error::other("invalid saved Reader preferences").into());
            }
            *template = normalized;
        }
    }
    Ok(documents)
}
fn fields(documents: &BTreeMap<String, Value>) -> Fields {
    let mut result = Fields::new();
    for (key, value) in documents {
        flatten(key, "", value, &mut result);
    }
    result
}
fn read_map(store: &nova_sync::Store, key: &str) -> nova_sync::Result<Fields> {
    store
        .extra_value(&kv(key))?
        .map(|v| serde_json::from_str(&v))
        .transpose()
        .map(|v| v.unwrap_or_default())
        .map_err(Into::into)
}
fn shared(store: &nova_sync::Store) -> Fields {
    store
        .records(&domain())
        .into_iter()
        .filter_map(|(key, raw)| serde_json::from_str(&raw).ok().map(|value| (key, value)))
        .collect()
}
/// Allow only known fields and values which the effective Reader schema accepts.
/// Network policy and projection both use this check; unsupported future fields
/// remain in the account store without replacing local preferences.
pub fn supported(key: &str, value: &Value) -> bool {
    let mut documents = templates();
    if let Some((storage, _)) = series_field(key)
        && let Some(value) = template(storage)
    {
        documents.insert(storage.into(), value);
    }
    for (storage, mut document) in documents {
        let Some(pointer) = document_pointer(&storage, key) else {
            continue;
        };
        let known = fields(&BTreeMap::from([(storage.clone(), document.clone())]));
        if !known.contains_key(key) {
            return false;
        }
        if pointer.is_empty() {
            document = value.clone();
        } else if let Some(slot) = document.pointer_mut(pointer) {
            *slot = value.clone();
        } else {
            return false;
        }
        return validate(&storage, &document);
    }
    false
}
/// Every portable field of the profile's documents, as `store` sees them.
pub(crate) fn snapshot_fields(store: &nova_sync::Store) -> nova_sync::Result<Fields> {
    Ok(fields(&documents(store)?))
}
/// Whether `stored` is exactly what an unedited document holds. A backup treats
/// such a document as absent: an untouched default has no authority over a value
/// that someone chose, and projection writes defaults to a fresh profile.
pub(crate) fn is_default_document(storage: &str, stored: &Value) -> bool {
    let Some(default) = template(storage) else {
        return false;
    };
    typed(storage, unpack_document(storage, stored.clone())).is_ok_and(|value| value == default)
}
/// Whether `stored`, a document exactly as it is saved, is a valid value of the
/// settings document `storage` (wrapper included). Unknown documents are not.
pub(crate) fn valid_stored(storage: &str, stored: &Value) -> bool {
    let value = unpack_document(storage, stored.clone());
    validate(storage, &value) && normalize_document(storage, value).is_ok()
}
/// Publish the fields a restore changed in the documents it wrote (`imported`),
/// as fresh edits, and move the baseline to what is now stored. `before` is
/// [`snapshot_fields`] taken before the documents were queued: a field the
/// restore left as it was (a merge that kept the local value) stays unpublished,
/// so it cannot override a newer account value that has not been projected yet.
/// Fields under a per-device override stay local, as for every other edit.
pub(crate) fn stage_import(
    store: &mut nova_sync::Store,
    dev: u64,
    before: &Fields,
    imported: &std::collections::BTreeSet<String>,
) -> nova_sync::Result<bool> {
    let documents = documents(store)?;
    let current = fields(&documents);
    let overrides = read_map(store, OVERRIDES)?;
    let account = shared(store);
    let mut changed = false;
    for (key, value) in &current {
        let Some(storage) = documents
            .keys()
            .find(|storage| document_pointer(storage, key).is_some())
        else {
            continue;
        };
        if !imported.contains(storage)
            || overrides.contains_key(key)
            || before.get(key) == Some(value)
            || account.get(key) == Some(value)
        {
            continue;
        }
        changed |= store.set(&domain(), key, Some(serde_json::to_string(value)?), 0, dev);
    }
    store.queue_extra(&kv(BASELINE), Some(serde_json::to_string(&current)?));
    Ok(changed)
}
/// Stage local differences, or acknowledge an already-applied projection. The
/// caller saves once alongside Reader's other domains before waking the mesh.
pub fn prepare(
    store: &mut nova_sync::Store,
    dev: u64,
    projection: bool,
    seed: bool,
) -> nova_sync::Result<bool> {
    let documents = documents(store)?;
    prepare_documents(store, dev, projection, seed, documents)
}
fn prepare_documents(
    store: &mut nova_sync::Store,
    dev: u64,
    projection: bool,
    seed: bool,
    documents: BTreeMap<String, Value>,
) -> nova_sync::Result<bool> {
    let current = fields(&documents);
    // A document created by projection or serde defaults is not evidence of a
    // user edit. Bootstrap only persisted legacy choices that differ from the
    // canonical defaults; fresh-device defaults must never gain LWW authority.
    let mut legacy = Fields::new();
    if seed && !projection {
        let defaults = fields(
            &documents
                .keys()
                .filter_map(|key| template(key).map(|value| (key.clone(), value)))
                .collect(),
        );
        for (storage, value) in &documents {
            if store.extra_value(&kv(storage))?.is_some() {
                flatten(storage, "", value, &mut legacy);
            }
        }
        legacy.retain(|key, value| defaults.get(key) != Some(value));
    }
    let baseline = read_map(store, BASELINE)?;
    let overrides = read_map(store, OVERRIDES)?;
    let account = shared(store);
    let mut changed = false;
    for (key, value) in &current {
        if overrides.contains_key(key) {
            continue;
        }
        if !projection
            && ((seed && legacy.contains_key(key) && store.record(&domain(), key).is_none())
                || (!seed
                    && baseline.get(key) != Some(value)
                    && (baseline.contains_key(key) || !account.contains_key(key))))
        {
            changed |= store.set(&domain(), key, Some(serde_json::to_string(value)?), 0, dev);
        }
    }
    store.queue_extra(&kv(BASELINE), Some(serde_json::to_string(&current)?));
    Ok(changed)
}
fn project(documents: &mut BTreeMap<String, Value>, account: &Fields, overrides: &mut Fields) {
    let local = fields(documents);
    for (key, value) in account {
        if !supported(key, value) {
            continue;
        }
        if let Some(baseline) = overrides.get_mut(key) {
            *baseline = value.clone();
            continue;
        }
        for (storage, document) in documents.iter_mut() {
            if let Some(pointer) = document_pointer(storage, key) {
                if pointer.is_empty() {
                    *document = value.clone();
                } else if let Some(slot) = document.pointer_mut(pointer) {
                    *slot = value.clone();
                }
                break;
            }
        }
    }
    if let Some(document) = documents.get_mut("reader:image-storage")
        && let Ok(normalized) = normalize_document("reader:image-storage", document.clone())
    {
        *document = normalized;
    }
    // Canonical migrations may touch more than the received field. Restore
    // explicit device overrides after migration so their mask remains exact.
    for key in overrides.keys() {
        if let Some(value) = local.get(key) {
            for (storage, document) in documents.iter_mut() {
                if let Some(pointer) = document_pointer(storage, key) {
                    if pointer.is_empty() {
                        *document = value.clone();
                    } else if let Some(slot) = document.pointer_mut(pointer) {
                        *slot = value.clone();
                    }
                    break;
                }
            }
        }
    }
}
fn queue_documents(
    store: &mut nova_sync::Store,
    documents: &BTreeMap<String, Value>,
) -> nova_sync::Result<()> {
    for (key, value) in documents {
        let value = stored_document(key, value);
        store.queue_extra(&kv(key), Some(serde_json::to_string(&value)?));
    }
    Ok(())
}
/// Stage account projection and the matching baseline in the same transaction.
/// This method does not publish, save, or access Slint from the network thread.
pub fn apply_remote(store: &mut nova_sync::Store) -> nova_sync::Result<()> {
    let mut documents = documents(store)?;
    let mut overrides = read_map(store, OVERRIDES)?;
    project(&mut documents, &shared(store), &mut overrides);
    queue_documents(store, &documents)?;
    store.queue_extra(
        &kv(BASELINE),
        Some(serde_json::to_string(&fields(&documents))?),
    );
    store.queue_extra(&kv(OVERRIDES), Some(serde_json::to_string(&overrides)?));
    Ok(())
}
/// Policy changes never stamp a shared record. Releasing a local override follows
/// the newest received account value immediately, falling back to its baseline.
pub fn set_override(
    store: &mut nova_sync::Store,
    key: &str,
    enabled: bool,
) -> nova_sync::Result<()> {
    let mut documents = documents(store)?;
    // An unedited series still offers the same override control as global
    // defaults. Materialize its inheritance only when policy is deliberately set.
    if let Some((storage, _)) = series_field(key)
        && let Some(value) = template(storage)
    {
        documents.entry(storage.into()).or_insert(value);
    }
    let current = fields(&documents);
    let mut overrides = read_map(store, OVERRIDES)?;
    let account = shared(store);
    let Some(value) = current.get(key) else {
        return Err(std::io::Error::other("setting cannot be overridden").into());
    };
    if enabled {
        overrides
            .entry(key.into())
            .or_insert_with(|| account.get(key).unwrap_or(value).clone());
    } else if let Some(fallback) = overrides.remove(key) {
        let restore = account.get(key).unwrap_or(&fallback);
        if !supported(key, restore) {
            return Err(
                std::io::Error::other("shared value requires a newer Reader version").into(),
            );
        }
        project(
            &mut documents,
            &Fields::from([(key.into(), restore.clone())]),
            &mut Fields::new(),
        );
    }
    queue_documents(store, &documents)?;
    store.queue_extra(
        &kv(BASELINE),
        Some(serde_json::to_string(&fields(&documents))?),
    );
    store.queue_extra(&kv(OVERRIDES), Some(serde_json::to_string(&overrides)?));
    store.save()
}
#[derive(Clone, Debug)]
pub struct SettingInfo {
    pub key: String,
    pub status: i32,
    pub local_value: String,
    pub shared_value: String,
    pub can_override: bool,
    pub has_shared: bool,
    pub shared_supported: bool,
}
#[cfg(test)]
pub fn rows(store: &nova_sync::Store) -> nova_sync::Result<Vec<SettingInfo>> {
    setting_rows(store, documents(store)?)
}
fn setting_rows(
    store: &nova_sync::Store,
    documents: BTreeMap<String, Value>,
) -> nova_sync::Result<Vec<SettingInfo>> {
    let overrides = read_map(store, OVERRIDES)?;
    let account = shared(store);
    let mut all = Fields::new();
    fn all_fields(prefix: &str, value: &Value, out: &mut Fields) {
        if !prefix.ends_with("/default_mode")
            && !prefix.starts_with(MODE_PREFIX)
            && let Some(object) = value.as_object()
        {
            for (key, value) in object {
                all_fields(&format!("{prefix}/{key}"), value, out);
            }
        } else {
            out.insert(prefix.into(), value.clone());
        }
    }
    for (key, value) in &documents {
        all_fields(key, value, &mut all);
    }
    let portable = fields(&documents);
    Ok(all
        .into_iter()
        .map(|(key, value)| {
            let can_override = portable.contains_key(&key);
            let shared = account.get(&key);
            SettingInfo {
                status: if !can_override {
                    0
                } else if overrides.contains_key(&key) {
                    2
                } else {
                    1
                },
                local_value: setting_value(&key, &value),
                shared_value: shared
                    .map(|v| setting_value(&key, v))
                    .unwrap_or_else(|| nova_reader_ui::backend_text::tr("Not received yet")),
                can_override,
                has_shared: shared.is_some(),
                shared_supported: shared.is_none_or(|v| supported(&key, v)),
                key,
            }
        })
        .collect())
}
fn setting_value(key: &str, value: &Value) -> String {
    if value.is_null() {
        return nova_reader_ui::backend_text::tr("Follow global default");
    }
    if (key == "reader:reader-settings/default_mode" || key.starts_with(MODE_PREFIX))
        && let Ok(mode) = serde_json::from_value::<crate::reader_modes::ModeChoice>(value.clone())
    {
        return nova_reader_ui::backend_text::tr(
            [
                // Matches the reader settings screen's label for mode 0.
                "Webtoon",
                "Left to right",
                "Right to left",
                "Vertical",
                "Automatic",
                "Long strip with gaps",
                "Default",
            ][mode.selection() as usize],
        );
    }
    // Pixel-valued presets are shown by the names the settings screens use.
    let presets: &[(u64, &str)] = match key {
        "reader:appearance/card_radius" => &[
            (0, "Square"),
            (4, "Subtle"),
            (10, "Soft"),
            (16, "Rounded"),
            (24, "Extra rounded"),
        ],
        "reader:appearance/button_radius" => &[
            (0, "Square"),
            (4, "Subtle"),
            (8, "Soft"),
            (12, "Rounded"),
            (24, "Extra rounded"),
        ],
        "reader:appearance/card_spacing" => &[
            (0, "None"),
            (4, "Compact"),
            (8, "Default"),
            (12, "Roomy"),
            (16, "Wide"),
        ],
        _ => &[],
    };
    if let Some((_, label)) = value
        .as_u64()
        .and_then(|v| presets.iter().find(|(pixels, _)| *pixels == v))
    {
        return nova_reader_ui::backend_text::tr(label);
    }
    if key == "reader:appearance/library_list_view"
        && let Some(list) = value.as_bool()
    {
        return nova_reader_ui::backend_text::tr(if list { "List" } else { "Grid" });
    }
    let choices: &[&str] = match key {
        "reader:appearance/library_sort" => &[
            "Recently added",
            "Title",
            "Unread chapters",
            "Last read",
            "Latest chapter",
        ],
        "reader:appearance/accent_color" => &[
            "Purple", "Orange", "Blue", "Teal", "Rose", "Cyan", "Amber", "Lime",
        ],
        "reader:appearance/background_style" => &["Default", "Accent-tinted", "True black"],
        "reader:captures/format" => &[
            "PNG",
            "JPEG",
            "WebP lossless",
            "WebP lossy",
            "AVIF",
            "JPEG XL lossless",
            "JPEG XL lossy",
        ],
        "reader:captures/resolution" => &["Device", "Chapter"],
        "reader:captures/conflict" => &["Ask", "Overwrite", "Do nothing"],
        "reader:image-storage/cache/format" | "reader:image-storage/downloads/format" => {
            &["JPEG", "WebP", "AVIF", "JPEG XL lossless", "JPEG XL lossy"]
        }
        "reader:reader-settings/background" => &["Black", "Grey", "White", "Automatic"],
        "reader:reader-settings/rotation" => &[
            "System",
            "Free",
            "Portrait",
            "Landscape",
            "Locked portrait",
            "Locked landscape",
            "Reverse portrait",
        ],
        _ => &[],
    };
    if let Some(label) = value.as_u64().and_then(|v| choices.get(v as usize)) {
        return nova_reader_ui::backend_text::tr(label);
    }
    display(value)
}
fn display(value: &Value) -> String {
    match value {
        Value::Bool(true) => nova_reader_ui::backend_text::tr("On"),
        Value::Bool(false) => nova_reader_ui::backend_text::tr("Off"),
        Value::String(v) => v.clone(),
        _ => value.to_string(),
    }
}
/// Rebind cached effective snapshots on Slint's event loop after a saved remote
/// transaction. Initializers only read storage and never publish local edits.
pub fn refresh_ui(ui: &crate::AppWindow) -> Result<(), String> {
    crate::appearance::init(ui)?;
    crate::capture_settings::init(ui)?;
    crate::image_storage::init(ui)?;
    crate::reader_settings::reload_global()?;
    crate::reader_settings_ui::apply(ui, &crate::reader_settings::snapshot());
    crate::reading_preferences::init(ui)?;
    let reapply = REAPPLY.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(reapply) = reapply {
        reapply(ui);
    }
    Ok(())
}

/// Scope status describes account policy; disabling sync hides every badge.
pub fn refresh_metadata(ui: &crate::AppWindow) -> Result<(), String> {
    use slint::ComponentHandle;
    let owner = nova_sync::local_store().map_err(|e| e.to_string())?;
    let store = owner.lock().unwrap_or_else(|e| e.into_inner());
    let mut documents = documents(&store).map_err(|e| e.to_string())?;
    // Inspector-only device policy: never add these to portable templates or
    // projection, even though their rows use the same scope badge/modal.
    documents.insert(
        "reader:image-limits".into(),
        serde_json::to_value(crate::image_limits::Settings::load()).map_err(|e| e.to_string())?,
    );
    documents.insert(
        "reader:download-cleanup".into(),
        serde_json::to_value(crate::download_cleanup::Settings::load().unwrap_or_default())
            .map_err(|e| e.to_string())?,
    );
    for key in [
        ui.global::<crate::ReaderSettings>()
            .get_series_settings_key(),
        ui.global::<crate::ReaderSettings>().get_series_mode_key(),
    ] {
        if let Some(value) = template(&key) {
            documents.entry(key.to_string()).or_insert(value);
        }
    }
    let mut rows = setting_rows(&store, documents)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|row| crate::SettingSyncInfo {
            key: row.key.into(),
            status: row.status,
            local_value: row.local_value.into(),
            shared_value: row.shared_value.into(),
            can_override: row.can_override,
            has_shared: row.has_shared,
            shared_supported: row.shared_supported,
        })
        .collect::<Vec<_>>();
    let sync = nova_sync::read_settings();
    for (key, value) in [
        ("sync_enabled", Value::Bool(sync.enabled)),
        ("sync_device_name", Value::String(sync.device_name)),
        ("sync_interval", Value::from(sync.interval_secs)),
        ("sync_background", Value::Bool(sync.background_enabled)),
        (
            "sync_local_discovery",
            Value::Bool(sync.enable_local_discovery),
        ),
        ("sync_pair_confirm", Value::Bool(sync.require_confirmation)),
    ] {
        rows.push(crate::SettingSyncInfo {
            key: key.into(),
            status: 0,
            local_value: display(&value).into(),
            shared_value: "".into(),
            can_override: false,
            has_shared: false,
            shared_supported: true,
        });
    }
    let settings = ui.global::<crate::ReaderSettingsSync>();
    settings.set_enabled(sync.enabled);
    settings.set_rows(crate::sync_ui::reuse_model(settings.get_rows(), rows));
    Ok(())
}
pub fn init(ui: &crate::AppWindow) -> Result<(), String> {
    use slint::{ComponentHandle, Model};
    let weak = ui.as_weak();
    ui.global::<crate::ReaderSettingsSync>()
        .on_lookup(move |key| {
            weak.upgrade()
                .and_then(|ui| {
                    ui.global::<crate::ReaderSettingsSync>()
                        .get_rows()
                        .iter()
                        .position(|row| row.key == key)
                })
                .map_or(-1, |i| i as i32)
        });
    let weak = ui.as_weak();
    ui.global::<crate::ReaderSettingsSync>()
        .on_requested(move || {
            if let Some(ui) = weak.upgrade()
                && let Err(error) = refresh_metadata(&ui)
            {
                ui.set_status(nova_reader_ui::backend_text::error(&error).into());
            }
        });
    let weak = ui.as_weak();
    ui.global::<crate::ReaderSettingsSync>()
        .on_override_changed(move |key, enabled| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let result = (|| {
                let owner = nova_sync::local_store().map_err(|e| e.to_string())?;
                let mut store = owner.lock().unwrap_or_else(|e| e.into_inner());
                let before = store.clone();
                if let Err(error) = set_override(&mut store, &key, enabled) {
                    *store = before;
                    return Err(error.to_string());
                }
                drop(store);
                refresh_ui(&ui)?;
                refresh_metadata(&ui)
            })();
            if let Err(error) = result {
                ui.set_status(nova_reader_ui::backend_text::error(&error).into());
            }
        });
    refresh_metadata(ui)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scope_modal_labels_match_settings_screens() {
        let label =
            |key: &str, value: Value| setting_value(&format!("reader:appearance/{key}"), &value);
        assert_eq!(label("card_radius", 10.into()), "Soft");
        assert_eq!(label("card_radius", 24.into()), "Extra rounded");
        assert_eq!(label("button_radius", 8.into()), "Soft");
        assert_eq!(label("card_spacing", 8.into()), "Default");
        assert_eq!(label("card_spacing", 0.into()), "None");
        assert_eq!(label("library_list_view", true.into()), "List");
        assert_eq!(label("library_list_view", false.into()), "Grid");
        assert_eq!(label("library_sort", 1.into()), "Title");
        assert_eq!(
            setting_value(
                "reader:reader-settings/default_mode",
                &serde_json::to_value(crate::reader_modes::ModeChoice::Manual(
                    reader_domain::ReadingMode::Webtoon
                ))
                .unwrap()
            ),
            "Webtoon"
        );
    }
    #[test]
    fn portable_schema_rejects_paths_quotas_device_policy_and_invalid_values() {
        for key in [
            "reader:captures/conflict",
            "reader:captures/resolution",
            "reader:captures/save_source",
            "reader:captures/convert_saves",
            "reader:image-storage/policy/chapter_disk_limit_mb",
            "reader:reader-settings/brightness",
            "reader:reader-settings/keep_awake",
            "source:token",
            "reader:appearance/path",
        ] {
            assert!(!supported(key, &Value::from(1)), "{key}");
        }
        assert!(supported("reader:appearance/accent_color", &Value::from(3)));
        assert!(!supported(
            "reader:appearance/accent_color",
            &Value::from(99)
        ));
        assert!(!supported(
            "reader:reader-settings/paged/side_padding",
            &Value::from(26)
        ));
        assert!(supported(
            "reader:reader-settings/default_mode",
            &serde_json::json!({"Manual":"RightToLeft"})
        ));
        assert!(supported(
            "reader:image-storage/downloads/lossless_inputs",
            &Value::Bool(true)
        ));
    }
    #[test]
    fn durable_overrides_follow_latest_shared_without_publishing_or_echoing() {
        const CHILD: &str = "READER_SETTINGS_SYNC_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let directory = tempfile::tempdir().unwrap();
            for phase in ["write", "restart"] {
                let result = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "sync_settings::tests::durable_overrides_follow_latest_shared_without_publishing_or_echoing", "--nocapture"])
                    .env(CHILD, phase).env("READER_SETTINGS_SYNC_TEST_DIR",directory.path()).status().unwrap();
                assert!(result.success());
            }
            return;
        }
        nova_storage::init_at(std::path::Path::new(
            &std::env::var("READER_SETTINGS_SYNC_TEST_DIR").unwrap(),
        ));
        nova_sync::configure_protocol(nova_sync::ProtocolConfig::reader()).unwrap();
        let mut store = nova_sync::Store::try_load().unwrap();
        let key = "reader:appearance/accent_color";
        if std::env::var(CHILD).unwrap() == "restart" {
            assert!(read_map(&store, OVERRIDES).unwrap().is_empty());
            assert_eq!(fields(&documents(&store).unwrap())[key], Value::from(5));
            assert!(!prepare(&mut store, 1, false, false).unwrap());
            return;
        }
        let mut legacy_appearance =
            serde_json::to_value(crate::appearance::Preferences::default()).unwrap();
        legacy_appearance["accent_color"] = Value::from(1);
        store.queue_extra("reader:appearance", Some(legacy_appearance.to_string()));
        assert!(prepare(&mut store, 1, false, true).unwrap());
        store.save().unwrap();
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let ui = crate::AppWindow::new().unwrap();
        crate::reader_settings::init().unwrap();
        let local_series = reader_domain::SeriesId::new("source", "local-overrides");
        crate::reader_settings::update_series(&local_series, "rotation", 3).unwrap();
        refresh_metadata(&ui).unwrap();
        use slint::ComponentHandle;
        assert!(!ui.global::<crate::ReaderSettingsSync>().get_enabled());
        // Exercise the actual shared scope badges and modal through Reader's
        // retained Theme page, including disabled visibility and policy edits.
        init(&ui).unwrap();
        let reapplied = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = reapplied.clone();
        install_reapply(move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        ui.global::<crate::Anim>().set_enabled(false);
        ui.window().set_size(slint::PhysicalSize::new(1280, 1400));
        ui.set_section(14);
        ui.show().unwrap();
        for _ in 0..40 {
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
        }
        assert_eq!(
            i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "Synced").count(),
            0
        );
        nova_sync::write_settings(&nova_sync::SyncSettings {
            enabled: true,
            ..Default::default()
        });
        refresh_metadata(&ui).unwrap();
        let badge = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "Synced")
            .next()
            .expect("theme scope badge");
        badge.mock_single_click(slint::platform::PointerEventButton::Left);
        for _ in 0..16 {
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(20));
        }
        assert!(ui.global::<crate::ReaderSettingsSync>().get_open());
        let selected = ui.global::<crate::ReaderSettingsSync>().get_selected_key();
        assert!(selected.starts_with("reader:appearance/"));
        i_slint_backend_testing::ElementHandle::find_by_accessible_label(
            &ui,
            "Use device override",
        )
        .next()
        .expect("override action")
        .mock_single_click(slint::platform::PointerEventButton::Left);
        for _ in 0..16 {
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(20));
        }
        assert!(!ui.global::<crate::ReaderSettingsSync>().get_open());
        // Use the public callback for the return path; its persisted behavior
        // is tested below again against later account changes.
        ui.global::<crate::ReaderSettingsSync>()
            .invoke_override_changed(selected, false);
        ui.hide().unwrap();
        assert_eq!(reapplied.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            crate::reader_settings::series_snapshot(&local_series).rotation_selection(),
            3
        );
        // UI policy writes used the process-wide owner. Reload its commit before
        // continuing this independent record fixture.
        store = nova_sync::Store::try_load().unwrap();
        let saved_rows = rows(&store).unwrap();
        assert!(
            saved_rows
                .iter()
                .any(|row| row.key == key && row.status == 1)
        );
        assert!(
            saved_rows
                .iter()
                .any(|row| row.key == "reader:reader-settings/brightness" && row.status == 0)
        );
        let before = store.record(&domain(), key).unwrap().clone();
        set_override(&mut store, key, true).unwrap();
        assert_eq!(store.record(&domain(), key), Some(&before));
        let mut local = documents(&store).unwrap();
        local.get_mut("reader:appearance").unwrap()["accent_color"] = Value::from(2);
        queue_documents(&mut store, &local).unwrap();
        assert!(!prepare(&mut store, 1, false, false).unwrap());
        store.save().unwrap();
        assert_eq!(store.record(&domain(), key), Some(&before));
        // A remote edit arriving after the override refreshes the fallback while
        // preserving the effective local value and the incoming record clock.
        store.set(&domain(), key, Some("5".into()), 0, 2);
        let remote = store.record(&domain(), key).unwrap().clone();
        apply_remote(&mut store).unwrap();
        store.save().unwrap();
        assert_eq!(fields(&documents(&store).unwrap())[key], Value::from(2));
        assert_eq!(read_map(&store, OVERRIDES).unwrap()[key], Value::from(5));
        assert!(!prepare(&mut store, 1, false, false).unwrap());
        assert_eq!(store.record(&domain(), key), Some(&remote));
        set_override(&mut store, key, false).unwrap();
        assert_eq!(fields(&documents(&store).unwrap())[key], Value::from(5));
        assert!(!prepare(&mut store, 1, false, false).unwrap());
        assert_eq!(store.record(&domain(), key), Some(&remote));
        // Independent fields keep their own record clocks when a device edits
        // an unrelated preference after receiving a remote change.
        let background = "reader:appearance/background_style";
        store.set(&domain(), background, Some("2".into()), 0, 2);
        let background_record = store.record(&domain(), background).unwrap().clone();
        apply_remote(&mut store).unwrap();
        store.save().unwrap();
        let mut local = documents(&store).unwrap();
        local.get_mut("reader:appearance").unwrap()["card_radius"] = Value::from(16);
        queue_documents(&mut store, &local).unwrap();
        assert!(prepare(&mut store, 1, false, false).unwrap());
        store.save().unwrap();
        assert_eq!(store.record(&domain(), key), Some(&remote));
        assert_eq!(
            store.record(&domain(), background),
            Some(&background_record)
        );
        assert_eq!(
            store
                .record(&domain(), "reader:appearance/card_radius")
                .unwrap()
                .value
                .as_deref(),
            Some("16")
        );
        assert!(!prepare(&mut store, 1, false, false).unwrap());
        // A new device seeds missing defaults without replacing existing values.
        store.queue_extra(BASELINE, None);
        assert!(!prepare(&mut store, 1, false, true).unwrap());
        store.save().unwrap();
    }
    #[test]
    fn fresh_pairing_defaults_have_no_authority_but_explicit_resets_do() {
        const CHILD: &str = "READER_SETTINGS_BOOTSTRAP_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let directory = tempfile::tempdir().unwrap();
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "sync_settings::tests::fresh_pairing_defaults_have_no_authority_but_explicit_resets_do", "--nocapture"])
                .env(CHILD,"1").env("READER_SETTINGS_BOOTSTRAP_DIR",directory.path()).status().unwrap().success());
            return;
        }
        nova_storage::init_at(std::path::Path::new(
            &std::env::var("READER_SETTINGS_BOOTSTRAP_DIR").unwrap(),
        ));
        nova_sync::configure_protocol(nova_sync::ProtocolConfig::reader()).unwrap();
        fn isolated_owner() -> nova_sync::Store {
            let mut owner = nova_sync::Store::default();
            for key in templates().keys() {
                owner.queue_extra(key, None);
            }
            owner.queue_extra(BASELINE, None);
            owner.queue_extra(OVERRIDES, None);
            owner
        }
        let mut host = isolated_owner();
        let mut choices = templates();
        choices.get_mut("reader:appearance").unwrap()["accent_color"] = Value::from(3);
        choices.get_mut("reader:appearance").unwrap()["background_style"] = Value::from(2);
        choices.insert("reader:show-adult".into(), Value::Bool(true));
        choices.get_mut("reader:reader-settings").unwrap()["paged"]["side_padding"] =
            Value::from(12);
        choices.get_mut("reader:reader-settings").unwrap()["default_mode"] =
            serde_json::json!({"Manual":"RightToLeft"});
        queue_documents(&mut host, &choices).unwrap();
        assert!(prepare(&mut host, 1, false, true).unwrap());
        assert_eq!(
            host.records(&domain()).len(),
            5,
            "only persisted nondefault legacy fields seed records"
        );
        let original = host
            .records(&domain())
            .into_iter()
            .map(|(key, _)| {
                let record = host.record(&domain(), &key).unwrap().clone();
                (key, record)
            })
            .collect::<BTreeMap<_, _>>();
        let mut fresh = isolated_owner();
        // Reproduce a later-starting peer without sleeps: even a clock ahead
        // of the host cannot make virtual defaults into account writes.
        fresh.set("test-clock", "ahead", Some("1".into()), u64::MAX, 2);
        assert!(fresh.clock().newer_than(host.clock()));
        let before_seed = fresh.clock();
        assert!(!prepare(&mut fresh, 2, false, true).unwrap());
        assert_eq!(fresh.clock(), before_seed);
        assert!(fresh.records(&domain()).is_empty());
        assert_eq!(read_map(&fresh, BASELINE).unwrap(), fields(&templates()));
        assert!(
            rows(&fresh)
                .unwrap()
                .into_iter()
                .filter(|row| row.can_override)
                .all(|row| !row.has_shared && row.shared_value == "Not received yet")
        );
        // Persisting an entire default document is equally insufficient proof
        // of user intent; neither defaults nor tombstones get reseeded.
        let mut default_documents = isolated_owner();
        queue_documents(&mut default_documents, &templates()).unwrap();
        assert!(!prepare(&mut default_documents, 3, false, true).unwrap());
        assert!(default_documents.records(&domain()).is_empty());
        let mut retired = isolated_owner();
        queue_documents(&mut retired, &choices).unwrap();
        retired.set(&domain(), "reader:appearance/accent_color", None, 0, 3);
        let tombstone = retired
            .record(&domain(), "reader:appearance/accent_color")
            .unwrap()
            .clone();
        prepare(&mut retired, 3, false, true).unwrap();
        assert_eq!(
            retired.record(&domain(), "reader:appearance/accent_color"),
            Some(&tombstone)
        );
        for out in fresh.outbound(&host.digest()) {
            host.apply(&out.domain, &out.key, out.record);
        }
        for (key, record) in &original {
            assert_eq!(host.record(&domain(), key), Some(record));
        }
        for out in host.outbound(&fresh.digest()) {
            fresh.apply(&out.domain, &out.key, out.record);
        }
        apply_remote(&mut fresh).unwrap();
        assert_eq!(fields(&documents(&fresh).unwrap()), fields(&choices));
        assert!(
            !prepare(&mut fresh, 2, false, false).unwrap(),
            "projection must not echo"
        );
        // Existing account records remain authoritative on every later seed.
        let original_background = fresh
            .record(&domain(), "reader:appearance/background_style")
            .unwrap()
            .clone();
        assert!(!prepare(&mut fresh, 2, false, true).unwrap());
        assert_eq!(
            fresh.record(&domain(), "reader:appearance/background_style"),
            Some(&original_background)
        );
        let previous = documents(&fresh).unwrap()["reader:appearance"].clone();
        let mut reset = previous.clone();
        reset["background_style"] = Value::from(0);
        let merged = merge_editor(&mut fresh, "reader:appearance", &previous, &reset).unwrap();
        assert_eq!(merged["accent_color"], Value::from(3));
        assert!(
            prepare(&mut fresh, 2, false, false).unwrap(),
            "explicit reset to default is a real edit"
        );
        assert_eq!(
            fresh
                .record(&domain(), "reader:appearance/background_style")
                .unwrap()
                .value
                .as_deref(),
            Some("0")
        );
        assert!(
            fresh
                .record(&domain(), "reader:appearance/background_style")
                .unwrap()
                .version
                .newer_than(&original_background.version)
        );
        for out in fresh.outbound(&host.digest()) {
            host.apply(&out.domain, &out.key, out.record);
        }
        apply_remote(&mut host).unwrap();
        let host_values = fields(&documents(&host).unwrap());
        assert_eq!(
            host_values["reader:appearance/background_style"],
            Value::from(0)
        );
        assert_eq!(
            host_values["reader:appearance/accent_color"],
            Value::from(3)
        );
        assert_eq!(host_values["reader:show-adult"], Value::Bool(true));
        assert_eq!(
            host_values["reader:reader-settings/paged/side_padding"],
            Value::from(12)
        );
        assert_eq!(
            host_values["reader:reader-settings/default_mode"],
            serde_json::json!({"Manual":"RightToLeft"})
        );
    }
    #[test]
    fn stale_editors_publish_only_their_changed_fields() {
        const CHILD: &str = "READER_SETTINGS_STALE_EDITOR_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let directory = tempfile::tempdir().unwrap();
            assert!(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "sync_settings::tests::stale_editors_publish_only_their_changed_fields",
                        "--nocapture"
                    ])
                    .env(CHILD, "1")
                    .env("READER_SETTINGS_STALE_EDITOR_DIR", directory.path())
                    .status()
                    .unwrap()
                    .success()
            );
            return;
        }
        nova_storage::init_at(std::path::Path::new(
            &std::env::var("READER_SETTINGS_STALE_EDITOR_DIR").unwrap(),
        ));
        nova_sync::configure_protocol(nova_sync::ProtocolConfig::reader()).unwrap();
        let mut legacy = crate::image_storage::ImageStoragePreferences::default();
        legacy.cache.format = crate::image_storage::ImageStorageFormat::JxlLossless;
        legacy.downloads.format = crate::image_storage::ImageStorageFormat::JxlLossless;
        nova_storage::try_set_str(
            "reader:image-storage",
            &serde_json::to_string(&legacy).unwrap(),
        )
        .unwrap();
        let owner = nova_sync::local_store().unwrap();
        {
            let mut store = owner.lock().unwrap();
            prepare(&mut store, 1, false, true).unwrap();
            store.save().unwrap();
        }
        install_publisher(|| {});
        let codec_key = "reader:image-storage/cache/format";
        let canonical_codec = owner
            .lock()
            .unwrap()
            .record(&domain(), codec_key)
            .unwrap()
            .clone();
        assert_eq!(canonical_codec.value.as_deref(), Some("4"));
        let previous_image = crate::image_storage::load();
        let mut next_image = previous_image;
        next_image.cache.quality = 64;
        let image =
            persist_preferences("reader:image-storage", &previous_image, &next_image).unwrap();
        assert_eq!(
            image.cache.format,
            crate::image_storage::ImageStorageFormat::JxlLossy
        );
        assert_eq!(
            image.downloads.format,
            crate::image_storage::ImageStorageFormat::JxlLossy
        );
        assert!(image.cache.lossless_inputs && image.downloads.lossless_inputs);
        assert_eq!(image.cache.quality, 64);
        assert_eq!(
            owner.lock().unwrap().record(&domain(), codec_key),
            Some(&canonical_codec)
        );

        i_slint_backend_testing::init_integration_test_with_mock_time();
        let ui = crate::AppWindow::new().unwrap();
        crate::appearance::init(&ui).unwrap();
        crate::reader_settings::init().unwrap();
        let previous = crate::appearance::Preferences::default();
        let mut draft = serde_json::to_value(&previous).unwrap();
        draft["accent_color"] = Value::from(3);
        let next: crate::appearance::Preferences = serde_json::from_value(draft).unwrap();
        let background = "reader:appearance/background_style";
        let remote_background = {
            let mut store = owner.lock().unwrap();
            store.set(&domain(), background, Some("2".into()), 0, 2);
            apply_remote(&mut store).unwrap();
            store.save().unwrap();
            store.record(&domain(), background).unwrap().clone()
        };
        // Slint has not consumed the queued projection: its editor still owns
        // the old complete appearance document when the local accent changes.
        use slint::ComponentHandle;
        ui.global::<crate::Appearance>()
            .invoke_setting_changed("accent_color".into(), 3);
        assert_eq!(ui.global::<crate::Appearance>().get_accent_color(), 3);
        assert_eq!(ui.global::<crate::Appearance>().get_background_style(), 2);
        let effective = persist_preferences("reader:appearance", &previous, &next).unwrap();
        let effective = serde_json::to_value(effective).unwrap();
        assert_eq!(effective["accent_color"], Value::from(3));
        assert_eq!(effective["background_style"], Value::from(2));
        assert_eq!(
            owner.lock().unwrap().record(&domain(), background),
            Some(&remote_background)
        );

        let mode = "reader:reader-settings/default_mode";
        let expected =
            crate::reader_modes::ModeChoice::Manual(reader_domain::ReadingMode::RightToLeft);
        let remote_mode = {
            let mut store = owner.lock().unwrap();
            store.set(
                &domain(),
                mode,
                Some(serde_json::to_string(&expected).unwrap()),
                0,
                2,
            );
            apply_remote(&mut store).unwrap();
            store.save().unwrap();
            store.record(&domain(), mode).unwrap().clone()
        };
        let effective = crate::reader_settings::update("paged_side_padding", 12).unwrap();
        assert_eq!(crate::reader_settings::snapshot(), effective);
        assert_eq!(effective.default_mode, expected);
        assert_eq!(effective.paged.side_padding, 12);
        let store = owner.lock().unwrap();
        assert_eq!(store.record(&domain(), mode), Some(&remote_mode));
        let durable = documents(&store).unwrap();
        assert_eq!(
            durable["reader:reader-settings"]["default_mode"],
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(
            durable["reader:appearance"]["background_style"],
            Value::from(2)
        );
    }
    #[test]
    fn series_edits_sync_independently_preserve_local_rotation_and_reset_inheritance() {
        const CHILD: &str = "READER_SERIES_SETTINGS_SYNC_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let directory = tempfile::tempdir().unwrap();
            for phase in ["write", "restart"] {
                assert!(std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "sync_settings::tests::series_edits_sync_independently_preserve_local_rotation_and_reset_inheritance", "--nocapture"])
                    .env(CHILD, phase).env("READER_SERIES_SETTINGS_SYNC_DIR", directory.path())
                    .status().unwrap().success());
            }
            return;
        }
        nova_storage::init_at(std::path::Path::new(
            &std::env::var("READER_SERIES_SETTINGS_SYNC_DIR").unwrap(),
        ));
        nova_sync::configure_protocol(nova_sync::ProtocolConfig::reader()).unwrap();
        let series = reader_domain::SeriesId::new("source/with:punctuation", "series/\"quoted\"");
        let other = reader_domain::SeriesId::new("another-source", &series.local_id);
        let storage = crate::reader_settings::series_sync_key(&series);
        let padding = format!("{storage}/paged/side_padding");
        let crop = format!("{storage}/paged/crop_borders");
        let rotation = format!("{storage}/rotation");
        let mode = crate::reader_modes::sync_key(&series);
        assert!(supported(&padding, &Value::Null));
        assert!(supported(&padding, &Value::from(12)));
        assert!(!supported(&padding, &Value::from(26)));
        assert!(!supported(&rotation, &Value::from(3)));
        assert!(!supported(
            "reader:reader-settings/desktop_webtoon_width",
            &Value::from(720)
        ));
        assert!(supported(
            &mode,
            &serde_json::json!({"Manual":"RightToLeft"})
        ));
        assert!(!supported(&mode, &serde_json::json!({"Manual":"unknown"})));
        assert_ne!(storage, crate::reader_settings::series_sync_key(&other));
        crate::reader_settings::init().unwrap();
        crate::reader_settings::load_series(&series).unwrap();
        let owner = nova_sync::local_store().unwrap();
        if std::env::var(CHILD).unwrap() == "restart" {
            assert_eq!(
                crate::reader_settings::series_snapshot(&series),
                crate::reader_settings::SeriesOverrides::default()
            );
            assert_eq!(
                crate::reader_modes::load(&series, None).unwrap(),
                crate::reader_modes::ModeChoice::Default
            );
            let mut store = owner.lock().unwrap();
            assert_eq!(
                store.record(&domain(), &padding).unwrap().value.as_deref(),
                Some("null")
            );
            assert!(!prepare(&mut store, 1, false, false).unwrap());
            return;
        }
        {
            let mut store = owner.lock().unwrap();
            prepare(&mut store, 1, false, true).unwrap();
            store.save().unwrap();
        }
        install_publisher(|| {});
        crate::reader_settings::update_series(&series, "paged_side_padding", 12).unwrap();
        crate::reader_settings::update_series(&series, "rotation", 3).unwrap();
        {
            let store = owner.lock().unwrap();
            assert_eq!(
                store.records_with_prefix(&domain(), &storage).len(),
                1,
                "untouched inheritance must not be published"
            );
            assert!(store.record(&domain(), &rotation).is_none());
        }
        crate::reader_modes::save(&series, crate::reader_modes::ModeChoice::LongStripWithGaps)
            .unwrap();
        let remote_crop = {
            let mut store = owner.lock().unwrap();
            store.set(&domain(), &crop, Some("true".into()), 0, 2);
            store.set(
                &domain(),
                &mode,
                Some(serde_json::json!({"Manual":"RightToLeft"}).to_string()),
                0,
                2,
            );
            apply_remote(&mut store).unwrap();
            store.save().unwrap();
            store.record(&domain(), &crop).unwrap().clone()
        };
        // The mounted series cache is stale. A local padding edit must preserve
        // the independently received crop value, not republish its stale None.
        let effective =
            crate::reader_settings::update_series(&series, "paged_side_padding", 15).unwrap();
        assert_eq!(effective.paged.crop_borders, Some(true));
        assert_eq!(effective.rotation_selection(), 3);
        assert_eq!(
            owner.lock().unwrap().record(&domain(), &crop),
            Some(&remote_crop)
        );
        assert_eq!(
            crate::reader_modes::load(&series, None)
                .unwrap()
                .selection(),
            2
        );
        {
            let mut store = owner.lock().unwrap();
            set_override(&mut store, &padding, true).unwrap();
        }
        crate::reader_settings::update_series(&series, "paged_side_padding", 20).unwrap();
        {
            let mut store = owner.lock().unwrap();
            assert_eq!(
                store.record(&domain(), &padding).unwrap().value.as_deref(),
                Some("15")
            );
            store.set(&domain(), &padding, Some("8".into()), 0, 2);
            apply_remote(&mut store).unwrap();
            store.save().unwrap();
        }
        crate::reader_settings::reload_global().unwrap();
        assert_eq!(
            crate::reader_settings::series_snapshot(&series)
                .paged
                .side_padding,
            Some(20)
        );
        {
            let mut store = owner.lock().unwrap();
            set_override(&mut store, &padding, false).unwrap();
        }
        crate::reader_settings::reload_global().unwrap();
        assert_eq!(
            crate::reader_settings::series_snapshot(&series)
                .paged
                .side_padding,
            Some(8)
        );
        assert_eq!(
            crate::reader_settings::series_snapshot(&series).rotation_selection(),
            3
        );
        // A peer which has never opened the series receives source-scoped fields
        // without copying orientation, and projection never advances their clocks.
        {
            let store = owner.lock().unwrap();
            let mut peer = nova_sync::Store::default();
            peer.queue_extra(&storage, None);
            peer.queue_extra(&mode, None);
            peer.queue_extra(BASELINE, None);
            peer.queue_extra(OVERRIDES, None);
            for out in store.outbound(&peer.digest()) {
                peer.apply(&out.domain, &out.key, out.record);
            }
            apply_remote(&mut peer).unwrap();
            let projected = documents(&peer).unwrap();
            assert_eq!(projected[&storage]["paged"]["side_padding"], Value::from(8));
            assert_eq!(projected[&storage]["rotation"], Value::Null);
            assert!(!prepare(&mut peer, 3, false, false).unwrap());
        }
        i_slint_backend_testing::init_no_event_loop();
        let ui = crate::AppWindow::new().unwrap();
        use slint::{ComponentHandle, Model};
        ui.global::<crate::ReaderSettings>()
            .set_series_settings_key(storage.clone().into());
        ui.global::<crate::ReaderSettings>()
            .set_series_mode_key(mode.clone().into());
        refresh_metadata(&ui).unwrap();
        let rows = ui.global::<crate::ReaderSettingsSync>().get_rows();
        assert!(
            rows.iter()
                .any(|row| row.key == padding && row.status == 1 && row.can_override)
        );
        assert!(
            rows.iter()
                .any(|row| row.key == rotation && row.status == 0 && !row.can_override)
        );
        assert!(rows.iter().any(|row| row.key == mode && row.status == 1));
        crate::reader_settings::reset_series(&series).unwrap();
        crate::reader_modes::save(&series, crate::reader_modes::ModeChoice::Default).unwrap();
        let mut store = owner.lock().unwrap();
        assert_eq!(
            store.record(&domain(), &padding).unwrap().value.as_deref(),
            Some("null")
        );
        assert_eq!(
            store.record(&domain(), &crop).unwrap().value.as_deref(),
            Some("null")
        );
        assert!(!prepare(&mut store, 1, false, false).unwrap());
    }
    #[test]
    fn failed_persistence_does_not_publish() {
        const CHILD: &str = "READER_SETTINGS_SYNC_FAILURE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            assert!(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "sync_settings::tests::failed_persistence_does_not_publish",
                        "--nocapture"
                    ])
                    .env(CHILD, "1")
                    .status()
                    .unwrap()
                    .success()
            );
            return;
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let published = calls.clone();
        install_publisher(move || {
            published.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert!(persist_preferences("reader:show-adult", &false, &true).is_err());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn split_settings_sync_field_by_field_and_device_overrides_stay_local() {
        const CHILD: &str = "READER_SPLIT_SETTINGS_SYNC_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let directory = tempfile::tempdir().unwrap();
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "sync_settings::tests::split_settings_sync_field_by_field_and_device_overrides_stay_local", "--nocapture"])
                .env(CHILD, "1").env("READER_SPLIT_SETTINGS_SYNC_DIR", directory.path()).status().unwrap().success());
            return;
        }
        nova_storage::init_at(std::path::Path::new(
            &std::env::var("READER_SPLIT_SETTINGS_SYNC_DIR").unwrap(),
        ));
        nova_sync::configure_protocol(nova_sync::ProtocolConfig::reader()).unwrap();
        let toggle = "reader:image-storage/cache/split_tall_pages";
        let threshold = "reader:image-storage/cache/split_threshold_rows";
        let strip = "reader:image-storage/downloads/strip_rows";
        let downloads_toggle = "reader:image-storage/downloads/split_tall_pages";
        // Portable schema: both stores' split fields, validated like any value.
        for key in [toggle, downloads_toggle] {
            assert!(supported(key, &Value::Bool(false)), "{key}");
            assert!(!supported(key, &Value::from(7)), "{key}");
        }
        assert!(supported(threshold, &Value::from(8192)));
        assert!(!supported(threshold, &Value::from(100)));
        assert!(supported(strip, &Value::from(1024)));
        assert!(!supported(strip, &Value::from(1000)), "multiple of 16 rows");
        assert!(!supported(
            "reader:image-storage/cache/surprise",
            &Value::from(1)
        ));
        // Device policy stays out of the account.
        assert!(!supported(
            "reader:image-storage/policy/disk_enabled",
            &Value::Bool(true)
        ));

        fn isolated_owner() -> nova_sync::Store {
            let mut owner = nova_sync::Store::default();
            for key in templates().keys() {
                owner.queue_extra(key, None);
            }
            owner.queue_extra(BASELINE, None);
            owner.queue_extra(OVERRIDES, None);
            owner
        }
        let mut host = isolated_owner();
        let mut choices = templates();
        {
            let storage = &mut choices.get_mut("reader:image-storage").unwrap();
            storage["cache"]["split_tall_pages"] = Value::Bool(false);
            storage["cache"]["split_threshold_rows"] = Value::from(8192);
            storage["downloads"]["split_tall_pages"] = Value::Bool(true);
            storage["downloads"]["strip_rows"] = Value::from(1024);
        }
        queue_documents(&mut host, &choices).unwrap();
        assert!(prepare(&mut host, 1, false, true).unwrap());
        // Only the four fields that differ from the defaults are published.
        let published = host
            .records(&domain())
            .into_iter()
            .map(|(key, _)| key)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            published,
            [toggle, threshold, strip, downloads_toggle]
                .into_iter()
                .map(String::from)
                .collect()
        );
        // A second device receives them and its typed preferences follow.
        let mut fresh = isolated_owner();
        prepare(&mut fresh, 2, false, true).unwrap();
        for out in host.outbound(&fresh.digest()) {
            fresh.apply(&out.domain, &out.key, out.record);
        }
        apply_remote(&mut fresh).unwrap();
        let received: crate::image_storage::ImageStoragePreferences =
            serde_json::from_value(documents(&fresh).unwrap()["reader:image-storage"].clone())
                .unwrap();
        assert!(!received.cache.split_tall_pages);
        assert_eq!(received.cache.split_threshold_rows, 8192);
        assert_eq!(
            received.cache.strip_rows, 2048,
            "untouched fields keep the default"
        );
        assert!(received.downloads.split_tall_pages);
        assert_eq!(received.downloads.strip_rows, 1024);
        assert!(!prepare(&mut fresh, 2, false, false).unwrap(), "no echo");

        // The second device overrides one field. It wins locally and the
        // account keeps the shared value; nothing is published.
        let shared = fresh.record(&domain(), toggle).unwrap().clone();
        set_override(&mut fresh, toggle, true).unwrap();
        let mut local = documents(&fresh).unwrap();
        local.get_mut("reader:image-storage").unwrap()["cache"]["split_tall_pages"] =
            Value::Bool(true);
        queue_documents(&mut fresh, &local).unwrap();
        assert!(!prepare(&mut fresh, 2, false, false).unwrap());
        assert_eq!(fresh.record(&domain(), toggle), Some(&shared));
        assert_eq!(
            fields(&documents(&fresh).unwrap())[toggle],
            Value::Bool(true)
        );
        assert_eq!(
            read_map(&fresh, OVERRIDES).unwrap()[toggle],
            Value::Bool(false)
        );
        // Another device's later change refreshes the fallback, not the choice.
        host.set(&domain(), toggle, Some("true".into()), 0, 1);
        let later = host.record(&domain(), toggle).unwrap().clone();
        for out in host.outbound(&fresh.digest()) {
            fresh.apply(&out.domain, &out.key, out.record);
        }
        apply_remote(&mut fresh).unwrap();
        assert_eq!(fresh.record(&domain(), toggle), Some(&later));
        assert_eq!(
            fields(&documents(&fresh).unwrap())[toggle],
            Value::Bool(true)
        );

        // A peer that predates the fields keeps working: its saved document has
        // no split fields, and each store reads its own default for them.
        let mut legacy = isolated_owner();
        let mut old =
            serde_json::to_value(crate::image_storage::ImageStoragePreferences::default()).unwrap();
        for store in ["cache", "downloads"] {
            let object = old[store].as_object_mut().unwrap();
            for field in ["split_tall_pages", "split_threshold_rows", "strip_rows"] {
                object.remove(field);
            }
        }
        old["cache"]["quality"] = Value::from(70);
        legacy.queue_extra("reader:image-storage", Some(old.to_string()));
        let normalized = documents(&legacy).unwrap();
        let typed: crate::image_storage::ImageStoragePreferences =
            serde_json::from_value(normalized["reader:image-storage"].clone()).unwrap();
        assert!(typed.cache.split_tall_pages && !typed.downloads.split_tall_pages);
        assert_eq!(
            (typed.cache.split_threshold_rows, typed.cache.strip_rows),
            (4096, 2048)
        );
        assert_eq!(typed.cache.quality, 70);
    }
}
