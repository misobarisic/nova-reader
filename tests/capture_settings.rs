use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{
    AppWindow, CaptureSettings,
    capture_settings::{self, CaptureFormat, CapturePreferences},
};
use slint::ComponentHandle;
use std::process::Command;

fn settle() {
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
    }
}

fn select_format(ui: &AppWindow, label: &str) {
    ElementHandle::find_by_accessible_label(ui, "Screenshot format")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Combobox))
        .expect("accessible screenshot format selector")
        .invoke_accessible_default_action();
    settle();
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .expect("accessible format option")
        .invoke_accessible_default_action();
    settle();
}

#[test]
fn capture_preferences_persist_recover_and_drive_accessible_controls() {
    const MODE: &str = "READER_CAPTURE_TEST_MODE";
    const DIRECTORY: &str = "READER_CAPTURE_TEST_DIRECTORY";
    if let Ok(mode) = std::env::var(MODE) {
        i_slint_backend_testing::init_no_event_loop();
        nova_storage::init_at(std::path::Path::new(&std::env::var(DIRECTORY).unwrap()));
        let ui = AppWindow::new().unwrap();
        capture_settings::init(&ui).unwrap();
        let settings = ui.global::<CaptureSettings>();
        match mode.as_str() {
            "write" => {
                assert_eq!(
                    capture_settings::preferences(&ui),
                    CapturePreferences::default()
                );
                for (key, value) in [
                    ("format", 3),
                    ("jpeg_quality", 73),
                    ("webp_quality", 64),
                    ("series_folders", 1),
                ] {
                    settings.invoke_settings_changed(key.into(), value);
                }
                for (key, value) in [
                    ("format", 4),
                    ("jpeg_quality", 0),
                    ("webp_quality", 101),
                    ("series_folders", 2),
                    ("unknown", 1),
                ] {
                    settings.invoke_settings_changed(key.into(), value);
                }
                // Invalid edits restore even a two-way control's already-edited draft.
                settings.set_jpeg_quality(0.);
                settings.invoke_settings_changed("jpeg_quality".into(), 0);
                assert_eq!(settings.get_jpeg_quality(), 73.);
            }
            "reload" => assert_eq!(
                capture_settings::preferences(&ui),
                CapturePreferences {
                    format: CaptureFormat::WebpLossy,
                    jpeg_quality: 73,
                    webp_quality: 64,
                    series_folders: true,
                }
            ),
            "controls" => {
                for (key, value) in [
                    ("format", 0),
                    ("jpeg_quality", 85),
                    ("webp_quality", 85),
                    ("series_folders", 0),
                ] {
                    settings.invoke_settings_changed(key.into(), value);
                }
                ui.show().unwrap();
                ui.window().set_size(slint::LogicalSize::new(390., 1800.));
                ui.set_section(18);
                settle();
                assert!(
                    ElementHandle::find_by_accessible_label(&ui, "Screenshot folder")
                        .next()
                        .is_some()
                );
                select_format(&ui, "JPEG");
                assert_eq!(
                    capture_settings::preferences(&ui).format,
                    CaptureFormat::Jpeg
                );
                let quality = ElementHandle::find_by_accessible_label(&ui, "JPEG quality")
                    .find(|element| element.accessible_role() == Some(AccessibleRole::Slider))
                    .expect("accessible JPEG quality slider");
                quality.invoke_accessible_increment_action();
                assert_eq!(capture_settings::preferences(&ui).jpeg_quality, 86);
                ElementHandle::find_by_accessible_label(&ui, "Increase screenshot quality")
                    .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
                    .unwrap()
                    .invoke_accessible_default_action();
                assert_eq!(capture_settings::preferences(&ui).jpeg_quality, 87);
                ElementHandle::find_by_accessible_label(&ui, "Series folders")
                    .find(|element| element.accessible_checkable() == Some(true))
                    .unwrap()
                    .invoke_accessible_default_action();
                assert!(capture_settings::preferences(&ui).series_folders);
                select_format(&ui, "WebP (lossless)");
                assert!(
                    !ElementHandle::find_by_accessible_label(&ui, "WebP quality")
                        .any(|element| element.accessible_role() == Some(AccessibleRole::Slider))
                );
                select_format(&ui, "WebP (lossy)");
                let quality = ElementHandle::find_by_accessible_label(&ui, "WebP quality")
                    .find(|element| element.accessible_role() == Some(AccessibleRole::Slider))
                    .expect("accessible lossy WebP quality slider");
                quality.invoke_accessible_decrement_action();
                assert_eq!(capture_settings::preferences(&ui).webp_quality, 84);
                for width in [320., 390., 768.] {
                    ui.window().set_size(slint::LogicalSize::new(width, 1800.));
                    settle();
                    let selector =
                        ElementHandle::find_by_accessible_label(&ui, "Screenshot format")
                            .find(|element| {
                                element.accessible_role() == Some(AccessibleRole::Combobox)
                            })
                            .unwrap();
                    assert!(selector.size().width <= width && selector.size().width > 0.);
                }
            }
            "corrupt" => {
                for raw in [
                    "broken JSON",
                    r#"{"format":4}"#,
                    r#"{"jpeg_quality":0}"#,
                    r#"{"unexpected":true}"#,
                ] {
                    nova_storage::try_set_str("reader:captures", raw).unwrap();
                    let recovered = AppWindow::new().unwrap();
                    capture_settings::init(&recovered).unwrap();
                    assert_eq!(
                        capture_settings::preferences(&recovered),
                        CapturePreferences::default()
                    );
                    assert!(!recovered.get_status().is_empty());
                    assert_eq!(
                        nova_storage::try_get_str("reader:captures")
                            .unwrap()
                            .as_deref(),
                        Some(raw)
                    );
                    recovered
                        .global::<CaptureSettings>()
                        .invoke_settings_changed("format".into(), 1);
                    let restarted = AppWindow::new().unwrap();
                    capture_settings::init(&restarted).unwrap();
                    assert_eq!(
                        capture_settings::preferences(&restarted).format,
                        CaptureFormat::Jpeg
                    );
                }
            }
            _ => panic!("unexpected child mode"),
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    for mode in ["write", "reload", "controls", "corrupt"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "capture_preferences_persist_recover_and_drive_accessible_controls",
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
