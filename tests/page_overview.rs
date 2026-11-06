//! The shared production overview keeps a long inventory virtualized and
//! reports source identities rather than presentation positions.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, mock_elapsed_time};
use nova_reader::{Anim, AppWindow, OverviewTile};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, LogicalSize, Model, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..12 {
        mock_elapsed_time(Duration::from_millis(16));
    }
}

fn inventory() -> ModelRc<OverviewTile> {
    ModelRc::new(VecModel::from(
        (0..320)
            .map(|index| OverviewTile {
                page_id: format!("stable-page-{index}").into(),
                page_number: index + 1,
                loading: index != 5,
                error: if index == 5 {
                    "Fixture failure".into()
                } else {
                    "".into()
                },
                tall_preview: index == 2,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ))
}

fn scene(width: f32, height: f32, current: i32) -> AppWindow {
    let ui = AppWindow::new().unwrap();
    ui.global::<Anim>().set_enabled(false);
    ui.set_overview_title("Chapter 12".into());
    ui.set_overview_subtitle("Neutral fixture".into());
    ui.set_overview_tiles(inventory());
    ui.set_overview_current_index(current);
    ui.set_overview_restore_index(current.max(0));
    ui.set_overview_restore_serial(1);
    ui.set_section(23);
    ui.window().set_size(LogicalSize::new(width, height));
    ui.show().unwrap();
    settle();
    ui
}

fn tile(ui: &AppWindow, number: i32) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, &format!("Page {number}"))
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("page {number} should have a visible delegate"))
}

fn key(ui: &AppWindow, key: Key) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    settle();
}

#[test]
fn long_inventory_virtualizes_rows_and_mirrors_columns_without_reindexing() {
    i_slint_backend_testing::init_no_event_loop();
    for (width, height) in [(320., 640.), (390., 844.), (1120., 780.)] {
        let ui = scene(width, height, 127);
        let delegates = ElementHandle::find_by_element_type_name(&ui, "PageThumbnail").count();
        assert!(
            delegates > 0 && delegates < 60,
            "visible delegates: {delegates}"
        );
        assert!(
            ElementHandle::find_by_accessible_label(&ui, "Page 1")
                .next()
                .is_none()
        );
        let before = tile(&ui, 128).absolute_position();
        let next_before = tile(&ui, 129).absolute_position();
        let calls = Rc::new(RefCell::new(Vec::new()));
        ui.on_overview_selected({
            let calls = calls.clone();
            move |id| calls.borrow_mut().push(id.to_string())
        });
        tile(&ui, 128).invoke_accessible_default_action();
        assert_eq!(*calls.borrow(), ["stable-page-127"]);
        ui.set_overview_rtl(true);
        settle();
        let after = tile(&ui, 128).absolute_position();
        let next_after = tile(&ui, 129).absolute_position();
        if (before.y - next_before.y).abs() < 1. {
            assert!(before.x < next_before.x && after.x > next_after.x);
        }
        assert!((before.y - after.y).abs() < 1.);
        tile(&ui, 128).invoke_accessible_default_action();
        assert_eq!(calls.borrow().last().unwrap(), "stable-page-127");
        for label in [
            "Back",
            "Current page",
            "Smaller thumbnails",
            "Larger thumbnails",
            "Overview options",
        ] {
            let control = ElementHandle::find_by_accessible_label(&ui, label)
                .next()
                .unwrap();
            assert!(control.size().height >= 48., "{label}");
            let position = control.absolute_position();
            assert!(
                position.x >= 0. && position.x + control.size().width <= width + 0.5,
                "{label} fits {width}px"
            );
        }
    }
}

#[test]
fn keyboard_selection_density_current_and_back_use_shared_callbacks() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = scene(390., 844., 0);
    assert_eq!(tile(&ui, 1).accessible_checked(), Some(true));
    assert!(tile(&ui, 1).accessible_value().unwrap().contains("Current"));
    assert!(
        tile(&ui, 3)
            .accessible_value()
            .unwrap()
            .contains("Top section preview; one page")
    );
    // A failed preview remains independently actionable alongside loaded or
    // loading pages, and does not turn into a chapter-wide error.
    let failed = tile(&ui, 6);
    assert!(
        failed
            .accessible_value()
            .unwrap()
            .contains("Fixture failure")
    );
    let selected = Rc::new(RefCell::new(Vec::new()));
    ui.on_overview_selected({
        let selected = selected.clone();
        move |id| selected.borrow_mut().push(id.to_string())
    });
    tile(&ui, 1).mock_single_click(PointerEventButton::Left);
    key(&ui, Key::RightArrow);
    key(&ui, Key::Return);
    assert_eq!(selected.borrow().last().unwrap(), "stable-page-1");
    ui.set_overview_rtl(true);
    key(&ui, Key::RightArrow);
    key(&ui, Key::Return);
    assert_eq!(selected.borrow().last().unwrap(), "stable-page-0");
    let density = Rc::new(RefCell::new(Vec::new()));
    ui.on_overview_density_changed({
        let density = density.clone();
        move |value| density.borrow_mut().push(value)
    });
    ElementHandle::find_by_accessible_label(&ui, "Smaller thumbnails")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    ElementHandle::find_by_accessible_label(&ui, "Larger thumbnails")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    assert_eq!(*density.borrow(), [0, 2]);
    let current = Rc::new(RefCell::new(0));
    ui.on_overview_current({
        let current = current.clone();
        move || *current.borrow_mut() += 1
    });
    key(&ui, Key::End);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Page 1")
            .next()
            .is_none()
    );
    ElementHandle::find_by_accessible_label(&ui, "Current page")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    settle();
    assert_eq!(*current.borrow(), 1);
    assert!(tile(&ui, 1).absolute_position().y >= 0.);
    let back = Rc::new(RefCell::new(0));
    ui.on_overview_back({
        let back = back.clone();
        move || *back.borrow_mut() += 1
    });
    key(&ui, Key::Escape);
    key(&ui, Key::Back);
    assert_eq!(*back.borrow(), 2);
}

