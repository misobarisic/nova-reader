use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{
    AppWindow, ImageStorageSettings,
    image_storage::{self, ImageCompression, ImageStorageFormat, ImageStoragePreferences},
};
use slint::ComponentHandle;
use std::process::Command;

fn settle() {
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
    }
}

fn toggle(ui: &AppWindow, label: &str) {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_checkable() == Some(true))
        .unwrap_or_else(|| panic!("missing toggle {label}"))
        .invoke_accessible_default_action();
    settle();
}

fn quality(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Slider))
        .unwrap_or_else(|| panic!("missing quality slider {label}"))
}

#[test]
fn image_storage_preferences_persist_recover_and_control_independent_groups() {
    const MODE: &str = "READER_IMAGE_STORAGE_TEST_MODE";
    const DIRECTORY: &str = "READER_IMAGE_STORAGE_TEST_DIRECTORY";
    if let Ok(mode) = std::env::var(MODE) {
        i_slint_backend_testing::init_no_event_loop();
        nova_storage::init_at(std::path::Path::new(&std::env::var(DIRECTORY).unwrap()));
        let ui = AppWindow::new().unwrap();
        image_storage::init(&ui).unwrap();
        let settings = ui.global::<ImageStorageSettings>();
        match mode.as_str() {
            "write" => {
                assert_eq!(image_storage::load(), ImageStoragePreferences::default());
                for (key, value) in [
                    ("cache_enabled", 0),
                    ("cache_format", 2),
                    ("cache_quality", 64),
                    ("downloads_enabled", 1),
                    ("downloads_format", 2),
                    ("downloads_quality", 73),
                ] {
                    settings.invoke_settings_changed(key.into(), value);
                }
                for (key, value) in [
                    ("cache_format", 3),
                    ("cache_quality", 0),
                    ("downloads_quality", 101),
                    ("downloads_enabled", 2),
                    ("unknown", 1),
                ] {
                    settings.invoke_settings_changed(key.into(), value);
                }
                settings.set_cache_quality(0.);
                settings.invoke_settings_changed("cache_quality".into(), 0);
                assert_eq!(settings.get_cache_quality(), 64.);
            }
            "reload" => assert_eq!(
                image_storage::load(),
                ImageStoragePreferences {
                    cache: ImageCompression {
                        enabled: false,
                        format: ImageStorageFormat::Avif,
                        quality: 64
                    },
                    downloads: ImageCompression {
                        enabled: true,
                        format: ImageStorageFormat::Avif,
                        quality: 73
                    },
                }
            ),
            "controls" => {
                for (key, value) in [
                    ("cache_enabled", 1),
                    ("cache_format", 1),
                    ("cache_quality", 85),
                    ("downloads_enabled", 0),
                    ("downloads_format", 1),
                    ("downloads_quality", 85),
                ] {
                    settings.invoke_settings_changed(key.into(), value);
                }
                ui.show().unwrap();
                ui.window().set_size(slint::LogicalSize::new(390., 2400.));
                ui.set_section(17);
                settle();
                quality(&ui, "Download quality").invoke_accessible_increment_action();
                assert_eq!(
                    image_storage::load().downloads.quality,
                    85,
                    "disabled download quality rejects activation"
                );
                quality(&ui, "Cache quality").invoke_accessible_increment_action();
                assert_eq!(image_storage::load().cache.quality, 86);
                ElementHandle::find_by_accessible_label(&ui, "AVIF")
                    .find(|element| element.accessible_enabled() == Some(false))
                    .unwrap()
                    .invoke_accessible_default_action();
                assert_eq!(
                    image_storage::load().downloads.format,
                    ImageStorageFormat::Webp,
                    "disabled AVIF option rejects activation"
                );
                ElementHandle::find_by_accessible_label(&ui, "JPEG")
                    .find(|element| {
                        element.accessible_checkable() == Some(true)
                            && element.accessible_enabled() == Some(true)
                    })
                    .unwrap()
                    .invoke_accessible_default_action();
                assert_eq!(image_storage::load().cache.format, ImageStorageFormat::Jpeg);
                ElementHandle::find_by_accessible_label(&ui, "AVIF")
                    .find(|element| element.accessible_enabled() == Some(true))
                    .unwrap()
                    .invoke_accessible_default_action();
                assert_eq!(image_storage::load().cache.format, ImageStorageFormat::Avif);
                toggle(&ui, "Compress new cached images");
                assert!(!image_storage::load().cache.enabled);
                quality(&ui, "Cache quality").invoke_accessible_increment_action();
                assert_eq!(image_storage::load().cache.quality, 86);
                toggle(&ui, "Compress new chapter downloads");
                assert!(image_storage::load().downloads.enabled);
                ElementHandle::find_by_accessible_label(&ui, "JPEG")
                    .find(|element| {
                        element.accessible_checkable() == Some(true)
                            && element.accessible_enabled() == Some(true)
                    })
                    .unwrap()
                    .invoke_accessible_default_action();
                assert_eq!(
                    image_storage::load().downloads.format,
                    ImageStorageFormat::Jpeg
                );
                ElementHandle::find_by_accessible_label(&ui, "AVIF")
                    .find(|element| element.accessible_enabled() == Some(true))
                    .unwrap()
                    .invoke_accessible_default_action();
                assert_eq!(
                    image_storage::load().downloads.format,
                    ImageStorageFormat::Avif
                );
                ElementHandle::find_by_accessible_label(&ui, "Increase download quality")
                    .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
                    .unwrap()
                    .invoke_accessible_default_action();
                assert_eq!(image_storage::load().downloads.quality, 86);
                assert!(!image_storage::load().cache.enabled);
                for width in [320., 390., 768.] {
                    ui.window().set_size(slint::LogicalSize::new(width, 2400.));
                    settle();
                    assert!(quality(&ui, "Download quality").size().width > 0.);
                    let option = ElementHandle::find_by_accessible_label(&ui, "AVIF")
                        .find(|element| element.accessible_enabled() == Some(true))
                        .unwrap();
                    assert!(option.absolute_position().x >= 0.);
                    assert!(
                        option.absolute_position().x + option.size().width <= width + 1.,
                        "AVIF option must fit a {width}px window"
                    );
                }
            }
            "corrupt" => {
                for raw in [
                    "broken JSON",
                    r#"{"cache":{"enabled":true,"format":3,"quality":85}}"#,
                    r#"{"downloads":{"quality":0}}"#,
                    r#"{"downloads":{"format":1,"quality":85}}"#,
                    r#"{"unknown":true}"#,
                ] {
                    nova_storage::try_set_str("reader:image-storage", raw).unwrap();
                    assert_eq!(image_storage::load(), ImageStoragePreferences::default());
                    let recovered = AppWindow::new().unwrap();
                    image_storage::init(&recovered).unwrap();
                    assert!(
                        recovered
                            .global::<ImageStorageSettings>()
                            .get_cache_enabled()
                    );
                    assert!(
                        !recovered
                            .global::<ImageStorageSettings>()
                            .get_downloads_enabled()
                    );
                    assert!(!recovered.get_status().is_empty());
                    assert_eq!(
                        nova_storage::try_get_str("reader:image-storage")
                            .unwrap()
                            .as_deref(),
                        Some(raw)
                    );
                    recovered
                        .global::<ImageStorageSettings>()
                        .invoke_settings_changed("cache_quality".into(), 80);
                    assert_eq!(image_storage::load().cache.quality, 80);
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
                "image_storage_preferences_persist_recover_and_control_independent_groups",
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

#[test]
fn image_storage_format_indices_roundtrip_without_changing_existing_values() {
    for (index, format) in [
        ImageStorageFormat::Jpeg,
        ImageStorageFormat::Webp,
        ImageStorageFormat::Avif,
    ]
    .into_iter()
    .enumerate()
    {
        let encoded = serde_json::to_string(&format).unwrap();
        assert_eq!(encoded, index.to_string());
        assert_eq!(
            serde_json::from_str::<ImageStorageFormat>(&encoded).unwrap(),
            format
        );
    }
}
