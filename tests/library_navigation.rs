use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{AppWindow, ItemRow};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

fn settle() {
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}
fn button(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing {label}"))
}
fn element(ui: &AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(ui, id)
        .next()
        .unwrap_or_else(|| panic!("missing {id}"))
}
fn key(ui: &AppWindow, text: impl Into<slint::SharedString>) {
    let text = text.into();
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
    settle();
}
fn control_f(ui: &AppWindow) {
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    key(ui, "f");
    ui.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
    settle();
}
fn rows(count: usize) -> ModelRc<ItemRow> {
    ModelRc::new(VecModel::from(
        (0..count)
            .map(|index| ItemRow {
                title: format!("Chapter {index}").into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ))
}
#[test]
fn library_search_reveals_focuses_filters_and_closes_like_nova() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let searches = Rc::new(RefCell::new(Vec::new()));
    ui.on_search({
        let searches = searches.clone();
        move |query| searches.borrow_mut().push(query.to_string())
    });
    let routes = Rc::new(Cell::new(0));
    ui.on_navigate({
        let weak = ui.as_weak();
        let routes = routes.clone();
        move |section| {
            routes.set(routes.get() + 1);
            weak.unwrap().set_section(section);
        }
    });
    ui.set_items(rows(80));
    ui.set_categories(ModelRc::new(VecModel::from(vec!["All categories".into()])));
    ui.show().unwrap();
    for (width, height) in [(390., 844.), (1120., 780.), (640., 240.), (320., 640.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        settle();
        assert!(
            ElementHandle::find_by_element_id(&ui, "AppWindow::library-search")
                .next()
                .is_none()
        );
        key(&ui, "q");
        assert!(
            ui.get_library_query().is_empty(),
            "closed field cannot receive typing"
        );
        button(&ui, "Search library").mock_single_click(PointerEventButton::Left);
        settle();
        assert_eq!(element(&ui, "AppWindow::library-search").size().height, 58.);
        let field = element(&ui, "LibrarySearchReveal::search-line");
        let viewport = element(&ui, "AppWindow::page-scroll");
        assert!(field.absolute_position().y >= viewport.absolute_position().y - 0.5);
        assert!(
            field.absolute_position().y + field.size().height
                <= viewport.absolute_position().y + viewport.size().height + 0.5
        );
        if width >= 700. {
            assert!(
                field.absolute_position().y
                    < button(&ui, "Close library search").absolute_position().y
            );
        } else if height > 300. {
            assert!(
                field.absolute_position().y
                    > button(&ui, "Close library search").absolute_position().y
            );
        }
        key(&ui, "s");
        key(&ui, "t");
        assert_eq!(ui.get_library_query(), "st");
        assert_eq!(
            searches.borrow().last().unwrap(),
            "st",
            "live filtering on edits"
        );
        control_f(&ui);
        assert_eq!(
            ui.get_library_query(),
            "st",
            "Ctrl+F focuses without clearing"
        );
        key(&ui, Key::Escape);
        assert!(ui.get_library_query().is_empty());
        assert_eq!(searches.borrow().last().unwrap(), "");
        assert!(
            ElementHandle::find_by_element_id(&ui, "AppWindow::library-search")
                .next()
                .is_none()
        );
        assert_eq!(
            routes.get(),
            0,
            "first Escape closes search without navigating"
        );
        // Keyboard opening reveals the search even after a long grid scroll.
        element(&ui, "AppWindow::page-scroll").scroll(0., -900.);
        settle();
        control_f(&ui);
        key(&ui, "a");
        assert_eq!(ui.get_library_query(), "a");
        key(&ui, Key::Back);
        assert!(ui.get_library_query().is_empty());
        assert_eq!(routes.get(), 0);
    }
    control_f(&ui);
    key(&ui, "x");
    ui.set_section(5);
    settle();
    ui.invoke_go_back();
    settle();
    assert_eq!(ui.get_section(), 0);
    assert_eq!(
        ui.get_library_query(),
        "x",
        "returning from details retains library search"
    );
    key(&ui, "z");
    assert_eq!(
        ui.get_library_query(),
        "x",
        "returning does not reopen the keyboard"
    );
    button(&ui, "Close library search").mock_single_click(PointerEventButton::Left);
    settle();
    assert!(ui.get_library_query().is_empty());
}
#[test]
fn detail_chapter_return_restores_scroll_and_expanded_synopsis_for_the_same_series() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.on_navigate({
        let weak = ui.as_weak();
        move |section| weak.unwrap().set_section(section)
    });
    ui.on_open_item({
        let weak = ui.as_weak();
        move |_| weak.unwrap().set_section(6)
    });
    ui.on_chapter_step({
        let weak = ui.as_weak();
        move |_| weak.unwrap().set_section(6)
    });
    ui.set_series_title("First series".into());
    ui.set_series_description(
        "A long synopsis that can be expanded before browsing the chapters. "
            .repeat(70)
            .into(),
    );
    ui.set_detail_series_key("source:first".into());
    ui.set_section(5);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();
    // The optimistic hero is already usable while chapters are loading, but
    // Continue must never try to open a chapter from the previous entry.
    button(&ui, "Continue").mock_single_click(PointerEventButton::Left);
    settle();
    assert_eq!(ui.get_section(), 5);
    ui.set_items(rows(120));
    settle();
    for expanded in [false, true] {
        ui.set_detail_synopsis_expanded(expanded);
        settle();
        element(&ui, "AppWindow::page-scroll").scroll(0., if expanded { -5000. } else { -1800. });
        settle();
        let row = ElementHandle::find_by_element_type_name(&ui, "ChapterRow")
            .find(|row| {
                row.absolute_position().y > 120.
                    && row.absolute_position().y + row.size().height < 800.
            })
            .unwrap();
        let label = row.accessible_label().unwrap();
        let before_y = row.absolute_position().y;
        let before_scroll = ui.get_detail_scroll_y();
        assert!(before_scroll < -500.);
        for _ in 0..3 {
            button(&ui, &label).mock_single_click(PointerEventButton::Left);
            settle();
            assert_eq!(ui.get_section(), 6);
            ui.invoke_go_back();
            settle();
            assert_eq!(ui.get_section(), 5);
            assert_eq!(ui.get_detail_synopsis_expanded(), expanded);
            assert!((ui.get_detail_scroll_y() - before_scroll).abs() < 1.);
            assert!((button(&ui, &label).absolute_position().y - before_y).abs() < 1.);
        }
    }
    ui.set_detail_series_key("other-source:same-local-id".into());
    ui.set_series_title("Different series".into());
    settle();
    assert_eq!(ui.get_detail_scroll_y(), 0.);
    assert!(
        !ui.get_detail_synopsis_expanded(),
        "new series starts at its own header"
    );
}