#[test]
fn mouse_and_touch_scroll_cancel_tile_activation() {
    use i_slint_core::{input::TouchPhase, lengths::LogicalPoint, platform::InternalEvent};
    i_slint_backend_testing::init_no_event_loop();
    for touch in [false, true] {
        let ui = scene(390., 844., 0);
        let selected = Rc::new(RefCell::new(Vec::new()));
        ui.on_overview_selected({
            let selected = selected.clone();
            move |id| selected.borrow_mut().push(id.to_string())
        });
        let anchors = Rc::new(RefCell::new(Vec::new()));
        ui.on_overview_anchor({
            let anchors = anchors.clone();
            move |id, fraction| anchors.borrow_mut().push((id.to_string(), fraction))
        });
        let pressed_tile = tile(&ui, 3);
        let x = pressed_tile.absolute_position().x + pressed_tile.size().width / 2.;
        let y = pressed_tile.absolute_position().y + pressed_tile.size().height * 0.65;
        for (phase, offset) in [
            (TouchPhase::Started, 0.),
            (TouchPhase::Moved, -40.),
            (TouchPhase::Moved, -160.),
            (TouchPhase::Ended, -160.),
        ] {
            ui.window().dispatch_event(if touch {
                WindowEvent::internal(InternalEvent::Touch {
                    id: 1,
                    position: LogicalPoint::new(x, y + offset),
                    phase,
                })
            } else {
                let position = LogicalPosition::new(x, y + offset);
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
            mock_elapsed_time(Duration::from_millis(32));
        }
        settle();
        assert!(
            selected.borrow().is_empty(),
            "drag cannot select a tile: touch={touch}"
        );
        assert!(
            anchors
                .borrow()
                .iter()
                .any(|(id, fraction)| id != "stable-page-0" || *fraction < -0.05),
            "drag scrolls the viewport: touch={touch}"
        );
    }
}

#[test]
fn viewport_range_is_exclusive_and_restore_tracks_identity_across_reflow() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = scene(390., 844., 0);
    let ranges = Rc::new(RefCell::new(Vec::new()));
    let anchors = Rc::new(RefCell::new(Vec::new()));
    ui.on_overview_visible_range({
        let ranges = ranges.clone();
        move |first, last, columns| ranges.borrow_mut().push((first, last, columns))
    });
    ui.on_overview_anchor({
        let anchors = anchors.clone();
        move |id, fraction| anchors.borrow_mut().push((id.to_string(), fraction))
    });
    ui.set_overview_restore_index(170);
    ui.set_overview_restore_fraction(-0.15);
    ui.set_overview_restore_serial(2);
    settle();
    let (anchor, fraction) = anchors.borrow().last().unwrap().clone();
    let anchor_index: i32 = anchor
        .strip_prefix("stable-page-")
        .unwrap()
        .parse()
        .unwrap();
    let (first, last, columns) = *ranges.borrow().last().unwrap();
    assert!(first <= anchor_index && last > anchor_index && last <= 320);
    assert_eq!(first % columns, 0);
    assert!(last - first < 60);
    assert!(
        (fraction + 0.15).abs() < 0.01,
        "anchor {anchor} fraction {fraction}, range {first}..{last}, columns {columns}"
    );
    ui.set_overview_restore_index(anchor_index);
    ui.set_overview_restore_fraction(fraction);
    ui.window().set_size(LogicalSize::new(1120., 780.));
    ui.set_overview_restore_serial(3);
    settle();
    let page = tile(&ui, anchor_index + 1);
    let grid = ElementHandle::find_by_element_id(&ui, "PageOverview::grid")
        .next()
        .unwrap();
    let actual_fraction =
        (page.absolute_position().y - grid.absolute_position().y) / grid.size().height;
    assert!((actual_fraction - fraction).abs() < 0.02);
    assert!(ElementHandle::find_by_element_type_name(&ui, "PageThumbnail").count() < 60);
}

#[test]
fn collection_groups_use_the_shared_accessible_chapter_selector() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = scene(390., 844., 0);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Choose chapter")
            .next()
            .is_none()
    );
    ui.set_overview_collection(true);
    // More than eight groups keep the dropdown; fewer become chips (see
    // few_groups_are_chips_and_many_groups_stay_a_dropdown).
    ui.set_overview_groups(ModelRc::new(VecModel::from(
        (12..21)
            .map(|n| format!("Chapter {n}").into())
            .collect::<Vec<slint::SharedString>>(),
    )));
    ui.set_overview_group_index(0);
    let picked = Rc::new(RefCell::new(Vec::new()));
    ui.on_overview_group_selected({
        let picked = picked.clone();
        move |index| picked.borrow_mut().push(index)
    });
    settle();
    let selector = ElementHandle::find_by_accessible_label(&ui, "Choose chapter")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Combobox))
        .unwrap();
    assert!(selector.size().height >= 48.);
    selector.invoke_accessible_default_action();
    settle();
    ElementHandle::find_by_accessible_label(&ui, "Chapter 13")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap()
        .invoke_accessible_default_action();
    settle();
    assert_eq!(*picked.borrow(), [1]);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Image 1")
            .next()
            .is_some()
    );
}

#[test]
fn pending_inventory_and_source_failure_offer_one_explicit_retry() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = scene(390., 844., -1);
    ui.set_overview_collection(true);
    ui.set_overview_tiles(ModelRc::default());
    ui.set_overview_busy(true);
    let retries = Rc::new(RefCell::new(0));
    ui.on_overview_retry({
        let retries = retries.clone();
        move || *retries.borrow_mut() += 1
    });
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Loading image inventory")
            .next()
            .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Retry")
            .next()
            .is_none()
    );
    assert_eq!(
        ElementHandle::find_by_element_type_name(&ui, "PageThumbnail").count(),
        0
    );
    ui.set_overview_busy(false);
    ui.set_overview_status("The source is rate limited. Try again later.".into());
    settle();
    assert_eq!(
        *retries.borrow(),
        0,
        "status publication never retries automatically"
    );
    let retry = ElementHandle::find_by_accessible_label(&ui, "Retry")
        .next()
        .unwrap();
    assert!(retry.size().height >= 48.);
    retry.invoke_accessible_default_action();
    assert_eq!(*retries.borrow(), 1);
}

#[test]
fn collection_viewer_uses_images_groups_and_returns_to_gallery() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = scene(390., 844., 0);
    ui.set_reader_collection(true);
    ui.set_reader_gallery_group_count(1);
    ui.set_reader_return_section(23);
    ui.set_section(6);
    // Entry intentionally hides the OSD. Publish that route change before
    // exercising the user's subsequent request to show collection controls.
    settle();
    ui.set_reader_osd(true);
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Images")
            .next()
            .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Chapters")
            .next()
            .is_none()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Groups")
            .next()
            .is_none()
    );
    ui.set_reader_gallery_group_count(2);
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Groups")
            .next()
            .is_some()
    );
    ui.set_reader_osd(false);
    let destinations = Rc::new(RefCell::new(Vec::new()));
    ui.on_navigate({
        let destinations = destinations.clone();
        move |section| destinations.borrow_mut().push(section)
    });
    ui.invoke_go_back();
    assert_eq!(*destinations.borrow(), [23]);
    ui.set_reader_collection(false);
    ui.set_reader_osd(true);
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Pages")
            .next()
            .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Chapters")
            .next()
            .is_some()
    );
}

#[test]
fn single_gallery_metadata_hides_synthetic_chapter_but_ordinary_one_shot_keeps_it() {
    use nova_reader::ItemRow;
    i_slint_backend_testing::init_no_event_loop();
    for width in [390., 1120.] {
        let ui = scene(width, 844., 0);
        ui.set_series_title("Fixture collection".into());
        ui.set_items(ModelRc::new(VecModel::from(vec![ItemRow {
            title: "Synthetic Gallery Chapter".into(),
            ..Default::default()
        }])));
        ui.set_detail_gallery_single(true);
        ui.set_section(5);
        settle();
        assert_eq!(
            ElementHandle::find_by_element_type_name(&ui, "ChapterRow").count(),
            0
        );
        assert!(
            ElementHandle::find_by_accessible_label(&ui, "Synthetic Gallery Chapter")
                .next()
                .is_none()
        );
        let hero = ElementHandle::find_by_element_type_name(&ui, "DetailHero")
            .next()
            .unwrap();
        assert!(hero.size().width > 0. && hero.size().height > 0.);
        let back = ElementHandle::find_by_accessible_label(&ui, "Back")
            .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
            .unwrap();
        assert!(
            back.absolute_position().y >= 0.
                && back.absolute_position().y + back.size().height <= 844.
        );
        // One item alone never hides a genuine manga chapter. The host's flag
        // comes from effective Gallery mode and the unfiltered source inventory.
        ui.set_detail_gallery_single(false);
        settle();
        assert_eq!(
            ElementHandle::find_by_element_type_name(&ui, "ChapterRow").count(),
            1
        );
        assert!(
            ElementHandle::find_by_accessible_label(&ui, "Synthetic Gallery Chapter")
                .next()
                .is_some()
        );
    }
}

