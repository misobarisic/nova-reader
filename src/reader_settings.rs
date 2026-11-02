//! Device reader defaults and source-local layout overrides.
//!
//! Storage is read when initializing or opening a series, never while scrolling.
//! Edits publish an owned cached snapshot only after the fallible KV write succeeds.
use crate::reader_modes::ModeChoice;
use reader_domain::SeriesId;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

pub const SETTINGS_KEY: &str = "reader:reader-settings";
const SERIES_PREFIX: &str = "reader:reader-settings-series:";

macro_rules! numeric_enum {
    ($name:ident { $first:ident = $n:literal $(, $variant:ident = $value:literal)* $(,)? }) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(try_from = "i32", into = "i32")]
        #[repr(i32)]
        pub enum $name { #[default] $first = $n, $( $variant = $value, )* }
        impl TryFrom<i32> for $name {
            type Error = String;
            fn try_from(value: i32) -> Result<Self, Self::Error> {
                match value { $n => Ok(Self::$first), $( $value => Ok(Self::$variant), )*
                    _ => Err(format!("unsupported {}: {value}", stringify!($name))), }
            }
        }
        impl From<$name> for i32 { fn from(value: $name) -> Self { value as Self } }
    };
}
numeric_enum!(Rotation { System = 0, Free = 1, Portrait = 2, Landscape = 3, LockedPortrait = 4, LockedLandscape = 5, ReversePortrait = 6 });
numeric_enum!(TapZones { Default = 0, L = 1, Kindle = 2, Edge = 3, RightLeft = 4, Disabled = 5 });
numeric_enum!(TapInversion { None = 0, Horizontal = 1, Vertical = 2, Both = 3 });
numeric_enum!(Background { Black = 0, Grey = 1, White = 2, Auto = 3 });

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LayoutPreferences {
    pub tap_zones: TapZones,
    pub invert_taps: TapInversion,
    pub side_padding: u8,
    pub crop_borders: bool,
    pub split_wide: bool,
    pub rotate_wide: bool,
    pub flip_wide: bool,
    pub double_tap_zoom: bool,
}
impl Default for LayoutPreferences {
    fn default() -> Self {
        Self {
            tap_zones: TapZones::Default,
            invert_taps: TapInversion::None,
            side_padding: 0,
            crop_borders: false,
            split_wide: false,
            rotate_wide: false,
            flip_wide: false,
            double_tap_zoom: true,
        }
    }
}
impl LayoutPreferences {
    pub fn validate(&self) -> Result<(), String> {
        if self.side_padding > 25 {
            return Err("side padding must be between 0 and 25 percent".into());
        }
        Ok(())
    }
    pub fn change(&mut self, key: &str, value: i32) -> Result<(), String> {
        match key {
            "tap_zones" => self.tap_zones = value.try_into()?,
            "invert_taps" => self.invert_taps = value.try_into()?,
            "side_padding" => self.side_padding = bounded(value, 0, 25, key)? as u8,
            "crop_borders" => self.crop_borders = boolean(value)?,
            "split_wide" => self.split_wide = boolean(value)?,
            "rotate_wide" => self.rotate_wide = boolean(value)?,
            "flip_wide" => self.flip_wide = boolean(value)?,
            "double_tap_zoom" => self.double_tap_zoom = boolean(value)?,
            _ => return Err(format!("unknown reader layout setting: {key}")),
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReaderSettings {
    pub default_mode: ModeChoice,
    pub rotation: Rotation,
    /// Device-local logical-pixel column cap; ignored on Android and in paged modes.
    pub desktop_webtoon_width: u16,
    pub paged: LayoutPreferences,
    pub webtoon: LayoutPreferences,
    pub background: Background,
    pub show_page_number: bool,
    /// Legacy side-navigator values remain readable so removing its UI does not
    /// reset existing reader preferences. They have no display effect.
    pub navigator_modes: u8,
    pub navigator_left: bool,
    pub navigator_height: u8,
    pub fullscreen: bool,
    pub cutout: bool,
    pub keep_awake: bool,
    pub long_press_actions: bool,
    pub chapter_transition: bool,
    pub page_animations: bool,
    pub page_flash: bool,
    pub custom_brightness: bool,
    pub brightness: u8,
    pub color_filter: bool,
    /// RGB packed as 0xRRGGBB; intensity independently controls its alpha.
    pub filter_color: u32,
    pub filter_intensity: u8,
    pub grayscale: bool,
    pub invert_colors: bool,
}
impl Default for ReaderSettings {
    fn default() -> Self {
        Self {
            default_mode: ModeChoice::Automatic,
            rotation: Rotation::System,
            desktop_webtoon_width: 720,
            paged: LayoutPreferences::default(),
            webtoon: LayoutPreferences::default(),
            background: Background::Black,
            show_page_number: false,
            navigator_modes: 24,
            navigator_left: false,
            navigator_height: 65,
            fullscreen: true,
            cutout: true,
            keep_awake: false,
            long_press_actions: true,
            chapter_transition: true,
            page_animations: true,
            page_flash: false,
            custom_brightness: false,
            brightness: 100,
            color_filter: false,
            filter_color: 0,
            filter_intensity: 25,
            grayscale: false,
            invert_colors: false,
        }
    }
}
impl ReaderSettings {
    pub fn validate(&self) -> Result<(), String> {
        if self.default_mode == ModeChoice::Default {
            return Err("the global reading mode cannot inherit itself".into());
        }
        bounded(
            i32::from(self.desktop_webtoon_width),
            320,
            1600,
            "desktop_webtoon_width",
        )?;
        self.paged.validate()?;
        self.webtoon.validate()?;
        bounded(i32::from(self.navigator_modes), 0, 31, "navigator_modes")?;
        bounded(
            i32::from(self.navigator_height),
            20,
            100,
            "navigator_height",
        )?;
        bounded(i32::from(self.brightness), 1, 100, "brightness")?;
        bounded(i32::from(self.filter_intensity), 0, 100, "filter_intensity")?;
        if self.filter_color > 0xffffff {
            return Err("filter color must be an RGB color".into());
        }
        Ok(())
    }
    pub fn layout(&self, mode: reader_domain::ReadingMode) -> &LayoutPreferences {
        if mode == reader_domain::ReadingMode::Webtoon {
            &self.webtoon
        } else {
            &self.paged
        }
    }
    pub fn change(&mut self, key: &str, value: i32) -> Result<(), String> {
        if let Some(key) = key.strip_prefix("paged_") {
            return self.paged.change(key, value);
        }
        if let Some(key) = key.strip_prefix("webtoon_") {
            return self.webtoon.change(key, value);
        }
        match key {
            "default_mode" => {
                let mode =
                    ModeChoice::from_selection(value).ok_or("unsupported default reading mode")?;
                if mode == ModeChoice::Default {
                    return Err("the global reading mode cannot inherit itself".into());
                }
                self.default_mode = mode;
            }
            "rotation" => self.rotation = value.try_into()?,
            "desktop_webtoon_width" => {
                self.desktop_webtoon_width = bounded(value, 320, 1600, key)? as u16
            }
            "background" => self.background = value.try_into()?,
            "show_page_number" => self.show_page_number = boolean(value)?,
            "navigator_modes" => self.navigator_modes = bounded(value, 0, 31, key)? as u8,
            "navigator_left" => self.navigator_left = boolean(value)?,
            "navigator_height" => self.navigator_height = bounded(value, 20, 100, key)? as u8,
            "fullscreen" => self.fullscreen = boolean(value)?,
            "cutout" => self.cutout = boolean(value)?,
            "keep_awake" => self.keep_awake = boolean(value)?,
            "long_press_actions" => self.long_press_actions = boolean(value)?,
            "chapter_transition" => self.chapter_transition = boolean(value)?,
            "page_animations" => self.page_animations = boolean(value)?,
            "page_flash" => self.page_flash = boolean(value)?,
            "custom_brightness" => self.custom_brightness = boolean(value)?,
            "brightness" => self.brightness = bounded(value, 1, 100, key)? as u8,
            "color_filter" => self.color_filter = boolean(value)?,
            "filter_color" => self.filter_color = bounded(value, 0, 0xffffff, key)? as u32,
            "filter_red" | "filter_green" | "filter_blue" => {
                let component = bounded(value, 0, 255, key)? as u32;
                let shift = match key {
                    "filter_red" => 16,
                    "filter_green" => 8,
                    _ => 0,
                };
                self.filter_color = (self.filter_color & !(0xff << shift)) | (component << shift);
            }
            "filter_intensity" => self.filter_intensity = bounded(value, 0, 100, key)? as u8,
            "grayscale" => self.grayscale = boolean(value)?,
            "invert_colors" => self.invert_colors = boolean(value)?,
            _ => return Err(format!("unknown reader setting: {key}")),
        }
        Ok(())
    }
}

/// None means follow the matching global preference. Explicit false/zero are
/// retained, so changing a global default never overwrites a series decision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LayoutOverrides {
    pub tap_zones: Option<TapZones>,
    pub invert_taps: Option<TapInversion>,
    pub side_padding: Option<u8>,
    pub crop_borders: Option<bool>,
    pub split_wide: Option<bool>,
    pub rotate_wide: Option<bool>,
    pub flip_wide: Option<bool>,
    pub double_tap_zoom: Option<bool>,
}
impl LayoutOverrides {
    pub fn apply(self, mut base: LayoutPreferences) -> LayoutPreferences {
        macro_rules! apply { ($($field:ident),*) => { $(if let Some(value) = self.$field { base.$field = value; })* }; }
        apply!(
            tap_zones,
            invert_taps,
            side_padding,
            crop_borders,
            split_wide,
            rotate_wide,
            flip_wide,
            double_tap_zoom
        );
        base
    }
    fn validate(self) -> Result<(), String> {
        self.apply(LayoutPreferences::default()).validate()
    }
    fn change(&mut self, key: &str, value: i32) -> Result<(), String> {
        if value == -1 {
            match key {
                "tap_zones" => self.tap_zones = None,
                "invert_taps" => self.invert_taps = None,
                "side_padding" => self.side_padding = None,
                "crop_borders" => self.crop_borders = None,
                "split_wide" => self.split_wide = None,
                "rotate_wide" => self.rotate_wide = None,
                "flip_wide" => self.flip_wide = None,
                "double_tap_zoom" => self.double_tap_zoom = None,
                _ => return Err(format!("unknown reader layout override: {key}")),
            }
        } else {
            let mut checked = LayoutPreferences::default();
            checked.change(key, value)?;
            match key {
                "tap_zones" => self.tap_zones = Some(checked.tap_zones),
                "invert_taps" => self.invert_taps = Some(checked.invert_taps),
                "side_padding" => self.side_padding = Some(checked.side_padding),
                "crop_borders" => self.crop_borders = Some(checked.crop_borders),
                "split_wide" => self.split_wide = Some(checked.split_wide),
                "rotate_wide" => self.rotate_wide = Some(checked.rotate_wide),
                "flip_wide" => self.flip_wide = Some(checked.flip_wide),
                "double_tap_zoom" => self.double_tap_zoom = Some(checked.double_tap_zoom),
                _ => unreachable!("change validated the layout key"),
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeriesOverrides {
    pub rotation: Option<Rotation>,
    pub paged: LayoutOverrides,
    pub webtoon: LayoutOverrides,
}
impl SeriesOverrides {
    pub fn rotation_selection(&self) -> i32 {
        self.rotation.map_or(-1, i32::from)
    }
    pub fn validate(&self) -> Result<(), String> {
        self.paged.validate()?;
        self.webtoon.validate()
    }
    pub fn change(&mut self, key: &str, value: i32) -> Result<(), String> {
        if let Some(key) = key.strip_prefix("paged_") {
            return self.paged.change(key, value);
        }
        if let Some(key) = key.strip_prefix("webtoon_") {
            return self.webtoon.change(key, value);
        }
        match key {
            "rotation" => {
                bounded(value, -1, 6, key)?;
                self.rotation = if value == -1 {
                    None
                } else {
                    Some(value.try_into()?)
                };
            }
            _ => return Err(format!("unknown series reader setting: {key}")),
        }
        Ok(())
    }
    pub fn apply(&self, mut base: ReaderSettings) -> ReaderSettings {
        if let Some(rotation) = self.rotation {
            base.rotation = rotation;
        }
        base.paged = self.paged.apply(base.paged);
        base.webtoon = self.webtoon.apply(base.webtoon);
        base
    }
}

fn boolean(value: i32) -> Result<bool, String> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("boolean reader settings accept only zero or one".into()),
    }
}
fn bounded(value: i32, min: i32, max: i32, key: &str) -> Result<i32, String> {
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{key} must be between {min} and {max}"))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record<T> {
    schema_version: u32,
    preferences: T,
}
#[derive(Default)]
struct Cache {
    global: ReaderSettings,
    series: HashMap<SeriesId, SeriesOverrides>,
}
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new(Cache::default()));

pub fn series_sync_key(series: &SeriesId) -> String {
    format!(
        "{SERIES_PREFIX}{}",
        serde_json::to_string(series).expect("SeriesId serializes")
    )
}
fn read<T: serde::de::DeserializeOwned + Default>(
    key: &str,
    validate: impl FnOnce(&T) -> Result<(), String>,
) -> Result<T, String> {
    let Some(raw) = nova_storage::try_get_str(key).map_err(|e| e.to_string())? else {
        return Ok(T::default());
    };
    match serde_json::from_str::<Record<T>>(&raw) {
        Ok(record) if record.schema_version == 1 && validate(&record.preferences).is_ok() => {
            Ok(record.preferences)
        }
        _ => {
            // Do not replace a newer or damaged record until a deliberate edit.
            eprintln!(
                "Nova Reader: invalid reader settings at {key}; preserving record and using defaults"
            );
            Ok(T::default())
        }
    }
}
/// Call after opening storage, before applying the window's initial settings.
pub fn init() -> Result<(), String> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let loaded = read(SETTINGS_KEY, ReaderSettings::validate)?;
    cache.global = loaded;
    cache.series.clear();
    Ok(())
}
/// Reload defaults and already-opened series after remote projection. Read from
/// durable storage before taking the snapshot lock; rotation remains device-local.
pub fn reload_global() -> Result<(), String> {
    let loaded = read(SETTINGS_KEY, ReaderSettings::validate)?;
    let series = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .series
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let refreshed = series
        .into_iter()
        .map(|id| {
            let value = read(&series_sync_key(&id), SeriesOverrides::validate)?;
            Ok((id, value))
        })
        .collect::<Result<HashMap<_, _>, String>>()?;
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.global = loaded;
    cache.series = refreshed;
    Ok(())
}
/// An owned snapshot is safe to retain on workers; this never accesses storage.
pub fn snapshot() -> ReaderSettings {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .global
        .clone()
}
/// Call once on chapter open. Subsequent calls use the cached per-series value.
pub fn load_series(series: &SeriesId) -> Result<SeriesOverrides, String> {
    if let Some(value) = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .series
        .get(series)
        .cloned()
    {
        return Ok(value);
    }
    // Storage reads cannot hold up UI preference snapshots. A concurrent edit
    // that has already published its value wins over this first-load result.
    let value = read(&series_sync_key(series), SeriesOverrides::validate)?;
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    Ok(cache.series.entry(series.clone()).or_insert(value).clone())
}
pub fn series_snapshot(series: &SeriesId) -> SeriesOverrides {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .series
        .get(series)
        .cloned()
        .unwrap_or_default()
}
/// Use after load_series. Kept pure with respect to storage for scroll/input paths.
pub fn effective(series: &SeriesId) -> ReaderSettings {
    let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .series
        .get(series)
        .cloned()
        .unwrap_or_default()
        .apply(cache.global.clone())
}
fn commit_global(
    cache: &mut Cache,
    next: ReaderSettings,
    write: impl FnOnce(&ReaderSettings) -> Result<ReaderSettings, String>,
) -> Result<ReaderSettings, String> {
    next.validate()?;
    let next = write(&next)?;
    cache.global = next.clone();
    Ok(next)
}
pub fn update(key: &str, value: i32) -> Result<ReaderSettings, String> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let mut next = cache.global.clone();
    next.change(key, value)?;
    let previous = cache.global.clone();
    commit_global(&mut cache, next, |value| {
        crate::sync_settings::persist_preferences(SETTINGS_KEY, &previous, value)
    })
}
pub fn save(next: ReaderSettings) -> Result<ReaderSettings, String> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let previous = cache.global.clone();
    commit_global(&mut cache, next, |value| {
        crate::sync_settings::persist_preferences(SETTINGS_KEY, &previous, value)
    })
}
fn commit_series(
    cache: &mut Cache,
    series: &SeriesId,
    next: SeriesOverrides,
    write: impl FnOnce(&SeriesOverrides) -> Result<SeriesOverrides, String>,
) -> Result<SeriesOverrides, String> {
    next.validate()?;
    let next = write(&next)?;
    cache.series.insert(series.clone(), next.clone());
    Ok(next)
}
pub fn update_series(series: &SeriesId, key: &str, value: i32) -> Result<SeriesOverrides, String> {
    load_series(series)?;
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let previous = cache.series.get(series).cloned().unwrap_or_default();
    let mut next = previous.clone();
    next.change(key, value)?;
    commit_series(&mut cache, series, next, |value| {
        crate::sync_settings::persist_preferences(&series_sync_key(series), &previous, value)
    })
}
pub fn reset_series(series: &SeriesId) -> Result<SeriesOverrides, String> {
    load_series(series)?;
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let previous = cache.series.get(series).cloned().unwrap_or_default();
    commit_series(&mut cache, series, SeriesOverrides::default(), |value| {
        crate::sync_settings::persist_preferences(&series_sync_key(series), &previous, value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_validate_ranges_and_preserve_independent_layouts() {
        let mut settings = ReaderSettings::default();
        for (key, value) in [
            ("default_mode", 6),
            ("rotation", 7),
            ("desktop_webtoon_width", 319),
            ("desktop_webtoon_width", 1601),
            ("paged_side_padding", 26),
            ("navigator_modes", 32),
            ("navigator_height", 19),
            ("brightness", 0),
            ("filter_color", 0x1000000),
            ("filter_red", 256),
            ("filter_blue", -1),
            ("filter_intensity", 101),
            ("fullscreen", 2),
            ("paged_tap_zones", 6),
            ("webtoon_invert_taps", -1),
            ("unknown", 0),
        ] {
            let before = settings.clone();
            assert!(settings.change(key, value).is_err(), "{key} {value}");
            assert_eq!(settings, before);
        }
        settings.change("paged_side_padding", 25).unwrap();
        settings.change("webtoon_split_wide", 1).unwrap();
        assert_eq!(settings.paged.side_padding, 25);
        assert_eq!(settings.webtoon.side_padding, 0);
        assert!(!settings.paged.split_wide && settings.webtoon.split_wide);
        settings.change("filter_color", 0x123456).unwrap();
        settings.change("filter_green", 0xab).unwrap();
        assert_eq!(settings.filter_color, 0x12ab56);
        settings.change("filter_red", 0xcd).unwrap();
        settings.change("filter_blue", 0xef).unwrap();
        assert_eq!(settings.filter_color, 0xcdabef);
        assert_eq!(
            serde_json::from_str::<ReaderSettings>(&serde_json::to_string(&settings).unwrap())
                .unwrap(),
            settings
        );
        settings.default_mode = ModeChoice::Default;
        assert!(settings.validate().is_err());
    }
    #[test]
    fn failed_global_and_series_writes_leave_the_published_snapshot_unchanged() {
        let series = SeriesId::new("source", "title");
        let mut cache = Cache::default();
        let before = cache.global.clone();
        let mut next = before.clone();
        next.change("fullscreen", 0).unwrap();
        assert!(commit_global(&mut cache, next, |_| Err("test write failure".into())).is_err());
        assert_eq!(cache.global, before);
        let mut next = SeriesOverrides::default();
        next.change("rotation", 3).unwrap();
        assert!(
            commit_series(&mut cache, &series, next, |_| Err(
                "test write failure".into()
            ))
            .is_err()
        );
        assert!(cache.series.is_empty());
        assert_eq!(SeriesOverrides::default().apply(cache.global), before);
    }
    #[test]
    fn explicit_false_and_zero_override_globals_and_default_restores_inheritance() {
        let mut settings = ReaderSettings::default();
        settings.change("rotation", 3).unwrap();
        settings.change("paged_side_padding", 12).unwrap();
        settings.change("paged_crop_borders", 1).unwrap();
        let mut series = SeriesOverrides::default();
        assert_eq!(series.rotation_selection(), -1);
        series.change("rotation", 1).unwrap();
        series.change("paged_side_padding", 0).unwrap();
        series.change("paged_crop_borders", 0).unwrap();
        let effective = series.apply(settings.clone());
        assert_eq!(effective.rotation, Rotation::Free);
        assert_eq!(effective.paged.side_padding, 0);
        assert!(!effective.paged.crop_borders);
        series.change("rotation", -1).unwrap();
        series.change("paged_side_padding", -1).unwrap();
        series.change("paged_crop_borders", -1).unwrap();
        assert_eq!(series.apply(settings.clone()), settings);
        assert_ne!(
            series_sync_key(&SeriesId::new("a:b", "c")),
            series_sync_key(&SeriesId::new("a", "b:c"))
        );
    }
}
