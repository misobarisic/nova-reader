//! Host-owned addon compression: absence follows global policy, without addon JS.
use crate::{
    AppWindow, SourceCompressionSettings,
    image_storage::{self, ImageCompression, ImageStoragePreferences},
    services::Services,
};
use serde::{Deserialize, Serialize};
use slint::ComponentHandle;
use std::{
    collections::BTreeMap,
    sync::{LazyLock, Mutex, RwLock},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompressionOverride {
    pub cache: ImageCompression,
    pub downloads: ImageCompression,
}
impl CompressionOverride {
    pub(crate) fn valid(self) -> bool {
        (1..=100).contains(&self.cache.quality) && (1..=100).contains(&self.downloads.quality)
    }
    pub(crate) fn merge_intent(
        before: Option<Self>,
        actual: Option<Self>,
        shared: Option<Self>,
    ) -> Option<Self> {
        if before == actual {
            return shared;
        }
        let (Some(before), Some(actual), Some(mut shared)) = (before, actual, shared) else {
            return actual;
        };
        fn merge(before: ImageCompression, actual: ImageCompression, next: &mut ImageCompression) {
            macro_rules! field {
                ($field:ident) => {
                    if before.$field != actual.$field {
                        next.$field = actual.$field;
                    }
                };
            }
            field!(enabled);
            field!(format);
            field!(quality);
            field!(jxl_lossless_jpeg);
            field!(lossless_inputs);
        }
        merge(before.cache, actual.cache, &mut shared.cache);
        merge(before.downloads, actual.downloads, &mut shared.downloads);
        Some(shared)
    }
    fn normalized(self) -> Self {
        let preferences = ImageStoragePreferences {
            cache: self.cache,
            downloads: self.downloads,
            ..Default::default()
        }
        .normalized();
        Self {
            cache: preferences.cache,
            downloads: preferences.downloads,
        }
    }
}
fn key(source: &str) -> String {
    format!(
        "reader:source-compression:{}",
        serde_json::to_string(source).expect("string serializes")
    )
}
static MUTATION: Mutex<()> = Mutex::new(());
static CURRENT: LazyLock<RwLock<BTreeMap<String, Option<CompressionOverride>>>> =
    LazyLock::new(|| RwLock::new(BTreeMap::new()));
/// Cold loads happen once per package; worker hits never read storage.
pub fn snapshot(source: &str) -> Option<CompressionOverride> {
    if let Some(value) = CURRENT
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(source)
    {
        return *value;
    }
    let mut values = CURRENT.write().unwrap_or_else(|e| e.into_inner());
    *values.entry(source.to_owned()).or_insert_with(|| {
        nova_storage::try_get_str(&key(source))
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<Option<CompressionOverride>>(&raw).ok())
            .flatten()
            .filter(|value| value.valid())
            .map(CompressionOverride::normalized)
    })
}
/// Also used by addon sync projection; write succeeds before workers see it.
pub fn persist(source: &str, value: Option<CompressionOverride>) -> Result<(), String> {
    let _mutation = MUTATION.lock().unwrap_or_else(|e| e.into_inner());
    if source.is_empty()
        || source.len() > 64
        || !source
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(nova_reader_ui::backend_text::tr("Invalid addon identity"));
    }
    if value.is_some_and(|v| !v.valid()) {
        return Err(nova_reader_ui::backend_text::tr(
            "Invalid addon compression quality",
        ));
    }
    let value = value.map(CompressionOverride::normalized);
    nova_storage::try_set_str(
        &key(source),
        &serde_json::to_string(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    CURRENT
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(source.to_owned(), value);
    Ok(())
}
pub fn select(ui: &AppWindow, source: &str) {
    let settings = ui.global::<SourceCompressionSettings>();
    settings.set_source_id(source.into());
    settings.set_use_global(snapshot(source).is_none());
    let preferences = image_storage::effective_for_source(source);
    settings.set_cache_enabled(preferences.cache.enabled);
    settings.set_cache_format(format_index(preferences.cache));
    settings.set_cache_quality(f32::from(preferences.cache.quality));
    settings.set_cache_lossless_inputs(preferences.cache.lossless_inputs);
    settings.set_cache_jxl_lossless_jpeg(preferences.cache.jxl_lossless_jpeg);
    settings.set_downloads_enabled(preferences.downloads.enabled);
    settings.set_downloads_format(format_index(preferences.downloads));
    settings.set_downloads_quality(f32::from(preferences.downloads.quality));
    settings.set_downloads_lossless_inputs(preferences.downloads.lossless_inputs);
    settings.set_downloads_jxl_lossless_jpeg(preferences.downloads.jxl_lossless_jpeg);
}
fn format_index(compression: ImageCompression) -> i32 {
    match compression.format {
        image_storage::ImageStorageFormat::JxlLossless
        | image_storage::ImageStorageFormat::JxlLossy => 3,
        format => format.into(),
    }
}
pub fn init(ui: &AppWindow, services: Services) {
    let weak = ui.as_weak();
    ui.global::<SourceCompressionSettings>()
        .on_settings_changed(move |key, value| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let source = ui
                .global::<SourceCompressionSettings>()
                .get_source_id()
                .to_string();
            let result = crate::sync_addons::local_change(&services, || {
                // Only an installed package can be edited from this retained page.
                if !services
                    .sources
                    .list()
                    .map_err(|e| e.to_string())?
                    .iter()
                    .any(|s| s.id == source)
                {
                    return Err(nova_reader_ui::backend_text::tr("Source unavailable"));
                }
                let next = if key == "use_global" {
                    match value {
                        1 => None,
                        0 => {
                            let p = image_storage::effective_for_source(&source);
                            Some(CompressionOverride {
                                cache: p.cache,
                                downloads: p.downloads,
                            })
                        }
                        _ => return Ok(()),
                    }
                } else {
                    let Some(previous) = snapshot(&source) else {
                        return Ok(());
                    };
                    if !key.starts_with("cache_") && !key.starts_with("downloads_") {
                        return Ok(());
                    }
                    let mut p = ImageStoragePreferences {
                        cache: previous.cache,
                        downloads: previous.downloads,
                        ..Default::default()
                    };
                    if !p.change(&key, value) {
                        return Ok(());
                    }
                    Some(CompressionOverride {
                        cache: p.cache,
                        downloads: p.downloads,
                    })
                };
                persist(&source, next)
            });
            select(&ui, &source);
            match result {
                Ok(()) => ui.set_status("".into()),
                Err(error) => ui.set_status(
                    format!(
                        "{}: {error}",
                        nova_reader_ui::backend_text::tr(
                            "Could not save addon compression settings"
                        )
                    )
                    .into(),
                ),
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn override_validation_and_legacy_normalization_are_shared_with_global_policy() {
        let mut value = CompressionOverride {
            cache: ImageCompression::default(),
            downloads: ImageCompression::default(),
        };
        value.cache.format = image_storage::ImageStorageFormat::JxlLossless;
        let normalized = value.normalized();
        assert_eq!(
            normalized.cache.format,
            image_storage::ImageStorageFormat::JxlLossy
        );
        assert!(normalized.cache.lossless_inputs);
        assert!(!normalized.downloads.lossless_inputs);
        value.downloads.quality = 0;
        assert!(!value.valid());
    }
}
