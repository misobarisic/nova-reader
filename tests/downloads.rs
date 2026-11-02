//! Native production Downloads surface: geometry, accessible input and queue actions.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{Anim, AppWindow, DownloadEntry, ItemRow};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, LogicalSize, ModelRc, VecModel};
use std::{cell::RefCell, path::PathBuf, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn button(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing Downloads button {label}"))
}

fn activate(ui: &AppWindow, label: &str) {
    button(ui, label).invoke_accessible_default_action();
    settle();
}

fn model(rows: Vec<DownloadEntry>) -> ModelRc<DownloadEntry> {
    ModelRc::new(VecModel::from(rows))
}

fn entry(key: &str, title: &str, state: i32) -> DownloadEntry {
    DownloadEntry {
        key: key.into(),
        series_key: "fixture:series:lantern".into(),
        artwork_key: "fixture:cover:lantern".into(),
        title: title.into(),
        series_title: "The Last Lantern".into(),
        source: "Fixture source".into(),
        kind: 1,
        state,
        total_pages: 24,
        done_pages: if state == 2 { 14 } else { 24 },
        progress: if state == 2 { 14. / 24. } else { 1. },
        meta: "24 pages".into(),
        size_label: "42 MB".into(),
        read_meta: "Unread".into(),
        rate_label: "~2.4 MB/s".into(),
        ..Default::default()
    }
}

fn saved_rows() -> Vec<DownloadEntry> {
    vec![
        DownloadEntry {
            kind: 0,
            expanded: true,
            meta: "2 chapters".into(),
            size_label: "84 MB".into(),
            ..entry("fixture:series:lantern", "The Last Lantern", 0)
        },
        entry("fixture:chapter:32", "Chapter 32", 0),
        entry("fixture:chapter:31", "Chapter 31", 0),
    ]
}

fn queue_rows() -> Vec<DownloadEntry> {
    vec![
        entry("fixture:chapter:33", "Chapter 33", 2),
        entry("fixture:chapter:34", "Chapter 34", 1),
        DownloadEntry {
            error: "Connection interrupted".into(),
            ..entry("fixture:chapter:35", "Chapter 35", 4)
        },
    ]
}

fn scene(width: f32, height: f32) -> AppWindow {
    let ui = AppWindow::new().unwrap();
    ui.global::<Anim>().set_enabled(false);
    ui.set_downloaded_rows(model(saved_rows()));
    ui.set_download_queue(model(queue_rows()));
    ui.set_downloaded_count(2);
    ui.set_download_queue_count(3);
    ui.set_download_summary("1 title · 2 chapters · 84 MB".into());
    ui.set_download_queue_summary("1 downloading · 1 waiting · 1 needs attention".into());
    ui.set_section(7);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(width, height));
    settle();
    ui
}

fn assert_inside(element: &ElementHandle, width: f32) {
    let position = element.absolute_position();
    let size = element.size();
    assert!(
        size.width > 0.,
        "element has no width: {:?}",
        element.type_name()
    );
    assert!(position.x >= -0.5, "left overflow at {position:?}");
    assert!(
        position.x + size.width <= width + 0.5,
        "right overflow: {position:?} + {size:?}, viewport {width}"
    );
}

#[test]
fn downloads_responsive_panes_and_actions_preserve_stable_keys() {
    i_slint_backend_testing::init_no_event_loop();
    for (width, height) in [(320., 640.), (390., 844.), (1120., 780.)] {
        let ui = scene(width, height);
        let actions = Rc::new(RefCell::new(Vec::new()));
        ui.on_download_action({
            let actions = actions.clone();
            move |key, action| {
                actions
                    .borrow_mut()
                    .push((key.to_string(), action.to_string()))
            }
        });
        let saved = ElementHandle::find_by_element_type_name(&ui, "SavedRow")
            .next()
            .expect("saved series is rendered");
        assert_inside(&saved, width);
        for label in [
            "Select chapter",
            "Delete download",
            "Expand chapters",
            "Collapse chapters",
        ] {
            for element in ElementHandle::find_by_accessible_label(&ui, label) {
                assert_inside(&element, width);
                assert!(element.size().width >= 48. && element.size().height >= 48.);
            }
        }
        activate(&ui, "The Last Lantern");
        activate(&ui, "Chapter 32");
        assert_eq!(
            &*actions.borrow(),
            &[
                ("fixture:series:lantern".into(), "toggle-series".into()),
                ("fixture:chapter:32".into(), "open".into()),
            ]
        );
        if width < 920. {
            assert_eq!(
                ElementHandle::find_by_element_type_name(&ui, "QueueRow").count(),
                0
            );
            activate(&ui, "Queue 3");
            assert_eq!(
                ElementHandle::find_by_element_type_name(&ui, "SavedRow").count(),
                0
            );
            let queued = ElementHandle::find_by_element_type_name(&ui, "QueueRow")
                .next()
                .unwrap();
            assert_inside(&queued, width);
            for label in ["Pause download", "Cancel download", "Retry download"] {
                let element = button(&ui, label);
                assert_inside(&element, width);
                assert!(element.size().width >= 48. && element.size().height >= 48.);
            }
            activate(&ui, "Downloaded 2");
            assert!(
                ElementHandle::find_by_element_type_name(&ui, "SavedRow")
                    .next()
                    .is_some()
            );
        } else {
            let queued = ElementHandle::find_by_element_type_name(&ui, "QueueRow")
                .next()
                .unwrap();
            assert_inside(&queued, width);
            assert!(
                saved.absolute_position().x + saved.size().width < queued.absolute_position().x
            );
            assert!(
                saved.size().width > queued.size().width * 1.4,
                "desktop gives more room to the saved collection"
            );
        }
    }
}

