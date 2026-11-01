use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{AppWindow, Appearance, ExtensionRow, ItemRow, PageRow, SourceRow, TileRow};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalSize, Model, ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
fn settle() {
    for _ in 0..24 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn key(ui: &AppWindow, key: Key) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
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
fn reader_context_menu_targets_images_and_preserves_reader_gestures() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.on_navigate({
        let weak = ui.as_weak();
        move |section| weak.unwrap().set_section(section)
    });
    let lifecycle = Rc::new(RefCell::new(Vec::new()));
    ui.on_reader_context_opening({
        let weak = ui.as_weak();
        let lifecycle = lifecycle.clone();
        move |page| {
            let ui = weak.unwrap();
            assert!(!ui.get_reader_osd(), "capture must exclude OSD");
            assert!(
                !ui.get_reader_context_visible(),
                "capture must precede menu"
            );
            lifecycle.borrow_mut().push(format!("opening:{page}"));
        }
    });
    ui.on_reader_context_action({
        let weak = ui.as_weak();
        let lifecycle = lifecycle.clone();
        move |page, action| {
            assert!(!weak.unwrap().get_reader_context_visible());
            lifecycle
                .borrow_mut()
                .push(format!("action:{page}:{action}"));
        }
    });
    ui.on_reader_context_closed({
        let lifecycle = lifecycle.clone();
        move || lifecycle.borrow_mut().push("closed".to_owned())
    });
    let image = slint::Image::from_rgb8(slint::SharedPixelBuffer::new(2, 4));
    ui.set_pages(ModelRc::new(VecModel::from(
        (0..2)
            .map(|index| PageRow {
                tiles: ModelRc::new(VecModel::from(vec![TileRow {
                    image: image.clone(),
                    offset: 0.,
                    fraction: 1.,
                    ..Default::default()
                }])),
                ratio: 2.,
                position: index as f32 * 2.,
                loaded: true,
                page_id: format!("page-{index}").into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_total_ratio(4.);
    ui.set_reading_mode(0);
    ui.set_section(6);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();

    ui.set_reader_osd(true);
    button(&ui, "Page 1 image").mock_single_click(PointerEventButton::Right);
    settle();
    assert!(ui.get_reader_context_visible());
    assert_eq!(ui.get_reader_context_page(), 0);
    assert!(!ui.get_reader_osd());
    for label in [
        "Save image",
        "Save screencap",
        "Copy image to clipboard",
        "Copy screencap to clipboard",
    ] {
        button(&ui, label);
    }
    button(&ui, "Save image").mock_single_click(PointerEventButton::Left);
    settle();
    assert!(!ui.get_reader_context_visible());
    assert_eq!(
        &*lifecycle.borrow(),
        &["opening:0", "action:0:save-image", "closed"]
    );

    // Every menu placement stays inside the active viewport, including a short
    // landscape viewport where its own content can scroll with the keyboard.
    for (width, height) in [(320., 640.), (390., 844.), (1120., 800.), (740., 240.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        settle();
        ui.invoke_open_reader_context(0, width - 1., height - 1.);
        settle();
        let panel = ElementHandle::find_by_element_id(&ui, "ReaderContextMenu::menu-panel")
            .next()
            .unwrap();
        let position = panel.absolute_position();
        let size = panel.size();
        assert!(position.x >= 0. && position.y >= 0.);
        assert!(position.x + size.width <= width + 0.5);
        assert!(position.y + size.height <= height + 0.5);
        key(&ui, Key::Escape);
        assert!(!ui.get_reader_context_visible());
        assert_eq!(ui.get_section(), 6);
    }
    ui.window().set_size(LogicalSize::new(390., 844.));
    ui.set_scroll_y(-400.);
    ui.set_current_page(1);
    settle();
    button(&ui, "Page 2 image").mock_single_click(PointerEventButton::Right);
    settle();
    assert_eq!(ui.get_reader_context_page(), 1);
    button(&ui, "Copy image to clipboard").mock_single_click(PointerEventButton::Left);
    settle();
    assert_eq!(
        lifecycle.borrow().iter().rev().nth(1).unwrap(),
        "action:1:copy-image"
    );

    ui.invoke_open_reader_context(1, 200., 200.);
    settle();
    let outside = slint::LogicalPosition::new(4., 4.);
    pointer(&ui, outside, true);
    pointer(&ui, outside, false);
    settle();
    assert!(!ui.get_reader_context_visible());
    assert!(!ui.get_reader_osd(), "dismissal must consume its click");

    // A stationary hold opens once, and its eventual release is consumed.
    let hold_at = slint::LogicalPosition::new(195., 500.);
    pointer(&ui, hold_at, true);
    // Flickable admits a stationary press after its short pan-arbitration
    // delay. Advance that timer before measuring the gesture's own 600ms.
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(120));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(599));
    assert!(!ui.get_reader_context_visible());
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(2));
    settle();
    assert!(ui.get_reader_context_visible());
    assert_eq!(ui.get_reader_context_page(), 1);
    pointer(&ui, hold_at, false);
    settle();
    assert!(ui.get_reader_context_visible());
    assert!(!ui.get_reader_osd());
    ui.invoke_go_back();
    settle();
    assert!(!ui.get_reader_context_visible());
    assert_eq!(ui.get_section(), 6);

    let before = ui.get_scroll_y();
    let start = slint::LogicalPosition::new(195., 650.);
    let end = slint::LogicalPosition::new(195., 460.);
    pointer(&ui, start, true);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(180));
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position: end });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(700));
    assert!(!ui.get_reader_context_visible(), "drag cancels hold timer");
    pointer(&ui, end, false);
    settle();
    assert!(!ui.get_reader_osd());
    assert!(ui.get_scroll_y() < before, "drag remains a reader pan");

    key(&ui, Key::Menu);
    assert!(ui.get_reader_context_visible());
    assert_eq!(ui.get_reader_context_page(), 1);
    key(&ui, Key::DownArrow);
    key(&ui, Key::Return);
    assert!(!ui.get_reader_context_visible());
    assert_eq!(
        lifecycle.borrow().iter().rev().nth(1).unwrap(),
        "action:1:save-screencap"
    );
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Shift.into(),
    });
    key(&ui, Key::F10);
    ui.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::Shift.into(),
    });
    assert!(ui.get_reader_context_visible());
    ui.set_reader_context_busy(true);
    settle();
    let before = lifecycle.borrow().len();
    button(&ui, "Save image").mock_single_click(PointerEventButton::Left);
    settle();
    assert_eq!(lifecycle.borrow().len(), before);
    assert!(
        ui.get_reader_context_visible(),
        "busy rows remain visible but inactive"
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Preparing image…")
            .next()
            .is_some()
    );
    ui.set_reader_context_busy(false);
    button(&ui, "Copy screencap to clipboard").mock_single_click(PointerEventButton::Left);
    settle();
    assert_eq!(
        lifecycle.borrow().iter().rev().nth(1).unwrap(),
        "action:1:copy-screencap"
    );
    ui.set_reader_context_status("Copied image to clipboard.".into());
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Copied image to clipboard.")
            .next()
            .is_some()
    );
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(4100));
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Copied image to clipboard.")
            .next()
            .is_none()
    );
    // The next export can finish with exactly the same result text. Its busy
    // phase must start a fresh notice and a fresh four-second lifetime.
    ui.set_reader_context_busy(true);
    ui.set_reader_context_status("Preparing image…".into());
    settle();
    ui.set_reader_context_busy(false);
    ui.set_reader_context_status("Copied image to clipboard.".into());
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Copied image to clipboard.")
            .next()
            .is_some()
    );
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(4100));
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Copied image to clipboard.")
            .next()
            .is_none()
    );
    ui.invoke_open_reader_context(1, 200., 200.);
    settle();
    ui.set_section(5);
    settle();
    assert!(
        !ui.get_reader_context_visible(),
        "navigation discards context state"
    );
    ui.set_section(10);
    settle();
    button(&ui, "Screenshots").invoke_accessible_default_action();
    settle();
    assert_eq!(ui.get_section(), 18);
    ui.invoke_go_back();
    settle();
    assert_eq!(
        ui.get_section(),
        10,
        "Screenshots returns to retained Settings"
    );
}
fn button(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button {label}"))
}

