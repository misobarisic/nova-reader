//! Detail screen actions: the download ring, the chapter ⋯ menu, multi-chapter
//! selection, the download card and the title ⋯ menu, driven through the
//! real window. Backend choices (reading order, which chapters to download)
//! are unit-tested in `src/chapter_batch.rs`.
//!
//! The testing backend's mock clock is shared by every test thread in a
//! process, so the scenarios run in order from one test.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{Anim, AppWindow, ItemRow};
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..30 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn find(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
}

fn button(ui: &AppWindow, label: &str) -> ElementHandle {
    find(ui, label).unwrap_or_else(|| panic!("missing button {label}"))
}

fn press(ui: &AppWindow, label: &str) {
    button(ui, label).invoke_accessible_default_action();
    settle();
}

fn key(ui: &AppWindow, key: Key) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    settle();
}

fn chapter(number: &str, state: i32, progress: f32) -> ItemRow {
    ItemRow {
        title: format!("Chapter {number}").into(),
        short_title: format!("Title {number}").into(),
        number: number.into(),
        download_state: state,
        download_progress: progress,
        has_download: state != 0,
        downloaded: state == 5,
        ..Default::default()
    }
}

type Log = Rc<RefCell<Vec<String>>>;

fn detail(rows: Vec<ItemRow>) -> (AppWindow, Log) {
    let ui = AppWindow::new().unwrap();
    let log: Log = Rc::default();
    ui.on_item_action({
        let log = log.clone();
        move |index, action| log.borrow_mut().push(format!("item:{index}:{action}"))
    });
    ui.on_entry_action({
        let log = log.clone();
        move |key, action| log.borrow_mut().push(format!("entry:{key}:{action}"))
    });
    ui.on_chapter_selection({
        let log = log.clone();
        move |op, index| log.borrow_mut().push(format!("select:{op}:{index}"))
    });
    ui.on_detail_queue_action({
        let log = log.clone();
        move |action| log.borrow_mut().push(format!("queue:{action}"))
    });
    ui.set_items(ModelRc::new(VecModel::from(rows)));
    ui.set_series_title("Moonlit Cartographer".into());
    ui.set_detail_series_key("series-key".into());
    ui.set_section(5);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 2400.));
    settle();
    (ui, log)
}

fn last(log: &Log) -> String {
    log.borrow().last().cloned().unwrap_or_default()
}

fn download_ring_states_name_and_run_their_next_step() {
    let (ui, log) = detail(vec![
        chapter("1", 0, 0.),
        chapter("2", 1, 0.),
        chapter("3", 2, 0.35),
        chapter("4", 3, 0.6),
        chapter("5", 4, 0.1),
        chapter("6", 5, 1.),
    ]);
    for (index, label, action) in [
        (0, "Download", "download"),
        (1, "Queued, cancel", "cancel"),
        (2, "Downloading 35%, pause", "pause"),
        (3, "Paused 60%, resume", "resume"),
        (4, "Download failed, retry", "retry"),
    ] {
        let ring = ElementHandle::find_by_accessible_label(&ui, label)
            .find(|e| e.type_name().as_deref() == Some("DownloadRing"))
            .unwrap_or_else(|| panic!("missing ring {label}"));
        assert!(ring.size().width >= 44. && ring.size().height >= 44.);
        ring.invoke_accessible_default_action();
        settle();
        assert_eq!(last(&log), format!("item:{index}:{action}"));
    }
    // A finished download's ring opens the chapter menu instead.
    let before = log.borrow().len();
    button(&ui, "Downloaded, chapter options").invoke_accessible_default_action();
    settle();
    assert_eq!(log.borrow().len(), before);
    assert!(
        find(&ui, "Delete download").is_some(),
        "chapter menu opened"
    );
    key(&ui, Key::Escape);
    assert!(
        find(&ui, "Delete download").is_none(),
        "Escape closes the menu"
    );
}

