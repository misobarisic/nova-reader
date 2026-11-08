//! Destructive actions ask first, and empty or loading states are labelled
//! honestly. Headless Slint checks for the library, extension and detail pages.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{AppWindow, ExtensionRow, ItemRow};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..24 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}
fn button(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button {label}"))
}
fn has_label(ui: &AppWindow, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, label)
        .next()
        .is_some()
}

#[test]
fn removing_an_addon_waits_for_confirmation() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_extensions(ModelRc::new(VecModel::from(vec![ExtensionRow {
        id: "sample-package".into(),
        name: "Sample extension".into(),
        version: "1.0.0".into(),
        language: "en".into(),
        enabled: true,
        ..Default::default()
    }])));
    let calls = Rc::new(RefCell::new(Vec::new()));
    ui.on_extension_action({
        let calls = calls.clone();
        move |id, action| calls.borrow_mut().push(format!("{id}:{action}"))
    });
    ui.set_section(3);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();
    ElementHandle::find_by_accessible_label(&ui, "Extensions")
        .find(|e| e.accessible_role() == Some(AccessibleRole::Tab))
        .expect("Extensions tab")
        .invoke_accessible_default_action();
    settle();
    // Remove lives in the extension's ⋯ menu and still asks first.
    let remove = |ui: &AppWindow| {
        button(ui, "Sample extension options").invoke_accessible_default_action();
        settle();
        button(ui, "Remove").invoke_accessible_default_action();
        settle();
    };
    remove(&ui);
    assert!(calls.borrow().is_empty(), "Remove asks first");
    button(&ui, "Cancel").invoke_accessible_default_action();
    settle();
    assert!(calls.borrow().is_empty(), "Cancel keeps the addon");
    remove(&ui);
    button(&ui, "Remove addon").invoke_accessible_default_action();
    settle();
    assert_eq!(*calls.borrow(), ["sample-package:remove"]);
}

#[test]
fn library_distinguishes_no_matches_from_an_empty_library() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(0);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();
    assert!(has_label(&ui, "Your next story starts here"));
    assert!(!has_label(&ui, "No titles match"));
    ui.set_library_query("zzz".into());
    settle();
    assert!(has_label(&ui, "No titles match"));
    assert!(!has_label(&ui, "Your next story starts here"));
}

#[test]
fn refresh_library_is_disabled_while_it_runs() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(1);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();
    assert_eq!(
        button(&ui, "Refresh library").accessible_enabled(),
        Some(true)
    );
    // The Updates header shows the refresh's progress while it runs.
    let bridge = ui.global::<nova_reader::TimelineBridge>();
    bridge.set_refreshing(true);
    ui.set_busy(true);
    settle();
    assert_eq!(
        button(&ui, "Refresh library").accessible_enabled(),
        Some(false)
    );
    assert!(has_label(&ui, "Checking library…"));
    bridge.set_refresh_total(46);
    bridge.set_refresh_done(18);
    settle();
    assert!(has_label(&ui, "Checking 46 titles… 18 / 46"));
}

#[test]
fn chapter_filter_removes_hidden_rows_from_the_layout_and_explains_empty_results() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let row = |title: &str, read: bool| ItemRow {
        title: title.into(),
        read,
        ..Default::default()
    };
    ui.set_items(ModelRc::new(VecModel::from(vec![
        row("One", true),
        row("Two", true),
        row("Three", false),
    ])));
    ui.set_series_title("Series".into());
    ui.set_section(5);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 1600.));
    settle();
    let top = |title: &str| button(&ui, title).absolute_position().y;
    let first_row = top("One");
    let unfiltered = top("Three");
    ui.set_detail_unread_count(1);
    ui.set_chapter_filter(1);
    settle();
    assert!(
        (top("Three") - first_row).abs() < 1.,
        "filtered-out rows leave no spacing behind ({unfiltered} -> {})",
        top("Three")
    );
    assert!(!has_label(&ui, "No unread chapters"));
    ui.set_items(ModelRc::new(VecModel::from(vec![row("One", true)])));
    ui.set_detail_unread_count(0);
    settle();
    assert!(has_label(&ui, "No unread chapters"));
}
