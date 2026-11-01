//! Display preferences keep physical page ends and safe viewport geometry intact.
use i_slint_backend_testing::ElementHandle;
use i_slint_core::{lengths::LogicalEdges, window::WindowInner};
use nova_reader::{AppWindow, PageRow, ReaderSettings, TileRow};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};
use std::time::Duration;

fn settle() {
    for _ in 0..4 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}
fn element(ui: &AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(ui, id)
        .next()
        .unwrap_or_else(|| panic!("missing {id}"))
}
fn bounds(element: &ElementHandle) -> (f32, f32, f32, f32) {
    let position = element.absolute_position();
    let size = element.size();
    (position.x, position.y, size.width, size.height)
}
fn pages(width: f32, gap: f32) -> ModelRc<PageRow> {
    let image = slint::Image::from_rgb8(slint::SharedPixelBuffer::new(1, 1));
    ModelRc::new(VecModel::from(
        [0., 0.4 + gap / width]
            .into_iter()
            .enumerate()
            .map(|(index, position)| PageRow {
                page_id: format!("gap-{index}").into(),
                position,
                ratio: 0.4,
                loaded: true,
                tiles: ModelRc::new(VecModel::from(vec![TileRow {
                    image: image.clone(),
                    fraction: 1.,
                    ..Default::default()
                }])),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ))
}

#[test]
fn webtoon_final_tiles_end_before_the_layout_gap() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    ui.set_reading_mode(0);
    ui.set_reader_gap(16.);
    ui.show().unwrap();
    for (width, height) in [(390., 844.), (1120., 780.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        let page_width = ui.get_reader_width();
        ui.set_pages(pages(page_width, 16.));
        ui.set_total_ratio(0.8 + 16. / page_width);
        settle();
        let tiles: Vec<_> =
            ElementHandle::find_by_element_id(&ui, "AppWindow::reader-tile").collect();
        assert_eq!(tiles.len(), 2);
        let first = bounds(&tiles[0]);
        let second = bounds(&tiles[1]);
        assert!(
            (first.3 - 0.4 * page_width).abs() <= 1.,
            "the final image strip must not stretch into the gap: {first:?}"
        );
        assert!(
            (second.1 - first.1 - first.3 - 16.).abs() <= 1.,
            "the configured gap remains empty between actual image ends: {first:?} {second:?}"
        );
        let viewport = bounds(&element(&ui, "AppWindow::reader"));
        ui.set_reader_osd(true);
        settle();
        assert_eq!(
            bounds(&element(&ui, "AppWindow::reader")),
            viewport,
            "revealing controls preserves the reading viewport"
        );
        ui.set_reader_osd(false);
    }
}

#[test]
fn disabling_cutout_extension_reserves_each_inset_once() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    ui.set_reading_mode(0);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    WindowInner::from_pub(ui.window())
        .set_window_item_safe_area(LogicalEdges::new(32., 24., 12., 18.));
    ui.global::<ReaderSettings>().set_cutout(false);
    settle();
    let safe = bounds(&element(&ui, "AppWindow::reader"));
    assert_eq!(safe, (12., 32., 360., 788.));
    assert_eq!(ui.get_reader_width(), 360.);
    ui.set_reader_osd(true);
    settle();
    assert_eq!(bounds(&element(&ui, "AppWindow::reader")), safe);
    ui.set_reader_osd(false);
    ui.global::<ReaderSettings>().set_cutout(true);
    settle();
    assert_eq!(
        bounds(&element(&ui, "AppWindow::reader")),
        (0., 0., 390., 844.)
    );
    assert_eq!(ui.get_reader_width(), 390.);
}
