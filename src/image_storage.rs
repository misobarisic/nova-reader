//! Device cache budgets and future-write compression for artwork/chapter pages.
use crate::{AppWindow, ImageStorageSettings, page_store::SplitPlan};
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
    /// Store pages taller than `split_threshold_rows` as independently encoded
    /// strips of `strip_rows` rows, each in this store's format and quality, so
    /// scrolling decodes only the strips it shows. Only meaningful while
    /// `enabled`: a store that keeps source bytes has nothing to encode strips
    /// with. Existing files are never rewritten.
    #[serde(default)]
    pub split_tall_pages: bool,
    #[serde(default = "default_split_threshold_rows")]
    pub split_threshold_rows: u32,
    #[serde(default = "default_strip_rows")]
    pub strip_rows: u32,
}
fn default_split_threshold_rows() -> u32 {
    SplitPlan::DEFAULT_THRESHOLD_ROWS
}
fn default_strip_rows() -> u32 {
    SplitPlan::DEFAULT_STRIP_ROWS
}

impl Default for ImageCompression {
    fn default() -> Self {
        Self {
            enabled: true,
            format: ImageStorageFormat::Webp,
            quality: 85,
            jxl_lossless_jpeg: false,
            lossless_inputs: false,
            split_tall_pages: false,
            split_threshold_rows: default_split_threshold_rows(),
            strip_rows: default_strip_rows(),
        }
    }
}
impl ImageCompression {
    /// The temporary chapter cache splits tall pages unless told not to.
    pub fn cache_default() -> Self {
        Self {
            split_tall_pages: true,
            ..Self::default()
        }
    }
    /// Downloads keep source bytes, and split nothing, until asked.
    pub fn downloads_default() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
    pub(crate) fn split_valid(&self) -> bool {
        SplitPlan {
            threshold_rows: self.split_threshold_rows,
            strip_rows: self.strip_rows,
        }
        .valid()
    }
    /// How this store would split a tall page, if it may: splitting needs the
    /// toggle and a store that encodes.
    pub fn split_plan(&self) -> Option<SplitPlan> {
        (self.enabled && self.split_tall_pages).then_some(SplitPlan {
            threshold_rows: self.split_threshold_rows,
            strip_rows: self.strip_rows,
        })
    }
    /// This compression's codec choice with another's split settings. Addon
    /// overrides choose a codec; where to split is a device-wide choice.
    pub(crate) fn with_split_of(mut self, other: Self) -> Self {
        self.split_tall_pages = other.split_tall_pages;
        self.split_threshold_rows = other.split_threshold_rows;
        self.strip_rows = other.strip_rows;
        self
    }
}

/// Stored files predating the split settings carry no such fields, and each
/// store has its own default for them: wire values are optional so the store
/// decides what an absent one means.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompressionWire {
    enabled: bool,
    format: ImageStorageFormat,
    quality: u8,
    #[serde(default)]
    jxl_lossless_jpeg: bool,
    #[serde(default)]
    lossless_inputs: bool,
    #[serde(default)]
    split_tall_pages: Option<bool>,
    #[serde(default)]
    split_threshold_rows: Option<u32>,
    #[serde(default)]
    strip_rows: Option<u32>,
}
impl CompressionWire {
    fn resolve(self, defaults: ImageCompression) -> ImageCompression {
        ImageCompression {
            enabled: self.enabled,
            format: self.format,
            quality: self.quality,
            jxl_lossless_jpeg: self.jxl_lossless_jpeg,
            lossless_inputs: self.lossless_inputs,
            split_tall_pages: self.split_tall_pages.unwrap_or(defaults.split_tall_pages),
            split_threshold_rows: self
                .split_threshold_rows
                .unwrap_or(defaults.split_threshold_rows),
            strip_rows: self.strip_rows.unwrap_or(defaults.strip_rows),
        }
    }
}
fn cache_compression<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<ImageCompression, D::Error> {
    CompressionWire::deserialize(deserializer)
        .map(|wire| wire.resolve(ImageCompression::cache_default()))
}
fn downloads_compression<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<ImageCompression, D::Error> {
    CompressionWire::deserialize(deserializer)
        .map(|wire| wire.resolve(ImageCompression::downloads_default()))
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
        preferences.cache = compression.cache.with_split_of(preferences.cache);
        preferences.downloads = compression.downloads.with_split_of(preferences.downloads);
    }
    preferences
}

