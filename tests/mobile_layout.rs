use std::{cell::Cell, rc::Rc, time::Duration};

use i_slint_backend_testing::ElementHandle;
use nova_reader::{Anim, AppWindow, ItemRow, SourceRow, Theme};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};

fn settle() {
    // Allow deferred focus/restore publication and transition timers to run
    // across frames rather than advancing the clock in one jump.
    for _ in 0..24 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn action(ui: &AppWindow, label: &str) -> ElementHandle {
    for attempt in 0..32 {
        let mut direction = if label == "Back" || attempt >= 16 {
            100.
        } else {
            -100.
        };
        if let Some(action) = ElementHandle::find_by_accessible_label(ui, label).next() {
            let center = action.absolute_position().y + action.size().height / 2.;
            let height = ui.window().size().height as f32 / ui.window().scale_factor();
            let navigation = ["Library", "Updates", "History", "Browse", "More"].contains(&label);
            let bottom = if navigation {
                height
            } else {
                height - ui.get_bottom_navigation_height()
            };
            if center >= 0. && center < bottom {
                return action;
            }
            if center < 0. {
                direction = 100.;
            }
        }
        let id = if ui.get_section() == 4 {
            "MorePage::landing-scroll"
        } else if [11, 12, 14, 15, 16].contains(&ui.get_section()) {
            "MorePage::detail-scroll"
        } else if ui.get_section() >= 9 {
            "MorePage::child-scroll"
        } else {
            "AppWindow::page-scroll"
        };
        let Some(scroll) = ElementHandle::find_by_element_id(ui, id).next() else {
            break;
        };
        scroll.scroll(0., direction);
        settle();
    }
    panic!("missing action: {label}, section {}", ui.get_section());
}

fn tap(ui: &AppWindow, label: &str) {
    action(ui, label).mock_single_click(PointerEventButton::Left);
    settle();
}

fn back(ui: &AppWindow) {
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Back.into(),
    });
    settle();
}

fn assert_controls_fit(ui: &AppWindow, width: f32) {
    for kind in [
        "ActionButton",
        "IconAction",
        "NavigationItem",
        "BottomItem",
        "ReaderSelect",
        "ReaderField",
        "MenuLink",
        "BackButton",
    ] {
        for element in ElementHandle::find_by_element_type_name(ui, kind) {
            let position = element.absolute_position();
            let size = element.size();
            assert!(
                position.x >= -0.5 && position.x + size.width <= width + 0.5,
                "{kind} {:?} overflows {width}px: {position:?} {size:?}",
                element.accessible_label()
            );
            assert!(
                size.height >= 44.,
                "Nova touch target is too short: {kind} {size:?}"
            );
        }
    }
}