fn chapter_menu_items_follow_the_download_state() {
    let mut partly = chapter("1", 0, 0.);
    partly.reading_status = "Page 4".into();
    let (ui, log) = detail(vec![
        partly,
        chapter("2", 2, 0.4),
        chapter("3", 3, 0.4),
        chapter("4", 4, 0.),
        chapter("5", 5, 1.),
    ]);
    let options = || {
        ElementHandle::find_by_accessible_label(&ui, "Chapter options")
            .filter(|e| e.accessible_role() == Some(AccessibleRole::Button))
            .collect::<Vec<_>>()
    };
    let expect: [(&[&str], &[&str]); 5] = [
        (
            &[
                "Mark as read",
                "Mark previous as read",
                "Bookmark",
                "Clear progress",
                "Download",
                "Download next 5",
            ],
            &["Pause", "Cancel download", "Delete download"],
        ),
        (
            &["Pause", "Cancel download"],
            &["Download next 5", "Clear progress"],
        ),
        (&["Resume", "Cancel download"], &["Pause"]),
        (&["Retry", "Discard"], &["Cancel download"]),
        // (Another row's ring is labelled "Download", so check the menu-only items.)
        (&["Delete download"], &["Download next 5", "Pause", "Retry"]),
    ];
    for (row, (present, absent)) in expect.iter().enumerate() {
        options()[row].invoke_accessible_default_action();
        settle();
        for label in *present {
            assert!(find(&ui, label).is_some(), "row {row}: missing {label}");
        }
        for label in *absent {
            assert!(find(&ui, label).is_none(), "row {row}: unexpected {label}");
        }
        if row == 1 {
            // The progress card shows while a job exists.
            let bar = ElementHandle::find_by_accessible_label(&ui, "Download progress")
                .next()
                .expect("progress card");
            assert_eq!(bar.accessible_value().as_deref(), Some("40%"));
        }
        key(&ui, Key::Escape);
    }
    // Picking an item runs it once and closes the menu.
    options()[0].invoke_accessible_default_action();
    settle();
    press(&ui, "Mark previous as read");
    assert_eq!(last(&log), "item:0:read-previous");
    assert!(find(&ui, "Mark previous as read").is_none());
    options()[0].invoke_accessible_default_action();
    settle();
    press(&ui, "Download next 5");
    assert_eq!(last(&log), "item:0:download-next-5");

    // With animations off the menu opens and closes at once.
    ui.global::<Anim>().set_enabled(false);
    options()[4].invoke_accessible_default_action();
    for _ in 0..3 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
    }
    let panel = ElementHandle::find_by_accessible_label(&ui, "Chapter options")
        .find(|e| e.accessible_role() == Some(AccessibleRole::Groupbox))
        .expect("menu panel");
    assert!((panel.computed_opacity() - 1.).abs() < 0.01);
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Escape.into(),
    });
    for _ in 0..3 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
    }
    assert!(find(&ui, "Delete download").is_none());
    ui.global::<Anim>().set_enabled(true);
}

fn selection_mode_enters_acts_and_leaves() {
    let (ui, log) = detail(vec![chapter("1", 0, 0.), chapter("2", 5, 1.)]);
    press(&ui, "Select chapters");
    assert_eq!(last(&log), "select:enter:-1");
    // The backend reports the selection; rows become checkboxes and their
    // download and options buttons step aside.
    ui.set_chapter_selecting(true);
    ui.set_chapter_selected_count(2);
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "2 selected")
            .next()
            .is_some()
    );
    let row = ElementHandle::find_by_accessible_label(&ui, "Chapter 1")
        .find(|e| e.type_name().as_deref() == Some("ChapterRow"))
        .unwrap();
    assert_eq!(row.accessible_checkable(), Some(true));
    row.invoke_accessible_default_action();
    settle();
    assert_eq!(last(&log), "select:toggle:0");
    assert_eq!(button(&ui, "Download").accessible_enabled(), Some(false));
    for (label, op) in [("Select all", "all"), ("Mark read", "mark-read")] {
        press(&ui, label);
        assert_eq!(last(&log), format!("select:{op}:-1"));
    }
    // Deleting downloads asks once for the whole selection.
    press(&ui, "Delete downloads");
    assert_ne!(last(&log), "select:delete-downloads:-1");
    // The confirmation repeats the label; its row comes after the bar's button.
    ElementHandle::find_by_accessible_label(&ui, "Delete downloads")
        .filter(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .last()
        .expect("confirmation")
        .invoke_accessible_default_action();
    settle();
    assert_eq!(last(&log), "select:delete-downloads:-1");
    // Back leaves selection before it leaves the title.
    ui.invoke_go_back();
    settle();
    assert_eq!(last(&log), "select:clear:-1");
    assert_eq!(ui.get_section(), 5);
}

