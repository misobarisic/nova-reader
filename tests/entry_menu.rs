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
fn per_title_menu_gestures_and_category_editor_keep_the_full_identity() {
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
            let ui = weak.unwrap();
            assert!(
                ui.get_entry_menu_visible(),
                "dispatch before conditional dismissal"
            );
            actions
                .borrow_mut()
                .push((key.to_string(), action.to_string()));
            if action == "categories" {
                ui.set_entry_categories(ModelRc::new(VecModel::from(vec![EntryCategoryRow {
                    id: "favorite-id".into(),
                    label: "Favorites".into(),
                    checked: false,
                }])));
            }
            if action == "category:favorite-id" {
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
        assert!(
            ElementHandle::find_by_accessible_label(&ui, "Library options")
                .next()
                .is_none()
        );
        button(&ui, "Book 0").mock_single_click(PointerEventButton::Right);
        settle();
        assert!(ui.get_entry_menu_visible());
        assert_eq!(opens.get(), 0);
        assert!(button(&ui, "Mark all read").size().height > 0.);
        assert!(button(&ui, "Mark all unread").size().height > 0.);
        assert!(button(&ui, "Clear all chapter positions").size().height > 0.);
        button(&ui, "Categories").invoke_accessible_default_action();
        settle();
        let category = ElementHandle::find_by_accessible_label(&ui, "Favorites")
            .find(|e| e.accessible_checked().is_some())
            .unwrap();
        category.invoke_accessible_default_action();
        settle();
        assert!(ui.get_entry_menu_visible());
        assert_eq!(
            actions.borrow().last().unwrap(),
            &(keys[0].clone(), "category:favorite-id".into())
        );
        // A reorder/filter while editing cannot redirect the category mutation.
        let mut rows = ui.get_items().iter().collect::<Vec<_>>();
        rows.reverse();
        ui.set_items(ModelRc::new(VecModel::from(rows)));
        ElementHandle::find_by_accessible_label(&ui, "Favorites")
            .find(|e| e.accessible_checked() == Some(true))
            .unwrap()
            .invoke_accessible_default_action();
        assert_eq!(actions.borrow().last().unwrap().0, keys[0]);
        key(&ui, Key::Escape);
        assert!(!ui.get_entry_menu_visible());
        let card = button(&ui, "Book 1");
        let point = slint::LogicalPosition::new(
            card.absolute_position().x + 30.,
            card.absolute_position().y + 30.,
        );
        pointer(&ui, point, true);
        settle();
        pointer(&ui, point, false);
        settle();
        assert!(ui.get_entry_menu_visible());
        assert_eq!(opens.get(), 0);
        button(&ui, "Remove from library").invoke_accessible_default_action();
        settle();
        assert!(!ui.get_entry_menu_visible());
        assert_eq!(
            actions.borrow().last().unwrap(),
            &(keys[1].clone(), "remove".into())
        );
        button(&ui, "Book 1").mock_single_click(PointerEventButton::Right);
        settle();
        let outside = slint::LogicalPosition::new(width - 2., 2.);
        pointer(&ui, outside, true);
        pointer(&ui, outside, false);
        settle();
        assert!(!ui.get_entry_menu_visible());
        assert_eq!(opens.get(), 0);
        button(&ui, "Book 1").mock_single_click(PointerEventButton::Right);
        settle();
        key(&ui, Key::Back);
        assert!(!ui.get_entry_menu_visible());
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
        assert!(!ui.get_entry_menu_visible());
        assert_eq!(opens.get(), 0);
        ui.set_section(5);
        ui.set_detail_series_key(keys[1].clone().into());
        ui.set_series_title("Detail target".into());
        ui.set_in_library(true);
        settle();
        button(&ui, "Title options").mock_single_click(PointerEventButton::Left);
        settle();
        button(&ui, "Refresh details").invoke_accessible_default_action();
        settle();
        assert_eq!(
            actions.borrow().last().unwrap(),
            &(keys[1].clone(), "refresh".into())
        );
        assert!(!ui.get_entry_menu_visible());
    }
    ui.set_section(3);
    ui.set_source_catalog_open(true);
    settle();
    button(&ui, "Book 0").mock_single_click(PointerEventButton::Right);
    settle();
    assert!(!ui.get_entry_menu_visible());
}

#[test]
fn category_keyboard_navigation_reveals_rows_and_keeps_done_visible() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let target = serde_json::to_string(&SeriesId::new("source", "series")).unwrap();
    ui.set_section(5);
    ui.set_detail_series_key(target.clone().into());
    ui.set_series_title("Many categories".into());
    ui.set_in_library(true);
    let actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_entry_action({
        let weak = ui.as_weak();
        let actions = actions.clone();
        move |key, action| {
            actions
                .borrow_mut()
                .push((key.to_string(), action.to_string()));
            if action == "categories" {
                weak.unwrap()
                    .set_entry_categories(ModelRc::new(VecModel::from(
                        (0..50)
                            .map(|i| EntryCategoryRow {
                                id: format!("category-{i}").into(),
                                label: format!("Category {i}").into(),
                                checked: false,
                            })
                            .collect::<Vec<_>>(),
                    )));
            }
        }
    });
    ui.show().unwrap();
    for (width, height) in [(390., 240.), (1120., 240.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        settle();
        button(&ui, "Title options").mock_single_click(PointerEventButton::Left);
        settle();
        button(&ui, "Categories").invoke_accessible_default_action();
        settle();
        let assert_visible = |index: usize| {
            let row = ElementHandle::find_by_accessible_label(&ui, &format!("Category {index}"))
                .next()
                .unwrap();
            let viewport = ElementHandle::find_by_element_id(&ui, "EntryMenu::entry-scroll")
                .next()
                .unwrap();
            assert!(
                row.absolute_position().y >= viewport.absolute_position().y - 0.5,
                "selected row {index} above viewport"
            );
            assert!(
                row.absolute_position().y + row.size().height
                    <= viewport.absolute_position().y + viewport.size().height + 0.5,
                "selected row {index} below viewport"
            );
        };
        for index in 0..50 {
            if index > 0 {
                key(&ui, Key::DownArrow);
            }
            assert_visible(index);
        }
        key(&ui, Key::Return);
        assert_eq!(
            actions.borrow().last().unwrap(),
            &(target.clone(), "category:category-49".into())
        );
        for index in (0..49).rev() {
            key(&ui, Key::UpArrow);
            assert_visible(index);
        }
        for _ in 0..50 {
            key(&ui, Key::DownArrow);
        }
        let done = button(&ui, "Done");
        assert!(done.absolute_position().y >= 0.);
        assert!(done.absolute_position().y + done.size().height <= height + 0.5);
        key(&ui, Key::Return);
        assert!(
            !ui.get_entry_menu_visible(),
            "Done owns the final keyboard selection"
        );
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
        for (label, action) in [
            ("Mark all read", "mark-all-read"),
            ("Mark all unread", "mark-all-unread"),
            ("Clear all chapter positions", "clear-all-positions"),
        ] {
            button(&ui, "Title options").invoke_accessible_default_action();
            settle();
            button(&ui, label).invoke_accessible_default_action();
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
