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
    ui.set_busy(true);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();
    assert!(
        ElementHandle::find_by_element_type_name(&ui, "Spinner")
            .next()
            .is_none(),
        "details must not show a loading spinner"
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Loading chapters…")
            .next()
            .is_none()
    );
    assert_eq!(button(&ui, "Continue").accessible_enabled(), Some(false));
    ui.set_status("Fixture metadata error".into());
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Fixture metadata error")
            .next()
            .is_some(),
        "actual detail errors remain visible"
    );
    ui.set_status("".into());
    ui.set_busy(false);
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

#[test]
fn synopsis_uses_nova_tap_drag_and_borderless_keyboard_behavior() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        renderer_name: Some("software".into()),
        ..Default::default()
    })))
    .unwrap();
    let ui = AppWindow::new().unwrap();
    let theme = ui.global::<nova_reader::Theme>();
    let mut palette = theme.get_current();
    palette.focus_ring = slint::Color::from_rgb_u8(10, 240, 34);
    theme.set_current(palette);
    ui.set_series_title("Synopsis fixture".into());
    ui.set_series_description(
        "Synopsis text with enough words to wrap to several readable lines. "
            .repeat(6)
            .into(),
    );
    ui.set_detail_series_key("fixture:synopsis".into());
    ui.set_section(5);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();
    let before = ui.get_detail_scroll_y();
    let more = button(&ui, "Read more");
    let point = slint::LogicalPosition::new(
        more.absolute_position().x + more.size().width / 2.,
        more.absolute_position().y + more.size().height / 2.,
    );
    // Nova's native pointer test recipe: release at the original tap point.
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position: point,
        button: PointerEventButton::Left,
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position: point,
        button: PointerEventButton::Left,
    });
    settle();
    assert!(
        ui.get_detail_synopsis_expanded(),
        "tap at {point:?}, synopsis={:?}/{:?}, scroll={}",
        more.absolute_position(),
        more.size(),
        ui.get_detail_scroll_y()
    );
    assert!((ui.get_detail_scroll_y() - before).abs() < 1.);
    button(&ui, "Show less").invoke_accessible_default_action();
    settle();
    assert!(!ui.get_detail_synopsis_expanded());

    let synopsis = element(&ui, "DetailHero::synopsis");
    let start = slint::LogicalPosition::new(
        synopsis.absolute_position().x + synopsis.size().width / 2.,
        synopsis.absolute_position().y + 20.,
    );
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position: start,
        button: PointerEventButton::Left,
    });
    // Admit the press through the native scroller before testing the child's
    // horizontal movement guard; a drag within the synopsis must not toggle it.
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(120));
    for distance in [10., 20., 30., 40.] {
        ui.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(start.x + distance, start.y),
        });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position: slint::LogicalPosition::new(start.x + 40., start.y),
        button: PointerEventButton::Left,
    });
    settle();
    assert!(
        !ui.get_detail_synopsis_expanded(),
        "dragging must not expand the synopsis"
    );

    // Tab traversal still reaches the retained invisible keyboard scope.
    let mut keyboard_activated = false;
    for _ in 0..24 {
        key(&ui, Key::Tab);
        let scroll = ui.get_detail_scroll_y();
        key(&ui, " ");
        if ui.get_detail_synopsis_expanded() {
            assert!(
                (ui.get_detail_scroll_y() - scroll).abs() < 1.,
                "keyboard expansion must not jump the page"
            );
            keyboard_activated = true;
            break;
        }
    }
    assert!(keyboard_activated, "synopsis remains keyboard operable");
    let synopsis = element(&ui, "DetailHero::synopsis");
    let pixels = ui
        .window()
        .take_snapshot()
        .expect("software synopsis pixels");
    let x = synopsis.absolute_position().x.floor() as u32;
    let right = (synopsis.absolute_position().x + synopsis.size().width).floor() as u32;
    let top = synopsis.absolute_position().y.ceil().max(0.) as u32 + 2;
    let bottom = ((synopsis.absolute_position().y + synopsis.size().height).floor() as u32)
        .min(pixels.height())
        .saturating_sub(2);
    assert!(bottom > top + 2);
    for row in top..bottom {
        for column in [x, x + 1, right - 2, right - 1] {
            let pixel = pixels.as_slice()[(row * pixels.width() + column) as usize];
            assert!(
                !(pixel.g > 200 && pixel.r < 40 && pixel.b < 80),
                "synopsis must not paint a focus outline at {column},{row}: {pixel:?}"
            );
        }
    }
}
