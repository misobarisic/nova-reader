//! Exercise actual Window safe-area values, including a persistent camera cutout.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use i_slint_core::{lengths::LogicalEdges, window::WindowInner};
use nova_reader::{AppWindow, DownloadEntry, ItemRow, PageRow, TileRow};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};
use std::time::Duration;

fn settle() {
    for _ in 0..24 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}
fn insets(ui: &AppWindow, top: f32, bottom: f32, left: f32, right: f32) {
    WindowInner::from_pub(ui.window())
        .set_window_item_safe_area(LogicalEdges::new(top, bottom, left, right));
    settle();
}
fn element(ui: &AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(ui, id)
        .next()
        .unwrap_or_else(|| panic!("missing {id}"))
}
fn button(ui: &AppWindow, name: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, name)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing {name}"))
}
fn geometry(element: &ElementHandle) -> (f32, f32, f32, f32) {
    let position = element.absolute_position();
    let size = element.size();
    (position.x, position.y, size.width, size.height)
}
#[test]
fn reader_art_reaches_status_area_while_controls_and_normal_pages_respect_insets() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    // Exercise Android cutouts, not the desktop-only webtoon column cap.
    ui.set_reader_platform_android(true);
    ui.set_pages(ModelRc::new(VecModel::from(vec![PageRow {
        page_id: "first".into(),
        loaded: true,
        ratio: 4.,
        tiles: ModelRc::new(VecModel::from(vec![TileRow {
            image: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(1, 4)),
            offset: 0.,
            fraction: 1.,
            ..Default::default()
        }])),
        ..Default::default()
    }])));
    ui.set_total_ratio(4.);
    ui.set_section(6);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    settle();
    insets(&ui, 32., 24., 0., 0.);
    let viewport = geometry(&element(&ui, "AppWindow::reader"));
    assert_eq!(viewport, (0., 0., 390., 844.));
    assert_eq!(
        button(&ui, "Page 1 image").absolute_position().y,
        0.,
        "art fills the status/cutout area"
    );
    for _ in 0..3 {
        ui.set_reader_osd(true);
        insets(&ui, 32., 24., 0., 0.);
        assert_eq!(geometry(&element(&ui, "AppWindow::reader")), viewport);
        assert!(button(&ui, "Back").absolute_position().y >= 40.);
        ui.set_reader_osd(false);
        // Some devices retain a cutout inset after hiding the status bar.
        insets(&ui, 32., 24., 0., 0.);
        assert_eq!(geometry(&element(&ui, "AppWindow::reader")), viewport);
        insets(&ui, 0., 24., 0., 0.);
        assert_eq!(geometry(&element(&ui, "AppWindow::reader")), viewport);
    }
    ui.set_scroll_y(-160.);
    settle();
    button(&ui, "Page 1 image").invoke_accessible_default_action();
    settle();
    let capture = ui.get_reader_capture_viewport();
    assert_eq!(
        (capture.x, capture.y, capture.width, capture.height),
        viewport
    );
    assert_eq!(capture.content_y, ui.get_scroll_y());
    ui.set_scroll_y(-200.);
    settle();
    assert_eq!(
        ui.get_reader_capture_viewport(),
        capture,
        "capture framing is frozen before menu opening"
    );
    ui.invoke_close_reader_context();
    ui.set_scroll_y(0.);
    settle();
    insets(&ui, 32., 24., 12., 18.);
    ui.set_section(0);
    settle();
    assert_eq!(
        geometry(&element(&ui, "AppWindow::content-viewport")),
        (12., 32., 360., 788.)
    );
    ui.set_section(5);
    settle();
    assert_eq!(
        geometry(&element(&ui, "AppWindow::content-viewport")),
        (0., 0., 390., 820.)
    );
    assert!(button(&ui, "Back").absolute_position().y >= 42.);
    let scrim = element(&ui, "AppWindow::detail-status-scrim");
    assert_eq!(scrim.absolute_position().y, 0.);
    assert_eq!(scrim.size().height, 96.);
    insets(&ui, 32., 24., 44., 18.);
    ui.set_items(ModelRc::new(VecModel::from(vec![ItemRow {
        title: "Chapter 1".into(),
        chapter_meta: "Today · English".into(),
        ..Default::default()
    }])));
    settle();
    let row = ElementHandle::find_by_element_type_name(&ui, "ChapterRow")
        .next()
        .unwrap();
    assert!(row.absolute_position().x >= 64.);
    assert!(row.absolute_position().x + row.size().width <= 352.);
    for id in ["ChapterRow::chapter-title", "ChapterRow::chapter-metadata"] {
        let text = element(&ui, id);
        assert!(text.absolute_position().x >= 44.);
        assert!(text.absolute_position().x + text.size().width <= 372.);
    }
    for label in ["Download", "Chapter options"] {
        let action = button(&ui, label);
        assert!(action.absolute_position().x >= 44.);
        assert!(action.absolute_position().x + action.size().width <= 372.);
    }
    // Landscape artwork also reaches behind a side camera cutout; OSD buttons
    // keep their independent safe area without narrowing the page itself.
    ui.set_section(6);
    ui.window().set_size(LogicalSize::new(844., 390.));
    settle();
    insets(&ui, 0., 24., 44., 0.);
    assert_eq!(
        geometry(&element(&ui, "AppWindow::reader")),
        (0., 0., 844., 390.)
    );
    assert!(button(&ui, "Page 1 image").absolute_position().x < 44.);
    ui.set_reader_osd(true);
    settle();
    assert!(button(&ui, "Back").absolute_position().x >= 52.);
    assert_eq!(
        geometry(&element(&ui, "AppWindow::reader")),
        (0., 0., 844., 390.)
    );
    // Downloads exposes the same chapter action with usable phone targets.
    let chosen = std::rc::Rc::new(std::cell::RefCell::new(None));
    ui.on_download_action({
        let chosen = chosen.clone();
        move |key, action| *chosen.borrow_mut() = Some((key.to_string(), action.to_string()))
    });
    ui.set_downloaded_rows(ModelRc::new(VecModel::from(vec![DownloadEntry {
        key: "fixture:download".into(),
        kind: 1,
        title: "Downloaded chapter".into(),
        ..Default::default()
    }])));
    ui.set_section(7);
    for width in [320., 390.] {
        ui.window().set_size(LogicalSize::new(width, 844.));
        insets(&ui, 32., 24., 0., 0.);
        let delete = button(&ui, "Delete download");
        assert!(delete.size().height >= 48.);
        assert!(delete.absolute_position().x >= 0.);
        assert!(delete.absolute_position().x + delete.size().width <= width);
        delete.mock_single_click(slint::platform::PointerEventButton::Left);
        assert_eq!(
            *chosen.borrow(),
            Some(("fixture:download".to_owned(), "delete".to_owned()))
        );
    }
    ui.set_downloaded_rows(ModelRc::new(VecModel::from(vec![DownloadEntry {
        key: "fixture:import".into(),
        kind: 1,
        title: "Imported chapter".into(),
        local_import: true,
        ..Default::default()
    }])));
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Delete download")
            .next()
            .is_none()
    );
}
