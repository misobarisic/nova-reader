//! The library "Refresh": counted tonal category pills, the sort pill and
//! menu, the view switch, cover badges and detail lines, through the real
//! window. Scenarios share the testing backend's process-wide mock clock, so
//! they run in order from one test. Sorting, counts and detail text are
//! unit-tested in `src/library_view.rs`.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{AppWindow, Appearance, ItemRow};
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, LogicalSize, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..30 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn button(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button {label}"))
}

fn count(ui: &AppWindow, label: &str) -> usize {
    ElementHandle::find_by_accessible_label(ui, label).count()
}

fn key(ui: &AppWindow, key: Key) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    settle();
}

fn title(n: usize, completed: i32, total: i32) -> ItemRow {
    ItemRow {
        title: format!("Title {n}").into(),
        series_key: format!("series-{n}").into(),
        completed,
        total,
        ..Default::default()
    }
}

type Log = Rc<RefCell<Vec<(String, i32)>>>;

fn library(width: f32, items: Vec<ItemRow>) -> (AppWindow, Log) {
    let ui = AppWindow::new().unwrap();
    let log: Log = Rc::default();
    ui.global::<Appearance>().on_setting_changed({
        let log = log.clone();
        let weak = ui.as_weak();
        move |key, value| {
            log.borrow_mut().push((key.to_string(), value));
            let ui = weak.unwrap();
            let appearance = ui.global::<Appearance>();
            match key.as_str() {
                "library_sort" => appearance.set_library_sort(value),
                "library_sort_reversed" => appearance.set_library_sort_reversed(value == 1),
                "library_list_view" => appearance.set_library_list_view(value == 1),
                _ => {}
            }
        }
    });
    ui.set_items(ModelRc::new(VecModel::from(items)));
    ui.set_categories(ModelRc::new(VecModel::from(
        ["All", "Reading", "Plan to read", "Favourites"]
            .map(SharedString::from)
            .to_vec(),
    )));
    ui.set_category_counts(ModelRc::new(VecModel::from(vec![18, 8, 3, 0])));
    ui.set_library_count_label("18 titles".into());
    ui.set_heading("Library".into());
    ui.set_section(0);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(width, 1600.));
    settle();
    (ui, log)
}

fn header_and_chips() {
    let (ui, _) = library(390., vec![title(1, 0, 10)]);
    assert!(count(&ui, "Library") > 0);
    assert!(count(&ui, "18 titles") > 0);
    // Chips read their count; selection never changes a chip's width.
    let reading = button(&ui, "Reading, 8 titles");
    let width = reading.size().width;
    assert!(
        reading.size().height < 44.,
        "visible pill is shorter than 44px"
    );
    let picked = Rc::new(std::cell::Cell::new(-1));
    ui.on_category_selected({
        let picked = picked.clone();
        let weak = ui.as_weak();
        move |index| {
            picked.set(index);
            weak.unwrap().set_category_index(index);
        }
    });
    reading.invoke_accessible_default_action();
    settle();
    assert_eq!(picked.get(), 1);
    assert_eq!(button(&ui, "Reading, 8 titles").size().width, width);
    let all = button(&ui, "All, 18 titles");
    assert!(all.size().width < width, "count is part of the width");
}

fn sort_menu() {
    let (ui, log) = library(390., vec![title(1, 0, 10)]);
    let pill = || {
        ElementHandle::find_by_element_id(&ui, "LibraryPage::sort-button")
            .next()
            .expect("sort pill")
    };
    assert_eq!(
        pill().accessible_label().as_deref(),
        Some("Sort: Recently added, descending")
    );
    // One icon on the pill, flipped for ascending.
    assert_eq!(
        ElementHandle::find_by_element_type_name(&ui, "SortGlyph").count(),
        1
    );
    pill().invoke_accessible_default_action();
    settle();
    for label in [
        "Last read",
        "Recently added",
        "Title",
        "Unread chapters",
        "Latest chapter",
        "Descending",
        "Ascending",
    ] {
        assert!(count(&ui, label) > 0, "menu offers {label}");
    }
    // Escape closes without changing anything.
    let before = log.borrow().len();
    key(&ui, Key::Escape);
    assert_eq!(log.borrow().len(), before);
    assert_eq!(count(&ui, "Latest chapter"), 0);
    // Picking a sort applies it and closes.
    pill().invoke_accessible_default_action();
    settle();
    ElementHandle::find_by_accessible_label(&ui, "Last read")
        .find(|e| e.accessible_role() == Some(AccessibleRole::RadioButton))
        .unwrap()
        .invoke_accessible_default_action();
    settle();
    assert_eq!(
        log.borrow().last().cloned(),
        Some(("library_sort".into(), 3))
    );
    assert_eq!(
        pill().accessible_label().as_deref(),
        Some("Sort: Last read, descending")
    );
    // Ascending reverses Last read's natural (newest first) order.
    pill().invoke_accessible_default_action();
    settle();
    ElementHandle::find_by_accessible_label(&ui, "Ascending")
        .find(|e| e.accessible_role() == Some(AccessibleRole::RadioButton))
        .unwrap()
        .invoke_accessible_default_action();
    settle();
    assert_eq!(
        log.borrow().last().cloned(),
        Some(("library_sort_reversed".into(), 1))
    );
    assert_eq!(
        pill().accessible_label().as_deref(),
        Some("Sort: Last read, ascending")
    );
    // Title is naturally A→Z: ascending is its unreversed order.
    ui.global::<Appearance>().set_library_sort(1);
    ui.global::<Appearance>().set_library_sort_reversed(false);
    settle();
    assert_eq!(
        pill().accessible_label().as_deref(),
        Some("Sort: Title, ascending")
    );
    // The keyboard moves through the menu and Enter picks.
    pill().invoke_accessible_default_action();
    settle();
    key(&ui, Key::DownArrow);
    key(&ui, Key::Return);
    assert_eq!(
        log.borrow().last().cloned(),
        Some(("library_sort".into(), 0))
    );
}