#[test]
fn gallery_menu_exposes_metadata_download_and_three_presentation_choices() {
    i_slint_backend_testing::init_no_event_loop();
    for width in [390., 1120.] {
        let ui = scene(width, 844., 0);
        ui.set_overview_collection(true);
        ui.set_entry_menu_presentation(2);
        let actions = Rc::new(RefCell::new(Vec::new()));
        ui.on_entry_action({
            let actions = actions.clone();
            move |key, action| {
                actions
                    .borrow_mut()
                    .push((key.to_string(), action.to_string()))
            }
        });
        let requested = Rc::new(RefCell::new(Vec::new()));
        ui.on_entry_menu_requested({
            let requested = requested.clone();
            move |key| requested.borrow_mut().push(key.to_string())
        });
        for label in ["Metadata", "Download", "Presentation"] {
            ui.invoke_open_entry_menu("stable-series".into(), "Fixture".into(), true, 20., 20.);
            settle();
            ElementHandle::find_by_accessible_label(&ui, label)
                .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
                .unwrap()
                .invoke_accessible_default_action();
            settle();
        }
        assert_eq!(requested.borrow().len(), 3);
        for label in ["Default", "Reader", "Gallery"] {
            let choice = ElementHandle::find_by_accessible_label(&ui, label)
                .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
                .unwrap();
            assert!(choice.size().height >= 48.);
            assert_ne!(choice.accessible_role(), Some(AccessibleRole::Checkbox));
        }
        ElementHandle::find_by_accessible_label(&ui, "Gallery")
            .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
            .unwrap()
            .invoke_accessible_default_action();
        settle();
        assert_eq!(
            *actions.borrow(),
            [
                ("stable-series".to_owned(), "details".to_owned()),
                ("stable-series".to_owned(), "download".to_owned()),
                (
                    "stable-series".to_owned(),
                    "presentation-gallery".to_owned()
                )
            ]
        );
        ui.set_overview_collection(false);
        ui.invoke_open_entry_menu("ordinary-series".into(), "Fixture".into(), true, 20., 20.);
        settle();
        assert!(
            ElementHandle::find_by_accessible_label(&ui, "Metadata")
                .next()
                .is_none()
        );
        assert!(
            ElementHandle::find_by_accessible_label(&ui, "Download")
                .next()
                .is_none()
        );
    }
}

#[test]
fn thumbnail_clear_remains_available_with_disk_off_and_obeys_maintenance_busy() {
    use nova_reader::ImageStorageSettings;
    i_slint_backend_testing::init_no_event_loop();
    let ui = scene(390., 844., 0);
    let refreshes = Rc::new(RefCell::new((0, 0)));
    let settings = ui.global::<ImageStorageSettings>();
    settings.on_chapter_cache_refresh({
        let refreshes = refreshes.clone();
        move || refreshes.borrow_mut().0 += 1
    });
    settings.on_thumbnail_cache_refresh({
        let refreshes = refreshes.clone();
        move || refreshes.borrow_mut().1 += 1
    });
    settings.set_disk_enabled(false);
    let clears = Rc::new(RefCell::new(0));
    settings.on_thumbnail_cache_clear({
        let clears = clears.clone();
        move || *clears.borrow_mut() += 1
    });
    ui.set_section(17);
    settle();
    assert!(refreshes.borrow().1 > 0);
    assert_eq!(refreshes.borrow().0, refreshes.borrow().1);
    let clear = ElementHandle::find_by_accessible_label(&ui, "Clear thumbnails")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap();
    assert!(clear.size().height >= 48.);
    assert_eq!(clear.accessible_enabled(), Some(true));
    clear.invoke_accessible_default_action();
    assert_eq!(*clears.borrow(), 1);
    settings.set_thumbnail_cache_busy(true);
    settle();
    let clear = ElementHandle::find_by_accessible_label(&ui, "Clear thumbnails")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap();
    assert_eq!(clear.accessible_enabled(), Some(false));
    clear.invoke_accessible_default_action();
    assert_eq!(*clears.borrow(), 1);
}

#[test]
fn collection_grid_is_edge_to_edge_square_and_virtualized_at_each_density() {
    i_slint_backend_testing::init_no_event_loop();
    for width in [320., 390., 1120.] {
        let ui = scene(width, 844., 0);
        ui.set_overview_collection(true);
        ui.set_overview_subtitle("".into());
        for density in [0, 1, 2] {
            ui.set_overview_density(density);
            ui.set_overview_restore_index(0);
            ui.set_overview_restore_fraction(0.);
            ui.set_overview_restore_serial(ui.get_overview_restore_serial() + 1);
            settle();
            let grid = ElementHandle::find_by_element_id(&ui, "PageOverview::grid")
                .next()
                .unwrap();
            let columns = if grid.size().width < 700. {
                4 - density
            } else {
                (grid.size().width / 390. * (4 - density) as f32)
                    .round()
                    .clamp(2., 12.) as i32
            };
            let image = |number| {
                ElementHandle::find_by_accessible_label(&ui, &format!("Image {number}"))
                    .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
                    .unwrap()
            };
            let first = image(1);
            let second = image(2);
            let last = image(columns);
            let next_row = image(columns + 1);
            assert!((first.size().width - first.size().height).abs() < 0.01);
            assert!((first.absolute_position().x - grid.absolute_position().x).abs() < 0.01);
            assert!(
                (last.absolute_position().x + last.size().width
                    - grid.absolute_position().x
                    - grid.size().width)
                    .abs()
                    < 0.02
            );
            assert!(
                (second.absolute_position().x
                    - first.absolute_position().x
                    - first.size().width
                    - 2.)
                    .abs()
                    < 0.02
            );
            assert!(
                (next_row.absolute_position().y
                    - first.absolute_position().y
                    - first.size().height
                    - 2.)
                    .abs()
                    < 0.02
            );
            assert!(first.accessible_value().unwrap().contains("Current"));
            assert!(
                image(6)
                    .accessible_value()
                    .unwrap()
                    .contains("Fixture failure")
            );
            let maximum = columns as usize
                * ((grid.size().height / (first.size().height + 2.)).ceil() as usize + 1);
            let delegates = ElementHandle::find_by_element_type_name(&ui, "PageThumbnail").count();
            assert!(
                delegates <= maximum && delegates < 110,
                "visible delegates={delegates}, density={density}, width={width}"
            );
            assert!(
                ElementHandle::find_by_accessible_label(&ui, "Image 320")
                    .next()
                    .is_none()
            );
        }
    }
}

