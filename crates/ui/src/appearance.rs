//! Palette application adapted from Nova b69a1e5, crates/ui/src/lib.rs.
use crate::{AppWindow, Appearance, Theme, ThemePalette};

pub fn apply_theme(app: &AppWindow) {
    use slint::ComponentHandle;
    let theme = app.global::<Theme>();
    let settings = app.global::<Appearance>();
    let true_black = settings.get_background_style() == 2;
    let mut palette = theme.get_standard();
    apply_accent(&mut palette, settings.get_accent_color());
    if !true_black && settings.get_background_style() == 1 {
        apply_background_tint(&mut palette);
    }
    if true_black {
        let black = slint::Color::from_rgb_u8(0, 0, 0);
        palette.canvas = black;
        palette.artwork_canvas = black;
        palette.artwork_backdrop = black;
        palette.nav_bottom = black;
        palette.nav_rail = black;
        palette.scrim_artwork = black;
        palette.episode_card = black;
        palette.scrim_episode_card = black;
        palette.season_scrim = theme.get_black_season_scrim();
        // True black also flattens neutral cards and controls. Keep accents,
        // status badges and artwork intact, and use outlines for separation.
        palette.card = black.into();
        palette.popup = black;
        palette.category_panel = black;
        palette.control = black;
        palette.hero_control = black;
        palette.hero_metadata_badge = black;
        palette.settings_control = black;
        palette.stream_card = black;
        palette.season_card = black;
        palette.episode_placeholder = black;
        palette.episode_toolbar = black;
        palette.episode_search = black;
        palette.episode_empty_panel = black;
        palette.home_artwork_placeholder = black;
        palette.calendar_control = black;
        palette.tracking_control = black;
        palette.tracking_mapping = black;
        palette.tracking_panel = black;
        palette.tracking_service_control = black;
        palette.player_menu = black;
        let hover = slint::Color::from_rgb_u8(18, 18, 18);
        palette.control_hover = hover;
        palette.hero_control_hover = hover;
        palette.detail_filter_hover = hover;
        palette.input_hover = hover;
        palette.settings_control_hover = hover;
        palette.tracking_control_hover = hover;
        palette.popup_divider = slint::Color::from_rgb_u8(32, 32, 32);
        let outline = slint::Color::from_rgb_u8(48, 48, 48);
        palette.control_border = outline;
        palette.step_border = outline;
        palette.border_soft = slint::Color::from_argb_u8(48, 255, 255, 255);
        palette.border_card = slint::Color::from_argb_u8(32, 255, 255, 255);
        palette.border_subtle = slint::Color::from_argb_u8(24, 255, 255, 255);
    }
    theme.set_button_corner_radius(settings.get_button_radius() as f32);
    theme.set_card_corner_radius(settings.get_card_radius() as f32);
    theme.set_card_spacing(settings.get_card_spacing() as f32);
    theme.set_transparent_navbar(settings.get_transparent_navbar());
    theme.set_true_black(true_black);
    theme.set_accent_tinted(!true_black && settings.get_background_style() == 1);
    theme.set_accent_color_index(settings.get_accent_color());
    theme.set_current(palette);
}

/// Derive a restrained surface ramp from the accent's hue. Equal peak channel
/// values keep the presets similarly dark; lightness steps keep cards and menus
/// distinct. Video scrims, semantic badges, text and artwork stay neutral/intact.
fn apply_background_tint(palette: &mut ThemePalette) {
    use i_slint_core::graphics::{GradientStop, LinearGradientBrush};
    use slint::{Brush, Color};
    let accent = palette.accent;
    let peak = u16::from(accent.red().max(accent.green()).max(accent.blue())).max(1);
    let surface = |level: u8| {
        let channel = |value: u8| level + (u16::from(value) * 12 / peak) as u8;
        Color::from_rgb_u8(
            channel(accent.red()),
            channel(accent.green()),
            channel(accent.blue()),
        )
    };
    let canvas = surface(12);
    let card = surface(22);
    let popup = surface(28);
    let control = surface(32);
    let hover = surface(42);
    palette.canvas = canvas;
    palette.artwork_canvas = canvas;
    palette.artwork_backdrop = canvas;
    palette.nav_bottom = canvas;
    palette.nav_rail = canvas;
    palette.scrim_artwork = canvas;
    palette.card = card.into();
    palette.popup = popup;
    palette.popup_divider = surface(45);
    palette.category_panel = popup;
    palette.control = control;
    palette.settings_control = control;
    palette.hero_control = control;
    palette.hero_metadata_badge = card;
    palette.stream_card = card;
    palette.season_card = card;
    palette.episode_card = card;
    palette.scrim_episode_card = card;
    palette.episode_placeholder = card;
    palette.episode_toolbar = card;
    palette.episode_search = popup;
    palette.episode_empty_panel = card;
    palette.home_artwork_placeholder = card;
    palette.calendar_control = control;
    palette.tracking_control = control;
    palette.tracking_mapping = card;
    palette.tracking_panel = popup;
    palette.tracking_service_control = control;
    palette.player_menu = popup;
    palette.control_hover = hover;
    palette.input_hover = hover;
    palette.settings_control_hover = hover;
    palette.hero_control_hover = hover;
    palette.detail_filter_hover = hover;
    palette.episode_search_hover = hover;
    palette.tracking_control_hover = hover;
    palette.control_border = surface(55);
    palette.step_border = surface(55);
    palette.slider_track = surface(42);
    palette.season_scrim = Brush::LinearGradient(LinearGradientBrush::new(
        180.0,
        [
            GradientStop {
                position: 0.0,
                color: Color::from_argb_u8(0, canvas.red(), canvas.green(), canvas.blue()),
            },
            GradientStop {
                position: 1.0,
                color: Color::from_argb_u8(192, canvas.red(), canvas.green(), canvas.blue()),
            },
        ],
    ));
}