#[test]
fn downloads_search_filters_selection_and_empty_states() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = scene(390., 844.);
    let searches = Rc::new(RefCell::new(Vec::new()));
    let filters = Rc::new(RefCell::new(Vec::new()));
    let actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_download_search({
        let searches = searches.clone();
        move |query| searches.borrow_mut().push(query.to_string())
    });
    ui.on_download_filter({
        let filters = filters.clone();
        move |unread, sort| filters.borrow_mut().push((unread, sort))
    });
    ui.on_download_action({
        let weak = ui.as_weak();
        let actions = actions.clone();
        move |key, action| {
            actions
                .borrow_mut()
                .push((key.to_string(), action.to_string()));
            let ui = weak.unwrap();
            if action == "select" {
                let mut rows = saved_rows();
                rows[1].selected = true;
                ui.set_downloaded_rows(model(rows));
                ui.set_download_selected_count(1);
                ui.set_download_selected_size("42 MB".into());
            }
        }
    });
    let search = ElementHandle::find_by_accessible_label(&ui, "Search titles or chapters")
        .find(|element| element.accessible_role() == Some(AccessibleRole::TextInput))
        .unwrap();
    search.set_accessible_value("Lantern");
    settle();
    assert_eq!(&*searches.borrow(), &["Lantern"]);
    activate(&ui, "Unread");
    activate(&ui, "All");
    assert_eq!(&*filters.borrow(), &[(true, 0), (false, 0)]);
    activate(&ui, "Select chapter");
    button(&ui, "Deselect chapter");
    activate(&ui, "Delete 42 MB");
    assert_eq!(
        actions.borrow()[0],
        ("fixture:chapter:32".into(), "select".into())
    );
    assert_eq!(actions.borrow()[1], ("".into(), "delete-selected".into()));
    ui.set_downloaded_rows(model(vec![DownloadEntry {
        local_import: true,
        ..entry("local:sole-copy", "Local chapter", 0)
    }]));
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Delete download")
            .next()
            .is_none(),
        "local imports have no delete control"
    );
    ui.set_download_queue(model(Vec::new()));
    ui.set_download_queue_count(0);
    activate(&ui, "Queue 0");
    assert!(
        ElementHandle::find_by_accessible_label(
            &ui,
            "Your queue is clear. New chapter downloads will appear here."
        )
        .next()
        .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Pause all")
            .next()
            .is_none()
    );
}

#[test]
fn downloads_queue_controls_and_keyboard_reorder() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = scene(390., 844.);
    activate(&ui, "Queue 3");
    let actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_download_action({
        let actions = actions.clone();
        move |key, action| {
            actions
                .borrow_mut()
                .push((key.to_string(), action.to_string()))
        }
    });
    activate(&ui, "Pause download");
    activate(&ui, "Retry download");
    activate(&ui, "Cancel download");
    assert_eq!(
        &*actions.borrow(),
        &[
            ("fixture:chapter:33".into(), "pause".into()),
            ("fixture:chapter:35".into(), "retry".into()),
            ("fixture:chapter:33".into(), "cancel".into()),
        ]
    );
    let handle = button(
        &ui,
        "Reorder download. Control+Up moves earlier; Control+Down moves later.",
    );
    handle.mock_single_click(PointerEventButton::Left);
    settle();
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::DownArrow.into(),
    });
    ui.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::DownArrow.into(),
    });
    ui.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
    settle();
    assert_eq!(
        actions.borrow().last().unwrap(),
        &("fixture:chapter:34".into(), "move-down".into())
    );
    handle.invoke_accessible_default_action();
    settle();
    activate(&ui, "Move earlier");
    assert_eq!(
        actions.borrow().last().unwrap(),
        &("fixture:chapter:34".into(), "move-up".into())
    );
    let reorders = Rc::new(RefCell::new(Vec::new()));
    ui.on_download_reorder({
        let reorders = reorders.clone();
        move |key, before| {
            reorders
                .borrow_mut()
                .push((key.to_string(), before.to_string()))
        }
    });
    ui.set_download_queue(model(vec![
        entry("fixture:chapter:34", "Chapter 34", 1),
        entry("fixture:chapter:37", "Chapter 37", 1),
    ]));
    settle();
    let target = ElementHandle::find_by_element_type_name(&ui, "QueueRow")
        .nth(1)
        .unwrap()
        .absolute_position();
    button(
        &ui,
        "Reorder download. Control+Up moves earlier; Control+Down moves later.",
    )
    .mock_drag(
        LogicalPosition::new(target.x + 22., target.y + 24.),
        PointerEventButton::Left,
    );
    settle();
    assert_eq!(
        &*reorders.borrow(),
        &[("fixture:chapter:34".into(), "fixture:chapter:37".into())]
    );
    for (state, label, expected) in [
        (3, "Resume download", "resume"),
        (5, "Delete cancelled download", "delete"),
    ] {
        ui.set_download_queue(model(vec![entry(
            "fixture:chapter:36",
            "Chapter 36",
            state,
        )]));
        settle();
        activate(&ui, label);
        assert_eq!(
            actions.borrow().last().unwrap(),
            &("fixture:chapter:36".into(), expected.into())
        );
    }
}

