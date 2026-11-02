use nova_reader::{
    Anim, AppWindow, ImageStorageSettings, SourceCompressionSettings, image_storage,
    services::Services,
    source_compression::{self, CompressionOverride},
};
use slint::ComponentHandle;
use std::{
    io::{Cursor, Write},
    process::Command,
    time::Duration,
};

fn fixture(services: &Services) {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", br#"{"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"fixture","name":"Fixture source","version":"1.0.0","language":"en","entry":"main.js","permissions":{"domains":["example.com"]}}"#.to_vec()),
        ("main.js", b"globalThis.source = {};".to_vec()),
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(&bytes).unwrap();
    }
    services
        .sources
        .install_zip(&zip.finish().unwrap().into_inner())
        .unwrap();
}

#[test]
fn addon_inheritance_enabled_switches_persist_and_isolate_sources() {
    const DIRECTORY: &str = "READER_SOURCE_COMPRESSION_TEST_DIRECTORY";
    if let Ok(directory) = std::env::var(DIRECTORY) {
        i_slint_backend_testing::init_no_event_loop();
        let services = Services::open(std::path::Path::new(&directory)).unwrap();
        let ui = AppWindow::new().unwrap();
        image_storage::init(&ui).unwrap();
        source_compression::init(&ui, services.clone());
        if std::env::var("READER_SOURCE_COMPRESSION_RELOAD").is_ok() {
            assert!(source_compression::snapshot("fixture").is_none());
            assert!(!image_storage::effective_for_source("fixture").cache.enabled);
            let saved = source_compression::snapshot("other").unwrap();
            assert!(saved.cache.enabled);
            assert!(!saved.downloads.enabled);
            return;
        }
        fixture(&services);
        source_compression::select(&ui, "fixture");
        let addon = ui.global::<SourceCompressionSettings>();
        let global = ui.global::<ImageStorageSettings>();
        assert!(addon.get_use_global());
        global.invoke_settings_changed("cache_enabled".into(), 0);
        global.invoke_settings_changed("downloads_enabled".into(), 1);
        addon.invoke_settings_changed("use_global".into(), 0);
        assert!(!addon.get_cache_enabled());
        assert!(addon.get_downloads_enabled());
        addon.invoke_settings_changed("cache_enabled".into(), 1);
        addon.invoke_settings_changed("downloads_enabled".into(), 0);
        addon.invoke_settings_changed("cache_format".into(), 3);
        addon.invoke_settings_changed("cache_quality".into(), 67);
        addon.invoke_settings_changed("cache_lossless_inputs".into(), 1);
        addon.invoke_settings_changed("cache_jxl_lossless_jpeg".into(), 1);
        let effective = image_storage::effective_for_source("fixture");
        assert!(
            effective.cache.enabled,
            "addon can compress when global is off"
        );
        assert!(
            !effective.downloads.enabled,
            "addon can preserve originals when global is on"
        );
        assert_eq!(effective.cache.quality, 67);
        assert!(effective.cache.lossless_inputs && effective.cache.jxl_lossless_jpeg);
        assert!(
            !image_storage::effective_for_source("unrelated")
                .cache
                .enabled
        );
        assert!(
            image_storage::effective_for_source("unrelated")
                .downloads
                .enabled
        );
        assert_eq!(effective.policy, image_storage::policy_snapshot());
        source_compression::persist(
            "other",
            Some(CompressionOverride {
                cache: effective.cache,
                downloads: effective.downloads,
            }),
        )
        .unwrap();
        // Global changes cannot overwrite a source override or its enabled state.
        global.invoke_settings_changed("cache_quality".into(), 42);
        assert_eq!(
            image_storage::effective_for_source("fixture").cache.quality,
            67
        );
        addon.invoke_settings_changed("cache_quality".into(), 0);
        assert_eq!(addon.get_cache_quality(), 67.);
        addon.invoke_settings_changed("use_global".into(), 1);
        assert!(source_compression::snapshot("fixture").is_none());
        assert!(!image_storage::effective_for_source("fixture").cache.enabled);
        assert!(
            image_storage::effective_for_source("fixture")
                .downloads
                .enabled
        );
        assert_eq!(
            image_storage::effective_for_source("fixture").cache.quality,
            42
        );
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    for reload in [false, true] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "addon_inheritance_enabled_switches_persist_and_isolate_sources",
                "--nocapture",
            ])
            .env(DIRECTORY, directory.path());
        if reload {
            child.env("READER_SOURCE_COMPRESSION_RELOAD", "1");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn native_addon_controls_expose_enabled_switches_and_disable_dependent_quality() {
    use i_slint_backend_testing::{AccessibleRole, ElementHandle, mock_elapsed_time};
    use std::{cell::RefCell, rc::Rc};
    i_slint_backend_testing::init_no_event_loop();
    for width in [390., 1120.] {
        let ui = AppWindow::new().unwrap();
        ui.global::<Anim>().set_enabled(false);
        // The testing accessibility search omits controls clipped below the
        // scroll viewport. A tall surface exposes both groups at each responsive
        // width; the preview test separately captures ordinary phone/desktop sizes.
        ui.window().set_size(slint::LogicalSize::new(width, 2000.));
        ui.set_section(8);
        let settings = ui.global::<SourceCompressionSettings>();
        settings.set_use_global(false);
        settings.set_cache_enabled(true);
        settings.set_downloads_enabled(false);
        let changes = Rc::new(RefCell::new(Vec::new()));
        settings.on_settings_changed({
            let changes = changes.clone();
            move |key, value| changes.borrow_mut().push((key.to_string(), value))
        });
        ui.show().unwrap();
        mock_elapsed_time(Duration::from_millis(100));
        let slider_enabled = |label| {
            ElementHandle::find_by_accessible_label(&ui, label)
                .find(|e| e.accessible_role() == Some(AccessibleRole::Slider))
                .unwrap_or_else(|| {
                    panic!(
                        "missing slider {label} width={width}: {:?}",
                        ElementHandle::find_by_accessible_label(&ui, label)
                            .map(|e| (e.accessible_role(), e.absolute_position()))
                            .collect::<Vec<_>>()
                    )
                })
                .accessible_enabled()
                .unwrap()
        };
        assert!(slider_enabled("Cache quality"));
        assert!(!slider_enabled("Download quality"));
        for label in ["Compress cached images", "Compress chapter downloads"] {
            ElementHandle::find_by_accessible_label(&ui, label)
                .find(|e| e.accessible_checkable() == Some(true))
                .unwrap()
                .invoke_accessible_default_action();
        }
        mock_elapsed_time(Duration::from_millis(100));
        assert!(!slider_enabled("Cache quality"));
        assert!(slider_enabled("Download quality"));
        assert_eq!(
            *changes.borrow(),
            [
                ("cache_enabled".to_owned(), 0),
                ("downloads_enabled".to_owned(), 1)
            ]
        );
    }
}

#[test]
#[ignore = "opt-in native addon settings screenshots"]
fn render_source_compression_previews() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions, mock_elapsed_time};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let ui = AppWindow::new().unwrap();
    ui.global::<Anim>().set_enabled(false);
    ui.set_section(8);
    ui.set_heading("Fixture source".into());
    ui.set_caption("Settings".into());
    let addon = ui.global::<SourceCompressionSettings>();
    addon.set_use_global(false);
    addon.set_cache_enabled(true);
    addon.set_downloads_enabled(false);
    ui.show().unwrap();
    let dir = std::path::PathBuf::from("/workspace/artifacts/source-compression-preview");
    std::fs::create_dir_all(&dir).unwrap();
    for (width, height) in [(390, 844), (1120, 780), (390, 2000)] {
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));
        mock_elapsed_time(Duration::from_millis(100));
        let shot = ui.window().take_snapshot().unwrap();
        image::save_buffer(
            dir.join(if height == 2000 {
                format!("compression-full-{width}x{height}.png")
            } else {
                format!("compression-{width}x{height}.png")
            }),
            shot.as_bytes(),
            shot.width(),
            shot.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
    addon.set_use_global(true);
    ui.window().set_size(slint::PhysicalSize::new(390, 844));
    mock_elapsed_time(Duration::from_millis(100));
    let shot = ui.window().take_snapshot().unwrap();
    image::save_buffer(
        dir.join("compression-inherited-390x844.png"),
        shot.as_bytes(),
        shot.width(),
        shot.height(),
        image::ColorType::Rgba8,
    )
    .unwrap();
}

