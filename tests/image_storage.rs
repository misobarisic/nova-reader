use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{
    AppWindow, ImageStorageSettings,
    image_storage::{
        self, CacheKind, CachePolicyPreferences, ImageCompression, ImageStorageFormat,
        ImageStoragePreferences,
    },
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
                    policy: CachePolicyPreferences::default(),
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

#[test]
fn cache_policy_migrates_persists_and_publishes_cached_snapshots_after_commits() {
    const MODE: &str = "READER_CACHE_POLICY_TEST_MODE";
    const DIRECTORY: &str = "READER_CACHE_POLICY_TEST_DIRECTORY";
    if let Ok(mode) = std::env::var(MODE) {
        i_slint_backend_testing::init_no_event_loop();
        nova_storage::init_at(std::path::Path::new(&std::env::var(DIRECTORY).unwrap()));
        if mode == "legacy" {
            let raw = r#"{"cache":{"enabled":true,"format":2,"quality":85},"downloads":{"enabled":false,"format":1,"quality":85}}"#;
            nova_storage::try_set_str("reader:image-storage", raw).unwrap();
        }
        let original = nova_storage::try_get_str("reader:image-storage").unwrap();
        let ui = AppWindow::new().unwrap();
        image_storage::init(&ui).unwrap();
        let settings = ui.global::<ImageStorageSettings>();
        match mode.as_str() {
            "legacy" => {
                assert_eq!(
                    image_storage::policy_snapshot(),
                    CachePolicyPreferences::default()
                );
                assert_eq!(
                    image_storage::policy_snapshot().chapter_disk_limit_mb,
                    Some(1024)
                );
                assert_eq!(
                    image_storage::policy_snapshot().artwork_disk_limit_mb,
                    Some(1024)
                );
                assert_eq!(image_storage::policy_snapshot().poster_max_side, None);
                assert_eq!(image_storage::load().cache.format, ImageStorageFormat::Avif);
                assert_eq!(image_storage::load().cache.quality, 85);
                assert_eq!(
                    nova_storage::try_get_str("reader:image-storage").unwrap(),
                    original,
                    "migration must not rewrite existing settings"
                );
            }
            "write" => {
                let wakes = std::rc::Rc::new(std::cell::Cell::new(0));
                settings.on_policy_changed({
                    let wakes = wakes.clone();
                    move || wakes.set(wakes.get() + 1)
                });
                for (key, value) in [
                    ("disk_enabled", 0),
                    ("chapter_disk_limit_index", 1),
                    ("artwork_disk_limit_index", 6),
                    ("memory_budget", 256),
                    ("poster_resolution_index", 4),
                ] {
                    settings.invoke_settings_changed(key.into(), value);
                }
                assert_eq!(wakes.get(), 5);
                let snapshot = image_storage::settings_snapshot();
                for (key, value) in [
                    ("disk_enabled", 2),
                    ("chapter_disk_limit_index", -1),
                    ("artwork_disk_limit_index", 7),
                    ("memory_budget", 33),
                    ("memory_budget", 528),
                    ("poster_resolution_index", 10),
                ] {
                    settings.invoke_settings_changed(key.into(), value);
                }
                assert_eq!(image_storage::settings_snapshot(), snapshot);
                assert_eq!(wakes.get(), 5, "invalid edits must not wake cache workers");
                assert_eq!(snapshot.cache.format, ImageStorageFormat::Avif);
                assert_eq!(snapshot.cache.quality, 85);
                assert!(!snapshot.downloads.enabled);
            }
            "reload" => {
                let policy = CachePolicyPreferences {
                    disk_enabled: false,
                    chapter_disk_limit_mb: Some(256),
                    artwork_disk_limit_mb: Some(8192),
                    decoded_memory_mb: 256,
                    poster_max_side: Some(720),
                };
                assert_eq!(image_storage::policy_snapshot(), policy);
                assert_eq!(image_storage::settings_snapshot().policy, policy);
                assert_eq!(image_storage::memory_limit_bytes(), 256 * 1024 * 1024);
                let chapters = image_storage::cache_policy(CacheKind::Chapters);
                let artwork = image_storage::cache_policy(CacheKind::Artwork);
                assert!(!chapters.enabled && !artwork.enabled);
                assert_eq!(chapters.limit_bytes, Some(256 * 1024 * 1024));
                assert_eq!(artwork.limit_bytes, Some(8192_u64 * 1024 * 1024));
                // A getter must not hit KV/SQLite after cold initialization.
                nova_storage::try_set_str("reader:image-storage", "broken").unwrap();
                assert_eq!(image_storage::policy_snapshot(), policy);
                assert_eq!(image_storage::cache_policy(CacheKind::Artwork), artwork);
                nova_storage::try_set_str("reader:image-storage", original.as_deref().unwrap())
                    .unwrap();
                settings.invoke_settings_changed("chapter_disk_limit_index".into(), 0);
                assert_eq!(
                    image_storage::cache_policy(CacheKind::Chapters).limit_bytes,
                    None
                );
            }
            "controls" => {
                for (key, value) in [
                    ("disk_enabled", 1),
                    ("chapter_disk_limit_index", 3),
                    ("artwork_disk_limit_index", 3),
                    ("memory_budget", 128),
                    ("poster_resolution_index", 0),
                    ("cache_enabled", 1),
                    ("downloads_enabled", 0),
                ] {
                    settings.invoke_settings_changed(key.into(), value);
                }
                let clear = std::rc::Rc::new(std::cell::Cell::new(0));
                let artwork_clear = std::rc::Rc::new(std::cell::Cell::new(0));
                settings.on_chapter_cache_clear({
                    let clear = clear.clone();
                    move || clear.set(clear.get() + 1)
                });
                ui.on_image_cache_clear({
                    let artwork_clear = artwork_clear.clone();
                    move || artwork_clear.set(artwork_clear.get() + 1)
                });
                ui.show().unwrap();
                ui.window().set_size(slint::LogicalSize::new(390., 3000.));
                ui.set_section(17);
                settle();
                let button = |label: &str| {
                    ElementHandle::find_by_accessible_label(&ui, label)
                        .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
                        .unwrap_or_else(|| panic!("missing button {label}"))
                };
                button("Clear chapter cache").invoke_accessible_default_action();
                assert_eq!(clear.get(), 1);
                assert_eq!(
                    artwork_clear.get(),
                    0,
                    "chapter maintenance must not dispatch artwork-only clear"
                );
                toggle(&ui, "Disk caching");
                assert!(!image_storage::cache_policy(CacheKind::Chapters).enabled);
                assert!(!image_storage::cache_policy(CacheKind::Artwork).enabled);
                assert_eq!(
                    button("Chapter disk-cache limit").accessible_enabled(),
                    Some(false)
                );
                assert_eq!(
                    quality(&ui, "Cache quality").accessible_enabled(),
                    Some(false)
                );
                toggle(&ui, "Compress new chapter downloads");
                quality(&ui, "Download quality").invoke_accessible_increment_action();
                assert_eq!(
                    image_storage::load().downloads.quality,
                    86,
                    "explicit downloads remain independent of temporary disk caching"
                );
                quality(&ui, "Decoded-image budget").invoke_accessible_increment_action();
                assert_eq!(image_storage::policy_snapshot().decoded_memory_mb, 144);
                toggle(&ui, "Disk caching");
                button("Chapter disk-cache limit").invoke_accessible_default_action();
                settle();
                button("256 MiB").invoke_accessible_default_action();
                settle();
                assert_eq!(
                    image_storage::policy_snapshot().chapter_disk_limit_mb,
                    Some(256)
                );
                button("Artwork disk-cache limit").invoke_accessible_default_action();
                settle();
                button("Unlimited").invoke_accessible_default_action();
                settle();
                assert_eq!(image_storage::policy_snapshot().artwork_disk_limit_mb, None);
                button("Poster resolution").invoke_accessible_default_action();
                settle();
                button("720 px").invoke_accessible_default_action();
                settle();
                assert_eq!(image_storage::policy_snapshot().poster_max_side, Some(720));
                settings.set_chapter_cache_busy(true);
                assert_eq!(
                    button("Clear chapter cache").accessible_enabled(),
                    Some(false)
                );
                quality(&ui, "Decoded-image budget").invoke_accessible_increment_action();
                assert_eq!(
                    image_storage::policy_snapshot().decoded_memory_mb,
                    160,
                    "memory remains adjustable during disk maintenance as in Nova"
                );
                settings.set_chapter_cache_busy(false);
                for width in [320., 390., 768.] {
                    ui.window().set_size(slint::LogicalSize::new(width, 3000.));
                    settle();
                    for label in [
                        "Chapter disk-cache limit",
                        "Artwork disk-cache limit",
                        "Poster resolution",
                    ] {
                        let control = button(label);
                        assert!(control.size().width > 0.);
                        assert!(control.absolute_position().x >= 0.);
                        assert!(
                            control.absolute_position().x + control.size().width <= width + 1.,
                            "{label} must fit width {width}"
                        );
                    }
                }
            }
            "corrupt" => {
                for raw in [
                    r#"{"policy":{"decoded_memory_mb":0}}"#,
                    r#"{"policy":{"decoded_memory_mb":33}}"#,
                    r#"{"policy":{"chapter_disk_limit_mb":0}}"#,
                    r#"{"policy":{"poster_max_side":111}}"#,
                    r#"{"policy":{"unknown":true}}"#,
                ] {
                    nova_storage::try_set_str("reader:image-storage", raw).unwrap();
                    assert_eq!(image_storage::load(), ImageStoragePreferences::default());
                    let recovered = AppWindow::new().unwrap();
                    image_storage::init(&recovered).unwrap();
                    assert_eq!(
                        image_storage::policy_snapshot(),
                        CachePolicyPreferences::default()
                    );
                    assert_eq!(
                        nova_storage::try_get_str("reader:image-storage")
                            .unwrap()
                            .as_deref(),
                        Some(raw)
                    );
                    assert!(!recovered.get_status().is_empty());
                    recovered
                        .global::<ImageStorageSettings>()
                        .invoke_settings_changed("memory_budget".into(), 128);
                    assert_eq!(
                        image_storage::load().policy,
                        CachePolicyPreferences::default()
                    );
                }
            }
            _ => panic!("unexpected mode"),
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    for mode in ["legacy", "write", "reload", "controls", "corrupt"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cache_policy_migrates_persists_and_publishes_cached_snapshots_after_commits",
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
