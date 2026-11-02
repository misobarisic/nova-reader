//! Actual finger/mouse input exercises the gallery independently of tap zones.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, mock_elapsed_time};
use i_slint_core::{input::TouchPhase, lengths::LogicalPoint, platform::InternalEvent};
use nova_reader::{AppWindow, PageRow, ReaderSettings, TileRow};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, LogicalSize, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn advance(ms: u64) {
    for _ in 0..ms / 10 {
        mock_elapsed_time(Duration::from_millis(10));
    }
}
fn input(ui: &AppWindow, touch: bool, phase: TouchPhase, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    ui.window().dispatch_event(if touch {
        WindowEvent::internal(InternalEvent::Touch {
            id: 1,
            position: LogicalPoint::new(x, y),
            phase,
        })
    } else {
        match phase {
            TouchPhase::Started => WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            },
            TouchPhase::Ended => WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            },
            _ => WindowEvent::PointerMoved { position },
        }
    });
}
fn artwork(ui: &AppWindow, index: i32) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, &format!("Page {} image", index + 1))
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap()
}
fn scene(width: f32, height: f32) -> AppWindow {
    let ui = AppWindow::new().unwrap();
    ui.set_reader_platform_android(true);
    ui.global::<ReaderSettings>().set_page_animations(true);
    ui.set_pages(ModelRc::new(VecModel::from(
        (0..3)
            .map(|index| PageRow {
                page_id: format!("neutral-{index}").into(),
                position: index as f32 * 1.5,
                ratio: 1.5,
                loaded: true,
                tiles: ModelRc::new(VecModel::from(vec![TileRow {
                    image: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(720, 1080)),
                    fraction: 1.,
                    ..Default::default()
                }])),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_section(6);
    ui.set_current_page(1);
    ui.set_total_ratio(4.5);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(width, height));
    advance(100);
    ui
}

#[test]
fn pagers_follow_drag_and_settle_in_reading_direction_without_taps_or_holds() {
    i_slint_backend_testing::init_no_event_loop();
    for (width, height) in [(390., 844.), (1120., 780.)] {
        for touch in [true, false] {
            let ui = scene(width, height);
            let steps = Rc::new(RefCell::new(Vec::new()));
            ui.on_page_step({
                let steps = steps.clone();
                let weak = ui.as_weak();
                move |step| {
                    steps.borrow_mut().push(step);
                    let ui = weak.unwrap();
                    ui.set_current_page(ui.get_current_page() + step);
                }
            });
            let taps = Rc::new(RefCell::new(0));
            ui.on_reader_tap({
                let taps = taps.clone();
                move |_, _| *taps.borrow_mut() += 1
            });
            for mode in [1, 2, 3] {
                ui.set_reading_mode(mode);
                ui.set_current_page(1);
                advance(100);
                let origin = artwork(&ui, 1).absolute_position();
                let x = width / 2.;
                let y = height / 2.;
                let (dx, dy) = if mode == 3 { (0., -180.) } else { (-180., 0.) };
                input(&ui, touch, TouchPhase::Started, x, y);
                advance(60);
                input(&ui, touch, TouchPhase::Moved, x + dx / 2., y + dy / 2.);
                advance(20);
                let dragged = artwork(&ui, 1).absolute_position();
                assert!(
                    if mode == 3 {
                        dragged.y < origin.y - 40.
                    } else {
                        dragged.x < origin.x - 40.
                    },
                    "artwork follows the pointer: mode={mode} touch={touch} origin={origin:?} dragged={dragged:?}"
                );
                input(&ui, touch, TouchPhase::Moved, x + dx, y + dy);
                input(&ui, touch, TouchPhase::Ended, x + dx, y + dy);
                advance(400);
                let expected = if mode == 2 { -1 } else { 1 };
                assert_eq!(
                    steps.borrow().last(),
                    Some(&expected),
                    "mode={mode} touch={touch}"
                );
                assert_eq!(ui.get_current_page(), 1 + expected);
                assert_eq!(*taps.borrow(), 0, "dragging never invokes tap zones");
                assert!(!ui.get_reader_context_visible());
                let current = artwork(&ui, ui.get_current_page()).absolute_position();
                assert!((current.x - origin.x).abs() < 1. && (current.y - origin.y).abs() < 1.);
                input(&ui, touch, TouchPhase::Started, x, y);
                advance(60);
                input(&ui, touch, TouchPhase::Moved, x - dx, y - dy);
                input(&ui, touch, TouchPhase::Ended, x - dx, y - dy);
                advance(400);
                assert_eq!(steps.borrow().last(), Some(&-expected));
                assert_eq!(
                    ui.get_current_page(),
                    1,
                    "opposite drag returns to the middle"
                );
            }

            ui.set_reading_mode(1);
            ui.set_current_page(1);
            advance(100);
            let before = steps.borrow().len();
            let (x, y) = (width / 2., height / 2.);
            input(&ui, touch, TouchPhase::Started, x, y);
            advance(60);
            input(&ui, touch, TouchPhase::Moved, x - 25., y);
            input(&ui, touch, TouchPhase::Ended, x - 25., y);
            advance(400);
            assert_eq!(steps.borrow().len(), before, "short drags spring back");
            input(&ui, touch, TouchPhase::Started, x, y);
            advance(60);
            input(&ui, touch, TouchPhase::Moved, x, y - 180.);
            input(&ui, touch, TouchPhase::Ended, x, y - 180.);
            advance(400);
            assert_eq!(
                steps.borrow().len(),
                before,
                "cross-axis drags do not turn pages"
            );

            for (page, delta) in [(0, 180.), (2, -180.)] {
                ui.set_current_page(page);
                advance(100);
                input(&ui, touch, TouchPhase::Started, x, y);
                advance(60);
                input(&ui, touch, TouchPhase::Moved, x + delta, y);
                input(&ui, touch, TouchPhase::Ended, x + delta, y);
                advance(400);
                assert_eq!(ui.get_current_page(), page, "gallery edges spring back");
                assert_eq!(steps.borrow().len(), before);
            }
            ui.set_current_page(1);
            // Zoom gives the Flickable real image extent instead of paging.
            ui.set_zoom(2.);
            advance(100);
            let old_width = ui.get_reader_paged_width();
            assert!(old_width > width.min(height / 1.5) * 1.8);
            input(&ui, touch, TouchPhase::Started, x, y);
            advance(60);
            input(&ui, touch, TouchPhase::Moved, x - 100., y - 100.);
            advance(30);
            input(&ui, touch, TouchPhase::Moved, x - 180., y - 180.);
            input(&ui, touch, TouchPhase::Ended, x - 180., y - 180.);
            advance(400);
            assert_eq!(
                steps.borrow().len(),
                before,
                "zoomed drags pan instead of turning pages"
            );
            assert!(ui.get_scroll_x() < 0. || ui.get_scroll_y() < 0.);
        }
    }
}

#[test]
#[ignore = "manual native paged preview using an external public-domain image"]
fn paged_reader_previews() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        renderer_name: Some("software".into()),
        ..Default::default()
    })))
    .unwrap();
    let storage = tempfile::tempdir().unwrap();
    nova_storage::init_at(storage.path());
    let source = std::env::var("NOVA_READER_PAGER_PREVIEW_IMAGE").unwrap_or_else(|_| {
        "/workspace/artifacts/reader-preview-little-nemo-1905-10-15.jpg".into()
    });
    let decoded = image::open(source).unwrap().to_rgb8();
    let artwork = slint::Image::from_rgb8(slint::SharedPixelBuffer::clone_from_slice(
        decoded.as_raw(),
        decoded.width(),
        decoded.height(),
    ));
    let ui = scene(390., 844.);
    nova_reader::appearance::init(&ui).unwrap();
    ui.set_pages(ModelRc::new(VecModel::from(vec![PageRow {
        page_id: "public-domain-preview".into(),
        loaded: true,
        ratio: decoded.height() as f32 / decoded.width() as f32,
        tiles: ModelRc::new(VecModel::from(vec![TileRow {
            image: artwork,
            fraction: 1.,
            ..Default::default()
        }])),
        ..Default::default()
    }])));
    ui.set_current_page(0);
    ui.set_series_title("Little Nemo".into());
    ui.set_reader_chapter_title("Public-domain preview".into());
    ui.set_reader_local_page(1);
    ui.set_reader_page_count(1);
    let output = std::path::PathBuf::from(
        std::env::var("NOVA_READER_PAGER_PREVIEW_DIR")
            .unwrap_or_else(|_| "/workspace/artifacts/reader-v0127".into()),
    );
    std::fs::create_dir_all(&output).unwrap();
    for (width, height, density, mode, label) in [
        (390., 844., 2.75, 2, "Right to left"),
        (1120., 780., 1., 1, "Left to right"),
    ] {
        ui.window().dispatch_event(WindowEvent::ScaleFactorChanged {
            scale_factor: density,
        });
        ui.window().set_size(slint::PhysicalSize::new(
            (width * density) as u32,
            (height * density) as u32,
        ));
        ui.window().dispatch_event(WindowEvent::Resized {
            size: LogicalSize::new(width, height),
        });
        ui.set_reading_mode(mode);
        ui.set_reader_mode_label(label.into());
        ui.set_reader_osd(true);
        advance(400);
        let snapshot = ui.window().take_snapshot().unwrap();
        image::save_buffer(
            output.join(format!(
                "paged-reader-{}x{}.png",
                width as u32, height as u32
            )),
            snapshot.as_bytes(),
            snapshot.width(),
            snapshot.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}