#[test]
fn chapter_download_delete_actions_follow_saved_remote_jobs() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    let actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_item_action({
        let actions = actions.clone();
        move |index, action| actions.borrow_mut().push((index, action.to_string()))
    });
    let rows = Rc::new(VecModel::from(vec![
        ItemRow {
            title: "Imported chapter".into(),
            local_import: true,
            ..Default::default()
        },
        ItemRow {
            title: "Saved remote chapter".into(),
            has_download: true,
            downloaded: true,
            ..Default::default()
        },
    ]));
    ui.set_items(ModelRc::from(rows.clone()));
    ui.set_series_title("Download fixture".into());
    ui.set_section(5);
    ui.window().set_size(LogicalSize::new(390., 844.));
    ui.show().unwrap();
    settle();

    for complete in [true, false] {
        let mut chapter = rows.row_data(1).unwrap();
        chapter.downloaded = complete;
        rows.set_row_data(1, chapter);
        settle();
        button(&ui, "Delete download").invoke_accessible_default_action();
        assert_eq!(
            actions.borrow().last().unwrap(),
            &(1, "delete-download".into())
        );
        ElementHandle::find_by_accessible_label(&ui, "Chapter options")
            .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
            .nth(1)
            .unwrap()
            .invoke_accessible_default_action();
        settle();
        let before = actions.borrow().len();
        // Popup coordinates are local to their own window. Identify the text
        // action by component instead of comparing it with the trailing icon.
        ElementHandle::find_by_accessible_label(&ui, "Delete download")
            .find(|element| element.type_name().as_deref() == Some("PillButton"))
            .unwrap()
            .invoke_accessible_default_action();
        settle();
        assert_eq!(actions.borrow().len(), before + 1);
        assert_eq!(
            actions.borrow().last().unwrap(),
            &(1, "delete-download".into())
        );
    }
    let mut chapter = rows.row_data(1).unwrap();
    chapter.has_download = false;
    chapter.downloaded = false;
    rows.set_row_data(1, chapter.clone());
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Delete download")
            .next()
            .is_none()
    );
    button(&ui, "Download").invoke_accessible_default_action();
    assert_eq!(actions.borrow().last().unwrap(), &(1, "download".into()));

    chapter.local_import = true;
    chapter.has_download = true; // local identity overrides even a stale job flag
    rows.set_row_data(1, chapter);
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Delete download")
            .next()
            .is_none()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Download")
            .next()
            .is_none()
    );
    ElementHandle::find_by_accessible_label(&ui, "Chapter options")
        .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .nth(1)
        .unwrap()
        .invoke_accessible_default_action();
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Delete download")
            .next()
            .is_none()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Download")
            .next()
            .is_none()
    );
}