fn badges_and_details() {
    let mut fresh = title(1, 4, 12);
    fresh.new_count = 2;
    fresh.downloaded_count = 3;
    fresh.detail = "Ch. 5 of 12".into();
    let mut caught_up = title(2, 12, 12);
    caught_up.detail = "Completed".into();
    caught_up.detail_success = true;
    let mut unread = title(3, 0, 94);
    unread.detail = "94 chapters".into();
    let (ui, _) = library(390., vec![fresh, caught_up, unread]);
    // "+N" replaces the unread count when there is something new.
    assert!(count(&ui, "2 new chapters") > 0);
    assert!(count(&ui, "94 unread") > 0);
    // Nothing unread: no badge.
    assert_eq!(count(&ui, "0 unread"), 0);
    // Only titles with downloads carry the download badge.
    assert_eq!(count(&ui, "Downloaded"), 1);
    for line in ["Ch. 5 of 12", "Completed", "94 chapters"] {
        assert!(count(&ui, line) > 0, "detail {line}");
    }
    // Three columns on a phone.
    let cards: Vec<_> = ElementHandle::find_by_element_type_name(&ui, "LibraryCard").collect();
    assert_eq!(cards.len(), 3);
    let y = cards[0].absolute_position().y;
    assert!(
        cards
            .iter()
            .all(|c| (c.absolute_position().y - y).abs() < 0.5)
    );
}

fn layout_fits() {
    for width in [320., 390., 1280., 1920.] {
        for list in [false, true] {
            let items = (0..12).map(|n| title(n, (n % 3) as i32, 10)).collect();
            let (ui, _) = library(width, items);
            ui.global::<Appearance>().set_library_list_view(list);
            settle();
            for kind in [
                "LibraryCard",
                "LibraryViewSwitch",
                "LibraryFilterChip",
                "IconAction",
            ] {
                for e in ElementHandle::find_by_element_type_name(&ui, kind) {
                    let at = e.absolute_position();
                    let size = e.size();
                    // The category rail scrolls; its chips may extend past the edge.
                    if kind == "LibraryFilterChip" {
                        continue;
                    }
                    assert!(
                        at.x >= -0.5 && at.x + size.width <= width + 0.5,
                        "{kind} {:?} overflows {width}px (list={list}): {at:?} {size:?}",
                        e.accessible_label()
                    );
                }
            }
            let rail = ElementHandle::find_by_element_id(&ui, "LibraryPage::filter-rail")
                .next()
                .unwrap();
            assert!(rail.absolute_position().x + rail.size().width <= width + 0.5);
            assert!(rail.size().height >= 44., "chip touch lane is 44px");
            let sort = ElementHandle::find_by_element_id(&ui, "LibraryPage::sort-touch")
                .next()
                .unwrap();
            assert!(sort.size().height >= 44.);
        }
    }
}

#[test]
fn library_refresh() {
    i_slint_backend_testing::init_no_event_loop();
    header_and_chips();
    sort_menu();
    badges_and_details();
    layout_fits();
}

#[test]
fn library_files_take_radii_from_appearance() {
    // Corners follow Settings → Appearance: no literal radius may remain.
    for file in [
        "crates/ui/library-page.slint",
        "crates/ui/library-cards.slint",
        "crates/ui/library-search.slint",
    ] {
        let text = std::fs::read_to_string(file).unwrap();
        for (line, content) in text.lines().enumerate() {
            if let Some(at) = content.find("border-radius:") {
                let value = content[at + "border-radius:".len()..].trim_start();
                assert!(
                    !value.starts_with(|c: char| c.is_ascii_digit()),
                    "{file}:{} uses a literal radius: {content}",
                    line + 1
                );
            }
        }
    }
}
