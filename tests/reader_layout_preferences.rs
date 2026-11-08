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

#[test]
fn compact_mode_label_uses_the_effective_mode_and_aligned_action_slots() {
    use i_slint_backend_testing::AccessibleRole;
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    ui.show().unwrap();
    settle();
    ui.set_reader_osd(true);
    settle();
    for width in [320., 390.] {
        ui.window().set_size(LogicalSize::new(width, 844.));
        settle();
        // Automatic is already resolved by the controller before this label is
        // published; its effective Webtoon/LTR/RTL label uses the same slot.
        assert_eq!(
            ElementHandle::find_by_accessible_label(&ui, "More")
                .filter(|control| control.accessible_role() == Some(AccessibleRole::Button))
                .count(),
            1,
            "compact controls keep only the bottom More action"
        );
        for mode in [
            "Webtoon",
            "Left to right",
            "Right to left",
            "Long strip with gaps",
        ] {
            ui.set_reader_mode_label(mode.into());
            settle();
            let mut top: Option<f32> = None;
            for glyph in ["chapters", "mode", "settings", "more"] {
                let control = ElementHandle::find_by_element_type_name(&ui, "AddonIconButton")
                    .find(|control| {
                        control.accessible_id().as_deref()
                            == Some(format!("reader-osd-action:{glyph}:labeled").as_str())
                    })
                    .expect("labeled reader action");
                let (x, y, w, h) = bounds(&control);
                assert!(x >= 0. && x + w <= width && w >= 44. && h >= 44.);
                if let Some(top) = top {
                    assert!((y - top).abs() <= 0.5, "mode and sibling icon slots align");
                } else {
                    top = Some(y);
                }
                if glyph == "mode" {
                    assert_eq!(control.accessible_label().as_deref(), Some(mode));
                    assert_eq!(control.accessible_role(), Some(AccessibleRole::Button));
                    control.invoke_accessible_default_action();
                    settle();
                    ElementHandle::find_by_accessible_label(&ui, "Close reader settings")
                        .next()
                        .expect("effective mode opens the same picker")
                        .invoke_accessible_default_action();
                    settle();
                }
            }
            assert!(
                ElementHandle::find_by_accessible_label(&ui, "Mode")
                    .next()
                    .is_none()
            );
        }
    }
    ui.window().set_size(LogicalSize::new(1280., 900.));
    settle();
    for glyph in ["chapters", "settings", "fullscreen"] {
        let control = ElementHandle::find_by_element_type_name(&ui, "AddonIconButton")
            .find(|control| {
                control.accessible_id().as_deref()
                    == Some(format!("reader-osd-action:{glyph}:labeled").as_str())
            })
            .expect("desktop reader action");
        let (x, _, w, h) = bounds(&control);
        assert!(x >= 0. && x + w <= 1280. && w >= 44. && h >= 44.);
    }
    // Wide windows give spare width to the spacer only: the labelled actions keep
    // their 76px slots and stay grouped against the trailing gutter.
    for width in [1280., 1920.] {
        ui.window().set_size(LogicalSize::new(width, 900.));
        settle();
        let xs: Vec<f32> = [
            "fit-width",
            "zoom",
            "screencap",
            "pages",
            "chapters",
            "settings",
            "fullscreen",
        ]
        .iter()
        .map(|glyph| {
            let control = ElementHandle::find_by_element_type_name(&ui, "AddonIconButton")
                .find(|control| {
                    control.accessible_id().as_deref()
                        == Some(format!("reader-osd-action:{glyph}:labeled").as_str())
                })
                .expect("desktop reader action");
            bounds(&control).0
        })
        .collect();
        for pair in xs.windows(2) {
            assert!(
                (pair[1] - pair[0] - 76.).abs() <= 0.5,
                "labelled actions are packed at {width}px: {xs:?}"
            );
        }
        // The last 44px icon is centred in a 76px slot that ends at the 18px gutter.
        let last = xs[xs.len() - 1];
        assert!(
            (last + 44. + 16. - (width - 18.)).abs() <= 0.5,
            "actions end at the trailing gutter at {width}px: {xs:?}"
        );
    }
}
