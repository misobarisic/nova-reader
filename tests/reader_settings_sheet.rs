//! The redesigned reader settings sheet: half height on phones, the Layout,
//! Display and Color tabs' visual controls, scope labels, and the same form
//! under More → Settings. Scenarios share the testing backend's process-wide
//! mock clock, so they run in order from one test.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{Anim, AppWindow, ReaderSettings};
use slint::{ComponentHandle, LogicalSize};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..30 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn find(ui: &AppWindow, label: &str, role: AccessibleRole) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label).find(|e| e.accessible_role() == Some(role))
}

fn get(ui: &AppWindow, label: &str, role: AccessibleRole) -> ElementHandle {
    find(ui, label, role).unwrap_or_else(|| panic!("missing {role:?} {label}"))
}

fn count(ui: &AppWindow, label: &str) -> usize {
    ElementHandle::find_by_accessible_label(ui, label).count()
}

type Log = Rc<RefCell<Vec<(String, i32)>>>;

/// Settings edits are logged and applied, as the backend would.
fn record(ui: &AppWindow) -> Log {
    let log: Log = Rc::default();
    ui.global::<ReaderSettings>().on_settings_changed({
        let log = log.clone();
        let weak = ui.as_weak();
        move |key, value| {
            log.borrow_mut().push((key.to_string(), value));
            let ui = weak.unwrap();
            let s = ui.global::<ReaderSettings>();
            match key.as_str() {
                "paged_rotate_wide" => s.set_paged_rotate_wide(value != 0),
                "paged_tap_zones" => s.set_paged_tap_zones(value),
                "webtoon_tap_zones" => s.set_webtoon_tap_zones(value),
                "filter_color" => s.set_filter_color(value),
                "filter_intensity" => s.set_filter_intensity(value),
                "color_filter" => s.set_color_filter(value != 0),
                _ => {}
            }
        }
    });
    log
}

fn last(log: &Log) -> (String, i32) {
    log.borrow().last().cloned().unwrap_or_default()
}

fn open_reader_sheet(ui: &AppWindow) {
    ui.set_reader_osd(true);
    settle();
    get(ui, "Settings", AccessibleRole::Button).invoke_accessible_default_action();
    settle();
}

fn panel(ui: &AppWindow) -> ElementHandle {
    ElementHandle::find_by_element_id(ui, "ReaderSettingsPanel::panel")
        .next()
        .expect("settings panel")
}

fn half_height_and_handle() {
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    ui.set_series_title("Moonlit Cartographer".into());
    ui.set_reader_chapter_title("Chapter 7".into());
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();
    open_reader_sheet(&ui);
    // Opens at about 56% of the safe height (at least 420px), below full height.
    let resting = panel(&ui).size().height;
    assert!(
        (resting - 844. * 0.56).abs() < 1.,
        "resting height {resting}"
    );
    assert!(
        count(&ui, "Moonlit Cartographer · Chapter 7") > 0,
        "subtitle"
    );
    get(&ui, "Expand reader settings", AccessibleRole::Button).invoke_accessible_default_action();
    settle();
    let full = panel(&ui).size().height;
    assert!((full - 680.).abs() < 1., "full height {full}");
    get(&ui, "Collapse reader settings", AccessibleRole::Button).invoke_accessible_default_action();
    settle();
    assert!((panel(&ui).size().height - resting).abs() < 1.);
    // The reader viewport underneath does not change size.
    let reader = ElementHandle::find_by_element_id(&ui, "AppWindow::reader")
        .next()
        .unwrap();
    assert!((reader.size().height - 844.).abs() < 1.);

    // With animations off the height changes at once.
    ui.global::<Anim>().set_enabled(false);
    get(&ui, "Expand reader settings", AccessibleRole::Button).invoke_accessible_default_action();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
    assert!((panel(&ui).size().height - 680.).abs() < 1.);
    ui.global::<Anim>().set_enabled(true);

    // Reopening starts at half height again.
    get(&ui, "Close reader settings", AccessibleRole::Button).invoke_accessible_default_action();
    settle();
    open_reader_sheet(&ui);
    assert!((panel(&ui).size().height - resting).abs() < 1.);

    // Wide windows keep the anchored full-height panel, with no handle.
    ui.window().set_size(LogicalSize::new(1280., 900.));
    settle();
    assert!(find(&ui, "Expand reader settings", AccessibleRole::Button).is_none());
}

/// Scroll the settings form down until `label` is on screen: clipped
/// elements are not searchable.
fn reach(ui: &AppWindow, label: &str, role: AccessibleRole) -> ElementHandle {
    for _ in 0..40 {
        if let Some(found) = find(ui, label, role) {
            return found;
        }
        if let Some(scroll) =
            ElementHandle::find_by_element_id(ui, "ReaderSettingsForm::settings-scroll").next()
        {
            scroll.scroll(0., -60.);
        }
        settle();
    }
    panic!("unreachable {role:?} {label}")
}

