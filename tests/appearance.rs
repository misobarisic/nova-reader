use i_slint_backend_testing::ElementHandle;
use nova_reader::{Anim, AppWindow, Appearance, Theme, appearance};
use slint::ComponentHandle;
use std::process::Command;

fn settle() {
    for _ in 0..24 {
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
    }
}

fn control(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_checkable() == Some(true))
        .unwrap_or_else(|| panic!("missing {label}"))
}

#[test]
fn appearance_survives_restart_and_theme_reset_preserves_motion() {
    const MODE: &str = "READER_APPEARANCE_TEST_MODE";
    const DIRECTORY: &str = "READER_APPEARANCE_TEST_DIRECTORY";
    if let Ok(mode) = std::env::var(MODE) {
        i_slint_backend_testing::init_no_event_loop();
        nova_storage::init_at(std::path::Path::new(&std::env::var(DIRECTORY).unwrap()));
        let ui = AppWindow::new().unwrap();
        appearance::init(&ui).unwrap();
        let prefs = ui.global::<Appearance>();
        let theme = ui.global::<Theme>();
        match mode.as_str() {
            "write" => {
                for (key, value) in [
                    ("accent_color", 2),
                    ("background_style", 1),
                    ("card_radius", 24),
                    ("button_radius", 0),
                    ("card_spacing", 16),
                    ("transparent_navbar", 0),
                    ("animations", 0),
                    ("transitions", 0),
                    ("hover", 0),
                    ("navigation_feedback", 0),
                    ("library_list_view", 1),
                    ("library_min_cols", 3),
                    ("browse_min_cols", 4),
                    ("library_sort", 2),
                ] {
                    prefs.invoke_setting_changed(key.into(), value);
                }
                assert_eq!(
                    theme.get_current().accent,
                    slint::Color::from_rgb_u8(103, 169, 255)
                );
                assert!(theme.get_accent_tinted());
                assert_ne!(
                    theme.get_current().artwork_canvas,
                    theme.get_standard().artwork_canvas
                );
                // Invalid and unknown edits cannot alter or poison persisted data.
                prefs.invoke_setting_changed("accent_color".into(), 100);
                prefs.invoke_setting_changed("card_radius".into(), -1);
                prefs.invoke_setting_changed("unknown".into(), 1);
                assert_eq!(prefs.get_accent_color(), 2);
                assert_eq!(theme.get_card_corner_radius(), 24.);
            }
            "reload" => {
                assert!(prefs.get_library_list_view());
                assert_eq!(prefs.get_library_min_cols(), 3);
                assert_eq!(prefs.get_browse_min_cols(), 4);
                assert_eq!(prefs.get_library_sort(), 2);
                assert_eq!(prefs.get_accent_color(), 2);
                assert_eq!(prefs.get_background_style(), 1);
                assert_eq!(theme.get_card_corner_radius(), 24.);
                assert_eq!(theme.get_button_corner_radius(), 0.);
                assert_eq!(theme.get_card_spacing(), 16.);
                assert!(!theme.get_transparent_navbar());
                assert!(!ui.global::<Anim>().get_enabled());
                assert!(!ui.global::<Anim>().get_transitions());
                assert!(!ui.global::<Anim>().get_hover());
                assert!(!ui.global::<Anim>().get_nav_slide());
                prefs.invoke_setting_changed("background_style".into(), 2);
                assert!(theme.get_true_black());
                assert!(!theme.get_accent_tinted());
                assert_eq!(
                    theme.get_current().artwork_canvas,
                    slint::Color::from_rgb_u8(0, 0, 0)
                );
                assert_eq!(
                    theme.get_current().card,
                    slint::Brush::from(slint::Color::from_rgb_u8(0, 0, 0))
                );
                prefs.invoke_restore_theme();
            }
            "reset" => {
                assert!(prefs.get_library_list_view());
                assert_eq!(prefs.get_library_min_cols(), 3);
                assert_eq!(prefs.get_browse_min_cols(), 4);
                assert_eq!(prefs.get_library_sort(), 2);
                assert_eq!(theme.get_current(), theme.get_standard());
                assert_eq!(theme.get_card_corner_radius(), 10.);
                assert_eq!(theme.get_button_corner_radius(), 8.);
                assert_eq!(theme.get_card_spacing(), 8.);
                assert!(theme.get_transparent_navbar());
                assert!(!ui.global::<Anim>().get_enabled());
                assert!(!ui.global::<Anim>().get_transitions());
                assert!(!ui.global::<Anim>().get_hover());
                assert!(!ui.global::<Anim>().get_nav_slide());
            }
            "controls" => {
                ui.show().unwrap();
                ui.window().set_size(slint::LogicalSize::new(390., 2400.));
                ui.set_section(14);
                settle();
                control(&ui, "True black").invoke_accessible_default_action();
                assert_eq!(prefs.get_background_style(), 2);
                assert!(theme.get_true_black());
                control(&ui, "Blue").invoke_accessible_default_action();
                assert_eq!(prefs.get_accent_color(), 2);
                let toggle = control(&ui, "Transparent bottom navigation bar");
                toggle.mock_single_click(slint::platform::PointerEventButton::Left);
                settle();
                assert!(!prefs.get_transparent_navbar());
                assert_eq!(toggle.accessible_checked(), Some(false));
                let reset = ElementHandle::find_by_accessible_label(&ui, "Restore theme defaults")
                    .find(|element| element.accessible_enabled() == Some(true))
                    .unwrap();
                assert!(
                    reset.size().width > 140.,
                    "reset must retain Nova's intrinsic button width"
                );
                reset.invoke_accessible_default_action();
                assert_eq!(
                    toggle.accessible_checked(),
                    Some(true),
                    "reset updates a previously clicked retained toggle"
                );
                toggle.invoke_accessible_default_action();
                assert!(!prefs.get_transparent_navbar());
                assert_eq!(toggle.accessible_checked(), Some(false));
                prefs.invoke_restore_theme();
                ui.set_section(15);
                settle();
                control(&ui, "Page transitions").invoke_accessible_default_action();
                assert!(
                    !prefs.get_transitions(),
                    "master-off motion options must reject activation"
                );
                control(&ui, "Enable animations").invoke_accessible_default_action();
                assert!(prefs.get_animations());
                for label in ["Page transitions", "Hover effects", "Navigation feedback"] {
                    control(&ui, label).invoke_accessible_default_action();
                }
                assert!(ui.global::<Anim>().get_transitions());
                assert!(ui.global::<Anim>().get_hover());
                assert!(ui.global::<Anim>().get_nav_slide());
            }
            "corrupt" => {
                for raw in ["broken JSON", r#"{"future_setting":true}"#] {
                    nova_storage::try_set_str("reader:appearance", raw).unwrap();
                    let recovered = AppWindow::new().unwrap();
                    appearance::init(&recovered)
                        .expect("bad cosmetic settings cannot block startup");
                    assert_eq!(
                        recovered.global::<Theme>().get_current(),
                        recovered.global::<Theme>().get_standard()
                    );
                    assert!(!recovered.get_status().is_empty());
                    assert_eq!(
                        nova_storage::try_get_str("reader:appearance")
                            .unwrap()
                            .as_deref(),
                        Some(raw)
                    );
                    recovered.global::<Appearance>().invoke_restore_theme();
                    let restarted = AppWindow::new().unwrap();
                    appearance::init(&restarted).unwrap();
                    assert!(restarted.get_status().is_empty());
                }
            }
            _ => panic!("unexpected child mode"),
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    for mode in ["write", "reload", "reset", "controls", "corrupt"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "appearance_survives_restart_and_theme_reset_preserves_motion",
                "--nocapture",
            ])
            .env(MODE, mode)
            .env(DIRECTORY, directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
