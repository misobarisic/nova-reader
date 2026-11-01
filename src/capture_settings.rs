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
            _ => Err("unsupported screenshot format"),
        }
    }
}

impl From<CaptureFormat> for i32 {
    fn from(value: CaptureFormat) -> Self {
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
    pub series_folders: bool,
}

impl Default for CapturePreferences {
    fn default() -> Self {
        Self {
            format: CaptureFormat::Png,
            resolution: CaptureResolution::Device,
            jpeg_quality: 85,
            webp_quality: 85,
            avif_quality: 85,
            series_folders: false,
        }
    }
}

impl CapturePreferences {
    fn valid(&self) -> bool {
        (1..=100).contains(&self.jpeg_quality)
            && (1..=100).contains(&self.webp_quality)
            && (1..=100).contains(&self.avif_quality)
    }

    fn apply(&self, ui: &AppWindow) {
        let settings = ui.global::<CaptureSettings>();
        settings.set_format_index(self.format.into());
        settings.set_resolution_index(self.resolution.into());
        settings.set_jpeg_quality(f32::from(self.jpeg_quality));
        settings.set_webp_quality(f32::from(self.webp_quality));
        settings.set_avif_quality(f32::from(self.avif_quality));
        settings.set_series_folders(self.series_folders);
    }

    fn change(&mut self, key: &str, value: i32) -> bool {
        match key {
            "format" => match CaptureFormat::try_from(value) {
                Ok(format) => self.format = format,
                Err(_) => return false,
            },
            "resolution" => match CaptureResolution::try_from(value) {
                Ok(resolution) => self.resolution = resolution,
                Err(_) => return false,
            },
            "jpeg_quality" if (1..=100).contains(&value) => self.jpeg_quality = value as u8,
            "webp_quality" if (1..=100).contains(&value) => self.webp_quality = value as u8,
            "avif_quality" if (1..=100).contains(&value) => self.avif_quality = value as u8,
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
        series_folders: settings.get_series_folders(),
    }
}

fn persist(preferences: &CapturePreferences) -> Result<(), String> {
    let json = serde_json::to_string(preferences).map_err(|error| error.to_string())?;
    nova_storage::try_set_str(KEY, &json).map_err(|error| error.to_string())
}

fn save(
    ui: &AppWindow,
    state: &RefCell<CapturePreferences>,
    next: CapturePreferences,
    write: impl FnOnce(&CapturePreferences) -> Result<(), String>,
) {
    match write(&next) {
        Ok(()) => {
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
                    save(&ui, &state, next, persist);
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
    fn failed_capture_save_restores_draft_and_effective_preferences() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let previous = CapturePreferences::default();
        previous.apply(&ui);
        let state = RefCell::new(previous);
        let next = CapturePreferences {
            format: CaptureFormat::Jpeg,
            resolution: CaptureResolution::Chapter,
            jpeg_quality: 70,
            series_folders: true,
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