#[test]
fn collection_real_pinch_and_ctrl_wheel_preserve_focal_image_and_cancel_taps() {
    use i_slint_core::{input::TouchPhase, lengths::LogicalPoint, platform::InternalEvent};
    i_slint_backend_testing::init_no_event_loop();
    let ui = scene(390., 844., 90);
    ui.set_overview_collection(true);
    ui.set_overview_subtitle("".into());
    ui.set_overview_restore_fraction(-0.05);
    ui.set_overview_restore_serial(2);
    let requests = Rc::new(RefCell::new(Vec::new()));
    ui.on_overview_zoom_requested({
        let requests = requests.clone();
        let weak = ui.as_weak();
        move |density, id, fraction| {
            requests
                .borrow_mut()
                .push((density, id.to_string(), fraction));
            let ui = weak.unwrap();
            ui.set_overview_restore_index(
                id.strip_prefix("stable-page-").unwrap().parse().unwrap(),
            );
            ui.set_overview_restore_fraction(fraction);
            ui.set_overview_density(density);
            ui.set_overview_restore_serial(ui.get_overview_restore_serial() + 1);
        }
    });
    let selected = Rc::new(RefCell::new(Vec::new()));
    ui.on_overview_selected({
        let selected = selected.clone();
        move |id| selected.borrow_mut().push(id.to_string())
    });
    settle();
    let image = || {
        ElementHandle::find_by_accessible_label(&ui, "Image 98")
            .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
            .unwrap()
    };
    let before = image();
    let cx = before.absolute_position().x + before.size().width / 2.;
    let cy = before.absolute_position().y + before.size().height * 0.4;
    let row_fraction = (cy - before.absolute_position().y) / (before.size().height + 2.);
    let touch = |id, x, y, phase| {
        ui.window()
            .dispatch_event(WindowEvent::internal(InternalEvent::Touch {
                id,
                position: LogicalPoint::new(x, y),
                phase,
            }))
    };
    touch(11, cx - 20., cy, TouchPhase::Started);
    mock_elapsed_time(Duration::from_millis(30));
    touch(22, cx + 20., cy, TouchPhase::Started);
    mock_elapsed_time(Duration::from_millis(30));
    touch(11, cx - 30., cy, TouchPhase::Moved);
    mock_elapsed_time(Duration::from_millis(20));
    touch(22, cx + 45., cy, TouchPhase::Moved);
    settle();
    assert_eq!(
        ui.get_overview_density(),
        2,
        "native spread pinch enlarges squares"
    );
    assert_eq!(requests.borrow().last().unwrap().1, "stable-page-97");
    let enlarged = image();
    assert!(
        (enlarged.absolute_position().y + row_fraction * (enlarged.size().height + 2.) - cy).abs()
            < 1.5
    );
    touch(11, cx - 30., cy + 20., TouchPhase::Moved);
    mock_elapsed_time(Duration::from_millis(20));
    touch(22, cx + 45., cy + 20., TouchPhase::Moved);
    settle();
    let translated = image();
    assert!(
        (translated.absolute_position().y + row_fraction * (translated.size().height + 2.)
            - cy
            - 20.)
            .abs()
            < 1.5,
        "the same focal point follows a moving pinch midpoint"
    );
    touch(11, cx - 30., cy + 20., TouchPhase::Ended);
    mock_elapsed_time(Duration::from_millis(30));
    touch(22, cx + 50., cy + 20., TouchPhase::Moved);
    touch(22, cx + 50., cy + 20., TouchPhase::Ended);
    settle();
    assert!(
        selected.borrow().is_empty(),
        "pinch and remaining finger never open an image"
    );

    let enlarged = image();
    let wx = enlarged.absolute_position().x + enlarged.size().width / 2.;
    let wy = enlarged.absolute_position().y + enlarged.size().height * 0.6;
    let wheel_fraction = (wy - enlarged.absolute_position().y) / (enlarged.size().height + 2.);
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    ui.window().dispatch_event(WindowEvent::PointerScrolled {
        position: LogicalPosition::new(wx, wy),
        delta_x: 0.,
        delta_y: -120.,
    });
    ui.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
    settle();
    assert_eq!(
        ui.get_overview_density(),
        1,
        "Ctrl+wheel changes collection density"
    );
    let smaller = image();
    assert!(
        (smaller.absolute_position().y + wheel_fraction * (smaller.size().height + 2.) - wy).abs()
            < 1.5
    );
    assert_eq!(requests.borrow().last().unwrap().1, "stable-page-97");

    // Cancellation before recognition also consumes the surviving contact.
    let tx = smaller.absolute_position().x + smaller.size().width / 2.;
    let ty = smaller.absolute_position().y + smaller.size().height / 2.;
    touch(33, tx - 20., ty, TouchPhase::Started);
    mock_elapsed_time(Duration::from_millis(30));
    touch(44, tx + 20., ty, TouchPhase::Started);
    mock_elapsed_time(Duration::from_millis(30));
    touch(33, tx - 20., ty, TouchPhase::Cancelled);
    mock_elapsed_time(Duration::from_millis(30));
    touch(44, tx + 25., ty, TouchPhase::Moved);
    touch(44, tx + 25., ty, TouchPhase::Ended);
    settle();
    assert!(selected.borrow().is_empty());
    touch(55, tx, ty, TouchPhase::Started);
    mock_elapsed_time(Duration::from_millis(30));
    touch(55, tx, ty, TouchPhase::Ended);
    settle();
    assert_eq!(
        *selected.borrow(),
        ["stable-page-97"],
        "a fresh tap works after both pinch contacts end"
    );
}

/// Collection scene with the production gallery chrome on a 320-image inventory.
fn gallery(width: f32, height: f32, current: i32) -> AppWindow {
    let ui = scene(width, height, current);
    ui.set_overview_collection(true);
    ui.set_overview_subtitle("".into());
    // Like opening the overview: the grid shell owns the keyboard.
    ui.set_overview_focus_request(ui.get_overview_focus_request() + 1);
    settle();
    ui
}

fn by_id(ui: &AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(ui, id)
        .next()
        .unwrap_or_else(|| panic!("{id} should exist"))
}

fn button(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
}

fn header_shown(ui: &AppWindow) -> bool {
    // A fully hidden header is not instantiated for hit testing at all.
    ElementHandle::find_by_element_id(ui, "PageOverview::gallery-header")
        .next()
        .is_some_and(|header| {
            header.computed_opacity() > 0.99 && header.absolute_position().y >= -0.5
        })
}

fn wheel_grid(ui: &AppWindow, delta: f32) {
    // Positive wheel delta scrolls toward the start; one step per notch keeps
    // the movement a continuous run, as a finger or trackpad would.
    let grid = by_id(ui, "PageOverview::grid");
    for _ in 0..4 {
        grid.scroll(0., delta / 4.);
        settle();
    }
    // Wheel scrolling eases toward its target; let that finish.
    for _ in 0..4 {
        settle();
    }
}

