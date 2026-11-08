//! Device screenshot choices, separate from portable library metadata.
use crate::{AppWindow, CaptureSettings};
use nova_reader_ui::backend_text as text;
use serde::{Deserialize, Serialize};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

const KEY: &str = "reader:captures";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i32", into = "i32")]
#[repr(i32)]
pub enum CaptureResolution {
    #[default]
    Device = 0,
    Chapter = 1,
}

impl TryFrom<i32> for CaptureResolution {
    type Error = &'static str;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Device),
            1 => Ok(Self::Chapter),
            _ => Err("unsupported screenshot resolution"),
        }
    }
}

impl From<CaptureResolution> for i32 {
    fn from(value: CaptureResolution) -> Self {
        value as Self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i32", into = "i32")]
#[repr(i32)]
pub enum CaptureFormat {
    #[default]
    Png = 0,
    Jpeg = 1,
    WebpLossless = 2,
    WebpLossy = 3,
    Avif = 4,
    JxlLossless = 5,
    JxlLossy = 6,
}

impl TryFrom<i32> for CaptureFormat {
    type Error = &'static str;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Png),
            1 => Ok(Self::Jpeg),
            2 => Ok(Self::WebpLossless),
            3 => Ok(Self::WebpLossy),
            4 => Ok(Self::Avif),
            5 => Ok(Self::JxlLossless),
            6 => Ok(Self::JxlLossy),
            _ => Err("unsupported screenshot format"),
        }
    }
}

impl From<CaptureFormat> for i32 {
    fn from(value: CaptureFormat) -> Self {
        value as Self
    }
}

/// Where the simple Save image action reads a page from. `AsIs` is the page as
/// the reader has it: the source for an online chapter, the download for a
/// downloaded one, the file for an import. `PreferCached` lets an online
/// chapter use its page-cache entry when one is on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i32", into = "i32")]
#[repr(i32)]
pub enum SaveSourcePreference {
    #[default]
    AsIs = 0,
    PreferCached = 1,
}
impl TryFrom<i32> for SaveSourcePreference {
    type Error = &'static str;
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::AsIs),
            1 => Ok(Self::PreferCached),
            _ => Err("unsupported save image source"),
        }
    }
}
impl From<SaveSourcePreference> for i32 {
    fn from(value: SaveSourcePreference) -> Self {
        value as Self
    }
}

