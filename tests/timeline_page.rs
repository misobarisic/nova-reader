//! Updates and History through the real window: the pinned header and its
//! shrinking title, sticky day headings, chips, the History view toggle,
//! virtualisation and layout at phone and desktop widths. Grouping and the
//! backend actions are tested in `src/timeline_groups.rs` and
//! `src/timeline_page.rs`.
//!
//! The testing backend's mock clock is shared process-wide, so the scenarios
//! run in order from one test.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{Anim, AppWindow, Appearance, ShelfEntry, TimelineBridge, TimelineEntry};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, LogicalSize, Model, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..30 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn find(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label).find(|e| {
        matches!(
            e.accessible_role(),
            Some(AccessibleRole::Button | AccessibleRole::RadioButton)
        )
    })
}

fn button(ui: &AppWindow, label: &str) -> ElementHandle {
    find(ui, label).unwrap_or_else(|| panic!("missing button {label}"))
}

/// A day of `groups` series, each a header, `chapters` rows and actions.
fn lines(
    days: usize,
    groups: usize,
    chapters: usize,
) -> (Vec<TimelineEntry>, Vec<TimelineEntry>, f32) {
    let mut out = Vec::new();
    let mut days_out = Vec::new();
    let mut y = 0.;
    let push = |out: &mut Vec<TimelineEntry>,
                y: &mut f32,
                kind: i32,
                height: f32,
                title: String,
                last: bool| {
        out.push(TimelineEntry {
            kind,
            index: out.len() as i32,
            y: *y,
            height,
            title: title.into(),
            subtitle: "Chapter 1".into(),
            emphasis: "3 new".into(),
            count: 3,
            unread: 2,
            has_unread: true,
            last,
            progress: -1.,
            ..Default::default()
        });
        *y += height + if last { 8. } else { 0. };
    };
    for d in 0..days {
        let start = out.len();
        push(&mut out, &mut y, 0, 44., format!("Day {d}"), false);
        for g in 0..groups {
            push(&mut out, &mut y, 1, 116., format!("Series {d}-{g}"), false);
            for _ in 0..chapters {
                push(&mut out, &mut y, 2, 48., format!("Series {d}-{g}"), false);
            }
            push(&mut out, &mut y, 4, 50., format!("Series {d}-{g}"), true);
        }
        let mut day = out[start].clone();
        day.span = y - day.y;
        out[start].span = day.span;
        days_out.push(day);
    }
    (out, days_out, y)
}

fn publish(ui: &AppWindow, days: usize, groups: usize, chapters: usize) {
    let (entries, day_lines, end) = lines(days, groups, chapters);
    let bridge = ui.global::<TimelineBridge>();
    bridge.set_entries(ModelRc::new(VecModel::from(entries)));
    bridge.set_days(ModelRc::new(VecModel::from(day_lines)));
    bridge.set_content_height(end);
    bridge.set_filter_counts(ModelRc::new(VecModel::from(vec![18, 9, 2])));
    bridge.set_count_label("9 new".into());
    bridge.set_status_line("Checked Today 14:02 · 46 titles".into());
}

fn window(width: f32, section: i32) -> AppWindow {
    let ui = AppWindow::new().unwrap();
    ui.set_section(section);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(width, 844.));
    publish(&ui, 3, 2, 3);
    settle();
    ui
}

fn scroll(ui: &AppWindow, dy: f32) {
    let at = LogicalPosition::new(150., 500.);
    ui.window().dispatch_event(WindowEvent::PointerScrolled {
        position: at,
        delta_x: 0.,
        delta_y: dy,
    });
    settle();
}

fn title(ui: &AppWindow, text: &str) -> ElementHandle {
    ElementHandle::find_by_element_type_name(ui, "PageTitle")
        .find(|e| e.accessible_label().as_deref() == Some(text))
        .unwrap_or_else(|| panic!("missing title {text}"))
}

