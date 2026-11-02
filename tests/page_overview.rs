//! The shared production overview keeps a long inventory virtualized and
//! reports source identities rather than presentation positions.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, mock_elapsed_time};
use nova_reader::{Anim, AppWindow, OverviewTile};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, LogicalSize, ModelRc, VecModel};
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
    ui.set_overview_groups(ModelRc::new(VecModel::from(vec![
        "Chapter 12".into(),
        "Chapter 13".into(),
    ])));
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
    ui.set_overview_tiles(ModelRc::from(tiles));
    let directory = std::path::PathBuf::from(
        std::env::var("NOVA_OVERVIEW_PREVIEW_DIR")
            .unwrap_or_else(|_| "/workspace/artifacts/page-overview-preview".into()),
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
