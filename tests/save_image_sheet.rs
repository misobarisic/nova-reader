//! The Save image… sheet: source rows, Keep/Convert output, format limits,
//! the summary line and Save/Cancel, driven through the real window.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{AppWindow, CaptureSettings, SaveImageSource};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, LogicalSize, ModelRc, SharedString, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..24 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn find(ui: &AppWindow, label: &str, role: AccessibleRole) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(role))
        .unwrap_or_else(|| panic!("missing {role:?} {label}"))
}

fn has_text(ui: &AppWindow, text: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, text)
        .next()
        .is_some()
}

fn source(title: &str, detail: &str, enabled: bool, keepable: bool) -> SaveImageSource {
    let blocked: Vec<SharedString> = (0..7)
        .map(|format| {
            if format == 2 || format == 3 {
                "WebP supports image dimensions up to 16383 pixels".into()
            } else {
                SharedString::default()
            }
        })
        .collect();
    SaveImageSource {
        title: title.into(),
        detail: detail.into(),
        enabled,
        keepable,
        width: if title == "Original from source" {
            0
        } else {
            1200
        },
        height: if title == "Original from source" {
            0
        } else {
            20000
        },
        format: if keepable && title != "Original from source" {
            "WebP".into()
        } else {
            SharedString::default()
        },
        size: if title == "Original from source" {
            SharedString::default()
        } else {
            "380 KiB".into()
        },
        blocked: ModelRc::new(VecModel::from(blocked)),
    }
}

#[test]
fn save_image_sheet_offers_sources_output_and_remembers_nothing_silently() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    let saved = Rc::new(Cell::new(0));
    ui.on_save_image_confirmed({
        let saved = saved.clone();
        move || saved.set(saved.get() + 1)
    });
    let cancelled = Rc::new(Cell::new(0));
    ui.on_save_image_cancelled({
        let cancelled = cancelled.clone();
        move || cancelled.set(cancelled.get() + 1)
    });

    ui.set_save_image_sources(ModelRc::new(VecModel::from(vec![
        source("Original from source", "Downloads again", true, true),
        source(
            "Cached copy",
            "Split page · 380 KiB · no network",
            true,
            false,
        ),
        source("Downloaded copy", "Not cached yet", false, true),
    ])));
    ui.set_save_image_subtitle("Page 14".into());
    ui.set_save_image_source_index(0);
    ui.set_save_image_keep(true);
    ui.set_save_image_format(4);
    ui.set_save_image_quality(85.);
    ui.set_save_image_open(true);
    settle();

    // Original is kept as received: its format is not known without a fetch.
    for title in ["Original from source", "Cached copy", "Downloaded copy"] {
        find(&ui, title, AccessibleRole::RadioButton);
    }
    assert!(has_text(&ui, "Original file"));
    assert_eq!(
        find(&ui, "Downloaded copy", AccessibleRole::RadioButton).accessible_enabled(),
        Some(false),
        "an unavailable source is shown with its reason but cannot be picked"
    );
    find(&ui, "Downloaded copy", AccessibleRole::RadioButton).invoke_accessible_default_action();
    settle();
    assert_eq!(ui.get_save_image_source_index(), 0);

    // A split page has no file to keep: picking it switches to Convert.
    find(&ui, "Cached copy", AccessibleRole::RadioButton).invoke_accessible_default_action();
    settle();
    assert_eq!(ui.get_save_image_source_index(), 1);
    assert!(!ui.get_save_image_keep());
    assert!(has_text(&ui, "1200 × 20000 · AVIF 85"));
    let keep = find(&ui, "Keep file as is", AccessibleRole::Button);
    assert_eq!(keep.accessible_enabled(), Some(false));

    // WebP cannot hold this page: the reason shows and Save is disabled.
    find(&ui, "WebP", AccessibleRole::Button).mock_single_click(PointerEventButton::Left);
    settle();
    assert_eq!(ui.get_save_image_format(), 3);
    assert!(has_text(
        &ui,
        "WebP supports image dimensions up to 16383 pixels"
    ));
    let save = find(&ui, "Save", AccessibleRole::Button);
    save.invoke_accessible_default_action();
    settle();
    assert_eq!(saved.get(), 0);

    // JPEG starts from the Screenshots JPEG quality and can be saved.
    ui.global::<CaptureSettings>().set_jpeg_quality(70.);
    find(&ui, "JPEG", AccessibleRole::Button).mock_single_click(PointerEventButton::Left);
    settle();
    assert_eq!(ui.get_save_image_format(), 1);
    assert_eq!(ui.get_save_image_quality().round(), 70.);
    assert!(has_text(&ui, "1200 × 20000 · JPEG 70"));
    assert!(!ui.get_save_image_remember());
    find(&ui, "Save", AccessibleRole::Button).invoke_accessible_default_action();
    settle();
    assert_eq!(saved.get(), 1);

    // Cancel closes and tells the backend.
    find(&ui, "Cancel", AccessibleRole::Button).invoke_accessible_default_action();
    settle();
    assert!(!ui.get_save_image_open());
    assert_eq!(cancelled.get(), 1);
}

#[test]
fn simple_save_settings_live_with_the_screenshot_settings() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let changes = Rc::new(std::cell::RefCell::new(Vec::new()));
    ui.global::<CaptureSettings>().on_settings_changed({
        let changes = changes.clone();
        move |key, value| changes.borrow_mut().push((key.to_string(), value))
    });
    ui.set_section(18);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 1600.));
    settle();
    for label in [
        "Simple save image source",
        "Convert simple saves",
        "Show simple image actions",
    ] {
        assert!(has_text(&ui, label), "missing setting {label}");
    }
    find(&ui, "Convert simple saves", AccessibleRole::Checkbox).invoke_accessible_default_action();
    settle();
    assert!(ui.global::<CaptureSettings>().get_convert_saves());
    assert_eq!(
        changes.borrow().last(),
        Some(&("convert_saves".to_owned(), 1))
    );
}