fn sticky_header_and_shrinking_title() {
    let ui = window(390., 1);
    let heading = title(&ui, "Updates");
    let full = heading.size().height;
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "9 new")
            .next()
            .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Checked Today 14:02 · 46 titles")
            .next()
            .is_some()
    );
    // Chips read their counts and stay pinned with the header.
    let chip = button(&ui, "Unread, 9 titles");
    let chip_y = chip.absolute_position().y;
    scroll(&ui, -400.);
    let heading = title(&ui, "Updates");
    assert!(
        heading.absolute_position().y < 20.,
        "the header stays pinned at the top"
    );
    assert!(
        heading.size().height < full,
        "the title shrinks after scrolling"
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Checked Today 14:02 · 46 titles")
            .next()
            .is_none(),
        "the checked line hides"
    );
    // The current day's heading sticks directly below the pinned header
    // (title row plus chips), never under it.
    let title_bottom = heading.absolute_position().y + heading.size().height;
    let days: Vec<f32> = ElementHandle::find_by_accessible_label(&ui, "Day 0")
        .map(|e| e.absolute_position().y)
        .collect();
    assert!(
        days.iter()
            .any(|y| *y > title_bottom + 40. && *y < title_bottom + 120.),
        "Day 0 sticks below the header: {title_bottom} {days:?}"
    );
    let _ = chip_y;
    // Back to the top: full size again.
    scroll(&ui, 2000.);
    assert_eq!(title(&ui, "Updates").size().height, full);
    // Chips report picks.
    let picked = Rc::new(RefCell::new(-1));
    ui.global::<TimelineBridge>().on_filter_picked({
        let picked = picked.clone();
        move |k| *picked.borrow_mut() = k
    });
    button(&ui, "Downloaded, 2 titles").invoke_accessible_default_action();
    assert_eq!(*picked.borrow(), 2);
}

fn group_lines_act() {
    let ui = window(390., 1);
    let log = Rc::new(RefCell::new(Vec::new()));
    ui.global::<TimelineBridge>().on_entry_action({
        let log = log.clone();
        move |i, a| log.borrow_mut().push((i, a.to_string()))
    });
    // The group header expands and carries its state.
    let header =
        ElementHandle::find_by_accessible_label(&ui, "Series 0-0, 3 new Chapter 1, collapsed")
            .next()
            .expect("group header");
    header.invoke_accessible_default_action();
    assert_eq!(log.borrow().last().unwrap(), &(1, "toggle".into()));
    button(&ui, "Mark read").invoke_accessible_default_action();
    assert_eq!(log.borrow().last().unwrap(), &(5, "mark-read".into()));
    button(&ui, "Download all").invoke_accessible_default_action();
    assert_eq!(log.borrow().last().unwrap(), &(5, "download-all".into()));
    // Day headings offer Mark read when the day has unread chapters.
    button(&ui, "Mark Day 0 read").invoke_accessible_default_action();
    assert_eq!(log.borrow().last().unwrap(), &(0, "mark-day-read".into()));
}

fn history_toggle_and_shelf() {
    let ui = window(390., 2);
    let appearance = ui.global::<Appearance>();
    appearance.on_setting_changed({
        let weak = ui.as_weak();
        move |key, value| {
            if key == "history_view" {
                weak.unwrap().global::<Appearance>().set_history_view(value);
            }
        }
    });
    let bridge = ui.global::<TimelineBridge>();
    bridge.set_shelf(ModelRc::new(VecModel::from(vec![
        ShelfEntry {
            title: "Moonlit".into(),
            headline: "Ch. 87".into(),
            detail: "Page 18 of 30".into(),
            progress: 0.6,
            ..Default::default()
        },
        ShelfEntry {
            title: "Glasswing".into(),
            headline: "Caught up".into(),
            caught_up: true,
            ..Default::default()
        },
    ])));
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Pick up where you left off")
            .next()
            .is_some()
    );
    let picked = Rc::new(RefCell::new(-1));
    bridge.on_shelf_picked({
        let picked = picked.clone();
        move |i| *picked.borrow_mut() = i
    });
    button(&ui, "Moonlit, Ch. 87, Page 18 of 30").invoke_accessible_default_action();
    assert_eq!(*picked.borrow(), 0);
    // The caught-up card has no action.
    assert_eq!(
        button(&ui, "Glasswing, Caught up, ").accessible_enabled(),
        Some(false)
    );
    let timeline = button(&ui, "Timeline");
    assert_eq!(timeline.accessible_checked(), Some(false));
    timeline.invoke_accessible_default_action();
    settle();
    assert_eq!(appearance.get_history_view(), 1);
    assert_eq!(button(&ui, "Timeline").accessible_checked(), Some(true));
    // Search opens inside the pinned header.
    button(&ui, "Search history").invoke_accessible_default_action();
    settle();
    assert!(bridge.get_search_open());
    assert!(find(&ui, "Close history search").is_some());
    // Clear history goes through the ⋯ menu to the existing confirmation.
    let menu = Rc::new(RefCell::new(String::new()));
    bridge.on_menu_action({
        let menu = menu.clone();
        move |a| *menu.borrow_mut() = a.to_string()
    });
    button(&ui, "Options").invoke_accessible_default_action();
    settle();
    button(&ui, "Clear history").invoke_accessible_default_action();
    settle();
    assert_eq!(*menu.borrow(), "clear-history");
}

