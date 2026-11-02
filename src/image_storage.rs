//! Device cache budgets and future-write compression for artwork/chapter pages.
use crate::{AppWindow, ImageStorageSettings};
use nova_cache::config::{CacheSettings, DISK_CACHE_LIMITS, IMAGE_RESOLUTION_PRESETS};
use nova_reader_ui::backend_text as text;
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{LazyLock, RwLock},
};

const KEY: &str = "reader:image-storage";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i32", into = "i32")]
#[repr(i32)]
pub enum ImageStorageFormat {
    Jpeg = 0,
    #[default]
    Webp = 1,
    Avif = 2,
    JxlLossless = 3,
    JxlLossy = 4,
}

impl TryFrom<i32> for ImageStorageFormat {
    type Error = &'static str;
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Jpeg),
            1 => Ok(Self::Webp),
            2 => Ok(Self::Avif),
            3 => Ok(Self::JxlLossless),
            4 => Ok(Self::JxlLossy),
            _ => Err("unsupported image storage format"),
        }
    }
}

impl From<ImageStorageFormat> for i32 {
    fn from(value: ImageStorageFormat) -> Self {
        value as Self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageCompression {
    pub enabled: bool,
    pub format: ImageStorageFormat,
    pub quality: u8,
    #[serde(default)]
    pub jxl_lossless_jpeg: bool,
    /// Preserve known lossless inputs when the selected codec supports it.
    #[serde(default)]
    pub lossless_inputs: bool,
}

impl Default for ImageCompression {
    fn default() -> Self {
        Self {
            enabled: true,
            format: ImageStorageFormat::Webp,
            quality: 85,
            jxl_lossless_jpeg: false,
            lossless_inputs: false,
        }
    }
}

/// Reader uses Nova's disk/memory choices while isolating artwork from chapter
/// eviction. Resolution applies only to future processed posters, never pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CachePolicyPreferences {
    pub disk_enabled: bool,
    pub chapter_disk_limit_mb: Option<u32>,
    pub artwork_disk_limit_mb: Option<u32>,
    pub decoded_memory_mb: u32,
    pub poster_max_side: Option<u32>,
}
impl Default for CachePolicyPreferences {
    fn default() -> Self {
        let nova = CacheSettings::default();
        Self {
            disk_enabled: nova.cache_images,
            chapter_disk_limit_mb: nova.image_disk_cache_limit_mb,
            artwork_disk_limit_mb: nova.image_disk_cache_limit_mb,
            decoded_memory_mb: nova.lru_cache_mb,
            // Reader previously kept full encoded artwork and a separate
            // 460x690 display bound. Preserve that behavior until explicit edits.
            poster_max_side: None,
        }
    }
}
impl CachePolicyPreferences {
    pub(crate) fn valid(&self) -> bool {
        DISK_CACHE_LIMITS.contains(&self.chapter_disk_limit_mb)
            && DISK_CACHE_LIMITS.contains(&self.artwork_disk_limit_mb)
            && (32..=512).contains(&self.decoded_memory_mb)
            && self.decoded_memory_mb.is_multiple_of(16)
            && self
                .poster_max_side
                .is_none_or(|side| IMAGE_RESOLUTION_PRESETS.contains(&side))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheKind {
    Chapters,
    Artwork,
}

// First access is cold initialization only. Init and successful commits replace
// this snapshot before waking workers; a hit never reads Nova KV or SQLite.
static CURRENT: LazyLock<RwLock<ImageStoragePreferences>> = LazyLock::new(|| RwLock::new(load()));

pub fn settings_snapshot() -> ImageStoragePreferences {
    *CURRENT.read().unwrap_or_else(|error| error.into_inner())
}
/// Resolve a stable addon package's future-write compression, retaining the
/// device-wide cache budgets. Existing encoded files remain valid.
pub fn effective_for_source(source: &str) -> ImageStoragePreferences {
    let mut preferences = settings_snapshot();
    if let Some(compression) = crate::source_compression::snapshot(source) {
        preferences.cache = compression.cache;
        preferences.downloads = compression.downloads;
    }
    preferences
}

/// Transfers read the latest durable device preference once per chapter. This
/// also covers headless jobs; hot poster/page-cache reads keep their snapshot.
pub fn download_compression(source: &str) -> ImageCompression {
    crate::source_compression::snapshot(source)
        .map(|overrides| overrides.downloads)
        .unwrap_or_else(|| load().downloads)
}

pub fn policy_snapshot() -> CachePolicyPreferences {
    settings_snapshot().policy
}
pub fn cache_policy(kind: CacheKind) -> nova_cache::gc::DiskPolicy {
    let settings = policy_snapshot();
    let limit = match kind {
        CacheKind::Chapters => settings.chapter_disk_limit_mb,
        CacheKind::Artwork => settings.artwork_disk_limit_mb,
    };
    nova_cache::gc::DiskPolicy {
        enabled: settings.disk_enabled,
        limit_bytes: limit.map(|mb| u64::from(mb) * 1024 * 1024),
    }
}
pub fn memory_limit_bytes() -> usize {
    policy_snapshot().decoded_memory_mb as usize * 1024 * 1024
}
fn publish(preferences: ImageStoragePreferences) {
    *CURRENT.write().unwrap_or_else(|error| error.into_inner()) = preferences;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageStoragePreferences {
    pub cache: ImageCompression,
    pub downloads: ImageCompression,
    pub policy: CachePolicyPreferences,
}

impl Default for ImageStoragePreferences {
    fn default() -> Self {
        Self {
            cache: ImageCompression::default(),
            downloads: ImageCompression {
                enabled: false,
                ..ImageCompression::default()
            },
            policy: CachePolicyPreferences::default(),
        }
    }
}

impl ImageStoragePreferences {
    /// Canonical interpretation of the retained legacy JXL enum value. Keep
    /// this pure so durable field-intent merges use the same model as readers.
    pub(crate) fn normalized(mut self) -> Self {
        for compression in [&mut self.cache, &mut self.downloads] {
            if compression.format == ImageStorageFormat::JxlLossless {
                compression.format = ImageStorageFormat::JxlLossy;
                compression.lossless_inputs = true;
            }
        }
        self
    }
    pub(crate) fn valid(&self) -> bool {
        (1..=100).contains(&self.cache.quality)
            && (1..=100).contains(&self.downloads.quality)
            && self.policy.valid()
    }

    fn apply(&self, ui: &AppWindow) {
        let settings = ui.global::<ImageStorageSettings>();
        settings.set_cache_enabled(self.cache.enabled);
        settings.set_cache_format(match self.cache.format {
            ImageStorageFormat::JxlLossless | ImageStorageFormat::JxlLossy => 3,
            format => format.into(),
        });
        settings.set_cache_lossless_inputs(self.cache.lossless_inputs);
        settings.set_cache_quality(f32::from(self.cache.quality));
        settings.set_cache_jxl_lossless_jpeg(self.cache.jxl_lossless_jpeg);
        settings.set_downloads_enabled(self.downloads.enabled);
        settings.set_downloads_format(match self.downloads.format {
            ImageStorageFormat::JxlLossless | ImageStorageFormat::JxlLossy => 3,
            format => format.into(),
        });
        settings.set_downloads_lossless_inputs(self.downloads.lossless_inputs);
        settings.set_downloads_quality(f32::from(self.downloads.quality));
        settings.set_downloads_jxl_lossless_jpeg(self.downloads.jxl_lossless_jpeg);
        settings.set_disk_enabled(self.policy.disk_enabled);
        settings.set_disk_limit_names(ModelRc::new(VecModel::from(
            DISK_CACHE_LIMITS
                .iter()
                .map(|limit| match limit {
                    None => text::tr("Unlimited").into(),
                    Some(mb) if *mb >= 1024 => format!("{} GiB", mb / 1024).into(),
                    Some(mb) => format!("{mb} MiB").into(),
                })
                .collect::<Vec<slint::SharedString>>(),
        )));
        settings.set_chapter_disk_limit_index(
            DISK_CACHE_LIMITS
                .iter()
                .position(|limit| *limit == self.policy.chapter_disk_limit_mb)
                .unwrap_or(3) as i32,
        );
        settings.set_artwork_disk_limit_index(
            DISK_CACHE_LIMITS
                .iter()
                .position(|limit| *limit == self.policy.artwork_disk_limit_mb)
                .unwrap_or(3) as i32,
        );
        settings.set_memory_budget(f64::from(self.policy.decoded_memory_mb) as f32);
        let mut resolutions = vec![text::tr("Original").into()];
        resolutions.extend(
            IMAGE_RESOLUTION_PRESETS
                .iter()
                .map(|side| format!("{side} px").into()),
        );
        settings.set_poster_resolution_names(ModelRc::new(VecModel::from(resolutions)));
        settings.set_poster_resolution_index(
            self.policy
                .poster_max_side
                .and_then(|side| {
                    IMAGE_RESOLUTION_PRESETS
                        .iter()
                        .position(|preset| *preset == side)
                        .map(|i| i + 1)
                })
                .unwrap_or(0) as i32,
        );
    }

    pub(crate) fn change(&mut self, key: &str, value: i32) -> bool {
        match key {
            "disk_enabled" if value == 0 || value == 1 => {
                self.policy.disk_enabled = value == 1;
                return true;
            }
            "chapter_disk_limit_index" | "artwork_disk_limit_index" => {
                let Some(limit) = usize::try_from(value)
                    .ok()
                    .and_then(|i| DISK_CACHE_LIMITS.get(i))
                    .copied()
                else {
                    return false;
                };
                if key == "chapter_disk_limit_index" {
                    self.policy.chapter_disk_limit_mb = limit;
                } else {
                    self.policy.artwork_disk_limit_mb = limit;
                }
                return true;
            }
            "memory_budget" if (32..=512).contains(&value) && value % 16 == 0 => {
                self.policy.decoded_memory_mb = value as u32;
                return true;
            }
            "poster_resolution_index" => {
                self.policy.poster_max_side = match value {
                    0 => None,
                    value if value > 0 => {
                        match IMAGE_RESOLUTION_PRESETS.get((value - 1) as usize) {
                            Some(side) => Some(*side),
                            None => return false,
                        }
                    }
                    _ => return false,
                };
                return true;
            }
            _ => {}
        }
        let (compression, field) = if let Some(field) = key.strip_prefix("cache_") {
            (&mut self.cache, field)
        } else if let Some(field) = key.strip_prefix("downloads_") {
            (&mut self.downloads, field)
        } else {
            return false;
        };
        match field {
            "enabled" if value == 0 || value == 1 => compression.enabled = value == 1,
            "format" => match ImageStorageFormat::try_from(if value == 3 { 4 } else { value }) {
                Ok(format) if (0..=3).contains(&value) => compression.format = format,
                _ => return false,
            },
            "quality" if (1..=100).contains(&value) => compression.quality = value as u8,
            "lossless_inputs" if value == 0 || value == 1 => {
                compression.lossless_inputs = value == 1
            }
            "jxl_lossless_jpeg" if value == 0 || value == 1 => {
                compression.jxl_lossless_jpeg = value == 1
            }
            _ => return false,
        }
        true
    }
}

fn read() -> Result<ImageStoragePreferences, String> {
    let Some(json) = nova_storage::try_get_str(KEY).map_err(|error| error.to_string())? else {
        return Ok(ImageStoragePreferences::default());
    };
    let preferences: ImageStoragePreferences =
        serde_json::from_str(&json).map_err(|error| error.to_string())?;
    let preferences = preferences.normalized();
    if preferences.valid() {
        Ok(preferences)
    } else {
        Err("invalid image storage quality or cache policy".into())
    }
}

/// Worker-safe snapshot. Invalid or unavailable device settings use safe defaults
/// without replacing the saved value or modifying any existing image files.
pub fn load() -> ImageStoragePreferences {
    read().unwrap_or_default()
}

fn persist(
    previous: &ImageStoragePreferences,
    preferences: &ImageStoragePreferences,
) -> Result<ImageStoragePreferences, String> {
    crate::sync_settings::persist_preferences(KEY, previous, preferences)
}

fn save(
    ui: &AppWindow,
    state: &RefCell<ImageStoragePreferences>,
    next: ImageStoragePreferences,
    write: impl FnOnce(&ImageStoragePreferences) -> Result<ImageStoragePreferences, String>,
) {
    match write(&next) {
        Ok(next) => {
            let changed = next != *state.borrow();
            publish(next);
            next.apply(ui);
            *state.borrow_mut() = next;
            ui.set_status("".into());
            if changed {
                ui.global::<ImageStorageSettings>().invoke_policy_changed();
            }
        }
        Err(error) => {
            state.borrow().apply(ui);
            ui.set_status(
                format!(
                    "{}: {error}",
                    text::tr("Could not save image storage settings")
                )
                .into(),
            );
        }
    }
}

/// Initialize the settings controls after Reader storage opens.
pub fn init(ui: &AppWindow) -> Result<(), String> {
    let preferences = match read() {
        Ok(preferences) => preferences,
        Err(_) => {
            ui.set_status(
                text::tr("Could not read image storage settings; using defaults.").into(),
            );
            ImageStoragePreferences::default()
        }
    };
    publish(preferences);
    preferences.apply(ui);
    let state = Rc::new(RefCell::new(preferences));
    let weak = ui.as_weak();
    ui.global::<ImageStorageSettings>()
        .on_settings_changed(move |key, value| {
            if let Some(ui) = weak.upgrade() {
                let mut next = *state.borrow();
                if next.change(&key, value) {
                    let previous = *state.borrow();
                    save(&ui, &state, next, |next| persist(&previous, next));
                } else {
                    state.borrow().apply(&ui);
                }
            }
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_image_storage_save_restores_both_independent_groups() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let previous = ImageStoragePreferences::default();
        let state = RefCell::new(previous);
        let wakes = Rc::new(std::cell::Cell::new(0));
        ui.global::<ImageStorageSettings>().on_policy_changed({
            let wakes = wakes.clone();
            move || wakes.set(wakes.get() + 1)
        });
        let next = ImageStoragePreferences {
            cache: ImageCompression {
                enabled: false,
                format: ImageStorageFormat::Jpeg,
                quality: 40,
                jxl_lossless_jpeg: false,
                lossless_inputs: false,
            },
            downloads: ImageCompression {
                enabled: true,
                format: ImageStorageFormat::Jpeg,
                quality: 60,
                jxl_lossless_jpeg: false,
                lossless_inputs: false,
            },
            policy: CachePolicyPreferences {
                disk_enabled: false,
                chapter_disk_limit_mb: Some(256),
                ..CachePolicyPreferences::default()
            },
        };
        next.apply(&ui);
        save(&ui, &state, next, |_| Err("test write failure".into()));
        let settings = ui.global::<ImageStorageSettings>();
        assert!(settings.get_cache_enabled());
        assert_eq!(settings.get_cache_format(), 1);
        assert_eq!(settings.get_cache_quality(), 85.);
        assert!(!settings.get_downloads_enabled());
        assert_eq!(settings.get_downloads_format(), 1);
        assert_eq!(settings.get_downloads_quality(), 85.);
        assert_eq!(*state.borrow(), previous);
        assert!(settings.get_disk_enabled());
        assert_eq!(settings.get_chapter_disk_limit_index(), 3);
        assert_eq!(settings.get_memory_budget(), 128.);
        assert_eq!(wakes.get(), 0, "failed persistence must not wake GC");
        assert!(!ui.get_status().is_empty());
    }
}