#[test]
#[ignore = "opt-in production AppWindow software raster previews"]
fn render_downloads_previews() {
    if std::env::var("NOVA_READER_DOWNLOADS_PREVIEW").as_deref() != Ok("1") {
        eprintln!("Set NOVA_READER_DOWNLOADS_PREVIEW=1 to render synthetic Downloads scenes");
        return;
    }
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            mock_time: true,
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let storage = tempfile::tempdir().unwrap();
    nova_storage::init_at(storage.path());
    let ui = scene(390., 844.);
    nova_reader::appearance::init(&ui).unwrap();
    ui.global::<Anim>().set_enabled(false);
    let source = PathBuf::from(
        std::env::var("NOVA_READER_DOWNLOADS_PREVIEW_IMAGE").unwrap_or_else(|_| {
            "/workspace/artifacts/reader-preview-little-nemo-1905-10-15.jpg".into()
        }),
    );
    let poster = if source.exists() {
        let decoded = image::open(&source).unwrap().to_rgb8();
        slint::Image::from_rgb8(slint::SharedPixelBuffer::clone_from_slice(
            decoded.as_raw(),
            decoded.width(),
            decoded.height(),
        ))
    } else {
        slint::Image::default()
    };
    let mut saved = saved_rows();
    let mut queue = queue_rows();
    for entry in saved.iter_mut().chain(queue.iter_mut()) {
        entry.poster = poster.clone();
    }
    ui.set_downloaded_rows(model(saved));
    ui.set_download_queue(model(queue));
    let output = PathBuf::from(
        std::env::var("NOVA_READER_DOWNLOADS_PREVIEW_DIR")
            .unwrap_or_else(|_| "/workspace/artifacts/reader-downloads-preview".into()),
    );
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(output.join("preview-source.txt"), format!("Production AppWindow rendered with its native Slint software backend.\nTitles, chapters, sizes, page counts and transfer rates are synthetic visual fixtures, not live downloads.\nCover source when present: {}\nDefault image: Winsor McCay, Little Nemo in Slumberland, October 15, 1905 (public domain). https://commons.wikimedia.org/wiki/File:Little_Nemo_1905-10-15.jpg\n", source.display())).unwrap();
    for (width, height, tab, name) in [
        (390., 844., 0, "phone-downloaded"),
        (390., 844., 1, "phone-queue"),
        (1120., 780., 0, "desktop-downloads"),
    ] {
        ui.window().set_size(LogicalSize::new(width, height));
        ui.set_download_tab(tab);
        settle();
        save_preview(&ui, &output, name);
    }
    ui.window().set_size(LogicalSize::new(390., 844.));
    ui.set_series_title("The Last Lantern".into());
    ui.set_series_poster(poster);
    ui.set_series_metadata("Synthetic preview · Fixture source".into());
    ui.set_resume_chapter("32".into());
    ui.set_items(ModelRc::new(VecModel::from(vec![
        ItemRow {
            title: "Chapter 32".into(),
            chapter_meta: "24 pages · Preview fixture".into(),
            downloaded: true,
            has_download: true,
            download_state: 0,
            ..Default::default()
        },
        ItemRow {
            title: "Chapter 31".into(),
            chapter_meta: "Preview fixture".into(),
            read: true,
            ..Default::default()
        },
    ])));
    ui.set_section(5);
    settle();
    save_preview(&ui, &output, "phone-detail");
}

fn save_preview(ui: &AppWindow, output: &std::path::Path, name: &str) {
    let snapshot = ui.window().take_snapshot().expect("native software pixels");
    image::save_buffer(
        output.join(format!("{name}.png")),
        snapshot.as_bytes(),
        snapshot.width(),
        snapshot.height(),
        image::ColorType::Rgba8,
    )
    .unwrap();
    eprintln!(
        "Downloads preview: {}",
        output.join(format!("{name}.png")).display()
    );
}
