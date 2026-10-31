use std::{cell::Cell, rc::Rc};

use i_slint_backend_testing::ElementHandle;
use nova_reader::{AppWindow, ItemRow};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};

fn tap(ui: &AppWindow, label: &str) {
    let element = ElementHandle::find_by_accessible_label(ui, label)
        .next()
        .unwrap_or_else(|| panic!("missing action: {label}"));
    element.mock_single_click(PointerEventButton::Left);
}

fn assert_controls_fit(ui: &AppWindow, width: f32) {
    for kind in ["ActionButton", "NavigationItem", "ComboBox", "LineEdit"] {
        for element in ElementHandle::find_by_element_type_name(ui, kind) {
            let position = element.absolute_position();
            let size = element.size();
            assert!(
                position.x >= -0.5 && position.x + size.width <= width + 0.5,
                "{kind} {:?} overflows {width}px: {position:?} {size:?}",
                element.accessible_label()
            );
            assert!(
                size.height >= 48.,
                "touch target is too short: {kind} {size:?}"
            );
        }
    }
}

#[test]
fn android_portrait_landscape_and_desktop_shells() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.on_navigate({
        let weak = ui.as_weak();
        move |section| weak.unwrap().set_section(section)
    });
    let chapter_step = Rc::new(Cell::new(0));
    ui.on_chapter_step({
        let step = chapter_step.clone();
        move |value| step.set(value)
    });
    ui.set_desktop(false);
    ui.set_sources(ModelRc::new(VecModel::from(vec![
        "A very long installed source name that must never widen the window".into(),
    ])));
    ui.set_categories(ModelRc::new(VecModel::from(vec!["All categories".into()])));
    ui.set_items(ModelRc::new(VecModel::from(vec![ItemRow {
        title: "A long series or chapter title that should fit in a phone list".into(),
        subtitle: "A long subtitle with source and chapter information".into(),
        badge: "Unread".into(),
        bookmarked: false,
    }])));
    ui.show().unwrap();
    for (width, height) in [
        (320., 640.),
        (390., 844.),
        (844., 390.),
        (640., 280.),
        (1024., 768.),
    ] {
        ui.window().set_size(LogicalSize::new(width, height));
        assert!(
            ui.get_mobile_ui(),
            "Android must keep its mobile shell at {width}px"
        );
        assert_eq!(ui.get_navigation_width(), 0.);
        for section in [0, 1, 2, 3, 4, 5, 7, 8] {
            ui.set_section(section);
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
            assert_eq!(ui.get_bottom_navigation_height(), 68.);
            assert_controls_fit(&ui, width);
            let tab = ElementHandle::find_by_accessible_label(&ui, "Library")
                .next()
                .unwrap();
            let pos = tab.absolute_position();
            assert!(pos.y >= height - 68. && pos.y + tab.size().height <= height);
        }
        ElementHandle::find_by_accessible_label(&ui, "Browse")
            .next()
            .unwrap()
            .invoke_accessible_default_action();
        assert_eq!(ui.get_section(), 3);
        tap(&ui, "More");
        assert_eq!(ui.get_section(), 4);
        tap(&ui, "Downloads");
        assert_eq!(ui.get_section(), 7);
        tap(&ui, "Back");
        assert_eq!(ui.get_section(), 4);
        for origin in [0, 1, 2, 3, 7] {
            ui.set_detail_origin(origin);
            ui.set_section(5);
            tap(&ui, "Back");
            assert_eq!(ui.get_section(), origin);
        }

        ui.set_section(6);
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
        ui.set_can_previous(true);
        ui.set_can_next(true);
        assert_eq!(ui.get_bottom_navigation_height(), 0.);
        let reader = ElementHandle::find_by_element_id(&ui, "AppWindow::reader")
            .next()
            .unwrap();
        assert!(
            reader.size().height >= 48.,
            "reader viewport too short: {:?}",
            reader.size()
        );
        assert_controls_fit(&ui, width);
        ElementHandle::find_by_accessible_label(&ui, "Options")
            .next()
            .unwrap()
            .invoke_accessible_default_action();
        assert_controls_fit(&ui, width);
        assert!(
            reader.size().height >= 48.,
            "options must leave room for reading"
        );
        ui.set_can_previous(false);
        chapter_step.set(0);
        ElementHandle::find_by_accessible_label(&ui, "Previous chapter")
            .next()
            .unwrap()
            .invoke_accessible_default_action();
        assert_eq!(
            chapter_step.get(),
            0,
            "disabled accessible actions must not fire"
        );
        ui.set_can_previous(true);
        tap(&ui, "Previous chapter");
        assert_eq!(chapter_step.get(), -1);
        // A focused action must still bubble the Android back key to the shell.
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        });
        assert_eq!(ui.get_section(), 5);
    }
    ui.set_section(0);
    ui.window().set_size(LogicalSize::new(390., 720.));
    ui.set_items(ModelRc::new(VecModel::from(vec![ItemRow::default(); 30])));
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    let scroll = ElementHandle::find_by_element_id(&ui, "AppWindow::page-scroll")
        .next()
        .unwrap();
    let heading = ElementHandle::find_by_element_id(&ui, "AppWindow::page-heading")
        .next()
        .unwrap();
    let heading_y = || heading.absolute_position().y;
    scroll.scroll(0., -1000.);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(100));
    assert!(heading_y() < 0., "fixture should scroll down");
    ui.set_items(ModelRc::new(VecModel::from(vec![ItemRow::default(); 40])));
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    assert!(heading_y() < 0., "same-page loading must preserve scroll");
    ui.set_section(2);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    assert!(heading_y() >= 0., "new section must start at its header");

    ui.set_desktop(true);
    ui.set_section(0);
    ui.window().set_size(LogicalSize::new(1120., 780.));
    assert!(!ui.get_mobile_ui());
    assert_eq!(ui.get_navigation_width(), 216.);
    assert_eq!(ui.get_bottom_navigation_height(), 0.);
    assert_controls_fit(&ui, 1120.);
    ui.window().set_size(LogicalSize::new(390., 720.));
    assert!(ui.get_mobile_ui());
    ui.hide().unwrap();
}
