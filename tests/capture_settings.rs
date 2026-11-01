use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{
    AppWindow, CaptureSettings,
    capture_settings::{
        self, CaptureFormat, CapturePreferences, CaptureResolution, ConflictPolicy,
    },
};
use slint::ComponentHandle;
use std::process::Command;

fn settle() {
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
    }
}

fn select_format(ui: &AppWindow, label: &str) {
    select_option(ui, "Screenshot format", label);
}

fn select_option(ui: &AppWindow, selector: &str, label: &str) {
    ElementHandle::find_by_accessible_label(ui, selector)
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
                    ("format", 6),
                    ("resolution", 1),
                    ("conflict", 1),
                    ("jpeg_quality", 73),
                    ("webp_quality", 64),
                    ("avif_quality", 61),
                    ("jxl_quality", 72),
                    ("series_folders", 1),
                ] {
                    settings.invoke_settings_changed(key.into(), value);
                }
                for (key, value) in [
                    ("format", 7),
                    ("resolution", 2),
                    ("conflict", 3),
                    ("resolution", -1),
                    ("jpeg_quality", 0),
                    ("webp_quality", 101),
                    ("avif_quality", 0),
                    ("jxl_quality", 101),
                    ("series_folders", 2),
                    ("unknown", 1),
                ] {
                    settings.invoke_settings_changed(key.into(), value);
                }
                // Invalid edits restore even a two-way control's already-edited draft.
                settings.set_jpeg_quality(0.);
                settings.invoke_settings_changed("jpeg_quality".into(), 0);
                assert_eq!(settings.get_jpeg_quality(), 73.);
                settings.set_resolution_index(2);
                settings.invoke_settings_changed("resolution".into(), 2);
                assert_eq!(settings.get_resolution_index(), 1);
            }
            "reload" => assert_eq!(
                capture_settings::preferences(&ui),
                CapturePreferences {
                    format: CaptureFormat::JxlLossy,
                    resolution: CaptureResolution::Chapter,
                    conflict: ConflictPolicy::Overwrite,
                    jpeg_quality: 73,
                    webp_quality: 64,
                    avif_quality: 61,
                    jxl_quality: 72,
                    series_folders: true,
                }
            ),
            "legacy" => {
                nova_storage::try_set_str(
                    "reader:captures",
                    r#"{"format":3,"jpeg_quality":73,"webp_quality":64,"series_folders":true}"#,
                )
                .unwrap();
                let recovered = AppWindow::new().unwrap();
                capture_settings::init(&recovered).unwrap();
                let preferences = capture_settings::preferences(&recovered);
                assert_eq!(preferences.format, CaptureFormat::WebpLossy);
                assert_eq!(preferences.resolution, CaptureResolution::Device);
                assert_eq!(preferences.conflict, ConflictPolicy::Ask);
                assert_eq!(preferences.jpeg_quality, 73);
                assert_eq!(preferences.webp_quality, 64);
                assert_eq!(preferences.avif_quality, 85);
                assert_eq!(preferences.jxl_quality, 85);
                assert!(preferences.series_folders);
            }
            "controls" => {
                for (key, value) in [
                    ("format", 0),
                    ("resolution", 0),
                    ("conflict", 0),
                    ("jpeg_quality", 85),
                    ("webp_quality", 85),
                    ("avif_quality", 85),
                    ("jxl_quality", 85),
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
                for (label, policy) in [
                    ("Overwrite", ConflictPolicy::Overwrite),
                    ("Do nothing", ConflictPolicy::DoNothing),
                    ("Ask", ConflictPolicy::Ask),
                ] {
                    select_option(&ui, "File conflicts", label);
                    assert_eq!(capture_settings::preferences(&ui).conflict, policy);
                }
                select_option(&ui, "Screencap resolution", "Chapter resolution");
                assert_eq!(
                    capture_settings::preferences(&ui).resolution,
                    CaptureResolution::Chapter
                );
                select_option(&ui, "Screencap resolution", "Device resolution");
                assert_eq!(
                    capture_settings::preferences(&ui).resolution,
                    CaptureResolution::Device
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
                select_format(&ui, "AVIF");
                assert_eq!(
                    capture_settings::preferences(&ui).format,
                    CaptureFormat::Avif
                );
                let quality = ElementHandle::find_by_accessible_label(&ui, "AVIF quality")
                    .find(|element| element.accessible_role() == Some(AccessibleRole::Slider))
                    .expect("accessible AVIF quality slider");
                quality.invoke_accessible_increment_action();
                assert_eq!(capture_settings::preferences(&ui).avif_quality, 86);
                ElementHandle::find_by_accessible_label(&ui, "Increase screenshot quality")
                    .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
                    .unwrap()
                    .invoke_accessible_default_action();
                assert_eq!(capture_settings::preferences(&ui).avif_quality, 87);
                assert_eq!(capture_settings::preferences(&ui).jpeg_quality, 87);
                assert_eq!(capture_settings::preferences(&ui).webp_quality, 84);
                select_format(&ui, "JPEG XL (lossy)");
                assert_eq!(
                    capture_settings::preferences(&ui).format,
                    CaptureFormat::JxlLossy
                );
                let jxl = ElementHandle::find_by_accessible_label(&ui, "JPEG XL quality")
                    .find(|element| element.accessible_role() == Some(AccessibleRole::Slider))
                    .unwrap();
                jxl.invoke_accessible_decrement_action();
                assert_eq!(capture_settings::preferences(&ui).jxl_quality, 84);
                select_format(&ui, "JPEG XL (lossless)");
                assert_eq!(
                    capture_settings::preferences(&ui).format,
                    CaptureFormat::JxlLossless
                );
                assert!(
                    !ElementHandle::find_by_accessible_label(&ui, "JPEG XL quality")
                        .any(|element| element.accessible_role() == Some(AccessibleRole::Slider))
                );
                select_format(&ui, "JPEG XL (lossy)");
                assert_eq!(capture_settings::preferences(&ui).jxl_quality, 84);
                assert_eq!(capture_settings::preferences(&ui).avif_quality, 87);
                for width in [320., 390., 768., 1120.] {
                    ui.window().set_size(slint::LogicalSize::new(width, 1800.));
                    settle();
                    let selector =
                        ElementHandle::find_by_accessible_label(&ui, "Screenshot format")
                            .find(|element| {
                                element.accessible_role() == Some(AccessibleRole::Combobox)
                            })
                            .unwrap();
                    assert!(selector.size().width <= width && selector.size().width > 0.);
                    let conflicts = ElementHandle::find_by_accessible_label(&ui, "File conflicts")
                        .find(|element| element.accessible_role() == Some(AccessibleRole::Combobox))
                        .unwrap();
                    assert!(conflicts.absolute_position().x >= 0.);
                    assert!(
                        conflicts.absolute_position().x + conflicts.size().width <= width + 0.5
                    );
                    let resolution =
                        ElementHandle::find_by_accessible_label(&ui, "Screencap resolution")
                            .find(|element| {
                                element.accessible_role() == Some(AccessibleRole::Combobox)
                            })
                            .unwrap();
                    assert!(resolution.size().width > 0.);
                    assert!(resolution.absolute_position().x >= 0.);
                    assert!(
                        resolution.absolute_position().x + resolution.size().width <= width + 0.5
                    );
                    let quality = ElementHandle::find_by_accessible_label(&ui, "JPEG XL quality")
                        .find(|element| element.accessible_role() == Some(AccessibleRole::Slider))
                        .unwrap();
                    assert!(quality.absolute_position().y > selector.absolute_position().y);
                    assert!(
                        quality.absolute_position().y + quality.size().height
                            <= resolution.absolute_position().y,
                        "quality belongs immediately under format, before resolution/folder/conflicts"
                    );
                    select_option(&ui, "Screencap resolution", "Chapter resolution");
                    assert_eq!(
                        capture_settings::preferences(&ui).resolution,
                        CaptureResolution::Chapter
                    );
                    select_option(&ui, "Screencap resolution", "Device resolution");
                    assert_eq!(
                        capture_settings::preferences(&ui).resolution,
                        CaptureResolution::Device
                    );
                }
            }
            "corrupt" => {
                for raw in [
                    "broken JSON",
                    r#"{"format":7}"#,
                    r#"{"resolution":2}"#,
                    r#"{"conflict":3}"#,
                    r#"{"jpeg_quality":0}"#,
                    r#"{"avif_quality":0}"#,
                    r#"{"jxl_quality":0}"#,
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
    for mode in ["write", "reload", "legacy", "controls", "corrupt"] {
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

#[test]
fn capture_format_and_resolution_indices_roundtrip_compatibly() {
    for (index, format) in [
        CaptureFormat::Png,
        CaptureFormat::Jpeg,
        CaptureFormat::WebpLossless,
        CaptureFormat::WebpLossy,
        CaptureFormat::Avif,
        CaptureFormat::JxlLossless,
        CaptureFormat::JxlLossy,
    ]
    .into_iter()
    .enumerate()
    {
        let encoded = serde_json::to_string(&format).unwrap();
        assert_eq!(encoded, index.to_string());
        assert_eq!(
            serde_json::from_str::<CaptureFormat>(&encoded).unwrap(),
            format
        );
    }
    for (index, resolution) in [CaptureResolution::Device, CaptureResolution::Chapter]
        .into_iter()
        .enumerate()
    {
        let encoded = serde_json::to_string(&resolution).unwrap();
        assert_eq!(encoded, index.to_string());
        assert_eq!(
            serde_json::from_str::<CaptureResolution>(&encoded).unwrap(),
            resolution
        );
    }
    assert!(serde_json::from_str::<CaptureResolution>("2").is_err());
}

#[test]
fn save_conflict_sheet_keeps_choices_modal_accessible_and_inside_small_viewports() {
    use slint::platform::{Key, PointerEventButton, WindowEvent};
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let choices = Rc::new(RefCell::new(Vec::new()));
    ui.on_export_conflict_choice({
        let choices = choices.clone();
        move |choice| choices.borrow_mut().push(choice)
    });
    let steps = Rc::new(Cell::new(0));
    ui.on_page_step({
        let steps = steps.clone();
        move |_| steps.set(steps.get() + 1)
    });
    let navigations = Rc::new(Cell::new(0));
    ui.on_navigate({
        let navigations = navigations.clone();
        move |_| navigations.set(navigations.get() + 1)
    });
    ui.set_section(6);
    ui.set_reader_context_busy(true);
    ui.show().unwrap();
    let key = |text: Key| {
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.into() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text: text.into() });
        settle();
    };
    let button = |label: &str| {
        ElementHandle::find_by_accessible_label(&ui, label)
            .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
            .unwrap_or_else(|| panic!("missing conflict action {label}"))
    };
    let filename = format!(
        "{} - Chapter 12 - Page 4.jpg",
        "A long series title ".repeat(12)
    );
    for (width, height) in [(320., 240.), (390., 844.), (768., 240.), (1120., 780.)] {
        ui.window().set_size(slint::LogicalSize::new(width, height));
        ui.set_export_conflict_name(filename.clone().into());
        ui.set_export_conflict_open(true);
        settle();
        assert!(
            choices.borrow().is_empty(),
            "opening a conflict must not choose overwrite"
        );
        let dialog = ElementHandle::find_by_accessible_label(
            &ui,
            &format!("File already exists: {filename}"),
        )
        .find(|element| element.accessible_role() == Some(AccessibleRole::Groupbox))
        .unwrap();
        assert!(dialog.size().width > 0.);
        for label in ["Overwrite", "Add number", "Cancel"] {
            let action = button(label);
            let position = action.absolute_position();
            let size = action.size();
            assert!(
                position.x >= 0. && position.y >= 0.,
                "{label} must stay on screen"
            );
            assert!(position.x + size.width <= width + 0.5, "{label} width");
            assert!(position.y + size.height <= height + 0.5, "{label} height");
        }
        key(Key::RightArrow);
        assert_eq!(steps.get(), 0, "dialog must block chapter navigation");
        button("Add number").invoke_accessible_default_action();
        settle();
        assert!(!ui.get_export_conflict_open());
        assert_eq!(choices.borrow_mut().pop(), Some(1));
        ui.set_export_conflict_open(true);
        settle();
        button("Overwrite").invoke_accessible_default_action();
        settle();
        assert_eq!(choices.borrow_mut().pop(), Some(0));
        ui.set_export_conflict_open(true);
        settle();
        key(Key::Return);
        assert_eq!(
            choices.borrow_mut().pop(),
            Some(2),
            "initial keyboard action is safely Cancel"
        );
        ui.set_export_conflict_open(true);
        settle();
        key(Key::UpArrow);
        key(Key::Return);
        assert_eq!(choices.borrow_mut().pop(), Some(1));
        ui.set_export_conflict_open(true);
        settle();
        key(Key::Escape);
        assert_eq!(choices.borrow_mut().pop(), Some(2));
        ui.set_export_conflict_open(true);
        settle();
        ui.invoke_go_back();
        settle();
        assert_eq!(choices.borrow_mut().pop(), Some(2));
        assert_eq!(
            navigations.get(),
            0,
            "back dismisses the pending choice first"
        );
        ui.set_export_conflict_open(true);
        settle();
        let closed_cancel = button("Cancel");
        let position = slint::LogicalPosition::new(4., 4.);
        ui.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        ui.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
        settle();
        assert_eq!(choices.borrow_mut().pop(), Some(2), "outside tap cancels");
        closed_cancel.invoke_accessible_default_action();
        assert!(
            choices.borrow().is_empty(),
            "closed sheet actions cannot fire twice"
        );
    }
    ui.set_export_conflict_open(true);
    settle();
    ui.set_section(5);
    settle();
    assert_eq!(
        choices.borrow_mut().pop(),
        Some(2),
        "leaving the reader resolves pending work as Cancel"
    );
}

/// Optional native raster captures for reviewing the shared settings layout.
#[test]
#[ignore = "writes phone/desktop settings previews for visual review"]
fn render_capture_and_cache_settings_previews() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    nova_storage::init_at(directory.path());
    let ui = AppWindow::new().unwrap();
    nova_reader::appearance::init(&ui).unwrap();
    capture_settings::init(&ui).unwrap();
    nova_reader::image_storage::init(&ui).unwrap();
    ui.global::<CaptureSettings>().set_format_index(6);
    let cache = ui.global::<nova_reader::ImageStorageSettings>();
    cache.set_cache_format(4);
    cache.set_downloads_format(4);
    cache.set_downloads_enabled(true);
    ui.show().unwrap();
    let output = std::path::PathBuf::from(
        std::env::var("NOVA_READER_SETTINGS_PREVIEW_DIR")
            .unwrap_or_else(|_| "/tmp/nova-reader-settings-previews".into()),
    );
    std::fs::create_dir_all(&output).unwrap();
    for (section, name) in [(18, "screenshots"), (17, "cache")] {
        ui.set_section(section);
        for (width, height) in [(390, 844), (1120, 780), (390, 2800)] {
            ui.window()
                .set_size(slint::PhysicalSize::new(width, height));
            settle();
            let snapshot = ui.window().take_snapshot().unwrap();
            image::save_buffer(
                output.join(format!("{name}-{width}x{height}.png")),
                snapshot.as_bytes(),
                snapshot.width(),
                snapshot.height(),
                image::ColorType::Rgba8,
            )
            .unwrap();
        }
    }
}
