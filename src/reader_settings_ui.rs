//! UI-thread publication of the last durably saved reader defaults.
use crate::{AppWindow, ReaderSettings, reader_settings::ReaderSettings as Preferences};
use slint::ComponentHandle;

/// Also use on failed edits to restore controls from the unchanged saved snapshot.
/// Series mode and rotation selectors are published separately by the controller.
pub(crate) fn apply(ui: &AppWindow, preferences: &Preferences) {
    let settings = ui.global::<ReaderSettings>();
    settings.set_default_mode(preferences.default_mode.selection());
    settings.set_rotation(preferences.rotation.into());
    settings.set_platform_android(cfg!(target_os = "android"));
    settings.set_desktop_webtoon_width(i32::from(preferences.desktop_webtoon_width));
    settings.set_paged_tap_zones(preferences.paged.tap_zones.into());
    settings.set_paged_invert_taps(preferences.paged.invert_taps.into());
    settings.set_paged_side_padding(i32::from(preferences.paged.side_padding));
    settings.set_paged_crop_borders(preferences.paged.crop_borders);
    settings.set_paged_split_wide(preferences.paged.split_wide);
    settings.set_paged_rotate_wide(preferences.paged.rotate_wide);
    settings.set_paged_flip_wide(preferences.paged.flip_wide);
    settings.set_paged_double_tap_zoom(preferences.paged.double_tap_zoom);
    settings.set_webtoon_tap_zones(preferences.webtoon.tap_zones.into());
    settings.set_webtoon_invert_taps(preferences.webtoon.invert_taps.into());
    settings.set_webtoon_side_padding(i32::from(preferences.webtoon.side_padding));
    settings.set_webtoon_crop_borders(preferences.webtoon.crop_borders);
    settings.set_webtoon_split_wide(preferences.webtoon.split_wide);
    settings.set_webtoon_rotate_wide(preferences.webtoon.rotate_wide);
    settings.set_webtoon_flip_wide(preferences.webtoon.flip_wide);
    settings.set_webtoon_double_tap_zoom(preferences.webtoon.double_tap_zoom);
    settings.set_background(preferences.background.into());
    settings.set_show_page_number(preferences.show_page_number);
    settings.set_fullscreen(preferences.fullscreen);
    settings.set_cutout(preferences.cutout);
    settings.set_keep_awake(preferences.keep_awake);
    settings.set_long_press_actions(preferences.long_press_actions);
    settings.set_chapter_transition(preferences.chapter_transition);
    settings.set_page_animations(preferences.page_animations);
    settings.set_page_flash(preferences.page_flash);
    settings.set_custom_brightness(preferences.custom_brightness);
    settings.set_brightness(i32::from(preferences.brightness));
    settings.set_color_filter(preferences.color_filter);
    settings.set_filter_color(preferences.filter_color as i32);
    settings.set_filter_intensity(i32::from(preferences.filter_intensity));
    settings.set_grayscale(preferences.grayscale);
    settings.set_invert_colors(preferences.invert_colors);
}
