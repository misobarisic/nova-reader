use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{AppWindow, EntryCategoryRow, ItemRow};
use reader_domain::SeriesId;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalSize, Model, ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

fn settle() {
    for _ in 0..40 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}
fn button(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing {label}"))
}
fn key(ui: &AppWindow, text: Key) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: text.into() });
    settle();
}
fn pointer(ui: &AppWindow, position: slint::LogicalPosition, pressed: bool) {
    ui.window().dispatch_event(if pressed {
        WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        }
    } else {
        WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        }
    });
}
#[test]
fn library_holds_and_right_clicks_open_the_title_menu_for_the_full_identity() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let keys = [
        serde_json::to_string(&SeriesId::new("source-a", "same")).unwrap(),
        serde_json::to_string(&SeriesId::new("source-b", "same")).unwrap(),
    ];
    ui.set_items(ModelRc::new(VecModel::from(
        keys.iter()
            .enumerate()
            .map(|(i, key)| ItemRow {
                title: format!("Book {i}").into(),
                series_key: key.clone().into(),
                bookmarked: true,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_categories(ModelRc::new(VecModel::from(vec![
        "All".into(),
        "Favorites".into(),
    ])));
    // What Rust publishes when the menu opens (`title-menu-requested`).
    ui.set_entry_categories(ModelRc::new(VecModel::from(vec![EntryCategoryRow {
        id: "favorite-id".into(),
        label: "Favorites".into(),
        checked: false,
    }])));
    let requested = Rc::new(RefCell::new(Vec::new()));
    ui.on_title_menu_requested({
        let requested = requested.clone();
        move |key| requested.borrow_mut().push(key.to_string())
    });
    let opens = Rc::new(Cell::new(0));
    ui.on_open_item({
        let opens = opens.clone();
        move |_| opens.set(opens.get() + 1)
    });
    let actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_entry_action({
        let actions = actions.clone();
        let weak = ui.as_weak();
        move |key, action| {
            actions
                .borrow_mut()
                .push((key.to_string(), action.to_string()));
            if action == "category:favorite-id" {
                let ui = weak.unwrap();
                let rows = ui
                    .get_entry_categories()
                    .iter()
                    .map(|mut row| {
                        row.checked = !row.checked;
                        row
                    })
                    .collect::<Vec<_>>();
                ui.set_entry_categories(ModelRc::new(VecModel::from(rows)));
            }
        }
    });
    ui.show().unwrap();
    for (width, height) in [(390., 844.), (1120., 780.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        ui.set_section(0);
        settle();
        button(&ui, "Book 0").mock_single_click(PointerEventButton::Right);
        settle();
        assert!(ui.get_title_menu_open());
        assert!(
            !ui.get_entry_menu_visible(),
            "the title menu replaces the old sheet"
        );
        assert_eq!(requested.borrow().last().unwrap(), &keys[0]);
        assert_eq!(opens.get(), 0);
        assert!(button(&ui, "Mark all read").size().height > 0.);
        // Categories is a page of the menu; ticks keep it open.
        button(&ui, "Categories").invoke_accessible_default_action();
        settle();
        let category = || {
            ElementHandle::find_by_accessible_label(&ui, "Favorites")
                .find(|e| e.accessible_checked().is_some())
                .unwrap()
        };
        category().invoke_accessible_default_action();
        settle();
        assert!(ui.get_title_menu_open());
        assert_eq!(
            actions.borrow().last().unwrap(),
            &(keys[0].clone(), "category:favorite-id".into())
        );
        assert_eq!(category().accessible_checked(), Some(true));
        // A reorder while the menu is open cannot redirect the mutation.
        let mut rows = ui.get_items().iter().collect::<Vec<_>>();
        rows.reverse();
        ui.set_items(ModelRc::new(VecModel::from(rows)));
        category().invoke_accessible_default_action();
        assert_eq!(actions.borrow().last().unwrap().0, keys[0]);
        // Escape goes back to the main page, then closes.
        key(&ui, Key::Escape);
        assert!(ui.get_title_menu_open());
        assert!(button(&ui, "Mark all read").size().height > 0.);
        key(&ui, Key::Escape);
        settle();
        assert!(!ui.get_title_menu_open());
        // A hold opens it too.
        let card = button(&ui, "Book 1");
        let point = slint::LogicalPosition::new(
            card.absolute_position().x + 30.,
            card.absolute_position().y + 30.,
        );
        pointer(&ui, point, true);
        settle();
        pointer(&ui, point, false);
        settle();
        assert!(ui.get_title_menu_open());
        assert_eq!(opens.get(), 0);
        let before = actions.borrow().len();
        button(&ui, "Remove from library").invoke_accessible_default_action();
        settle();
        assert!(!ui.get_title_menu_open());
        // Removal asks first: nothing is dispatched until the sheet is confirmed.
        assert_eq!(actions.borrow().len(), before);
        button(&ui, "Cancel").invoke_accessible_default_action();
        settle();
        assert_eq!(actions.borrow().len(), before, "Cancel keeps the title");
        button(&ui, "Book 1").mock_single_click(PointerEventButton::Right);
        settle();
        button(&ui, "Remove from library").invoke_accessible_default_action();
        settle();
        button(&ui, "Remove from library").invoke_accessible_default_action();
        settle();
        assert_eq!(
            actions.borrow().last().unwrap(),
            &(keys[1].clone(), "remove".into())
        );
        // Outside taps and Back close it without opening the title.
        button(&ui, "Book 1").mock_single_click(PointerEventButton::Right);
        settle();
        let outside = slint::LogicalPosition::new(2., height - 2.);
        pointer(&ui, outside, true);
        pointer(&ui, outside, false);
        settle();
        assert!(!ui.get_title_menu_open());
        button(&ui, "Book 1").mock_single_click(PointerEventButton::Right);
        settle();
        key(&ui, Key::Back);
        settle();
        assert!(!ui.get_title_menu_open());
        assert_eq!(opens.get(), 0);
        // A pan cancels the stationary-hold gesture and never opens a title.
        let card = button(&ui, "Book 1");
        let start = slint::LogicalPosition::new(
            card.absolute_position().x + 30.,
            card.absolute_position().y + 30.,
        );
        pointer(&ui, start, true);
        ui.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(start.x + 25., start.y),
        });
        settle();
        pointer(
            &ui,
            slint::LogicalPosition::new(start.x + 25., start.y),
            false,
        );
        settle();
        assert!(!ui.get_title_menu_open());
        assert_eq!(opens.get(), 0);
        ui.set_section(5);
        ui.set_detail_series_key(keys[1].clone().into());
        ui.set_series_title("Detail target".into());
        ui.set_in_library(true);
        settle();
        // The detail screen's ⋯ opens the same menu for its title.
        button(&ui, "Title options").mock_single_click(PointerEventButton::Left);
        settle();
        button(&ui, "Refresh").invoke_accessible_default_action();
        settle();
        assert_eq!(
            actions.borrow().last().unwrap(),
            &(keys[1].clone(), "refresh".into())
        );
        assert!(!ui.get_title_menu_open());
    }
    ui.set_section(3);
    ui.set_source_catalog_open(true);
    settle();
    button(&ui, "Book 0").mock_single_click(PointerEventButton::Right);
    settle();
    assert!(!ui.get_entry_menu_visible());
}

#[test]
fn title_menu_status_layout_and_long_category_pages() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let target = serde_json::to_string(&SeriesId::new("source", "series")).unwrap();
    ui.set_section(5);
    ui.set_detail_series_key(target.clone().into());
    ui.set_series_title("Many categories".into());
    ui.set_in_library(true);
    ui.set_title_menu_status(6);
    ui.set_entry_menu_presentation(2);
    ui.set_entry_categories(ModelRc::new(VecModel::from(
        (0..50)
            .map(|i| EntryCategoryRow {
                id: format!("category-{i}").into(),
                label: format!("Category {i}").into(),
                checked: false,
            })
            .collect::<Vec<_>>(),
    )));
    let actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_entry_action({
        let actions = actions.clone();
        move |key, action| {
            actions
                .borrow_mut()
                .push((key.to_string(), action.to_string()));
        }
    });
    ui.show().unwrap();
    for (width, height) in [(390., 640.), (1120., 640.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        settle();
        // Status: a page of choices; picking one closes the menu.
        for (label, code) in [
            ("Reading", 6),
            ("Plan to read", 5),
            ("Completed", 7),
            ("On hold", 1),
            ("Dropped", 2),
            ("Automatic", 0),
        ] {
            button(&ui, "Title options").invoke_accessible_default_action();
            settle();
            button(&ui, "Status").invoke_accessible_default_action();
            settle();
            assert!(ui.get_title_menu_open());
            button(&ui, label).invoke_accessible_default_action();
            settle();
            assert!(!ui.get_title_menu_open());
            assert_eq!(
                actions.borrow().last().unwrap(),
                &(target.clone(), format!("status:{code}"))
            );
        }
        // Layout.
        for (label, action) in [
            ("Default", "presentation-default"),
            ("Reader", "presentation-reader"),
            ("Gallery", "presentation-gallery"),
        ] {
            button(&ui, "Title options").invoke_accessible_default_action();
            settle();
            button(&ui, "Layout").invoke_accessible_default_action();
            settle();
            button(&ui, label).invoke_accessible_default_action();
            settle();
            assert_eq!(
                actions.borrow().last().unwrap(),
                &(target.clone(), action.to_string())
            );
        }
        // Fifty categories scroll inside the menu, which stays in the window.
        button(&ui, "Title options").invoke_accessible_default_action();
        settle();
        button(&ui, "Categories").invoke_accessible_default_action();
        settle();
        let panel = ElementHandle::find_by_accessible_label(&ui, "Title options")
            .find(|e| e.accessible_role() == Some(AccessibleRole::Groupbox))
            .unwrap();
        assert!(panel.absolute_position().y + panel.size().height <= height + 0.5);
        // Rows scroll within the page (the first six fit).
        ElementHandle::find_by_accessible_label(&ui, "Category 5")
            .next()
            .unwrap()
            .invoke_accessible_default_action();
        settle();
        assert_eq!(
            actions.borrow().last().unwrap(),
            &(target.clone(), "category:category-5".into())
        );
        assert!(ui.get_title_menu_open());
        key(&ui, Key::Escape);
        key(&ui, Key::Escape);
        settle();
        assert!(!ui.get_title_menu_open());
    }
}

#[test]
fn phone_sheet_rows_stay_index_aligned_with_the_desktop_actions() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let key = serde_json::to_string(&SeriesId::new("source-a", "sheet")).unwrap();
    let actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_entry_action({
        let actions = actions.clone();
        move |_, action| actions.borrow_mut().push(action.to_string())
    });
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();
    // The narrow MenuSheet keeps its own label list; every row must dispatch the
    // desktop action at the same position.
    for (label, expected) in [
        ("Change status", "change-status"),
        ("Categories", "categories"),
        ("Mark all read", "mark-all-read"),
        ("Mark all unread", "mark-all-unread"),
        ("Clear all chapter positions", "clear-all-positions"),
        ("Refresh details", "refresh"),
        ("Remove from library", "remove"),
    ] {
        ui.invoke_open_entry_menu(key.clone().into(), "Sheet title".into(), true, 30., 70.);
        settle();
        button(&ui, label).invoke_accessible_default_action();
        settle();
        if matches!(
            expected,
            "mark-all-read" | "mark-all-unread" | "clear-all-positions" | "remove"
        ) {
            // Destructive rows confirm through a sheet that repeats the label.
            assert_ne!(
                actions.borrow().last().map(String::as_str),
                Some(expected),
                "phone row {label} must ask first"
            );
            button(&ui, label).invoke_accessible_default_action();
            settle();
        }
        assert_eq!(
            actions.borrow().last().map(String::as_str),
            Some(expected),
            "phone row {label}"
        );
        ui.invoke_close_entry_menu();
        settle();
    }
}

#[test]
fn detail_series_and_chapter_menus_dispatch_separate_progress_actions() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let target = serde_json::to_string(&SeriesId::new("source-a", "shared")).unwrap();
    ui.set_section(5);
    ui.set_detail_series_key(target.clone().into());
    ui.set_series_title("Story".into());
    ui.set_in_library(true);
    ui.set_items(ModelRc::new(VecModel::from(vec![ItemRow {
        title: "Chapter 2".into(),
        local_import: true,
        // Clear progress is offered for a partly read chapter.
        reading_status: "Page 3".into(),
        ..Default::default()
    }])));
    let series_actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_entry_action({
        let actions = series_actions.clone();
        move |key, action| {
            actions
                .borrow_mut()
                .push((key.to_string(), action.to_string()))
        }
    });
    let chapter_actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_item_action({
        let actions = chapter_actions.clone();
        move |index, action| actions.borrow_mut().push((index, action.to_string()))
    });
    ui.show().unwrap();
    for width in [390., 1120.] {
        ui.window().set_size(LogicalSize::new(width, 1800.));
        settle();
        for (label, confirm, action) in [
            ("Mark all read", "Mark all read", "mark-all-read"),
            ("Mark all unread", "Mark all unread", "mark-all-unread"),
            (
                "Clear all positions",
                "Clear all chapter positions",
                "clear-all-positions",
            ),
        ] {
            button(&ui, "Title options").invoke_accessible_default_action();
            settle();
            button(&ui, label).invoke_accessible_default_action();
            settle();
            // The confirmation sheet names the action.
            button(&ui, confirm).invoke_accessible_default_action();
            settle();
            assert_eq!(
                series_actions.borrow().last(),
                Some(&(target.clone(), action.to_owned()))
            );
            assert!(!ui.get_entry_menu_visible());
        }
        button(&ui, "Chapter options").invoke_accessible_default_action();
        settle();
        button(&ui, "Clear progress").invoke_accessible_default_action();
        settle();
        assert_eq!(
            chapter_actions.borrow().last(),
            Some(&(0, "clear-progress".to_owned()))
        );
    }
}

#[test]
fn change_status_opens_a_second_menu_and_keeps_the_full_series_identity() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let key = serde_json::to_string(&SeriesId::new("source-a", "shared")).unwrap();
    let changes = Rc::new(RefCell::new(Vec::new()));
    ui.on_entry_action({
        let changes = changes.clone();
        move |id, action| {
            changes
                .borrow_mut()
                .push((id.to_string(), action.to_string()))
        }
    });
    ui.show().unwrap();
    for width in [390., 1120.] {
        ui.window().set_size(LogicalSize::new(width, 844.));
        settle();
        for (label, code) in [
            ("Mark as reading", 6),
            ("Mark as Plan to read", 5),
            ("Mark as completed", 7),
            ("Mark on hold", 1),
            ("Mark dropped", 2),
            ("Back to automatic", 0),
        ] {
            ui.invoke_open_entry_menu(key.clone().into(), "Shared title".into(), true, 30., 70.);
            settle();
            button(&ui, "Change status").invoke_accessible_default_action();
            settle();
            assert!(ui.get_entry_menu_visible());
            button(&ui, label).invoke_accessible_default_action();
            settle();
            assert!(!ui.get_entry_menu_visible());
            assert_eq!(
                changes.borrow().last().unwrap(),
                &(key.clone(), format!("status:{code}"))
            );
        }
    }
}
