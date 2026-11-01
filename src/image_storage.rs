//! Device compression policy for newly persisted artwork and chapter pages.
use crate::{AppWindow, ImageStorageSettings};
use nova_reader_ui::backend_text as text;
use serde::{Deserialize, Serialize};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

const KEY: &str = "reader:image-storage";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i32", into = "i32")]
#[repr(i32)]
pub enum ImageStorageFormat {
    Jpeg = 0,
    #[default]
    Webp = 1,
    Avif = 2,
}

impl TryFrom<i32> for ImageStorageFormat {
    type Error = &'static str;
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Jpeg),
            1 => Ok(Self::Webp),
            2 => Ok(Self::Avif),
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
}

impl Default for ImageCompression {
    fn default() -> Self {
        Self {
            enabled: true,
            format: ImageStorageFormat::Webp,
            quality: 85,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageStoragePreferences {
    pub cache: ImageCompression,
    pub downloads: ImageCompression,
}

impl Default for ImageStoragePreferences {
    fn default() -> Self {
        Self {
            cache: ImageCompression::default(),
            downloads: ImageCompression {
                enabled: false,
                ..ImageCompression::default()
            },
        }
    }
}

impl ImageStoragePreferences {
    fn valid(&self) -> bool {
        (1..=100).contains(&self.cache.quality) && (1..=100).contains(&self.downloads.quality)
    }

    fn apply(&self, ui: &AppWindow) {
        let settings = ui.global::<ImageStorageSettings>();
        settings.set_cache_enabled(self.cache.enabled);
        settings.set_cache_format(self.cache.format.into());
        settings.set_cache_quality(f32::from(self.cache.quality));
        settings.set_downloads_enabled(self.downloads.enabled);
        settings.set_downloads_format(self.downloads.format.into());
        settings.set_downloads_quality(f32::from(self.downloads.quality));
    }

    fn change(&mut self, key: &str, value: i32) -> bool {
        let (compression, field) = if let Some(field) = key.strip_prefix("cache_") {
            (&mut self.cache, field)
        } else if let Some(field) = key.strip_prefix("downloads_") {
            (&mut self.downloads, field)
        } else {
            return false;
        };
        match field {
            "enabled" if value == 0 || value == 1 => compression.enabled = value == 1,
            "format" => match ImageStorageFormat::try_from(value) {
                Ok(format) => compression.format = format,
                Err(_) => return false,
            },
            "quality" if (1..=100).contains(&value) => compression.quality = value as u8,
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
    if preferences.valid() {
        Ok(preferences)
    } else {
        Err("image quality must be between 1 and 100".into())
    }
}

/// Worker-safe snapshot. Invalid or unavailable device settings use safe defaults
/// without replacing the saved value or modifying any existing image files.
pub fn load() -> ImageStoragePreferences {
    read().unwrap_or_default()
}

fn persist(preferences: &ImageStoragePreferences) -> Result<(), String> {
    let json = serde_json::to_string(preferences).map_err(|error| error.to_string())?;
    nova_storage::try_set_str(KEY, &json).map_err(|error| error.to_string())
}

fn save(
    ui: &AppWindow,
    state: &RefCell<ImageStoragePreferences>,
    next: ImageStoragePreferences,
    write: impl FnOnce(&ImageStoragePreferences) -> Result<(), String>,
) {
    match write(&next) {
        Ok(()) => {
            next.apply(ui);
            *state.borrow_mut() = next;
            ui.set_status("".into());
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
    preferences.apply(ui);
    let state = Rc::new(RefCell::new(preferences));
    let weak = ui.as_weak();
    ui.global::<ImageStorageSettings>()
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
    fn failed_image_storage_save_restores_both_independent_groups() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let previous = ImageStoragePreferences::default();
        let state = RefCell::new(previous);
        let next = ImageStoragePreferences {
            cache: ImageCompression {
                enabled: false,
                format: ImageStorageFormat::Jpeg,
                quality: 40,
            },
            downloads: ImageCompression {
                enabled: true,
                format: ImageStorageFormat::Jpeg,
                quality: 60,
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
        assert!(!ui.get_status().is_empty());
    }
}