#[test]
fn nova_navigation_more_and_responsive_controls() {
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
    ui.set_sources(ModelRc::new(VecModel::from(vec![
        "A very long installed source name that must never widen the window".into(),
    ])));
    ui.set_categories(ModelRc::new(VecModel::from(vec!["All".into()])));
    ui.set_items(ModelRc::new(VecModel::from(vec![ItemRow {
        title: "A long series or chapter title that should fit in a phone list".into(),
        subtitle: "A long subtitle with source and chapter information".into(),
        badge: "Unread".into(),
        bookmarked: false,
        ..ItemRow::default()
    }])));
    ui.show().unwrap();
    for (width, height) in [
        (320., 640.),
        (390., 844.),
        (699., 720.),
        (700., 720.),
        (844., 390.),
        (640., 280.),
        (1024., 768.),
    ] {
        ui.window().set_size(LogicalSize::new(width, height));
        let narrow = width < 700.;
        assert_eq!(
            ui.get_mobile_ui(),
            narrow,
            "use Nova's breakpoint on Android too"
        );
        for section in [0, 1, 2, 3, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16] {
            ui.set_section(section);
            settle();
            let bottom = narrow && [0, 1, 2, 3, 4].contains(&section);
            assert_eq!(
                ui.get_bottom_navigation_height(),
                if bottom { 68. } else { 0. }
            );
            assert_eq!(
                ui.get_navigation_width(),
                if narrow || [5, 6].contains(&section) {
                    0.
                } else {
                    64.
                }
            );
            assert_controls_fit(&ui, width);
            let tabs =
                ElementHandle::find_by_element_type_name(&ui, "BottomItem").collect::<Vec<_>>();
            assert_eq!(tabs.len(), if bottom { 5 } else { 0 });
            if bottom {
                let labels = tabs
                    .iter()
                    .map(|tab| tab.accessible_label().unwrap().to_string())
                    .collect::<Vec<_>>();
                assert_eq!(labels, ["Library", "Updates", "History", "Browse", "More"]);
            }
            for tab in tabs {
                let pos = tab.absolute_position();
                assert!(pos.y >= height - 68. && pos.y + tab.size().height <= height);
            }
        }
        ui.set_section(0);
        settle();
        action(&ui, "Browse").invoke_accessible_default_action();
        settle();
        assert_eq!(ui.get_section(), 3);
        tap(&ui, "History");
        assert_eq!(ui.get_section(), 2);
        assert_eq!(
            ui.get_bottom_navigation_height(),
            if narrow { 68. } else { 0. }
        );
        assert_eq!(
            action(&ui, "History").accessible_item_selected(),
            Some(true)
        );
        back(&ui);
        assert_eq!(ui.get_section(), 0);
        tap(&ui, "More");
        assert_eq!(ui.get_section(), 4);
        if narrow {
            tap(&ui, "Downloads");
            assert_eq!(ui.get_section(), 7);
            assert_eq!(ui.get_bottom_navigation_height(), 0.);
            tap(&ui, "Back");
            assert_eq!(ui.get_section(), 4);
        }
        // More uses Nova's settings cards and pushes child pages while retaining
        // the wide rail. The narrow bottom bar belongs to the landing only.
        tap(&ui, "Settings");
        assert_eq!(ui.get_section(), 10);
        assert_eq!(ui.get_bottom_navigation_height(), 0.);
        tap(&ui, "Categories");
        assert_eq!(ui.get_section(), 11);
        tap(&ui, "Back");
        assert_eq!(ui.get_section(), 10);
        tap(&ui, "Theme");
        assert_eq!(ui.get_section(), 14);
        tap(&ui, "Back");
        assert_eq!(ui.get_section(), 10);
        tap(&ui, "Look and feel");
        assert_eq!(ui.get_section(), 15);
        tap(&ui, "Back");
        assert_eq!(ui.get_section(), 10);
        tap(&ui, "Back");
        assert_eq!(ui.get_section(), 4);
        for origin in [0, 1, 2, 3, 7] {
            ui.set_detail_origin(origin);
            ui.set_section(5);
            settle();
            tap(&ui, "Back");
            assert_eq!(ui.get_section(), origin);
        }

        ui.set_section(6);
        settle();
        ui.set_can_previous(true);
        ui.set_can_next(true);
        assert_eq!(ui.get_navigation_width(), 0.);
        assert_eq!(ui.get_bottom_navigation_height(), 0.);
        let reader = ElementHandle::find_by_element_id(&ui, "AppWindow::reader")
            .next()
            .unwrap();
        assert!(reader.size().height >= 48.);
        assert_controls_fit(&ui, width);
        assert!(!ui.get_reader_osd(), "chapters open with controls hidden");
        action(&ui, "Toggle reader controls").invoke_accessible_default_action();
        settle();
        action(&ui, "Options").invoke_accessible_default_action();
        settle();
        assert_controls_fit(&ui, width);
        assert!(
            reader.size().height >= 48.,
            "options must leave room for reading"
        );
        ui.set_can_previous(false);
        chapter_step.set(0);
        action(&ui, "Previous chapter").invoke_accessible_default_action();
        assert_eq!(
            chapter_step.get(),
            0,
            "disabled accessible actions must not fire"
        );
        ui.set_can_previous(true);
        tap(&ui, "Previous chapter");
        assert_eq!(chapter_step.get(), -1);
        back(&ui);
        assert_eq!(ui.get_section(), 5);
    }

    // Kit dropdown visuals retain native keyboard and accessible selection on
    // desktop as well as drag scrolling for long source lists on phones.
    ui.window().set_size(LogicalSize::new(390., 720.));
    ui.set_section(3);
    ui.set_sources(ModelRc::new(VecModel::from(
        (1..=20)
            .map(|i| format!("Source {i}").into())
            .collect::<Vec<_>>(),
    )));
    ui.set_source_rows(ModelRc::new(VecModel::from(
        (0..20)
            .map(|i| SourceRow {
                id: format!("source-{i}").into(),
                name: format!("Source {}", i + 1).into(),
                language: "English".into(),
                group: "English".into(),
                enabled: true,
                latest: true,
                index: i,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_source_index(0);
    settle();
    tap(&ui, "Source 1");
    tap(&ui, "Source options");
    let selector = ElementHandle::find_by_element_type_name(&ui, "ReaderSelect")
        .next()
        .unwrap();
    selector.invoke_accessible_default_action();
    settle();
    for _ in 0..9 {
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: slint::platform::Key::DownArrow.into(),
        });
    }
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Return.into(),
    });
    settle();
    assert_eq!(ui.get_source_index(), 9);
    // Popup selection restores closed-box focus, so Enter opens it again.
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Return.into(),
    });
    settle();
    action(&ui, "Source 10").invoke_accessible_default_action();
    settle();
    assert_eq!(ui.get_source_index(), 9);
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Return.into(),
    });
    settle();
    back(&ui);

    assert_eq!(
        ui.get_section(),
        3,
        "popup Back must not leave the source page"
    );

    selector.invoke_accessible_default_action();
    settle();
    let popup_scroll = ElementHandle::find_by_element_id(&ui, "Dropdown::popup_scroll")
        .next()
        .unwrap();
    for _ in 0..5 {
        let pos = popup_scroll.absolute_position();
        let size = popup_scroll.size();
        popup_scroll.mock_drag(
            slint::LogicalPosition::new(pos.x + size.width / 2., pos.y + size.height / 2. - 140.),
            PointerEventButton::Left,
        );
        settle();
    }
    action(&ui, "Source 20").invoke_accessible_default_action();
    settle();
    assert_eq!(
        ui.get_source_index(),
        19,
        "long dropdowns must scroll by touch"
    );
    back(&ui);

    // A retained More scroller restores its landing position on child return.
    ui.window().set_size(LogicalSize::new(390., 400.));
    tap(&ui, "More");
    let scroll = ElementHandle::find_by_element_id(&ui, "MorePage::landing-scroll")
        .next()
        .unwrap();
    let local = action(&ui, "Local chapters");
    let original_y = local.absolute_position().y;
    scroll.scroll(0., -180.);
    settle();
    let scrolled_y = local.absolute_position().y;
    assert!(scrolled_y < original_y);
    ui.set_section(10);
    settle();
    back(&ui);
    assert_eq!(ui.get_section(), 4);
    assert!(
        (local.absolute_position().y - scrolled_y).abs() < 1.,
        "More must retain its scroll: {scrolled_y} -> {} (viewport {:?})",
        local.absolute_position().y,
        scroll.size()
    );

    // Pop restores the More keyboard host after the landing is interactive.
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::DownArrow.into(),
    });
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Return.into(),
    });
    settle();
    assert_eq!(
        ui.get_section(),
        13,
        "More keyboard navigation must work after child return"
    );
    back(&ui);

    // A nested push retains the Settings landing and paints both layers in
    // motion. Back reverses that motion, including a partially entered leaf.
    ui.window().set_size(LogicalSize::new(390., 720.));
    ui.set_section(10);
    settle();
    ui.set_section(14);
    for _ in 0..5 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
    let detail = ElementHandle::find_by_element_id(&ui, "MorePage::detail")
        .next()
        .unwrap();
    let landing = ElementHandle::find_by_element_id(&ui, "MorePage::child")
        .next()
        .unwrap();
    assert!(detail.absolute_position().x > 0. && detail.absolute_position().x < 390.);
    assert!(landing.absolute_position().x < 0. && landing.absolute_position().x > -110.);
    ui.invoke_go_back();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    assert!(
        ElementHandle::find_by_element_id(&ui, "MorePage::detail")
            .next()
            .is_some(),
        "outgoing leaf must survive Back"
    );
    settle();
    assert_eq!(ui.get_section(), 10);
    assert!(
        ElementHandle::find_by_element_id(&ui, "MorePage::detail")
            .next()
            .is_none()
    );
    assert!(landing.absolute_position().x.abs() < 1.);

    ui.set_section(14);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(32));
    ui.global::<Anim>().set_enabled(false);
    assert!(
        ElementHandle::find_by_element_id(&ui, "MorePage::detail")
            .next()
            .unwrap()
            .absolute_position()
            .x
            .abs()
            < 1.
    );
    // Publish the off event before a second user event re-enables motion.
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
    // Re-enable before the old animation's duration has elapsed. It must not
    // expose a stale intermediate position that was merely painted over.
    ui.global::<Anim>().set_enabled(true);
    assert!(
        ElementHandle::find_by_element_id(&ui, "MorePage::detail")
            .next()
            .unwrap()
            .absolute_position()
            .x
            .abs()
            < 1.
    );
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    assert!(
        ElementHandle::find_by_element_id(&ui, "MorePage::detail")
            .next()
            .unwrap()
            .absolute_position()
            .x
            .abs()
            < 1.
    );
    ui.global::<Anim>().set_enabled(false);
    ui.invoke_go_back();
    assert!(
        ElementHandle::find_by_element_id(&ui, "MorePage::detail")
            .next()
            .is_none()
    );
    assert!(landing.absolute_position().x.abs() < 1.);
    settle();
    ui.global::<Anim>().set_enabled(true);

    // Settings has its own retained offset, independent of More's scroller.
    ui.window().set_size(LogicalSize::new(390., 300.));
    settle();
    let settings_scroll = ElementHandle::find_by_element_id(&ui, "MorePage::child-scroll")
        .next()
        .unwrap();
    let data = action(&ui, "Data");
    settings_scroll.scroll(0., -180.);
    settle();
    let settings_y = data.absolute_position().y;
    ui.set_section(14);
    settle();
    back(&ui);
    assert!((data.absolute_position().y - settings_y).abs() < 1.);

    // Both Nova bottom-bar modes preserve tab placement and usable content.
    ui.window().set_size(LogicalSize::new(390., 720.));
    ui.set_section(0);
    for transparent in [true, false] {
        ui.global::<Theme>().set_transparent_navbar(transparent);
        settle();
        for tab in ElementHandle::find_by_element_type_name(&ui, "BottomItem") {
            assert!(tab.absolute_position().y >= 720. - 68.);
            assert!(tab.absolute_position().y + tab.size().height <= 720.);
        }
    }
    ui.global::<Theme>().set_transparent_navbar(true);

    // Ordinary page switches reset scroll; loading more rows on the same page does not.
    ui.window().set_size(LogicalSize::new(390., 720.));
    ui.set_section(0);
    ui.set_items(ModelRc::new(VecModel::from(vec![ItemRow::default(); 30])));
    settle();
    let scroll = ElementHandle::find_by_element_id(&ui, "AppWindow::page-scroll")
        .next()
        .unwrap();
    let heading = ElementHandle::find_by_element_id(&ui, "AppWindow::page-heading")
        .next()
        .unwrap();
    scroll.scroll(0., -1000.);
    settle();
    assert!(heading.absolute_position().y < 0.);
    ui.set_items(ModelRc::new(VecModel::from(vec![ItemRow::default(); 40])));
    settle();
    assert!(
        heading.absolute_position().y < 0.,
        "same-page loading must preserve scroll"
    );
    ui.set_section(2);
    settle();
    assert!(
        heading.absolute_position().y >= 0.,
        "new section must start at its header"
    );

    ui.set_section(0);
    ui.window().set_size(LogicalSize::new(1120., 780.));
    settle();
    assert!(!ui.get_mobile_ui());
    assert_eq!(ui.get_navigation_width(), 64.);
    let marker = ElementHandle::find_by_element_id(&ui, "DesktopSidebar::marker-dot")
        .next()
        .unwrap();
    assert_eq!(marker.size(), LogicalSize::new(48., 48.));
    let first_y = marker.absolute_position().y;
    ui.set_section(1);
    settle();
    assert_eq!(marker.absolute_position().y - first_y, 56.);
    assert_controls_fit(&ui, 1120.);
    ui.window().set_size(LogicalSize::new(390., 720.));
    assert!(ui.get_mobile_ui());
    ui.hide().unwrap();
}