#[test]
fn gallery_images_start_at_the_top_under_a_floating_header() {
    i_slint_backend_testing::init_no_event_loop();
    for (width, height) in [(360., 800.), (1280., 800.)] {
        let ui = gallery(width, height, 0);
        let grid = by_id(&ui, "PageOverview::grid");
        assert!(
            grid.absolute_position().y.abs() < 0.01 && (grid.size().height - height).abs() < 0.5,
            "the grid owns the whole window: {:?} {:?}",
            grid.absolute_position(),
            grid.size()
        );
        let first = ElementHandle::find_by_accessible_label(&ui, "Image 1")
            .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
            .unwrap();
        assert!(first.absolute_position().y.abs() < 0.01);
        let header = by_id(&ui, "PageOverview::gallery-header");
        assert!(
            header.size().height >= 48. && header.size().height < 80.,
            "one compact row: {}",
            header.size().height
        );
        // Everything lives in that single row over the grid.
        let row = header.absolute_position().y;
        for label in ["Back", "Current image", "Overview options"] {
            let control = button(&ui, label).unwrap_or_else(|| panic!("{label}"));
            assert!(control.size().height >= 48., "{label} keeps a 48px target");
            assert!(
                control.absolute_position().y >= row - 0.5
                    && control.absolute_position().y + control.size().height
                        <= row + header.size().height + 0.5,
                "{label} sits in the header row"
            );
            assert!(
                control.absolute_position().x >= 0.
                    && control.absolute_position().x + control.size().width <= width + 0.5
            );
        }
        // Density moved into Options; zoom gestures remain the quick path.
        assert!(button(&ui, "Smaller thumbnails").is_none());
        assert!(button(&ui, "Larger thumbnails").is_none());
        assert!(
            ElementHandle::find_by_accessible_label(&ui, "Image 1")
                .next()
                .is_some()
        );
    }
}

#[test]
fn gallery_header_hides_scrolling_down_and_returns_on_scroll_up_tap_or_keyboard() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = gallery(390., 844., 0);
    assert!(header_shown(&ui), "visible at the top");
    wheel_grid(&ui, -600.);
    assert!(!header_shown(&ui), "scrolling down hides the header");
    wheel_grid(&ui, 120.);
    assert!(header_shown(&ui), "scrolling up brings it back");
    wheel_grid(&ui, -600.);
    assert!(!header_shown(&ui));
    // Tapping the top edge is "tapping empty space".
    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(195., 10.),
    });
    settle();
    assert!(header_shown(&ui), "the top edge reveals it");
    wheel_grid(&ui, -600.);
    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(195., 500.),
    });
    settle();
    assert!(!header_shown(&ui));
    key(&ui, Key::DownArrow);
    assert!(header_shown(&ui), "keyboard navigation reveals it");
    // Jumping to the start by Current keeps it up even though the viewport moves.
    wheel_grid(&ui, -900.);
    assert!(!header_shown(&ui));
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Current image")
            .next()
            .is_none(),
        "hidden chrome is not a hit target"
    );
    wheel_grid(&ui, 120.);
    assert!(header_shown(&ui));
    ElementHandle::find_by_accessible_label(&ui, "Current image")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    settle();
    assert!(header_shown(&ui));
}

#[test]
fn programmatic_scrolling_never_hides_the_header() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = gallery(390., 844., 0);
    ui.set_overview_restore_index(200);
    ui.set_overview_restore_fraction(0.);
    ui.set_overview_restore_serial(ui.get_overview_restore_serial() + 1);
    settle();
    assert!(
        header_shown(&ui),
        "restoring a far anchor is not a user scroll"
    );
    key(&ui, Key::End);
    assert!(header_shown(&ui), "keyboard jumps are not a user scroll");
}

#[test]
fn thumbnails_fade_in_unless_motion_is_off() {
    i_slint_backend_testing::init_no_event_loop();
    let picture =
        slint::Image::from_rgba8(slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(4, 4));
    for motion in [true, false] {
        let ui = gallery(390., 844., -1);
        ui.global::<Anim>().set_enabled(motion);
        let model = ui.get_overview_tiles();
        let image = || by_id(&ui, "PageThumbnail::thumb-image");
        assert!(
            image().computed_opacity() < 0.01,
            "placeholder until the thumbnail arrives"
        );
        assert!(
            ElementHandle::find_by_accessible_label(&ui, "Image 1")
                .next()
                .unwrap()
                .accessible_value()
                .unwrap()
                .contains("Loading preview"),
            "status stays available to assistive tech"
        );
        let mut tile = model.row_data(0).unwrap();
        tile.thumbnail = picture.clone();
        tile.loading = false;
        model.set_row_data(0, tile);
        if motion {
            mock_elapsed_time(Duration::from_millis(40));
            let partial = image().computed_opacity();
            assert!(partial > 0. && partial < 1., "fading in: opacity {partial}");
            mock_elapsed_time(Duration::from_millis(300));
        } else {
            // No elapsed time at all: the image is simply there.
            assert!(
                image().computed_opacity() > 0.99,
                "motion off shows the thumbnail immediately"
            );
        }
        assert!(image().computed_opacity() > 0.99);
    }
}

#[test]
fn failed_previews_show_a_centered_glyph_and_no_status_text() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = gallery(390., 844., 0);
    for text in [
        "Waiting for preview",
        "Loading preview",
        "Preview unavailable",
    ] {
        assert!(
            !ElementHandle::find_by_accessible_label(&ui, text)
                .any(|element| { element.accessible_role() == Some(AccessibleRole::Text) }),
            "{text:?} is not painted in collection tiles"
        );
    }
    let failed = ElementHandle::find_by_accessible_label(&ui, "Image 6")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap();
    assert!(
        failed
            .accessible_value()
            .unwrap()
            .contains("Fixture failure")
    );
}

#[test]
fn current_and_focused_markers_are_distinct_and_not_a_full_border() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = gallery(390., 844., 1);
    let rings = |id: &str| ElementHandle::find_by_element_id(&ui, id).count();
    assert_eq!(rings("PageThumbnail::current-ring"), 1);
    assert_eq!(
        rings("PageThumbnail::focus-ring"),
        0,
        "no focus without keys"
    );
    let current = ElementHandle::find_by_element_id(&ui, "PageThumbnail::current-ring")
        .next()
        .unwrap();
    let tile = ElementHandle::find_by_accessible_label(&ui, "Image 2")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap();
    assert!(
        current.size().width < tile.size().width && current.size().height < tile.size().height,
        "the current ring is inset inside the tile"
    );
    key(&ui, Key::RightArrow);
    assert_eq!(rings("PageThumbnail::focus-ring"), 1);
    let focus = ElementHandle::find_by_element_id(&ui, "PageThumbnail::focus-ring")
        .next()
        .unwrap();
    assert!(
        (focus.size().width - tile.size().width).abs() < 0.5,
        "keyboard focus covers the whole tile"
    );
}

