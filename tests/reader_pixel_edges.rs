//! Exercise actual Slint raster geometry and pixels, including fractional scroll.
//! Logical page positions remain unrounded; only shared drawing edges snap.
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use nova_reader::{AppWindow, PageRow, TileRow};
use slint::{ComponentHandle, LogicalSize, Model, ModelRc, VecModel};
use std::time::Duration;

fn settle() {
    for _ in 0..3 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}
fn light_image(height: u32) -> slint::Image {
    light_tile(2, height)
}
fn light_tile(width: u32, height: u32) -> slint::Image {
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
    for pixel in buffer.make_mut_slice() {
        *pixel = slint::Rgb8Pixel::new(242, 244, 246);
    }
    slint::Image::from_rgb8(buffer)
}
fn page(position: f32, height: u32) -> PageRow {
    let tiles = (0..height)
        .step_by(2048)
        .map(|start| {
            let rows = 2048.min(height - start);
            TileRow {
                image: light_image(rows),
                offset: start as f32 / height as f32,
                fraction: rows as f32 / height as f32,
                ..Default::default()
            }
        })
        .collect::<Vec<_>>();
    PageRow {
        page_id: format!("fixture-{height}").into(),
        loaded: true,
        position,
        ratio: height as f32 / 513.,
        tiles: ModelRc::new(VecModel::from(tiles)),
        ..Default::default()
    }
}
fn element(ui: &AppWindow, name: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(ui, name)
        .next()
        .unwrap_or_else(|| panic!("missing {name}"))
}
fn tiles(ui: &AppWindow) -> Vec<ElementHandle> {
    ElementHandle::find_by_element_id(ui, "AppWindow::reader-tile").collect()
}
fn assert_device_edge(logical: f32, dpr: f32) {
    let physical = logical * dpr;
    assert!(
        (physical - physical.round()).abs() < 0.025,
        "edge {logical} at DPR {dpr} lies at physical {physical}"
    );
}
#[test]
fn reader_shared_edges_cover_fractional_scroll_at_each_device_scale() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        renderer_name: Some("software".into()),
        ..Default::default()
    })))
    .unwrap();
    let ui = AppWindow::new().unwrap();
    let first = page(0., 6201);
    let second = page(first.ratio, 5179);
    let third = page(second.position + second.ratio, 7307);
    let total = third.position + third.ratio;
    ui.set_pages(ModelRc::new(VecModel::from(vec![
        first.clone(),
        second.clone(),
        third.clone(),
    ])));
    ui.set_total_ratio(total);
    ui.set_section(6);
    ui.set_reader_osd(false);
    ui.show().unwrap();

    for dpr in [1., 1.25, 1.5, 2.75, 3.] {
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: dpr });
        // The testing adapter's set_size assumes DPR 1. Supply its physical
        // surface size, then the logical resize event a real backend emits.
        let physical = slint::PhysicalSize::new((390. * dpr) as u32, (300. * dpr) as u32);
        ui.window().set_size(physical);
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::Resized {
                size: LogicalSize::new(physical.width as f32 / dpr, physical.height as f32 / dpr),
            });
        for zoom in [0.83, 1., 1.17] {
            ui.set_zoom(zoom);
            settle();
            let width = ui.get_reader_width();
            let viewport = element(&ui, "AppWindow::reader");
            // Internal 2048-row tile joins and the actual adjacent-page join.
            for boundary in [
                2048. / 513. * width,
                4096. / 513. * width,
                first.ratio * width,
                (second.position + 2048. / 513.) * width,
                third.position * width,
            ] {
                for fraction in [0.13, 0.49, 0.77] {
                    let scroll = -boundary + viewport.size().height / 2. + fraction;
                    ui.set_scroll_y(scroll);
                    settle();
                    assert_eq!(
                        ui.get_scroll_y(),
                        scroll,
                        "drawing must not round resume coordinates"
                    );
                    let images = tiles(&ui);
                    // Introspection excludes culled/offscreen images. This
                    // viewport straddles at least the two sides of a join.
                    assert!(images.len() >= 2, "fixture must straddle a tile/page join");
                    for image in &images {
                        let pos = image.absolute_position();
                        let size = image.size();
                        assert_device_edge(pos.x, dpr);
                        assert_device_edge(pos.x + size.width, dpr);
                        assert_device_edge(pos.y, dpr);
                        assert_device_edge(pos.y + size.height, dpr);
                    }
                    for pair in images.windows(2) {
                        let bottom = pair[0].absolute_position().y + pair[0].size().height;
                        let top = pair[1].absolute_position().y;
                        assert!(
                            (bottom - top).abs() * dpr < 0.025,
                            "tile/page join has a gap or overlap at DPR {dpr}, zoom {zoom}, scroll {scroll}: {bottom} vs {top}"
                        );
                    }
                    let pixels = ui
                        .window()
                        .take_snapshot()
                        .expect("software raster snapshot");
                    let column = pixels.width() / 2;
                    let top = (viewport.absolute_position().y * dpr).ceil().max(0.) as u32 + 1;
                    let bottom = ((viewport.absolute_position().y + viewport.size().height) * dpr)
                        .floor() as u32;
                    for row in top..bottom.min(pixels.height()).saturating_sub(1) {
                        let pixel = pixels.as_slice()[(row * pixels.width() + column) as usize];
                        assert!(
                            pixel.r >= 220 && pixel.g >= 220 && pixel.b >= 220,
                            "canvas seam at physical row {row}, DPR {dpr}, zoom {zoom}, scroll {scroll}: {pixel:?}"
                        );
                    }
                }
            }
            assert_eq!(ui.get_pages().row_data(0).unwrap().position, 0.);
            assert_eq!(ui.get_pages().row_data(1).unwrap().position, first.ratio);
        }
        // Paged modes fit before zooming, then center within their local pan
        // extent. The nominal Webtoon canvas width must not shift a slim page.
        for mode in [1, 2, 3] {
            ui.set_reading_mode(mode);
            ui.set_current_page(0);
            ui.set_scroll_y(0.);
            settle();
            let page_image = ElementHandle::find_by_accessible_label(&ui, "Page 1 image")
                .next()
                .unwrap();
            let size = page_image.size();
            let position = page_image.absolute_position();
            let viewport = element(&ui, "AppWindow::reader");
            assert!((size.height / size.width - first.ratio).abs() < 0.001);
            let fitted = ui
                .get_reader_fit_width()
                .min(viewport.size().height / first.ratio);
            assert!((size.width - fitted * ui.get_zoom()).abs() < 0.01);
            assert!(
                (position.x + size.width / 2.
                    - viewport.absolute_position().x
                    - ui.get_scroll_x()
                    - viewport.size().width.max(size.width) / 2.)
                    .abs()
                    < 0.01
            );
            assert!(
                (position.y + size.height / 2.
                    - viewport.absolute_position().y
                    - ui.get_scroll_y()
                    - viewport.size().height.max(size.height) / 2.)
                    .abs()
                    < 0.01
            );
            let images = tiles(&ui);
            for image in images.iter().take(4) {
                assert_device_edge(image.absolute_position().y, dpr);
            }
        }
        ui.set_reading_mode(0);
    }
    // A real 719×3500 source must fill the phone viewport horizontally at
    // zoom 1, even when its native raster is narrower than the physical screen.
    for dpr in [1., 1.25, 2.75, 3.] {
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: dpr });
        let physical = slint::PhysicalSize::new((390. * dpr) as u32, (844. * dpr) as u32);
        ui.window().set_size(physical);
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::Resized {
                size: LogicalSize::new(physical.width as f32 / dpr, physical.height as f32 / dpr),
            });
        ui.set_zoom(1.);
        ui.set_scroll_y(0.);
        let ratio = 3500. / 719.;
        let rows = [0u32, 2048]
            .into_iter()
            .map(|top| TileRow {
                image: light_tile(719, 2048.min(3500 - top)),
                offset: top as f32 / 3500.,
                fraction: 2048.min(3500 - top) as f32 / 3500.,
                bottom: (top + 2048.min(3500 - top)) as f32 / 3500.,
                right: 1.,
                ..Default::default()
            })
            .collect::<Vec<_>>();
        ui.set_pages(ModelRc::new(VecModel::from(vec![PageRow {
            loaded: true,
            ratio,
            tiles: ModelRc::new(VecModel::from(rows)),
            ..Default::default()
        }])));
        ui.set_total_ratio(ratio);
        settle();
        let viewport = element(&ui, "AppWindow::reader");
        assert!((ui.get_reader_width() - viewport.size().width).abs() < 0.01);
        for tile in tiles(&ui) {
            assert!(
                (tile.absolute_position().x - viewport.absolute_position().x).abs() * dpr < 0.025
            );
            assert!((tile.size().width - viewport.size().width).abs() * dpr < 0.025);
        }
        let snapshot = ui.window().take_snapshot().unwrap();
        if let Ok(directory) = std::env::var("NOVA_READER_VIEWER_PREVIEW_DIR") {
            std::fs::create_dir_all(&directory).unwrap();
            image::save_buffer(
                std::path::Path::new(&directory).join(format!("719x3500-dpr{dpr}.png")),
                snapshot.as_bytes(),
                snapshot.width(),
                snapshot.height(),
                image::ColorType::Rgba8,
            )
            .unwrap();
        }
        let row = snapshot.height() / 3;
        for column in [0, snapshot.width() - 1] {
            let pixel = snapshot.as_slice()[(row * snapshot.width() + column) as usize];
            assert!(
                pixel.r >= 220 && pixel.g >= 220 && pixel.b >= 220,
                "719×3500 must reach horizontal edge {column} at DPR {dpr}: {pixel:?}"
            );
        }
    }
    // A visible crop of a wide, tall original is three horizontal textures.
    // Its last row must stop at the crop's own endpoint, never stretch to the
    // logical page bottom; neighbouring columns must share physical edges too.
    ui.set_zoom(1.);
    let partial = (0..5000)
        .step_by(2048)
        .map(|left| {
            let columns = 2048.min(5000 - left);
            TileRow {
                image: light_tile(columns, 1500),
                offset: 0.25,
                fraction: 0.075,
                left: left as f32 / 5000.,
                right: (left + columns) as f32 / 5000.,
                bottom: 0.325,
            }
        })
        .collect::<Vec<_>>();
    ui.set_pages(ModelRc::new(VecModel::from(vec![PageRow {
        page_id: "partial-wide".into(),
        loaded: true,
        ratio: 4.,
        tiles: ModelRc::new(VecModel::from(partial)),
        ..Default::default()
    }])));
    ui.set_total_ratio(4.);
    settle();
    ui.set_scroll_y(40. - ui.get_reader_width());
    settle();
    let images = tiles(&ui);
    assert_eq!(images.len(), 3);
    for image in &images {
        let position = image.absolute_position();
        let size = image.size();
        assert_device_edge(position.x, 3.);
        assert_device_edge(position.x + size.width, 3.);
        assert_device_edge(position.y, 3.);
        assert_device_edge(position.y + size.height, 3.);
        assert!(
            (size.height - ui.get_reader_width() * 0.3).abs() < 1.,
            "partial window was stretched to page bottom"
        );
    }
    for pair in images.windows(2) {
        assert!(
            (pair[0].absolute_position().x + pair[0].size().width - pair[1].absolute_position().x)
                .abs()
                * 3.
                < 0.025
        );
        assert_eq!(pair[0].absolute_position().y, pair[1].absolute_position().y);
    }
    let snapshot = ui.window().take_snapshot().unwrap();
    let row = ((images[0].absolute_position().y + images[0].size().height / 2.) * 3.) as u32;
    let first = (images[0].absolute_position().x * 3.).ceil() as u32 + 1;
    let last = ((images[2].absolute_position().x + images[2].size().width) * 3.).floor() as u32;
    for column in first..last.saturating_sub(1) {
        let pixel = snapshot.as_slice()[(row * snapshot.width() + column) as usize];
        assert!(
            pixel.r >= 220 && pixel.g >= 220 && pixel.b >= 220,
            "horizontal tile seam at {column}: {pixel:?}"
        );
    }
}