/// Transfers read the latest durable device preference once per chapter. This
/// also covers headless jobs; hot poster/page-cache reads keep their snapshot.
pub fn download_compression(source: &str) -> ImageCompression {
    let global = load().downloads;
    crate::source_compression::snapshot(source)
        .map(|overrides| overrides.downloads.with_split_of(global))
        .unwrap_or(global)
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
    #[serde(deserialize_with = "cache_compression")]
    pub cache: ImageCompression,
    #[serde(deserialize_with = "downloads_compression")]
    pub downloads: ImageCompression,
    pub policy: CachePolicyPreferences,
}

impl Default for ImageStoragePreferences {
    fn default() -> Self {
        Self {
            cache: ImageCompression::cache_default(),
            downloads: ImageCompression::downloads_default(),
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
            && self.cache.split_valid()
            && self.downloads.split_valid()
            && self.policy.valid()
    }
    /// How a newly viewed page is split in the chapter cache. Splitting needs
    /// the disk cache itself and its compression: with either off nothing is
    /// split, whatever the saved toggle says (the saved choice is kept).
    pub fn cache_split(&self) -> Option<SplitPlan> {
        self.policy
            .disk_enabled
            .then(|| self.cache.split_plan())
            .flatten()
    }
    /// How a newly downloaded page is split: needs download compression.
    pub fn downloads_split(&self) -> Option<SplitPlan> {
        self.downloads.split_plan()
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
            "split_tall_pages" if value == 0 || value == 1 => {
                compression.split_tall_pages = value == 1
            }
            "split_threshold_rows"
                if SplitPlan::THRESHOLD_ROWS.contains(&(value as u32)) && value > 0 =>
            {
                compression.split_threshold_rows = value as u32
            }
            "strip_rows"
                if value > 0
                    && SplitPlan::STRIP_ROWS.contains(&(value as u32))
                    && value % 16 == 0 =>
            {
                compression.strip_rows = value as u32
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
                ..ImageCompression::cache_default()
            },
            downloads: ImageCompression {
                enabled: true,
                format: ImageStorageFormat::Jpeg,
                quality: 60,
                jxl_lossless_jpeg: false,
                lossless_inputs: false,
                ..ImageCompression::downloads_default()
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

    #[test]
    fn split_defaults_are_per_store_and_leave_the_codec_defaults_alone() {
        let defaults = ImageStoragePreferences::default();
        assert!(defaults.cache.split_tall_pages && !defaults.downloads.split_tall_pages);
        for compression in [defaults.cache, defaults.downloads] {
            assert_eq!(compression.split_threshold_rows, 4096);
            assert_eq!(compression.strip_rows, 2048);
        }
        // No other default moved: the cache is WebP 85, downloads stay uncompressed.
        assert!(defaults.cache.enabled);
        assert_eq!(defaults.cache.format, ImageStorageFormat::Webp);
        assert_eq!(defaults.cache.quality, 85);
        assert!(!defaults.downloads.enabled);
        assert_eq!(defaults.downloads.format, ImageStorageFormat::Webp);
        assert_eq!(defaults.downloads.quality, 85);
        assert!(defaults.valid());
    }

    #[test]
    fn files_saved_before_the_split_settings_read_with_each_stores_own_default() {
        // Exactly what an earlier build wrote: no split fields anywhere.
        let old = r#"{"cache":{"enabled":true,"format":1,"quality":70,"jxl_lossless_jpeg":false,"lossless_inputs":false},"downloads":{"enabled":true,"format":0,"quality":60},"policy":{"disk_enabled":true,"chapter_disk_limit_mb":null,"artwork_disk_limit_mb":null,"decoded_memory_mb":128,"poster_max_side":null}}"#;
        let read: ImageStoragePreferences = serde_json::from_str(old).unwrap();
        assert!(read.cache.split_tall_pages, "the cache default is on");
        assert!(
            !read.downloads.split_tall_pages,
            "download compression alone never starts splitting"
        );
        assert_eq!((read.cache.quality, read.downloads.quality), (70, 60));
        assert!(read.valid());
        // A saved choice survives, per store.
        let mut chosen = read;
        chosen.cache.split_tall_pages = false;
        chosen.downloads.split_tall_pages = true;
        chosen.downloads.strip_rows = 1024;
        let back: ImageStoragePreferences =
            serde_json::from_str(&serde_json::to_string(&chosen).unwrap()).unwrap();
        assert_eq!(back, chosen);
        // Unknown fields are still refused, in the codec objects too.
        assert!(
            serde_json::from_str::<ImageStoragePreferences>(
                r#"{"cache":{"enabled":true,"format":1,"quality":70,"surprise":1}}"#
            )
            .is_err()
        );
        // An addon override file (one codec object) reads with plain defaults.
        let overridden: crate::source_compression::CompressionOverride = serde_json::from_str(
            r#"{"cache":{"enabled":true,"format":1,"quality":70},"downloads":{"enabled":false,"format":1,"quality":85}}"#,
        )
        .unwrap();
        assert!(!overridden.cache.split_tall_pages);
    }

    #[test]
    fn splitting_needs_the_disk_cache_and_its_compression_or_download_compression() {
        let mut settings = ImageStoragePreferences::default();
        assert_eq!(
            settings.cache_split(),
            Some(SplitPlan {
                threshold_rows: 4096,
                strip_rows: 2048
            })
        );
        assert_eq!(settings.downloads_split(), None);
        // The toggle is kept when its prerequisite is off, but nothing splits.
        settings.policy.disk_enabled = false;
        assert!(settings.cache.split_tall_pages);
        assert_eq!(settings.cache_split(), None);
        settings.policy.disk_enabled = true;
        settings.cache.enabled = false;
        assert!(settings.cache.split_tall_pages);
        assert_eq!(settings.cache_split(), None);
        settings.cache.enabled = true;
        settings.cache.split_tall_pages = false;
        assert_eq!(settings.cache_split(), None);
        // Downloads: compression on and the toggle on.
        settings.downloads.split_tall_pages = true;
        assert_eq!(settings.downloads_split(), None, "compression is off");
        settings.downloads.enabled = true;
        assert_eq!(
            settings.downloads_split(),
            Some(SplitPlan {
                threshold_rows: 4096,
                strip_rows: 2048
            })
        );
        // The stores are independent.
        assert_eq!(settings.cache_split(), None);
    }

    #[test]
    fn split_edits_are_validated_and_independent_per_store() {
        let mut settings = ImageStoragePreferences::default();
        assert!(settings.change("downloads_split_tall_pages", 1));
        assert!(settings.downloads.split_tall_pages && settings.cache.split_tall_pages);
        assert!(settings.change("cache_split_tall_pages", 0));
        assert!(!settings.cache.split_tall_pages && settings.downloads.split_tall_pages);
        assert!(!settings.change("cache_split_tall_pages", 2));
        assert!(settings.change("cache_split_threshold_rows", 8192));
        assert!(settings.change("downloads_strip_rows", 1024));
        assert_eq!(settings.cache.split_threshold_rows, 8192);
        assert_eq!(settings.downloads.strip_rows, 1024);
        assert_eq!(settings.cache.strip_rows, 2048);
        for (key, value) in [
            ("cache_strip_rows", 1000),
            ("cache_strip_rows", 0),
            ("cache_strip_rows", 16),
            ("cache_strip_rows", 65536),
            ("cache_strip_rows", -2048),
            ("cache_split_threshold_rows", 100),
            ("cache_split_threshold_rows", -4096),
            ("downloads_split_threshold_rows", 1_000_000),
        ] {
            assert!(!settings.change(key, value), "{key} {value}");
        }
        assert!(settings.valid());
        settings.cache.strip_rows = 1000;
        assert!(!settings.valid(), "strips are a multiple of 16 rows");
    }

    #[test]
    fn addon_overrides_choose_a_codec_but_not_where_pages_split() {
        let mut global = ImageStoragePreferences::default();
        global.cache.split_threshold_rows = 8192;
        let overridden = ImageCompression {
            format: ImageStorageFormat::Jpeg,
            quality: 40,
            split_tall_pages: false,
            split_threshold_rows: 1024,
            strip_rows: 256,
            ..ImageCompression::default()
        }
        .with_split_of(global.cache);
        assert_eq!(overridden.format, ImageStorageFormat::Jpeg);
        assert_eq!(overridden.quality, 40);
        assert_eq!(overridden.split_threshold_rows, 8192);
        assert!(overridden.split_tall_pages);
        assert_eq!(overridden.strip_rows, 2048);
    }
}