#[test]
fn gallery_density_lives_in_the_options_menu() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = gallery(390., 844., 0);
    ui.set_section(23);
    ui.set_overview_density(1);
    let densities = Rc::new(RefCell::new(Vec::new()));
    ui.on_overview_density_changed({
        let densities = densities.clone();
        move |value| densities.borrow_mut().push(value)
    });
    let options = button(&ui, "Overview options").unwrap();
    options.invoke_accessible_default_action();
    settle();
    ui.invoke_open_entry_menu("stable-series".into(), "Fixture".into(), true, 20., 20.);
    settle();
    button(&ui, "Larger thumbnails")
        .expect("Options offers larger thumbnails")
        .invoke_accessible_default_action();
    settle();
    assert_eq!(*densities.borrow(), [2]);
    assert!(!ui.get_entry_menu_visible(), "choosing closes the menu");
    ui.set_overview_density(2);
    ui.invoke_open_entry_menu("stable-series".into(), "Fixture".into(), true, 20., 20.);
    settle();
    let larger = button(&ui, "Larger thumbnails").unwrap();
    assert_eq!(
        larger.accessible_enabled(),
        Some(false),
        "already at the largest"
    );
    button(&ui, "Smaller thumbnails")
        .unwrap()
        .invoke_accessible_default_action();
    settle();
    assert_eq!(*densities.borrow(), [2, 1]);
}

#[test]
fn few_groups_are_chips_and_many_groups_stay_a_dropdown() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = gallery(390., 844., 0);
    let picked = Rc::new(RefCell::new(Vec::new()));
    ui.on_overview_group_selected({
        let picked = picked.clone();
        move |index| picked.borrow_mut().push(index)
    });
    let groups = |count: usize| {
        ModelRc::new(VecModel::from(
            (0..count)
                .map(|index| format!("Chapter {}", index + 12).into())
                .collect::<Vec<slint::SharedString>>(),
        ))
    };
    // Up to eight groups: always-visible chips, no dropdown.
    ui.set_overview_groups(groups(8));
    ui.set_overview_group_index(1);
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Choose chapter")
            .all(|element| element.accessible_role() != Some(AccessibleRole::Combobox))
    );
    let chip = |n: i32| button(&ui, &format!("Chapter {n}")).unwrap_or_else(|| panic!("chip {n}"));
    assert_eq!(chip(13).accessible_checked(), Some(true));
    assert_eq!(chip(12).accessible_checked(), Some(false));
    assert!(chip(13).size().height >= 48.);
    chip(15).invoke_accessible_default_action();
    settle();
    assert_eq!(*picked.borrow(), [3]);
    assert_eq!(ui.get_overview_group_index(), 3);
    assert_eq!(chip(15).accessible_checked(), Some(true));
    // The selected chip is revealed when it starts outside the row.
    ui.set_overview_group_index(7);
    settle();
    let selected = chip(19);
    assert!(
        selected.absolute_position().x >= 0.
            && selected.absolute_position().x + selected.size().width <= 390.5,
        "selected chip scrolls into view: {:?}",
        selected.absolute_position()
    );
    // Nine groups: the existing accessible dropdown scales better.
    ui.set_overview_groups(groups(9));
    ui.set_overview_group_index(0);
    picked.borrow_mut().clear();
    settle();
    let selector = ElementHandle::find_by_accessible_label(&ui, "Choose chapter")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Combobox))
        .expect("more than eight groups use the dropdown");
    assert!(selector.size().height >= 48.);
    selector.invoke_accessible_default_action();
    settle();
    button(&ui, "Chapter 13")
        .unwrap()
        .invoke_accessible_default_action();
    settle();
    assert_eq!(*picked.borrow(), [1]);
}

fn scroller(ui: &AppWindow) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, "Fast scroller").next()
}

#[test]
fn fast_scroller_appears_for_long_collections_and_jumps_to_the_matching_row() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = gallery(390., 844., 0);
    let ranges = Rc::new(RefCell::new(Vec::new()));
    ui.on_overview_visible_range({
        let ranges = ranges.clone();
        move |first, last, columns| ranges.borrow_mut().push((first, last, columns))
    });
    let anchors = Rc::new(RefCell::new(Vec::new()));
    ui.on_overview_anchor({
        let anchors = anchors.clone();
        move |id, _| anchors.borrow_mut().push(id.to_string())
    });
    // Idle: nothing to grab, so it never steals taps from edge tiles.
    assert!(scroller(&ui).is_none_or(|s| s.computed_opacity() < 0.01));
    wheel_grid(&ui, -80.);
    let track = scroller(&ui).expect("a long collection offers a fast scroller");
    assert!(track.computed_opacity() > 0.99, "recent scrolling shows it");
    let origin = track.absolute_position();
    let size = track.size();
    let x = origin.x + size.width / 2.;
    let down = |y: f32| {
        ui.window().dispatch_event(WindowEvent::PointerPressed {
            position: LogicalPosition::new(x, origin.y + y),
            button: PointerEventButton::Left,
        });
        mock_elapsed_time(Duration::from_millis(16));
    };
    let drag = |y: f32| {
        ui.window().dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(x, origin.y + y),
        });
        mock_elapsed_time(Duration::from_millis(16));
    };
    let up = |y: f32| {
        ui.window().dispatch_event(WindowEvent::PointerReleased {
            position: LogicalPosition::new(x, origin.y + y),
            button: PointerEventButton::Left,
        });
        settle();
    };
    let columns = 3; // 390px phone at the default density
    let rows = 320_usize.div_ceil(columns);
    let (grid_height, row) = (844., (390. - 4.) / 3. + 2.);
    for fraction in [0.5_f32, 0.9, 0.25, 1.0, 0.0] {
        down(size.height * 0.1);
        drag(size.height * 0.1 + 6.);
        let thumb = 52.;
        drag(fraction * (size.height - thumb) + thumb / 2.);
        settle();
        // While dragging, a bubble names the image under the thumb.
        let top_row = ((fraction * (rows as f32 * row - grid_height)) / row).floor() as usize;
        let expected = (top_row * columns + 1).min(320);
        assert!(
            ElementHandle::find_by_accessible_label(&ui, &format!("Image {expected} of 320"))
                .next()
                .is_some(),
            "bubble for fraction {fraction}: expected Image {expected} of 320"
        );
        up(fraction * (size.height - thumb) + thumb / 2.);
        // The viewport lands on that row: its first image is visible at the top.
        let first = ElementHandle::find_by_accessible_label(&ui, &format!("Image {expected}"))
            .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
            .unwrap_or_else(|| panic!("row of image {expected} is instantiated"));
        let grid = by_id(&ui, "PageOverview::grid");
        let offset = first.absolute_position().y - grid.absolute_position().y;
        assert!(
            (-row..=row).contains(&offset),
            "fraction {fraction}: image {expected} is {offset}px from the top"
        );
        let (first_visible, last_visible, _) = *ranges.borrow().last().unwrap();
        assert!(
            (first_visible as i32) <= expected as i32
                && (last_visible as i32) > expected as i32 - 1,
            "range {first_visible}..{last_visible} includes {expected}"
        );
        assert!(
            ElementHandle::find_by_element_type_name(&ui, "PageThumbnail").count() < 60,
            "still virtualized after a jump"
        );
    }
    assert!(
        !anchors.borrow().is_empty(),
        "restore anchors keep following the jumped-to row"
    );
    // A short collection does not need one.
    let short = gallery(390., 844., 0);
    short.set_overview_tiles(ModelRc::new(VecModel::from(
        (0..20)
            .map(|index| OverviewTile {
                page_id: format!("stable-page-{index}").into(),
                page_number: index + 1,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    settle();
    wheel_grid(&short, -200.);
    assert!(scroller(&short).is_none());
}

#[test]
fn fast_scroller_follows_ordinary_scrolling_and_survives_density_changes() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = gallery(390., 844., 0);
    wheel_grid(&ui, -80.);
    let track = scroller(&ui).unwrap();
    let early = track.accessible_value().unwrap().to_string();
    wheel_grid(&ui, -4000.);
    let later = scroller(&ui)
        .unwrap()
        .accessible_value()
        .unwrap()
        .to_string();
    assert!(
        later.parse::<f32>().unwrap() > early.parse::<f32>().unwrap(),
        "thumb advances with the content: {early} -> {later}"
    );
    ui.set_overview_density(0);
    settle();
    assert!(ElementHandle::find_by_element_type_name(&ui, "PageThumbnail").count() < 110);
}

#[test]
fn viewer_counter_shows_without_chrome_and_tap_toggles_the_chrome() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = scene(390., 844., 0);
    ui.set_pages(ModelRc::new(VecModel::from(vec![nova_reader::PageRow {
        page_id: "fixture".into(),
        ratio: 1.,
        loaded: true,
        ..Default::default()
    }])));
    ui.set_reader_collection(true);
    ui.set_reader_gallery_group_count(1);
    ui.set_reader_page_count(40);
    ui.set_reader_local_page(7);
    ui.set_section(6);
    settle();
    assert!(!ui.get_reader_osd(), "chrome is hidden by default");
    let counter = ElementHandle::find_by_accessible_label(&ui, "Image 7 of 40")
        .next()
        .expect("collections always show an N / M counter");
    assert!(counter.size().width > 0.);
    ui.set_reader_local_page(8);
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Image 8 of 40")
            .next()
            .is_some()
    );
    // With chrome visible the OSD owns the position text instead.
    ui.set_reader_osd(true);
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Image 8 of 40").all(|element| element
            .accessible_role()
            != Some(AccessibleRole::Text)
            || element.size().width > 0.)
    );
    // Manga pages keep the counter behind the existing setting.
    ui.set_reader_osd(false);
    ui.set_reader_collection(false);
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Image 8 of 40")
            .next()
            .is_none()
    );
}