fn download_card_counts_and_acts_on_this_title() {
    let (ui, log) = detail(vec![chapter("9", 2, 0.35)]);
    assert!(
        ElementHandle::find_by_element_id(&ui, "AppWindow::detail-queue")
            .next()
            .is_none()
    );
    ui.set_detail_queue_visible(true);
    ui.set_detail_queue_title("Downloading chapter 9 · 35%".into());
    ui.set_detail_queue_counts("2 queued · 1 paused".into());
    ui.set_detail_queue_failed("1 failed".into());
    ui.set_detail_queue_running(true);
    ui.set_detail_queue_progress(0.35);
    ui.set_detail_queue_pausable(true);
    settle();
    for text in ["2 queued · 1 paused", "1 failed"] {
        assert!(
            ElementHandle::find_by_accessible_label(&ui, text)
                .next()
                .is_some()
        );
    }
    press(&ui, "Pause all");
    assert_eq!(last(&log), "queue:pause");
    ui.set_detail_queue_pausable(false);
    settle();
    press(&ui, "Resume");
    assert_eq!(last(&log), "queue:resume");
    ui.set_detail_queue_visible(false);
    settle();
    assert!(find(&ui, "Resume").is_none());
}

/// The title menu's own row, not a chapter row's "Download" ring.
fn menu_item(ui: &AppWindow, label: &str) {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|e| e.type_name().as_deref() == Some("MenuItem"))
        .unwrap_or_else(|| panic!("missing menu item {label}"))
        .invoke_accessible_default_action();
    settle();
}

fn title_menu_pages_and_actions() {
    let (ui, log) = detail(vec![chapter("1", 0, 0.)]);
    ui.set_detail_unread_to_download(7);
    ui.set_detail_all_to_download(12);
    for in_library in [false, true] {
        ui.set_in_library(in_library);
        press(&ui, "Title options");
        assert_eq!(find(&ui, "Status").is_some(), in_library);
        assert_eq!(find(&ui, "Categories").is_some(), in_library);
        for label in [
            "Refresh",
            "Layout",
            "Delete read downloads",
            "Mark all read",
        ] {
            assert!(find(&ui, label).is_some(), "missing {label}");
        }
        assert!(
            find(
                &ui,
                if in_library {
                    "Remove from library"
                } else {
                    "Add to library"
                }
            )
            .is_some()
        );
        key(&ui, Key::Escape);
        assert!(!ui.get_title_menu_open());
    }
    // The Download sub-page, and back.
    press(&ui, "Title options");
    menu_item(&ui, "Download");
    for label in [
        "Next chapter",
        "Next 5 chapters",
        "All unread",
        "Everything",
    ] {
        assert!(find(&ui, label).is_some(), "missing {label}");
    }
    assert_eq!(
        button(&ui, "All unread")
            .accessible_description()
            .as_deref(),
        Some("7")
    );
    key(&ui, Key::Escape);
    assert!(
        ui.get_title_menu_open(),
        "Escape returns to the main page first"
    );
    assert!(find(&ui, "Mark all read").is_some());
    menu_item(&ui, "Download");
    press(&ui, "Next 5 chapters");
    assert_eq!(last(&log), "entry:series-key:download-next:5");
    // Delete read downloads keeps its confirmation.
    press(&ui, "Title options");
    press(&ui, "Delete read downloads");
    assert_ne!(last(&log), "entry:series-key:delete-read-downloads");
    press(&ui, "Delete read downloads");
    assert_eq!(last(&log), "entry:series-key:delete-read-downloads");
}

#[test]
fn detail_screen_actions() {
    i_slint_backend_testing::init_no_event_loop();
    download_ring_states_name_and_run_their_next_step();
    chapter_menu_items_follow_the_download_state();
    selection_mode_enters_acts_and_leaves();
    download_card_counts_and_acts_on_this_title();
    title_menu_pages_and_actions();
}