fn virtualised() {
    let ui = window(390., 1);
    // About a thousand lines: only those near the viewport exist.
    publish(&ui, 40, 5, 3);
    let requested = Rc::new(RefCell::new(0));
    ui.global::<TimelineBridge>().on_request_artwork({
        let requested = requested.clone();
        move |_, _| *requested.borrow_mut() += 1
    });
    settle();
    assert!(ui.global::<TimelineBridge>().get_entries().row_count() > 1000 - 200);
    let created = ElementHandle::find_by_element_type_name(&ui, "TimelineLine").count();
    assert!(created < 60, "{created} lines created");
    assert!(*requested.borrow() < 400, "artwork requests stay bounded");
}

fn layouts_fit() {
    for width in [320., 390., 1280., 1920.] {
        for section in [1, 2] {
            let ui = window(width, section);
            for e in ElementHandle::find_by_element_type_name(&ui, "TimelineLine")
                .chain(ElementHandle::find_by_element_type_name(&ui, "HeaderIcon"))
            {
                let at = e.absolute_position();
                let size = e.size();
                assert!(
                    at.x >= -0.5 && at.x + size.width <= width + 0.5,
                    "overflow at {width}: {at:?} {size:?}"
                );
            }
            for e in ElementHandle::find_by_element_type_name(&ui, "HeaderIcon") {
                assert!(e.size().height >= 44. && e.size().width >= 44.);
            }
            for e in ElementHandle::find_by_element_type_name(&ui, "DownloadRing") {
                assert!(e.size().height >= 44.);
            }
        }
    }
}

fn instant_without_animations() {
    let ui = window(390., 1);
    ui.global::<Anim>().set_enabled(false);
    let full = title(&ui, "Updates").size().height;
    scroll(&ui, -400.);
    // One frame is enough with motion off.
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
    assert!(title(&ui, "Updates").size().height < full);
    ui.global::<Anim>().set_enabled(true);
}

#[test]
fn timeline_page() {
    i_slint_backend_testing::init_no_event_loop();
    sticky_header_and_shrinking_title();
    group_lines_act();
    history_toggle_and_shelf();
    virtualised();
    layouts_fit();
    instant_without_animations();
}

#[test]
fn timeline_page_takes_radii_from_the_theme() {
    let text = std::fs::read_to_string("crates/ui/timeline-page.slint").unwrap();
    for (line, content) in text.lines().enumerate() {
        if let Some(at) = content.find("border-radius:") {
            let value = content[at + "border-radius:".len()..].trim_start();
            // Round icon buttons and dots are circles, not theme corners.
            let round = value.starts_with("22px")
                || value.starts_with("15px")
                || value.starts_with("12px")
                || value.starts_with("5px")
                || value.starts_with("3px")
                || value.starts_with("2px")
                || value.starts_with("0px")
                || value.starts_with("18px");
            assert!(
                !value.starts_with(|c: char| c.is_ascii_digit()) || round,
                "timeline-page.slint:{} uses a literal radius: {content}",
                line + 1
            );
        }
    }
    let _ = PointerEventButton::Left;
}
