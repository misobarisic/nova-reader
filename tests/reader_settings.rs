use nova_reader::{
    reader_modes::{self, ModeChoice},
    reader_settings::{self, Rotation},
};
use reader_domain::{ReadingMode, SeriesId};
use std::process::Command;

#[test]
fn defaults_overrides_legacy_modes_and_failed_edits_survive_process_boundaries() {
    const PHASE: &str = "READER_SETTINGS_PHASE";
    const DIRECTORY: &str = "READER_SETTINGS_DIRECTORY";
    if let Ok(phase) = std::env::var(PHASE) {
        let a = SeriesId::new("source-a", "same-local");
        let b = SeriesId::new("source-b", "same-local");
        let automatic = SeriesId::new("source-a", "explicit-auto");
        let missing = SeriesId::new("source-a", "missing");
        let legacy = SeriesId::new("source-a", "legacy");
        if phase == "unavailable" {
            let before = reader_settings::snapshot();
            assert!(reader_settings::update("fullscreen", 0).is_err());
            assert_eq!(reader_settings::snapshot(), before);
            assert!(reader_settings::update_series(&a, "rotation", 3).is_err());
            assert_eq!(reader_settings::effective(&a), before);
            return;
        }
        nova_storage::init_at(std::path::Path::new(&std::env::var(DIRECTORY).unwrap()));
        reader_settings::init().unwrap();
        match phase.as_str() {
            "write" => {
                assert_eq!(
                    reader_settings::snapshot().default_mode,
                    ModeChoice::Automatic
                );
                assert_eq!(reader_settings::snapshot().rotation, Rotation::System);
                assert_eq!(
                    reader_modes::load(&missing, None).unwrap(),
                    ModeChoice::Default
                );
                reader_settings::update("default_mode", 2).unwrap();
                reader_settings::update("rotation", 3).unwrap();
                reader_settings::update("paged_side_padding", 12).unwrap();
                reader_settings::update("webtoon_crop_borders", 1).unwrap();
                reader_settings::update("filter_color", 0x4a719b).unwrap();
                reader_settings::update("desktop_webtoon_width", 640).unwrap();
                reader_settings::update_series(&a, "rotation", 1).unwrap();
                reader_settings::update_series(&a, "paged_side_padding", 0).unwrap();
                reader_settings::update_series(&b, "rotation", 2).unwrap();
                reader_modes::save(&a, ModeChoice::Default).unwrap();
                // This is the exact pre-settings Automatic record, written
                // before Default existed. It must retain its original intent.
                let key = format!(
                    "reader:mode-choice:{}",
                    serde_json::to_string(&automatic).unwrap()
                );
                nova_storage::try_set_str(&key, r#"{"schema_version":1,"choice":"Automatic"}"#)
                    .unwrap();
                assert_eq!(
                    reader_modes::load(&legacy, Some(ReadingMode::Vertical)).unwrap(),
                    ModeChoice::Manual(ReadingMode::Vertical)
                );
            }
            "reload" => {
                assert_eq!(
                    reader_settings::snapshot().default_mode,
                    ModeChoice::Manual(ReadingMode::RightToLeft)
                );
                assert_eq!(reader_settings::snapshot().filter_color, 0x4a719b);
                assert_eq!(reader_settings::snapshot().desktop_webtoon_width, 640);
                assert_eq!(
                    reader_modes::load(&missing, None)
                        .unwrap()
                        .resolve(ReadingMode::Webtoon),
                    ReadingMode::RightToLeft
                );
                assert_eq!(
                    reader_modes::load(&a, Some(ReadingMode::Vertical)).unwrap(),
                    ModeChoice::Default
                );
                assert_eq!(
                    reader_modes::load(&a, None)
                        .unwrap()
                        .resolve(ReadingMode::Webtoon),
                    ReadingMode::RightToLeft
                );
                // Explicit old Automatic remains independent of the new device default.
                assert_eq!(
                    reader_modes::load(&automatic, Some(ReadingMode::Vertical)).unwrap(),
                    ModeChoice::Automatic
                );
                assert_eq!(
                    reader_modes::load(&automatic, None)
                        .unwrap()
                        .resolve(ReadingMode::Webtoon),
                    ReadingMode::Webtoon
                );
                assert_eq!(
                    reader_modes::load(&legacy, None).unwrap(),
                    ModeChoice::Manual(ReadingMode::Vertical)
                );
                reader_settings::load_series(&a).unwrap();
                reader_settings::load_series(&b).unwrap();
                assert_eq!(reader_settings::effective(&a).rotation, Rotation::Free);
                assert_eq!(reader_settings::effective(&b).rotation, Rotation::Portrait);
                assert_eq!(reader_settings::effective(&a).paged.side_padding, 0);
                assert_eq!(reader_settings::effective(&b).paged.side_padding, 12);
                assert!(reader_settings::effective(&a).webtoon.crop_borders);
                let before = reader_settings::snapshot();
                let raw = nova_storage::try_get_str(reader_settings::SETTINGS_KEY).unwrap();
                for (key, value) in [
                    ("default_mode", 6),
                    ("navigator_modes", 32),
                    ("brightness", 0),
                    ("rotation", i32::MAX),
                ] {
                    assert!(reader_settings::update(key, value).is_err());
                    assert_eq!(reader_settings::snapshot(), before);
                    assert_eq!(
                        nova_storage::try_get_str(reader_settings::SETTINGS_KEY).unwrap(),
                        raw
                    );
                }
                let before = reader_settings::series_snapshot(&a);
                assert!(reader_settings::update_series(&a, "rotation", i32::MIN).is_err());
                assert_eq!(reader_settings::series_snapshot(&a), before);
                reader_settings::update("default_mode", 5).unwrap();
                reader_settings::update("paged_side_padding", 20).unwrap();
                reader_settings::update_series(&a, "rotation", -1).unwrap();
                reader_settings::update_series(&a, "paged_side_padding", -1).unwrap();
            }
            "changed" => {
                reader_settings::load_series(&a).unwrap();
                assert_eq!(
                    reader_modes::load(&missing, None)
                        .unwrap()
                        .resolve(ReadingMode::Vertical),
                    ReadingMode::Webtoon
                );
                assert!(reader_modes::load(&a, None).unwrap().is_gapped());
                assert!(!reader_modes::load(&automatic, None).unwrap().is_gapped());
                assert_eq!(
                    reader_modes::load(&automatic, None)
                        .unwrap()
                        .resolve(ReadingMode::Vertical),
                    ReadingMode::Vertical
                );
                assert_eq!(reader_settings::effective(&a).rotation, Rotation::Landscape);
                assert_eq!(reader_settings::effective(&a).paged.side_padding, 20);
                assert_eq!(
                    reader_modes::load(&legacy, None)
                        .unwrap()
                        .resolve(ReadingMode::Webtoon),
                    ReadingMode::Vertical
                );
            }
            "corrupt" => {
                for raw in [
                    r#"{"schema_version":999,"preferences":{}}"#,
                    r#"{"schema_version":1,"preferences":{"default_mode":"Default"}}"#,
                    r#"{"schema_version":1,"preferences":{"paged":{"side_padding":26}}}"#,
                    r#"{"schema_version":1,"preferences":{"rotation":7}}"#,
                    "invalid json",
                ] {
                    nova_storage::try_set_str(reader_settings::SETTINGS_KEY, raw).unwrap();
                    reader_settings::init().unwrap();
                    assert_eq!(
                        reader_settings::snapshot(),
                        reader_settings::ReaderSettings::default()
                    );
                    assert_eq!(
                        nova_storage::try_get_str(reader_settings::SETTINGS_KEY)
                            .unwrap()
                            .as_deref(),
                        Some(raw)
                    );
                }
                reader_settings::update("fullscreen", 0).unwrap();
                assert!(!reader_settings::snapshot().fullscreen);
            }
            _ => panic!("unknown phase: {phase}"),
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    for phase in ["unavailable", "write", "reload", "changed", "corrupt"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "defaults_overrides_legacy_modes_and_failed_edits_survive_process_boundaries",
                "--nocapture",
            ])
            .env(PHASE, phase)
            .env(DIRECTORY, directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{phase}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn settings_controls_follow_saved_values_after_edit_and_reset() {
    use i_slint_backend_testing::{AccessibleRole, ElementHandle};
    use nova_reader::{AppWindow, ReaderSettings};
    use slint::{ComponentHandle, LogicalSize};
    use std::time::Duration;
    fn settle() {
        for _ in 0..24 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        }
    }
    fn control(ui: &AppWindow, name: &str, role: AccessibleRole) -> ElementHandle {
        ElementHandle::find_by_accessible_label(ui, name)
            .find(|e| e.accessible_role() == Some(role))
            .unwrap_or_else(|| panic!("missing {name}"))
    }
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(19);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(1120., 1800.));
    let settings = ui.global::<ReaderSettings>();
    settings.on_settings_changed({
        let weak = ui.as_weak();
        move |key, value| {
            let ui = weak.unwrap();
            let s = ui.global::<ReaderSettings>();
            match key.as_str() {
                "show_page_number" => s.set_show_page_number(value != 0),
                "custom_brightness" => s.set_custom_brightness(value != 0),
                "brightness" => s.set_brightness(value),
                _ => panic!("unexpected setting {key}"),
            }
        }
    });
    settle();
    control(&ui, "General", AccessibleRole::Tab).invoke_accessible_default_action();
    settle();
    for removed in ["Chapter navigator", "Show on the left", "Navigator height"] {
        assert!(
            ElementHandle::find_by_accessible_label(&ui, removed)
                .next()
                .is_none()
        );
    }
    let counter = control(&ui, "Page counter", AccessibleRole::Checkbox);
    counter.mock_single_click(slint::platform::PointerEventButton::Left);
    settle();
    assert!(settings.get_show_page_number());
    settings.set_show_page_number(false);
    settle();
    assert_eq!(
        counter.accessible_checked(),
        Some(false),
        "switch follows a reset after pointer editing"
    );
    control(&ui, "Filters", AccessibleRole::Tab).invoke_accessible_default_action();
    settle();
    control(&ui, "Custom brightness", AccessibleRole::Checkbox).invoke_accessible_default_action();
    settle();
    let slider = control(&ui, "Brightness", AccessibleRole::Slider);
    control(&ui, "Decrease Brightness", AccessibleRole::Button).invoke_accessible_default_action();
    settle();
    assert_eq!(settings.get_brightness(), 99);
    settings.set_brightness(100);
    settle();
    assert_eq!(
        slider.accessible_value().as_deref(),
        Some("100"),
        "numeric control follows a reset after editing"
    );
}

#[test]
fn desktop_webtoon_column_padding_and_mobile_sizing_are_independent_of_zoom() {
    use nova_reader::{AppWindow, ReaderSettings};
    use slint::{ComponentHandle, LogicalSize};
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    ui.set_reading_mode(0);
    ui.set_zoom(1.);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(2560., 1440.));
    let settings = ui.global::<ReaderSettings>();
    assert_eq!(ui.get_reader_fit_width(), 720.);
    ui.set_reader_side_padding(10);
    assert_eq!(
        ui.get_reader_fit_width(),
        576.,
        "padding must work even on wide monitors"
    );
    ui.set_reader_side_padding(25);
    assert_eq!(ui.get_reader_fit_width(), 360.);
    settings.set_desktop_webtoon_width(640);
    assert_eq!(ui.get_reader_fit_width(), 320.);
    ui.set_zoom(2.);
    assert_eq!(
        ui.get_reader_width(),
        640.,
        "zoom multiplies the configured column, not the window"
    );
    ui.set_zoom(1.);
    ui.set_reader_side_padding(0);
    ui.set_reading_mode(1);
    assert_eq!(
        ui.get_reader_fit_width(),
        1100.,
        "paged layout retains its own cap"
    );
    ui.set_reading_mode(0);
    ui.set_reader_platform_android(true);
    assert_eq!(
        ui.get_reader_fit_width(),
        1100.,
        "Android ignores desktop sizing"
    );
    ui.window().set_size(LogicalSize::new(390., 844.));
    assert_eq!(ui.get_reader_fit_width(), 390.);
    ui.set_reader_platform_android(false);
    assert_eq!(
        ui.get_reader_fit_width(),
        390.,
        "small desktop windows never overflow"
    );
    ui.set_reader_side_padding(25);
    assert_eq!(ui.get_reader_fit_width(), 195.);

    // Android never offers a desktop-only sizing preference.
    use i_slint_backend_testing::{AccessibleRole, ElementHandle};
    ui.set_section(19);
    settings.set_default_mode(0);
    for _ in 0..24 {
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
    }
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Desktop webtoon width")
            .any(|element| element.accessible_role() == Some(AccessibleRole::Slider))
    );
    settings.set_platform_android(true);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Desktop webtoon width")
            .next()
            .is_none()
    );
}