fn layout_tab_controls() {
    // In the reader the Layout tab applies to this series and ends with Reset.
    let ui = AppWindow::new().unwrap();
    let log = record(&ui);
    ui.set_section(6);
    ui.set_reading_mode(1);
    ui.global::<ReaderSettings>()
        .set_series_settings_key("series".into());
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();
    open_reader_sheet(&ui);
    assert!(count(&ui, "This series") >= 1);
    reach(&ui, "Reset layout and orientation", AccessibleRole::Button)
        .invoke_accessible_default_action();
    settle();
    assert_eq!(last(&log), ("reset_series".into(), 1));

    // The same form under More → Settings, tall enough to show every control.
    let ui = AppWindow::new().unwrap();
    let log = record(&ui);
    ui.set_section(19);
    ui.show().unwrap();
    // Still the phone layout, but wide enough that no diagram is scrolled away.
    ui.window().set_size(LogicalSize::new(600., 2400.));
    settle();
    // Tap zone diagrams send the paged keys in Paged, webtoon keys in Webtoon.
    get(&ui, "Kindle", AccessibleRole::RadioButton).invoke_accessible_default_action();
    settle();
    assert_eq!(last(&log), ("paged_tap_zones".into(), 2));
    assert_eq!(
        get(&ui, "Kindle", AccessibleRole::RadioButton).accessible_checked(),
        Some(true)
    );
    // Invert is disabled while tap zones are Disabled.
    get(&ui, "Disabled", AccessibleRole::RadioButton).invoke_accessible_default_action();
    settle();
    assert_eq!(
        get(&ui, "Both", AccessibleRole::Button).accessible_enabled(),
        Some(false)
    );
    get(&ui, "Edge", AccessibleRole::RadioButton).invoke_accessible_default_action();
    settle();
    assert_eq!(
        get(&ui, "Both", AccessibleRole::Button).accessible_enabled(),
        Some(true)
    );
    get(&ui, "Both", AccessibleRole::Button).invoke_accessible_default_action();
    settle();
    assert_eq!(last(&log), ("paged_invert_taps".into(), 3));
    get(&ui, "Webtoon", AccessibleRole::Button).invoke_accessible_default_action();
    settle();
    get(&ui, "Kindle", AccessibleRole::RadioButton).invoke_accessible_default_action();
    settle();
    assert_eq!(last(&log), ("webtoon_tap_zones".into(), 2));
    get(&ui, "Paged", AccessibleRole::Button).invoke_accessible_default_action();
    settle();
    // Reverse rotation shows only while wide pages rotate.
    assert_eq!(count(&ui, "Reverse rotation"), 0);
    get(&ui, "Rotate wide pages", AccessibleRole::Checkbox).invoke_accessible_default_action();
    settle();
    assert!(count(&ui, "Reverse rotation") > 0);
}

fn color_presets() {
    let ui = AppWindow::new().unwrap();
    let log = record(&ui);
    ui.set_section(19);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 2400.));
    settle();
    get(&ui, "Color", AccessibleRole::Tab).invoke_accessible_default_action();
    settle();
    // Everything below the toggle waits for the filter.
    assert_eq!(
        get(&ui, "Sepia", AccessibleRole::RadioButton).accessible_enabled(),
        Some(false)
    );
    get(&ui, "Color filter", AccessibleRole::Checkbox).invoke_accessible_default_action();
    settle();
    get(&ui, "Sepia", AccessibleRole::RadioButton).invoke_accessible_default_action();
    settle();
    let edits = log.borrow().clone();
    assert!(edits.contains(&("filter_color".into(), (230 << 16) | (190 << 8) | 130)));
    assert!(edits.contains(&("filter_intensity".into(), 35)));
    assert_eq!(
        get(&ui, "Sepia", AccessibleRole::RadioButton).accessible_checked(),
        Some(true)
    );
    assert_eq!(count(&ui, "Red"), 0, "channels hide for a preset");
    // A colour that matches no preset is Custom and shows its channels.
    ui.global::<ReaderSettings>().set_filter_color(0x123456);
    settle();
    assert_eq!(
        get(&ui, "Custom", AccessibleRole::RadioButton).accessible_checked(),
        Some(true)
    );
    get(&ui, "Red", AccessibleRole::Slider).invoke_accessible_increment_action();
    settle();
    assert_eq!(last(&log), ("filter_red".into(), 0x13));
    // Grayscale and Invert are switches with the same keys.
    let gray = get(&ui, "Grayscale", AccessibleRole::Switch);
    gray.invoke_accessible_default_action();
    settle();
    assert_eq!(last(&log), ("grayscale".into(), 1));
}

fn settings_page_variant_and_layout() {
    for width in [320., 390., 1280., 1920.] {
        let ui = AppWindow::new().unwrap();
        ui.set_section(19);
        ui.show().unwrap();
        ui.window().set_size(LogicalSize::new(width, 2400.));
        settle();
        // Under More → Settings everything applies to all series, the series
        // default row is absent and there is no half-height handle.
        assert_eq!(count(&ui, "This series"), 0);
        assert!(count(&ui, "All series") >= 2);
        assert!(find(&ui, "Use default", AccessibleRole::RadioButton).is_none());
        assert!(find(&ui, "Expand reader settings", AccessibleRole::Button).is_none());
        for tab in ["Layout", "Display", "Color"] {
            get(&ui, tab, AccessibleRole::Tab).invoke_accessible_default_action();
            settle();
            for kind in [
                "ChoiceTile",
                "ZoneDiagram",
                "MiniSlider",
                "ToggleSwitch",
                "PillButton",
            ] {
                for e in ElementHandle::find_by_element_type_name(&ui, kind) {
                    let at = e.absolute_position();
                    let size = e.size();
                    assert!(
                        at.x >= -0.5 && at.x + size.width <= width + 0.5,
                        "{tab} {kind} {:?} overflows {width}px",
                        e.accessible_label()
                    );
                    if kind != "MiniSlider" {
                        assert!(
                            size.height >= 44.,
                            "{tab} {kind} {:?} too short",
                            e.accessible_label()
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn reader_settings_sheet() {
    i_slint_backend_testing::init_no_event_loop();
    half_height_and_handle();
    layout_tab_controls();
    color_presets();
    settings_page_variant_and_layout();
}