#[test]
#[ignore = "opt-in native shared overview phone/desktop/RTL/collection captures"]
fn render_page_overview_previews() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let ui = scene(390., 844., 0);
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let pictures = ["portrait", "wide", "tall"].map(|name| {
        let source =
            image::open(base.join(format!("examples/addons/neutral-fixture/pages/{name}.png")))
                .unwrap()
                .to_rgba8();
        // Model the worker's bounded top-section result for the tall fixture.
        let source = if name == "tall" {
            image::imageops::crop_imm(
                &source,
                0,
                0,
                source.width(),
                (source.width() * 2).min(source.height()),
            )
            .to_image()
        } else {
            source
        };
        slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
            source.as_raw(),
            source.width(),
            source.height(),
        ))
    });
    let gallery_pictures = ["portrait", "wide", "tall"].map(|name| {
        let source =
            image::open(base.join(format!("examples/addons/neutral-fixture/pages/{name}.png")))
                .unwrap()
                .resize_to_fill(320, 320, image::imageops::FilterType::Triangle)
                .to_rgba8();
        slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
            source.as_raw(),
            source.width(),
            source.height(),
        ))
    });
    let tiles = Rc::new(VecModel::from(
        (0..320)
            .map(|index| OverviewTile {
                page_id: format!("stable-page-{index}").into(),
                page_number: index + 1,
                // The native scene also demonstrates bounded UI image retention.
                thumbnail: if index < 42 && index != 5 {
                    pictures[index as usize % 3].clone()
                } else {
                    Default::default()
                },
                tall_preview: index % 3 == 2,
                loading: index >= 42,
                error: if index == 5 {
                    "Fixture failure".into()
                } else {
                    "".into()
                },
            })
            .collect::<Vec<_>>(),
    ));
    let gallery_tiles = Rc::new(VecModel::from(
        (0..320)
            .map(|index| OverviewTile {
                page_id: format!("stable-page-{index}").into(),
                page_number: index + 1,
                // The fixture shares three bounded pixel buffers across visible cells.
                thumbnail: if index < 96 {
                    gallery_pictures[index as usize % 3].clone()
                } else {
                    Default::default()
                },
                loading: index >= 96,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ));
    ui.set_overview_tiles(ModelRc::from(tiles.clone()));
    let directory = std::path::PathBuf::from(
        std::env::var("NOVA_OVERVIEW_PREVIEW_DIR")
            .unwrap_or_else(|_| "/workspace/artifacts/compact-gallery-preview".into()),
    );
    std::fs::create_dir_all(&directory).unwrap();
    for (width, height, rtl, collection) in [
        (390, 844, false, false),
        (1120, 780, false, false),
        (390, 844, true, false),
        (390, 844, false, true),
        (320, 640, false, false),
    ] {
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));
        ui.set_overview_rtl(rtl);
        ui.set_overview_collection(collection);
        ui.set_overview_subtitle(if collection {
            "".into()
        } else {
            "Neutral fixture".into()
        });
        ui.set_overview_tiles(if collection {
            ModelRc::from(gallery_tiles.clone())
        } else {
            ModelRc::from(tiles.clone())
        });
        ui.set_overview_groups(if collection {
            ModelRc::new(VecModel::from(vec![
                "Chapter 12".into(),
                "Chapter 13".into(),
            ]))
        } else {
            ModelRc::default()
        });
        ui.set_overview_group_index(0);
        ui.set_overview_title(if collection {
            "Gallery".into()
        } else {
            "Chapter 12".into()
        });
        ui.set_overview_restore_index(0);
        ui.set_overview_restore_fraction(0.);
        ui.set_overview_restore_serial(ui.get_overview_restore_serial() + 1);
        settle();
        let shot = ui.window().take_snapshot().unwrap();
        image::save_buffer(
            directory.join(format!(
                "overview-{width}x{height}-{}-{}.png",
                if rtl { "rtl" } else { "ltr" },
                if collection { "collection" } else { "chapter" }
            )),
            shot.as_bytes(),
            shot.width(),
            shot.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
    }

    // Review the actual authored menu, settings leaf and collection viewer in
    // the same native window, with explicit frames between route and actions.
    let capture = |name: &str| {
        let shot = ui.window().take_snapshot().unwrap();
        image::save_buffer(
            directory.join(name),
            shot.as_bytes(),
            shot.width(),
            shot.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
    };
    for (width, height, density, name) in [
        (390, 844, 2, "gallery-390x844-2-columns.png"),
        (390, 844, 1, "gallery-390x844-3-columns.png"),
        (390, 844, 0, "gallery-390x844-4-columns.png"),
        (1120, 780, 1, "gallery-1120x780-adaptive.png"),
    ] {
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));
        ui.set_overview_collection(true);
        ui.set_overview_rtl(false);
        ui.set_overview_title("Gallery".into());
        ui.set_overview_subtitle("".into());
        ui.set_overview_groups(ModelRc::default());
        ui.set_overview_tiles(ModelRc::from(gallery_tiles.clone()));
        ui.set_overview_density(density);
        ui.set_overview_restore_index(0);
        ui.set_overview_restore_fraction(0.);
        ui.set_overview_restore_serial(ui.get_overview_restore_serial() + 1);
        settle();
        capture(name);
    }
    ui.window().set_size(slint::PhysicalSize::new(390, 844));
    ui.set_overview_collection(true);
    ui.set_overview_groups(ModelRc::default());
    ui.set_overview_title("Gallery".into());
    ui.set_overview_subtitle("Fixture collection".into());
    ui.set_entry_menu_presentation(2);
    settle();
    ui.invoke_open_entry_menu(
        "native-gallery-fixture".into(),
        "Fixture collection".into(),
        true,
        340.,
        66.,
    );
    settle();
    ElementHandle::find_by_accessible_label(&ui, "Presentation")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap()
        .invoke_accessible_default_action();
    settle();
    for label in ["Default", "Reader", "Gallery"] {
        assert!(
            ElementHandle::find_by_accessible_label(&ui, label)
                .any(|element| element.accessible_role() == Some(AccessibleRole::Button))
        );
    }
    capture("gallery-presentation-menu-390x844.png");
    ui.invoke_close_entry_menu();

    let cache = ui.global::<nova_reader::ImageStorageSettings>();
    cache.set_disk_enabled(true);
    cache.set_disk_limit_names(ModelRc::new(VecModel::from(vec![
        "256 MiB".into(),
        "512 MiB".into(),
        "1 GiB".into(),
    ])));
    cache.set_chapter_disk_limit_index(2);
    cache.set_artwork_disk_limit_index(2);
    cache.set_chapter_cache_info("12 MiB of viewed pages".into());
    cache.set_thumbnail_cache_info("384 KiB · 12 thumbnails".into());
    cache.set_thumbnail_cache_status("".into());
    cache.set_thumbnail_cache_busy(false);
    ui.set_section(17);
    settle();
    for _ in 0..8 {
        let clear = ElementHandle::find_by_accessible_label(&ui, "Clear thumbnails")
            .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
            .unwrap();
        if clear.absolute_position().y >= 200.
            && clear.absolute_position().y + clear.size().height <= 780.
        {
            break;
        }
        let scroll = ElementHandle::find_by_element_id(&ui, "MorePage::detail-scroll")
            .next()
            .unwrap();
        scroll.scroll(
            0.,
            if clear.absolute_position().y < 200. {
                100.
            } else {
                -100.
            },
        );
        settle();
    }
    let clear = ElementHandle::find_by_accessible_label(&ui, "Clear thumbnails")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap();
    assert!(
        clear.absolute_position().y >= 0.
            && clear.absolute_position().y + clear.size().height <= 844.
    );
    capture("thumbnail-cache-390x844.png");

    let page_ratios = [1.5, 0.5, 2.];
    let page_positions = [0., 1.5, 2.];
    ui.set_pages(ModelRc::new(VecModel::from(
        pictures
            .iter()
            .enumerate()
            .map(|(index, picture)| nova_reader::PageRow {
                page_id: format!("fixture-image-{index}").into(),
                ratio: page_ratios[index],
                position: page_positions[index],
                loaded: true,
                tiles: ModelRc::new(VecModel::from(vec![nova_reader::TileRow {
                    image: picture.clone(),
                    fraction: 1.,
                    ..Default::default()
                }])),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_total_ratio(4.);
    ui.set_series_title("Fixture collection".into());
    // A synthetic source group name must not leak into a single-group caption.
    ui.set_reader_chapter_title("Gallery".into());
    ui.set_reader_collection(true);
    ui.set_reader_gallery_group_count(1);
    ui.set_reader_page_count(3);
    ui.set_reader_local_page(1);
    ui.set_reader_progress(0.);
    ui.set_reader_mode_label("Left to right".into());
    ui.set_reading_mode(1);
    ui.set_current_page(0);
    ui.set_scroll_x(0.);
    ui.set_scroll_y(0.);
    ui.set_zoom(1.);
    ui.set_section(6);
    settle();
    ui.set_reader_osd(true);
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Images")
            .next()
            .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Chapters")
            .next()
            .is_none()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Groups")
            .next()
            .is_none()
    );
    capture("gallery-viewer-single-390x844.png");
}

#[test]
#[ignore = "opt-in native gallery snapshots; set NOVA_GALLERY_SHOT_DIR and NOVA_GALLERY_SHOT_LABEL"]
fn render_gallery_snapshots() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let pictures = ["portrait", "wide", "tall"].map(|name| {
        let source =
            image::open(base.join(format!("examples/addons/neutral-fixture/pages/{name}.png")))
                .unwrap()
                .resize_to_fill(320, 320, image::imageops::FilterType::Triangle)
                .to_rgba8();
        slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
            source.as_raw(),
            source.width(),
            source.height(),
        ))
    });
    let tiles = (0..320)
        .map(|index| OverviewTile {
            page_id: format!("stable-page-{index}").into(),
            page_number: index + 1,
            thumbnail: if index < 120 && index != 5 && index != 9 {
                pictures[index as usize % 3].clone()
            } else {
                Default::default()
            },
            // Tiles 100.. are still loading, 5 and 9 failed.
            loading: index >= 100 && index != 9,
            error: if index == 5 || index == 9 {
                "Fixture failure".into()
            } else {
                "".into()
            },
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let label = std::env::var("NOVA_GALLERY_SHOT_LABEL").unwrap_or_else(|_| "after".into());
    let directory = std::path::PathBuf::from(std::env::var("NOVA_GALLERY_SHOT_DIR").unwrap_or_else(
        |_| "/tmp/claude-0/-home-user/fd9e969b-e896-516f-a49a-a3f9e3998d90/scratchpad/gallery-shots".into(),
    ));
    std::fs::create_dir_all(&directory).unwrap();
    for (width, height) in [(360, 800), (1280, 800)] {
        let ui = AppWindow::new().unwrap();
        ui.global::<Anim>().set_enabled(false);
        ui.set_overview_title("Holiday album".into());
        ui.set_overview_subtitle("".into());
        ui.set_overview_collection(true);
        ui.set_overview_tiles(ModelRc::new(VecModel::from(tiles.clone())));
        ui.set_overview_current_index(3);
        ui.set_overview_restore_index(0);
        ui.set_overview_restore_serial(1);
        ui.set_overview_groups(ModelRc::new(VecModel::from(vec![
            "Day 1".into(),
            "Day 2".into(),
            "Day 3".into(),
        ])));
        ui.set_section(23);
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));
        ui.show().unwrap();
        settle();
        let capture = |state: &str| {
            let shot = ui.window().take_snapshot().unwrap();
            image::save_buffer(
                directory.join(format!("{label}-{width}x{height}-{state}.png")),
                shot.as_bytes(),
                shot.width(),
                shot.height(),
                image::ColorType::Rgba8,
            )
            .unwrap();
        };
        capture("top");
        key(&ui, Key::RightArrow);
        capture("keyboard-focus");
        let grid = ElementHandle::find_by_element_id(&ui, "PageOverview::grid")
            .next()
            .unwrap();
        for _ in 0..6 {
            grid.scroll(0., -60.);
            settle();
        }
        capture("scrolling-down");
        grid.scroll(0., 40.);
        settle();
        capture("scrolling-up");
    }
}