#[test]
fn poster_layout_timelines_chapters_and_reader_controls() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.on_navigate({
        let weak = ui.as_weak();
        move |section| weak.unwrap().set_section(section)
    });
    let selected = Rc::new(Cell::new(-1));
    ui.on_open_item({
        let selected = selected.clone();
        move |i| selected.set(i)
    });
    let actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_item_action({
        let actions = actions.clone();
        move |i, a| actions.borrow_mut().push((i, a.to_string()))
    });
    ui.global::<Appearance>().on_setting_changed({
        let weak = ui.as_weak();
        move |key, value| {
            let ui = weak.unwrap();
            if key == "library_list_view" {
                ui.global::<Appearance>().set_library_list_view(value == 1);
            }
        }
    });
    ui.set_categories(ModelRc::new(VecModel::from(vec!["All".into()])));
    ui.set_items(ModelRc::new(VecModel::from(
        (0..24)
            .map(|i| ItemRow {
                title: format!("Story {i}").into(),
                subtitle: format!("Chapter {i}").into(),
                group: if i < 2 { "Today" } else { "Yesterday" }.into(),
                timestamp: "12:30".into(),
                completed: 3,
                total: 12,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();
    let cards = ElementHandle::find_by_element_type_name(&ui, "LibraryCard").collect::<Vec<_>>();
    assert!(
        cards.len() >= 4 && cards.len() < 24,
        "construct only viewport cards"
    );
    assert!((cards[0].absolute_position().y - cards[1].absolute_position().y).abs() < 1.);
    assert!(cards[1].absolute_position().x > cards[0].absolute_position().x);
    assert!(cards[2].absolute_position().y > cards[0].absolute_position().y);
    cards[1].invoke_accessible_default_action();
    assert_eq!(selected.get(), 1);
    button(&ui, "List view").invoke_accessible_default_action();
    settle();
    let cards = ElementHandle::find_by_element_type_name(&ui, "LibraryCard").collect::<Vec<_>>();
    assert!((cards[0].absolute_position().x - cards[1].absolute_position().x).abs() < 1.);
    assert!(cards[1].absolute_position().y > cards[0].absolute_position().y);
    assert!(cards[0].size().width > 330.);
    ui.global::<Appearance>().set_library_list_view(false);
    ui.global::<Appearance>().set_library_min_cols(3);
    settle();
    let cards = ElementHandle::find_by_element_type_name(&ui, "LibraryCard").collect::<Vec<_>>();
    assert!((cards[0].absolute_position().y - cards[2].absolute_position().y).abs() < 1.);
    ui.global::<Appearance>().set_library_min_cols(2);
    for (width, height) in [(320., 640.), (390., 844.), (700., 720.), (1120., 800.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        for section in [0, 1, 2, 5] {
            ui.set_section(section);
            settle();
            for kind in ["LibraryCard", "TimelineRow", "ChapterRow"] {
                for row in ElementHandle::find_by_element_type_name(&ui, kind) {
                    let p = row.absolute_position();
                    let s = row.size();
                    assert!(
                        p.x >= -0.5 && p.x + s.width <= width + 0.5,
                        "{kind} overflows {width}: {p:?} {s:?}"
                    );
                }
            }
            if section == 5 {
                let back = button(&ui, "Back");
                assert!(
                    back.absolute_position().y < 32.,
                    "detail toolbar remains at the top"
                );
            }
        }
    }
    ui.window().set_size(LogicalSize::new(390., 844.));
    ui.set_section(2);
    settle();
    let opened_series = Rc::new(Cell::new(-1));
    ui.on_open_series({
        let opened_series = opened_series.clone();
        move |index| opened_series.set(index)
    });
    selected.set(-1);
    let poster = button(&ui, "Open series details: Story 0");
    poster.mock_single_click(PointerEventButton::Left);
    settle();
    assert_eq!(opened_series.get(), 0);
    assert_eq!(selected.get(), -1);
    let history_row = ElementHandle::find_by_element_type_name(&ui, "TimelineRow")
        .next()
        .unwrap();
    history_row.mock_single_click(PointerEventButton::Left);
    settle();
    assert_eq!(selected.get(), 0);
    let remove = button(&ui, "Remove from history");
    remove.invoke_accessible_default_action();
    assert_eq!(
        actions.borrow().last().unwrap(),
        &(0, "remove-history".to_string())
    );
    ui.set_section(1);
    settle();
    button(&ui, "Download").invoke_accessible_default_action();
    assert_eq!(
        actions.borrow().last().unwrap(),
        &(0, "download".to_string())
    );
    ui.set_series_title("A long story title for the detail header".into());
    ui.set_resume_chapter("106.1".into());
    ui.set_series_description("A synopsis that stays readable over the shaded artwork.".into());
    ui.set_section(5);
    settle();
    let continue_actions = ElementHandle::find_by_accessible_label(&ui, "Continue chapter 106.1")
        .filter(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .collect::<Vec<_>>();
    assert_eq!(continue_actions.len(), 1);
    assert!(continue_actions.iter().all(|e| e.size().width > 100.));
    assert!(
        ElementHandle::find_by_element_type_name(&ui, "ChapterRow")
            .all(|row| row.size().height <= 80.)
    );
    let download = button(&ui, "Download");
    let options = button(&ui, "Chapter options");
    assert!(
        options.absolute_position().x - (download.absolute_position().x + download.size().width)
            >= 8.,
        "chapter actions keep their spacing"
    );
    button(&ui, "Chapter options").invoke_accessible_default_action();
    settle();
    ElementHandle::find_by_accessible_label(&ui, "Read / unread")
        .next()
        .expect("chapter action menu")
        .invoke_accessible_default_action();
    settle();
    assert_eq!(actions.borrow().last().unwrap(), &(0, "read".to_string()));
    // The Browse root groups stable sources and keeps row/Latest/pin actions
    // independent; extension management uses package IDs, not sorted indexes.
    ui.set_source_catalog_open(false);
    ui.set_source_rows(ModelRc::new(VecModel::from(vec![SourceRow {
        id: "sample-source.en".into(),
        name: "Sample provider".into(),
        language: "English".into(),
        group: "Pinned".into(),
        enabled: true,
        latest: true,
        index: 7,
        ..Default::default()
    }])));
    ui.set_extensions(ModelRc::new(VecModel::from(vec![ExtensionRow {
        id: "sample-package".into(),
        name: "Sample extension".into(),
        version: "1.0.0".into(),
        language: "en".into(),
        enabled: true,
        ..Default::default()
    }])));
    let pinned = Rc::new(RefCell::new(String::new()));
    ui.on_source_pin_picked({
        let pinned = pinned.clone();
        move |id| *pinned.borrow_mut() = id.to_string()
    });
    let latest = Rc::new(Cell::new(-1));
    ui.on_source_latest_selected({
        let latest = latest.clone();
        move |index| latest.set(index)
    });
    let extension = Rc::new(RefCell::new(String::new()));
    ui.on_extension_action({
        let extension = extension.clone();
        move |id, action| *extension.borrow_mut() = format!("{id}:{action}")
    });
    ui.set_section(3);
    settle();
    button(&ui, "Pin source").invoke_accessible_default_action();
    assert_eq!(*pinned.borrow(), "sample-source.en");
    assert!(!ui.get_source_catalog_open());
    button(&ui, "Latest").invoke_accessible_default_action();
    assert_eq!(latest.get(), 7);
    assert!(ui.get_source_catalog_open());
    ui.set_source_catalog_open(false);
    settle();
    button(&ui, "Extensions (1)").invoke_accessible_default_action();
    settle();
    button(&ui, "Disable").invoke_accessible_default_action();
    assert_eq!(*extension.borrow(), "sample-package:toggle");
    // Returning to a source result retains its scroll as well as its query.
    ui.set_browse_tab(0);
    ui.set_source_catalog_open(true);
    settle();
    let scroll = ElementHandle::find_by_element_id(&ui, "AppWindow::page-scroll")
        .next()
        .unwrap();
    scroll.scroll(0., -500.);
    settle();
    let card = ElementHandle::find_by_element_type_name(&ui, "LibraryCard")
        .find(|e| e.absolute_position().y >= 0. && e.absolute_position().y < 600.)
        .unwrap();
    let label = card.accessible_label().unwrap().to_string();
    let before_y = card.absolute_position().y;
    ui.set_detail_origin(3);
    ui.set_section(5);
    settle();
    ui.on_return_to_catalogue({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            ui.invoke_prepare_catalogue_return();
            ui.set_section(3);
        }
    });
    ui.invoke_go_back();
    settle();
    assert!(
        (button(&ui, &label).absolute_position().y - before_y).abs() < 1.,
        "Back restores the source catalogue viewport"
    );
    // A chapter starts immersive, and controls overlay rather than resize it.
    let immersive = Rc::new(Cell::new(false));
    ui.on_immersive_changed({
        let immersive = immersive.clone();
        move |hidden| immersive.set(hidden)
    });
    ui.set_pages(ModelRc::new(VecModel::from(vec![PageRow {
        ratio: 3.,
        position: 0.,
        ..Default::default()
    }])));
    ui.set_total_ratio(3.);
    ui.set_reading_mode(0);
    ui.set_section(6);
    settle();
    assert!(!ui.get_reader_osd());
    assert!(immersive.get());
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Options")
            .next()
            .is_none()
    );
    let reader = ElementHandle::find_by_element_id(&ui, "AppWindow::reader")
        .next()
        .unwrap();
    let initial_height = reader.size().height;
    let touch = button(&ui, "Toggle reader controls");
    touch.mock_single_click(PointerEventButton::Left);
    settle();
    assert!(ui.get_reader_osd());
    assert!(!immersive.get());
    assert_eq!(reader.size().height, initial_height);
    button(&ui, "Options").invoke_accessible_default_action();
    settle();
    assert_eq!(reader.size().height, initial_height);
    ui.invoke_toggle_reader_osd();
    settle();
    assert!(!ui.get_reader_osd());
    assert!(immersive.get());
    // Flickable pan arbitration must cancel click-to-toggle after a swipe.
    touch.mock_drag(
        slint::LogicalPosition::new(195., 320.),
        PointerEventButton::Left,
    );
    settle();
    assert!(!ui.get_reader_osd(), "scrolling must not reveal controls");
    assert!(
        ui.get_scroll_y() < 0.,
        "reading remains draggable with the OSD hidden"
    );
    ui.set_section(5);
    settle();
    assert!(!immersive.get(), "leaving reader restores status bar");
}

#[test]
fn browse_root_refreshes_do_not_show_loading_indicators() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(3);
    ui.set_busy(true);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    for adult in [false, true, false] {
        ui.set_show_adult(adult);
        settle();
        assert!(
            ElementHandle::find_by_accessible_label(&ui, "Loading…")
                .next()
                .is_none()
        );
        assert!(
            ElementHandle::find_by_element_type_name(&ui, "Spinner")
                .next()
                .is_none()
        );
    }
    // Actual source requests retain their existing loading feedback.
    ui.set_source_catalog_open(true);
    settle();
    assert!(
        ElementHandle::find_by_element_type_name(&ui, "Spinner")
            .next()
            .is_some()
    );
}

#[test]
fn addon_settings_show_disabled_catalogues_for_reenabling() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_source_catalog_settings(ModelRc::new(VecModel::from(vec![
        nova_reader::SettingRow {
            key: "fixture.en".into(),
            name: "English catalogue".into(),
            checked: true,
            ..Default::default()
        },
        nova_reader::SettingRow {
            key: "fixture.es".into(),
            name: "Spanish catalogue".into(),
            checked: false,
            ..Default::default()
        },
    ])));
    let changes = Rc::new(RefCell::new(Vec::new()));
    ui.on_catalog_enabled_changed({
        let changes = changes.clone();
        move |id, enabled| changes.borrow_mut().push((id.to_string(), enabled))
    });
    ui.set_section(8);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();
    ElementHandle::find_by_accessible_label(&ui, "Spanish catalogue")
        .find(|element| element.accessible_checkable() == Some(true))
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(
        changes.borrow().last().unwrap(),
        &("fixture.es".into(), true)
    );
    assert_eq!(ui.get_source_catalog_settings().row_count(), 2);
}

#[test]
fn android_scrolls_do_not_lift_cards_or_open_titles_while_desktop_hover_remains() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.global::<nova_reader::Anim>().set_enabled(true);
    ui.global::<nova_reader::Anim>().set_hover(true);
    ui.set_items(ModelRc::new(VecModel::from(
        (0..40)
            .map(|index| ItemRow {
                title: format!("Scroll title {index}").into(),
                series_key: format!("series-{index}").into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    let opens = Rc::new(Cell::new(0));
    ui.on_open_item({
        let opens = opens.clone();
        move |_| opens.set(opens.get() + 1)
    });
    ui.show().unwrap();
    for width in [390., 1120.] {
        ui.window().set_size(LogicalSize::new(width, 844.));
        for section in [0, 3] {
            ui.set_section(section);
            ui.set_source_catalog_open(section == 3);
            ui.invoke_reset_catalogue_scroll();
            ui.set_touch_menus(false);
            settle();
            let card = button(&ui, "Scroll title 0");
            let start = slint::LogicalPosition::new(
                card.absolute_position().x + 30.,
                card.absolute_position().y + 70.,
            );
            ui.window()
                .dispatch_event(WindowEvent::PointerMoved { position: start });
            settle();
            let lift = ElementHandle::find_by_element_id(&ui, "LibraryCard::lift")
                .next()
                .unwrap();
            assert!(
                (lift.absolute_position().y - card.absolute_position().y + 4.).abs() < 0.5,
                "desktop hover lifts the card at {width}px"
            );

            ui.set_touch_menus(true);
            settle();
            assert!(
                (lift.absolute_position().y - card.absolute_position().y).abs() < 0.5,
                "Android does not inherit mouse hover, including wide layouts"
            );
            pointer(&ui, start, true);
            for distance in [8., 18., 36., 70., 110.] {
                ui.window().dispatch_event(WindowEvent::PointerMoved {
                    position: slint::LogicalPosition::new(start.x, start.y - distance),
                });
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(32));
            }
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(800));
            pointer(
                &ui,
                slint::LogicalPosition::new(start.x, start.y - 110.),
                false,
            );
            settle();
            assert_eq!(opens.get(), 0, "scroll never activates a title");
            assert!(
                !ui.get_entry_menu_visible(),
                "scroll never becomes a long press"
            );
            assert!(
                (lift.absolute_position().y - card.absolute_position().y).abs() < 0.5,
                "no lingering lift after touch scrolling"
            );
            if section == 3 {
                assert!(
                    ui.get_catalogue_scroll_y() < 0.,
                    "the gesture actually scrolls Browse"
                );
            }
        }
    }
}