/// Accent roles are deliberately separate from status badges, service branding
/// and neutral surfaces. Bright presets use dark ink across every filled action.
fn apply_accent(palette: &mut ThemePalette, accent: i32) {
    use i_slint_core::graphics::{GradientStop, LinearGradientBrush};
    use slint::{Brush, Color};
    let (base, soft, hover, pressed, dark) = match accent {
        0 => return, // Preserve the original palette byte for byte.
        1 => (0xff8b35, 0xffbd88, 0xffa157, 0xed7620, 0x783b15),
        2 => (0x67a9ff, 0xa4ccff, 0x82baff, 0x4890ea, 0x244975),
        3 => (0x36d0b4, 0x86e5d4, 0x5cddc5, 0x18b79d, 0x18594e),
        4 => (0xfb7185, 0xfdb1bc, 0xff8d9e, 0xed5d74, 0x762636),
        5 => (0x22d3ee, 0x91eaf6, 0x5cdeef, 0x0db7d0, 0x115666),
        6 => (0xfbbf24, 0xffdc80, 0xffd04f, 0xe9a914, 0x70500c),
        7 => (0xa3e635, 0xcff18a, 0xb9ed5c, 0x8ccd21, 0x425c13),
        _ => return,
    };
    let rgb = |value: u32| Color::from_rgb_u8((value >> 16) as u8, (value >> 8) as u8, value as u8);
    let alpha = |value, opacity| {
        let c = rgb(value);
        Color::from_argb_u8(opacity, c.red(), c.green(), c.blue())
    };
    let gradient = |angle, start, end| {
        Brush::LinearGradient(LinearGradientBrush::new(
            angle,
            [
                GradientStop {
                    position: 0.0,
                    color: rgb(start),
                },
                GradientStop {
                    position: 1.0,
                    color: rgb(end),
                },
            ],
        ))
    };
    palette.accent = rgb(base);
    palette.accent_soft = rgb(soft);
    palette.accent_vivid = rgb(hover);
    palette.accent_hover = rgb(hover);
    palette.accent_pressed = rgb(pressed);
    palette.on_accent = Color::from_rgb_u8(20, 20, 26);
    palette.focus_ring = rgb(base);
    palette.focus_soft = alpha(hover, 96);
    palette.text_selected = rgb(soft);
    palette.selection_fill = rgb(dark);
    palette.selection_wash = alpha(base, 51);
    palette.input_selection = alpha(base, 85);
    palette.search_icon_focused = rgb(soft);
    palette.search_active_wash = alpha(base, 36);
    palette.settings_selected_wash = alpha(base, 34);
    palette.primary_button_border = alpha(soft, 112);
    palette.pairing_prompt = rgb(dark);
    palette.text_download = rgb(soft);
    palette.stream_download = rgb(dark);
    palette.download_track = alpha(base, 64);
    palette.text_search_match = rgb(soft);
    palette.text_episode_counter = rgb(soft);
    // Episode/status badges retain white text on a darker accent fill.
    palette.episode_number_badge = alpha(dark, 240);
    palette.episode_number_border = rgb(base);
    palette.text_calendar_today = rgb(soft);
    palette.text_tracking_notice = rgb(soft);
    palette.nav_selected = rgb(base);
    palette.nav_selected_border = rgb(hover);
    palette.nav_selected_glow = alpha(base, 128);
    palette.player_progress_fill = rgb(base);
    palette.primary_button = gradient(110.0, hover, pressed);
    palette.primary_button_hover = gradient(110.0, soft, base);
    palette.progress_fill = gradient(90.0, hover, pressed);
}
