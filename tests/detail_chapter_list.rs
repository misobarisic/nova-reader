//! Detail screen chapter list: number column, read state, status and progress,
//! filter chips with counts (including Bookmarked) and no overflow on phones
//! or wide windows.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{AppWindow, ItemRow};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};
use std::time::Duration;

fn settle() {
    for _ in 0..24 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn row(ui: &AppWindow, title: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, title)
        .find(|element| element.type_name().as_deref() == Some("ChapterRow"))
}

fn has_label(ui: &AppWindow, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, label)
        .next()
        .is_some()
}

fn chapters() -> Vec<ItemRow> {
    vec![
        ItemRow {
            title: "Chapter 16: Night Survey".into(),
            short_title: "Night Survey".into(),
            number: "16".into(),
            chapter_meta: "2d ago · Lumen Scans".into(),
            ..Default::default()
        },
        ItemRow {
            title: "Chapter 9: The Quiet Meridian".into(),
            short_title: "The Quiet Meridian".into(),
            number: "9".into(),
            status: "Downloading 35%".into(),
            download_state: 2,
            download_progress: 0.35,
            has_download: true,
            chapter_meta: "18 Sept 2026 · Lumen Scans".into(),
            ..Default::default()
        },
        ItemRow {
            title: "Chapter 7: Lanterns Under Ice".into(),
            short_title: "Lanterns Under Ice".into(),
            number: "7".into(),
            status: "Page 14".into(),
            read_fraction: 14. / 32.,
            bookmarked: true,
            downloaded: true,
            download_state: 5,
            has_download: true,
            chapter_meta: "4 Sept 2026 · Lumen Scans · 25 MiB".into(),
            ..Default::default()
        },
        ItemRow {
            title: "Chapter 3: Paper Compass".into(),
            short_title: "Paper Compass".into(),
            number: "3".into(),
            read: true,
            status: "Failed".into(),
            status_danger: true,
            download_state: 4,
            has_download: true,
            ..Default::default()
        },
    ]
}

fn detail(width: f32) -> AppWindow {
    let ui = AppWindow::new().unwrap();
    ui.set_items(ModelRc::new(VecModel::from(chapters())));
    ui.set_detail_unread_count(3);
    ui.set_detail_downloaded_count(1);
    ui.set_detail_bookmarked_count(1);
    ui.set_series_title("Moonlit Cartographer".into());
    ui.set_section(5);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(width, 2400.));
    settle();
    ui
}

#[test]
fn chapter_rows_show_number_state_status_and_progress() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = detail(390.);

    // The number column sits at the row's leading edge, 38px wide.
    let night = row(&ui, "Chapter 16: Night Survey").expect("row keeps the full title");
    let number = ElementHandle::find_by_accessible_label(&ui, "16")
        .next()
        .expect("number column");
    assert!((number.absolute_position().x - night.absolute_position().x).abs() < 0.5);
    assert!((number.size().width - 38.).abs() < 0.5);
    // The visible title drops the repeated prefix.
    assert!(has_label(&ui, "Night Survey"));

    // Read state is in the accessible description (it is only a colour on screen).
    let description = |title: &str| {
        row(&ui, title)
            .and_then(|row| row.accessible_description())
            .unwrap_or_default()
            .to_string()
    };
    assert!(description("Chapter 16: Night Survey").starts_with("Unread"));
    assert!(description("Chapter 3: Paper Compass").starts_with("Read"));
    assert!(description("Chapter 9: The Quiet Meridian").contains("Downloading 35%"));
    assert!(has_label(&ui, "Downloading 35%"));
    assert!(has_label(&ui, "Failed"));

    // Only the partly read chapter draws a reading progress bar.
    let bars: Vec<_> = ElementHandle::find_by_accessible_label(&ui, "Reading progress").collect();
    assert_eq!(bars.len(), 1);
    assert_eq!(bars[0].accessible_value().as_deref(), Some("44%"));
    assert_eq!(
        bars[0].accessible_role(),
        Some(AccessibleRole::ProgressIndicator)
    );
    assert!((bars[0].size().height - 3.).abs() < 0.5);
}

#[test]
fn filter_chips_count_and_bookmarked_filters_the_rows() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = detail(390.);
    for chip in ["All 4", "Unread 3", "Downloaded 1", "Bookmarked 1"] {
        assert!(has_label(&ui, chip), "missing chip {chip}");
    }
    assert!(has_label(&ui, "3 unread"));

    ElementHandle::find_by_accessible_label(&ui, "Bookmarked 1")
        .next()
        .unwrap()
        .invoke_accessible_default_action();
    settle();
    assert_eq!(ui.get_chapter_filter(), 3);
    assert!(row(&ui, "Chapter 7: Lanterns Under Ice").is_some());
    for hidden in [
        "Chapter 16: Night Survey",
        "Chapter 9: The Quiet Meridian",
        "Chapter 3: Paper Compass",
    ] {
        assert!(row(&ui, hidden).is_none(), "{hidden} is filtered out");
    }

    ui.set_detail_bookmarked_count(0);
    ui.set_items(ModelRc::new(VecModel::from(vec![chapters().remove(0)])));
    settle();
    assert!(has_label(&ui, "No bookmarked chapters"));
}

#[test]
fn chapter_rows_fit_phone_and_wide_windows() {
    i_slint_backend_testing::init_no_event_loop();
    for width in [320., 1280.] {
        let ui = detail(width);
        for kind in ["ChapterRow", "DownloadIconButton", "IconAction"] {
            for element in ElementHandle::find_by_element_type_name(&ui, kind) {
                let at = element.absolute_position();
                let size = element.size();
                assert!(
                    at.x >= -0.5 && at.x + size.width <= width + 0.5,
                    "{kind} {:?} overflows {width}px: {at:?} {size:?}",
                    element.accessible_label()
                );
            }
        }
        // Row buttons keep a 44px touch height; the chip row scrolls instead of
        // overflowing.
        for element in ElementHandle::find_by_element_type_name(&ui, "DownloadIconButton") {
            assert!(element.size().height >= 44. && element.size().width >= 44.);
        }
        let chips = ElementHandle::find_by_element_id(&ui, "AppWindow::chapter-chips")
            .next()
            .expect("chip row");
        assert!(chips.absolute_position().x + chips.size().width <= width + 0.5);
        for title in ["Night Survey", "Lanterns Under Ice"] {
            let text = ElementHandle::find_by_accessible_label(&ui, title)
                .next()
                .unwrap();
            assert!(text.absolute_position().x + text.size().width <= width + 0.5);
        }
    }
}