#[test]
fn extracted_source_settings_preserve_catalogue_bool_and_secret_callbacks() {
    use i_slint_backend_testing::{AccessibleRole, ElementHandle, mock_elapsed_time};
    use nova_reader::SettingRow;
    use slint::{
        ModelRc, VecModel,
        platform::{Key, PointerEventButton, WindowEvent},
    };
    use std::{cell::RefCell, rc::Rc};
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.global::<Anim>().set_enabled(false);
    ui.set_section(8);
    ui.window().set_size(slint::LogicalSize::new(390., 1000.));
    ui.set_source_catalog_settings(ModelRc::new(VecModel::from(vec![SettingRow {
        key: "fixture.en".into(),
        name: "English catalogue".into(),
        checked: true,
        ..Default::default()
    }])));
    ui.set_settings(ModelRc::new(VecModel::from(vec![
        SettingRow {
            key: "adult".into(),
            name: "Adult filter".into(),
            kind: "bool".into(),
            ..Default::default()
        },
        SettingRow {
            key: "token".into(),
            name: "Access token".into(),
            kind: "secret".into(),
            ..Default::default()
        },
    ])));
    let catalogues = Rc::new(RefCell::new(Vec::new()));
    ui.on_catalog_enabled_changed({
        let events = catalogues.clone();
        move |key, value| events.borrow_mut().push((key.to_string(), value))
    });
    let fields = Rc::new(RefCell::new(Vec::new()));
    ui.on_setting_changed({
        let events = fields.clone();
        move |key, value| {
            events
                .borrow_mut()
                .push((key.to_string(), value.to_string()))
        }
    });
    ui.show().unwrap();
    mock_elapsed_time(Duration::from_millis(100));
    for label in ["English catalogue", "Adult filter"] {
        ElementHandle::find_by_accessible_label(&ui, label)
            .find(|e| e.accessible_checkable() == Some(true))
            .unwrap_or_else(|| {
                panic!(
                    "missing source toggle {label}: {:?}",
                    ElementHandle::find_by_accessible_label(&ui, label)
                        .map(|e| (
                            e.accessible_role(),
                            e.accessible_checkable(),
                            e.absolute_position()
                        ))
                        .collect::<Vec<_>>()
                )
            })
            .invoke_accessible_default_action();
    }
    let secret = ElementHandle::find_by_accessible_label(&ui, "Access token")
        .find(|e| e.accessible_role() == Some(AccessibleRole::TextInput))
        .unwrap();
    secret.mock_single_click(PointerEventButton::Left);
    for text in ["private-token".into(), Key::Return.into()] {
        ui.window().dispatch_event(WindowEvent::KeyPressed { text });
    }
    assert_eq!(*catalogues.borrow(), [("fixture.en".to_owned(), false)]);
    assert_eq!(
        *fields.borrow(),
        [
            ("adult".to_owned(), "true".to_owned()),
            ("token".to_owned(), "private-token".to_owned())
        ]
    );
}