/// Existing destination handling for saved pages and screencaps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i32", into = "i32")]
#[repr(i32)]
pub enum ConflictPolicy {
    #[default]
    Ask = 0,
    Overwrite = 1,
    DoNothing = 2,
}
impl TryFrom<i32> for ConflictPolicy {
    type Error = &'static str;
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ask),
            1 => Ok(Self::Overwrite),
            2 => Ok(Self::DoNothing),
            _ => Err("unsupported image conflict policy"),
        }
    }
}
impl From<ConflictPolicy> for i32 {
    fn from(value: ConflictPolicy) -> Self {
        value as Self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapturePreferences {
    pub format: CaptureFormat,
    pub resolution: CaptureResolution,
    pub jpeg_quality: u8,
    pub webp_quality: u8,
    pub avif_quality: u8,
    pub jxl_quality: u8,
    pub series_folders: bool,
    /// Shows the simple (immediate) Save image and screencap actions. The
    /// stored name predates simple Save image and is kept for compatibility.
    pub simple_screencaps: bool,
    pub conflict: ConflictPolicy,
    pub save_source: SaveSourcePreference,
    /// Simple Save image converts with the screenshot format and quality.
    pub convert_saves: bool,
}

impl Default for CapturePreferences {
    fn default() -> Self {
        Self {
            format: CaptureFormat::Png,
            resolution: CaptureResolution::Device,
            jpeg_quality: 85,
            webp_quality: 85,
            avif_quality: 85,
            jxl_quality: 85,
            series_folders: false,
            simple_screencaps: true,
            conflict: ConflictPolicy::Ask,
            save_source: SaveSourcePreference::AsIs,
            convert_saves: false,
        }
    }
}

impl CapturePreferences {
    pub(crate) fn valid(&self) -> bool {
        (1..=100).contains(&self.jpeg_quality)
            && (1..=100).contains(&self.webp_quality)
            && (1..=100).contains(&self.avif_quality)
            && (1..=100).contains(&self.jxl_quality)
    }

    /// These preferences with another output format and that format's quality,
    /// for one save or screencap that chose its own output.
    pub fn with_output(mut self, format: CaptureFormat, quality: f32) -> Self {
        let quality = if quality.is_finite() {
            quality.round().clamp(1., 100.) as u8
        } else {
            85
        };
        self.format = format;
        match format {
            CaptureFormat::Jpeg => self.jpeg_quality = quality,
            CaptureFormat::WebpLossy => self.webp_quality = quality,
            CaptureFormat::Avif => self.avif_quality = quality,
            CaptureFormat::JxlLossy => self.jxl_quality = quality,
            _ => {}
        }
        self
    }

    fn apply(&self, ui: &AppWindow) {
        let settings = ui.global::<CaptureSettings>();
        settings.set_format_index(self.format.into());
        settings.set_resolution_index(self.resolution.into());
        settings.set_jpeg_quality(f32::from(self.jpeg_quality));
        settings.set_webp_quality(f32::from(self.webp_quality));
        settings.set_avif_quality(f32::from(self.avif_quality));
        settings.set_jxl_quality(f32::from(self.jxl_quality));
        settings.set_series_folders(self.series_folders);
        settings.set_simple_screencaps(self.simple_screencaps);
        settings.set_conflict_policy(self.conflict.into());
        settings.set_save_source_index(self.save_source.into());
        settings.set_convert_saves(self.convert_saves);
    }

    fn change(&mut self, key: &str, value: i32) -> bool {
        match key {
            "format" => match CaptureFormat::try_from(value) {
                Ok(format) => self.format = format,
                Err(_) => return false,
            },
            "conflict" => match ConflictPolicy::try_from(value) {
                Ok(policy) => self.conflict = policy,
                Err(_) => return false,
            },
            "resolution" => match CaptureResolution::try_from(value) {
                Ok(resolution) => self.resolution = resolution,
                Err(_) => return false,
            },
            "jpeg_quality" if (1..=100).contains(&value) => self.jpeg_quality = value as u8,
            "webp_quality" if (1..=100).contains(&value) => self.webp_quality = value as u8,
            "avif_quality" if (1..=100).contains(&value) => self.avif_quality = value as u8,
            "jxl_quality" if (1..=100).contains(&value) => self.jxl_quality = value as u8,
            "save_source" => match SaveSourcePreference::try_from(value) {
                Ok(source) => self.save_source = source,
                Err(_) => return false,
            },
            "convert_saves" if value == 0 || value == 1 => self.convert_saves = value == 1,
            "simple_screencaps" if value == 0 || value == 1 => self.simple_screencaps = value == 1,
            "series_folders" if value == 0 || value == 1 => self.series_folders = value == 1,
            _ => return false,
        }
        true
    }
}

/// Read on the UI thread before handing this owned snapshot to an export worker.
pub fn preferences(ui: &AppWindow) -> CapturePreferences {
    let settings = ui.global::<CaptureSettings>();
    let quality = |value: f32| {
        if value.is_finite() {
            value.round().clamp(1., 100.) as u8
        } else {
            85
        }
    };
    CapturePreferences {
        format: CaptureFormat::try_from(settings.get_format_index()).unwrap_or_default(),
        resolution: CaptureResolution::try_from(settings.get_resolution_index())
            .unwrap_or_default(),
        jpeg_quality: quality(settings.get_jpeg_quality()),
        webp_quality: quality(settings.get_webp_quality()),
        avif_quality: quality(settings.get_avif_quality()),
        jxl_quality: quality(settings.get_jxl_quality()),
        series_folders: settings.get_series_folders(),
        simple_screencaps: settings.get_simple_screencaps(),
        conflict: ConflictPolicy::try_from(settings.get_conflict_policy()).unwrap_or_default(),
        save_source: SaveSourcePreference::try_from(settings.get_save_source_index())
            .unwrap_or_default(),
        convert_saves: settings.get_convert_saves(),
    }
}

/// The settings key holding a lossy format's quality.
pub fn quality_key(format: CaptureFormat) -> Option<&'static str> {
    match format {
        CaptureFormat::Jpeg => Some("jpeg_quality"),
        CaptureFormat::WebpLossy => Some("webp_quality"),
        CaptureFormat::Avif => Some("avif_quality"),
        CaptureFormat::JxlLossy => Some("jxl_quality"),
        _ => None,
    }
}

/// Apply one settings change through the same path as the settings page, so a
/// choice remembered from the Save image sheet persists and syncs identically.
pub fn change(ui: &AppWindow, key: &str, value: i32) {
    ui.global::<CaptureSettings>()
        .invoke_settings_changed(key.into(), value);
}

fn persist(
    previous: &CapturePreferences,
    preferences: &CapturePreferences,
) -> Result<CapturePreferences, String> {
    crate::sync_settings::persist_preferences(KEY, previous, preferences)
}

fn save(
    ui: &AppWindow,
    state: &RefCell<CapturePreferences>,
    next: CapturePreferences,
    write: impl FnOnce(&CapturePreferences) -> Result<CapturePreferences, String>,
) {
    match write(&next) {
        Ok(next) => {
            next.apply(ui);
            *state.borrow_mut() = next;
            ui.set_status("".into());
        }
        Err(error) => {
            // Two-way controls hold a draft; restore it when persistence fails.
            state.borrow().apply(ui);
            ui.set_status(
                format!(
                    "{}: {error}",
                    text::tr("Could not save screenshot settings")
                )
                .into(),
            );
        }
    }
}

/// Initialize after Reader's storage opens and before showing the window.
pub fn init(ui: &AppWindow) -> Result<(), String> {
    let loaded = nova_storage::try_get_str(KEY).map_err(|error| error.to_string())?;
    let prefs = match loaded {
        Some(json) => match serde_json::from_str::<CapturePreferences>(&json) {
            Ok(preferences) if preferences.valid() => preferences,
            _ => {
                // Preserve the original value until a deliberate successful edit.
                ui.set_status(
                    text::tr("Could not read screenshot settings; using defaults.").into(),
                );
                CapturePreferences::default()
            }
        },
        None => CapturePreferences::default(),
    };
    prefs.apply(ui);
    let state = Rc::new(RefCell::new(prefs));
    let weak = ui.as_weak();
    ui.global::<CaptureSettings>()
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
    fn legacy_capture_preferences_default_to_ask_and_reject_unknown_policy() {
        let old = r#"{"format":0,"resolution":0,"jpeg_quality":85,"webp_quality":85,"avif_quality":85,"series_folders":false}"#;
        let loaded: CapturePreferences = serde_json::from_str(old).unwrap();
        assert_eq!(loaded.conflict, ConflictPolicy::Ask);
        assert_eq!(loaded.jxl_quality, 85);
        assert!(loaded.simple_screencaps);
        assert_eq!(loaded.save_source, SaveSourcePreference::AsIs);
        assert!(!loaded.convert_saves);
        for policy in [
            ConflictPolicy::Ask,
            ConflictPolicy::Overwrite,
            ConflictPolicy::DoNothing,
        ] {
            let next = CapturePreferences {
                conflict: policy,
                ..loaded
            };
            assert_eq!(
                serde_json::from_str::<CapturePreferences>(&serde_json::to_string(&next).unwrap())
                    .unwrap(),
                next
            );
        }
        assert!(
            serde_json::from_str::<CapturePreferences>(
                &old.replace("false}", "false,\"conflict\":3}")
            )
            .is_err()
        );
    }
    #[test]
    fn failed_capture_save_restores_draft_and_effective_preferences() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let previous = CapturePreferences::default();
        previous.apply(&ui);
        let state = RefCell::new(previous);
        let next = CapturePreferences {
            format: CaptureFormat::JxlLossy,
            resolution: CaptureResolution::Chapter,
            jpeg_quality: 70,
            jxl_quality: 69,
            series_folders: true,
            conflict: ConflictPolicy::Overwrite,
            save_source: SaveSourcePreference::PreferCached,
            convert_saves: true,
            ..previous
        };
        next.apply(&ui);
        save(
            &ui,
            &state,
            next,
            |_| Err("test storage unavailable".into()),
        );
        assert_eq!(preferences(&ui), previous);
        assert_eq!(*state.borrow(), previous);
        assert!(!ui.get_status().is_empty());
    }
}
