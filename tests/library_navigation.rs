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
/// The sort pill; its label names the current sort and direction.
fn sort_button(ui: &AppWindow) -> ElementHandle {
    ElementHandle::find_by_element_id(ui, "LibraryPage::sort-button")
        .next()
        .expect("sort pill")
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
                series_key: format!("entry-{index}").into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ))
}

#[test]
fn detail_entrance_fades_like_nova_and_respects_motion_settings_on_both_shells() {
    use nova_reader::Anim;
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            mock_time: true,
            threading: false,
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = AppWindow::new().unwrap();
    let mut poster = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(1, 1);
    poster.make_mut_slice()[0] = slint::Rgba8Pixel::new(230, 40, 30, 255);
    // As `artwork::set_series_poster` publishes it: the poster and the
    // few-pixel backdrop the desktop hero paints across the top.
    ui.set_series_backdrop(slint::Image::from_rgba8(poster.clone()));
    ui.set_series_poster(slint::Image::from_rgba8(poster));
    ui.set_series_title("Entrance fixture".into());
    ui.set_items(rows(8));
    ui.show().unwrap();
    let red = || {
        let pixels = ui.window().take_snapshot().unwrap();
        pixels.as_slice()[(130 * pixels.width() + pixels.width() - 32) as usize].r
    };
    let advance = |ms| {
        for _ in 0..ms / 10 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
            let _ = red(); // Paint intermediate frames, as the real event loop does.
        }
    };
    for (width, height) in [(390., 844.), (1120., 780.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        ui.set_section(0);
        ui.global::<Anim>().set_enabled(true);
        ui.global::<Anim>().set_transitions(true);
        advance(300);
        ui.set_detail_series_key(format!("entry-{width}").into());
        ui.set_section(5);
        let initial = red();
        advance(120);
        let middle = red();
        advance(300);
        let settled = red();
        assert!(
            settled > initial + 40,
            "poster must fade into view at {width}px"
        );
        assert!(
            middle > initial + 5 && middle + 5 < settled,
            "entrance must have intermediate painted frames: {initial}/{middle}/{settled}"
        );
        ui.set_series_title("Updated fixture".into());
        assert_eq!(red(), settled, "metadata completion must not restart entry");

        // Replacing a title while its detail is mounted starts a new entrance.
        ui.set_detail_series_key(format!("replacement-{width}").into());
        assert_eq!(red(), initial);
        advance(120);
        assert!(red() < settled);
        ui.global::<Anim>().set_enabled(false);
        assert_eq!(
            red(),
            settled,
            "master-off must finish an active fade immediately"
        );
        ui.global::<Anim>().set_enabled(true);
        assert_eq!(
            red(),
            settled,
            "re-enabling motion must not hide an open detail"
        );

        // Both switches bypass the deferred entrance entirely.
        for master_off in [true, false] {
            ui.set_section(0);
            ui.global::<Anim>().set_enabled(!master_off);
            ui.global::<Anim>().set_transitions(master_off);
            ui.set_section(5);
            assert_eq!(red(), settled);
            advance(20);
            assert_eq!(red(), settled);
        }
    }
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
    ui.set_categories(ModelRc::new(VecModel::from(vec!["All".into()])));
    ui.show().unwrap();
    for (width, height) in [(390., 844.), (1120., 780.), (640., 240.), (320., 640.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        // Start at the top: search lives in the header, which scrolls away.
        ui.set_library_scroll_y(0.);
        settle();
        assert!(
            ElementHandle::find_by_element_id(&ui, "LibraryPage::library-search")
                .next()
                .is_none()
        );
        key(&ui, "q");
        assert!(
            ui.get_library_query().is_empty(),
            "closed field cannot receive typing"
        );
        let category = button(&ui, "All");
        let sort = sort_button(&ui);
        assert!(
            category.absolute_position().y + category.size().height <= sort.absolute_position().y,
            "categories sit above the sort pill at {width}px"
        );
        // Search is the header's one action, above the categories.
        assert!(
            button(&ui, "Search library").absolute_position().y < category.absolute_position().y
        );
        button(&ui, "Search library").mock_single_click(PointerEventButton::Left);
        settle();
        assert_eq!(
            element(&ui, "LibraryPage::library-search").size().height,
            58.
        );
        let field = element(&ui, "LibrarySearchReveal::search-line");
        let viewport = element(&ui, "LibraryPage::lib-scroll");
        assert!(field.absolute_position().y >= viewport.absolute_position().y - 0.5);
        assert!(
            field.absolute_position().y + field.size().height
                <= viewport.absolute_position().y + viewport.size().height + 0.5
        );
        // The header's search button stays above the revealed field.
        if height > 300. {
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
            ElementHandle::find_by_element_id(&ui, "LibraryPage::library-search")
                .next()
                .is_none()
        );
        assert_eq!(
            routes.get(),
            0,
            "first Escape closes search without navigating"
        );
        // Keyboard opening reveals the search even after a long grid scroll.
        element(&ui, "LibraryPage::lib-scroll").scroll(0., -900.);
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

// Real single-contact input follows the same native Flickable path as Android.
fn library_touch(ui: &AppWindow, x: f32, y: f32, phase: i_slint_core::input::TouchPhase) {
    ui.window().dispatch_event(WindowEvent::internal(
        i_slint_core::platform::InternalEvent::Touch {
            id: 1,
            position: i_slint_core::lengths::LogicalPoint::new(x, y),
            phase,
        },
    ));
}
fn library_drag(ui: &AppWindow, x: f32, y: f32, dx: f32, dy: f32) {
    use i_slint_core::input::TouchPhase;
    library_touch(ui, x, y, TouchPhase::Started);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(120));
    for step in 1..=6 {
        library_touch(
            ui,
            x + dx * step as f32 / 6.,
            y + dy * step as f32 / 6.,
            TouchPhase::Moved,
        );
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
    library_touch(ui, x + dx, y + dy, TouchPhase::Ended);
    settle();
}
fn restore_library(ui: &AppWindow, anchor: &(String, f32, f32)) {
    let index = anchor
        .0
        .strip_prefix("entry-")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(-1);
    ui.set_library_scroll_y(anchor.2);
    ui.set_library_restore_index(index);
    ui.set_library_restore_fraction(anchor.1);
    ui.set_library_restore_serial(ui.get_library_restore_serial() + 1);
    settle();
}
fn assert_library_anchor(ui: &AppWindow, anchor: &(String, f32, f32)) {
    let index = anchor.0.strip_prefix("entry-").unwrap();
    let card = button(ui, &format!("Chapter {index}"));
    // The pinned toolbar ends where the grid is uncovered.
    let header = element(ui, "LibraryPage::library-header");
    let uncovered_top = header.absolute_position().y + header.size().height;
    let stride = card.size().height + ui.global::<nova_reader::Theme>().get_card_spacing();
    let fraction = (uncovered_top - card.absolute_position().y) / stride;
    assert!(
        (fraction - anchor.1).abs() < 0.025,
        "stable item {} should retain row fraction {} (actual {fraction})",
        anchor.0,
        anchor.1
    );
}

#[test]
fn library_pins_controls_and_restores_stable_rows_across_details_and_reflow() {
    i_slint_backend_testing::init_no_event_loop();
    for (width, height) in [(390., 844.), (1120., 780.)] {
        let ui = AppWindow::new().unwrap();
        let positions = Rc::new(RefCell::new(Vec::<(String, f32, f32)>::new()));
        ui.on_library_viewport_changed({
            let positions = positions.clone();
            move |key, fraction, y| positions.borrow_mut().push((key.to_string(), fraction, y))
        });
        ui.on_navigate({
            let weak = ui.as_weak();
            move |section| {
                let ui = weak.unwrap();
                ui.invoke_remember_library_position();
                ui.set_section(section);
            }
        });
        ui.on_open_item({
            let weak = ui.as_weak();
            move |_| {
                let ui = weak.unwrap();
                ui.invoke_remember_library_position();
                ui.set_section(5);
            }
        });
        ui.set_items(rows(180));
        ui.set_categories(ModelRc::new(VecModel::from(vec![
            "All".into(),
            "Reading".into(),
        ])));
        ui.show().unwrap();
        ui.window().set_size(LogicalSize::new(width, height));
        settle();
        element(&ui, "LibraryPage::lib-scroll").scroll(0., -1100.);
        settle();
        let category_y = button(&ui, "All").absolute_position().y;
        let grid_y = element(&ui, "LibraryPage::library-grid")
            .absolute_position()
            .y;
        element(&ui, "LibraryPage::lib-scroll").scroll(0., -317.);
        settle();
        assert!((button(&ui, "All").absolute_position().y - category_y).abs() < 0.5);
        assert!(
            element(&ui, "LibraryPage::library-grid")
                .absolute_position()
                .y
                < grid_y - 100.
        );
        ui.invoke_remember_library_position();
        let anchor = positions.borrow().last().unwrap().clone();
        assert!(!anchor.0.is_empty());
        assert!((0. ..1.).contains(&anchor.1));
        ui.invoke_open_item(0);
        settle();
        assert_eq!(ui.get_section(), 5);
        ui.invoke_go_back();
        restore_library(&ui, &anchor);
        assert_eq!(ui.get_section(), 0);
        assert_library_anchor(&ui, &anchor);
        // The persisted identity is resolved to the same model item after a
        // layout change; the containing row may start at a different index.
        ui.window().set_size(LogicalSize::new(
            if width < 700. { 1120. } else { 390. },
            height,
        ));
        restore_library(&ui, &anchor);
        assert_library_anchor(&ui, &anchor);
        button(&ui, "List view").invoke_accessible_default_action();
        restore_library(&ui, &anchor);
        assert_library_anchor(&ui, &anchor);
    }
}

#[test]
fn library_category_lane_rejects_vertical_pan_and_reveals_selected_pill() {
    i_slint_backend_testing::init_no_event_loop();
    for (width, height) in [(390., 844.), (1120., 780.)] {
        let ui = AppWindow::new().unwrap();
        ui.set_touch_menus(true);
        ui.set_items(rows(100));
        ui.set_categories(ModelRc::new(VecModel::from(
            (0..24)
                .map(|i| format!("Category {i}").into())
                .collect::<Vec<_>>(),
        )));
        ui.on_category_selected({
            let weak = ui.as_weak();
            move |i| weak.unwrap().set_category_index(i)
        });
        ui.show().unwrap();
        ui.window().set_size(LogicalSize::new(width, height));
        settle();
        element(&ui, "LibraryPage::lib-scroll").scroll(0., -900.);
        settle();
        let first = button(&ui, "Category 0");
        let first_y = first.absolute_position().y;
        let rail = element(&ui, "LibraryPage::filter-rail");
        let x = rail.absolute_position().x + rail.size().width / 2.;
        let y = rail.absolute_position().y + 22.;
        for (dx, dy) in [(0., -100.), (-40., 90.), (-90., -40.)] {
            library_drag(&ui, x, y, dx, dy);
            assert!(
                (first.absolute_position().y - first_y).abs() < 0.5,
                "vertical/diagonal touch must leave categories in their lane"
            );
        }
        rail.scroll(-300., 0.);
        settle();
        assert!((first.absolute_position().y - first_y).abs() < 0.5);
        let grid_x = element(&ui, "LibraryPage::library-grid")
            .absolute_position()
            .x;
        let viewport = element(&ui, "LibraryPage::lib-scroll");
        library_drag(
            &ui,
            viewport.absolute_position().x + viewport.size().width / 2.,
            viewport.absolute_position().y + viewport.size().height * 0.7,
            -100.,
            0.,
        );
        viewport.scroll(-250., 0.);
        settle();
        assert!(
            (element(&ui, "LibraryPage::library-grid")
                .absolute_position()
                .x
                - grid_x)
                .abs()
                < 0.5,
            "the native library viewport must have no horizontal pan"
        );
        ui.set_category_index(23);
        settle();
        for next_width in [width, 320., 1120.] {
            ui.window().set_size(LogicalSize::new(next_width, height));
            settle();
            let last = button(&ui, "Category 23");
            let rail = element(&ui, "LibraryPage::filter-rail");
            assert!(last.absolute_position().x >= rail.absolute_position().x - 0.5);
            assert!(
                last.absolute_position().x + last.size().width
                    <= rail.absolute_position().x + rail.size().width + 0.5,
                "selected last category remains revealed after resize"
            );
        }
    }
}

#[test]
fn library_navigation_captures_synchronously_and_restore_clamps_a_smaller_model() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let events = Rc::new(RefCell::new(Vec::<String>::new()));
    let positions = Rc::new(RefCell::new(Vec::<(String, f32, f32)>::new()));
    ui.on_library_viewport_changed({
        let events = events.clone();
        let positions = positions.clone();
        move |key, fraction, y| {
            events.borrow_mut().push("capture".into());
            positions.borrow_mut().push((key.to_string(), fraction, y));
        }
    });
    ui.on_navigate({
        let weak = ui.as_weak();
        let events = events.clone();
        move |section| {
            let ui = weak.unwrap();
            ui.invoke_remember_library_position();
            events.borrow_mut().push("navigate".into());
            ui.set_section(section);
        }
    });
    ui.set_items(rows(150));
    ui.set_categories(ModelRc::new(VecModel::from(vec!["All".into()])));
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();
    element(&ui, "LibraryPage::lib-scroll").scroll(0., -1900.);
    settle();
    events.borrow_mut().clear();
    let count = positions.borrow().len();
    ui.invoke_remember_library_position();
    assert_eq!(
        positions.borrow().len(),
        count + 1,
        "capture runs before returning to Rust"
    );
    let anchor = positions.borrow().last().unwrap().clone();
    assert!((anchor.2 - ui.get_library_scroll_y()).abs() < 0.5);
    events.borrow_mut().clear();
    ui.invoke_navigate(5);
    assert_eq!(&*events.borrow(), &["capture", "navigate"]);
    ui.set_items(rows(1));
    ui.set_section(0);
    restore_library(&ui, &anchor);
    assert_eq!(
        ui.get_library_scroll_y(),
        0.,
        "short content clamps an obsolete distant offset"
    );
    assert!(positions.borrow().last().unwrap().0.is_empty());
    // Header-only positions retain their raw offset and carry no card identity.
    restore_library(&ui, &(String::new(), 0., -12.));
    // A short viewport may not permit all 12px, but the result is always bounded.
    assert!(ui.get_library_scroll_y() >= -12. && ui.get_library_scroll_y() <= 0.);
}
