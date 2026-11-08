//! Browse "Refresh" through the real window: the source list's sections,
//! the ⋯ menu, extensions, the catalogue header, search, chips and filter
//! sheet, and results across sources. Sections, filter values and the
//! search itself are tested in `src/source_browser.rs`,
//! `src/source_filters.rs` and `src/browse_page.rs`.
//!
//! The testing backend's mock clock is shared process-wide, so the scenarios
//! run in order from one test.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{
    Anim, AppWindow, BrowseBridge, ExtensionRow, FilterGroup, FilterPill, ItemRow, SearchSection,
    SourceRow,
};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..30 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn find(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label).find(|e| {
        matches!(
            e.accessible_role(),
            Some(
                AccessibleRole::Button
                    | AccessibleRole::RadioButton
                    | AccessibleRole::Switch
                    | AccessibleRole::Tab
                    | AccessibleRole::Checkbox
            )
        )
    })
}

fn button(ui: &AppWindow, label: &str) -> ElementHandle {
    find(ui, label).unwrap_or_else(|| panic!("missing control {label}"))
}

fn press(ui: &AppWindow, label: &str) {
    button(ui, label).invoke_accessible_default_action();
    settle();
}

fn has(ui: &AppWindow, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, label)
        .next()
        .is_some()
}

fn source(i: i32, name: &str, adult: bool, pinned: bool) -> SourceRow {
    SourceRow {
        id: format!("s{i}").into(),
        name: name.into(),
        language: "English".into(),
        code: "EN".into(),
        extension: "Fixture".into(),
        enabled: true,
        latest: true,
        pinned,
        adult,
        index: i,
        ..Default::default()
    }
}

type Log = Rc<RefCell<Vec<String>>>;

fn browse(width: f32) -> (AppWindow, Log) {
    let ui = AppWindow::new().unwrap();
    let log: Log = Rc::default();
    let bridge = ui.global::<BrowseBridge>();
    macro_rules! record {
        ($($callback:ident($($arg:ident),*)),* $(,)?) => {$({
            let log = log.clone();
            ui.$callback(move |$($arg),*| {
                let parts: Vec<String> = vec![$($arg.to_string()),*];
                log.borrow_mut().push(format!("{} {}", stringify!($callback), parts.join(" ")).trim_end().to_string());
            });
        })*};
    }
    record!(
        on_source_selected(index),
        on_source_latest_selected(index),
        on_source_pin_picked(id),
        on_adult_visibility_changed(shown),
        on_extension_action(id, action),
        on_install_addon(),
        on_browse(kind),
        on_search(query),
        on_source_filters_applied(),
        on_source_filter_selected(key, index),
        on_source_filter_changed(key, value),
    );
    macro_rules! record_bridge {
        ($($callback:ident($($arg:ident),*)),* $(,)?) => {$({
            let log = log.clone();
            bridge.$callback(move |$($arg),*| {
                let parts: Vec<String> = vec![$($arg.to_string()),*];
                log.borrow_mut().push(format!("{} {}", stringify!($callback), parts.join(" ")).trim_end().to_string());
            });
        })*};
    }
    record_bridge!(
        on_language_picked(index),
        on_unpin(id),
        on_filters_reset(),
        on_global_search(query),
        on_global_more(section),
        on_global_retry(section),
        on_extension_remove(id),
    );
    let all = vec![
        source(0, "Alpha", false, true),
        source(1, "Beta", false, true),
        source(2, "Night Market", true, false),
    ];
    ui.set_source_rows(ModelRc::new(VecModel::from(all.clone())));
    bridge.set_pinned(ModelRc::new(VecModel::from(all[..2].to_vec())));
    bridge.set_languages(ModelRc::new(VecModel::from(
        ["All", "English", "Japanese"].map(Into::into).to_vec(),
    )));
    bridge.set_language_counts(ModelRc::new(VecModel::from(vec![3, 2, 1])));
    ui.set_extensions(ModelRc::new(VecModel::from(vec![ExtensionRow {
        id: "fixture".into(),
        name: "Fixture".into(),
        version: "1.0".into(),
        language: "en".into(),
        enabled: true,
        sources: 3,
        ..Default::default()
    }])));
    ui.set_section(3);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(width, 844.));
    settle();
    (ui, log)
}

fn last(log: &Log) -> String {
    log.borrow().last().cloned().unwrap_or_default()
}

fn sources_list() {
    let (ui, log) = browse(390.);
    let bridge = ui.global::<BrowseBridge>();
    // No last-used source, no card.
    assert!(!has(&ui, "Last used: Alpha"));
    bridge.set_last_used(source(0, "Alpha", false, true));
    bridge.set_has_last_used(true);
    settle();
    press(&ui, "Latest");
    assert_eq!(last(&log), "on_source_latest_selected 0");
    assert!(ui.get_source_catalog_open());
    assert_eq!(ui.get_source_catalogue_kind(), "latest");
    ui.set_source_catalog_open(false);
    settle();
    press(&ui, "Popular");
    assert_eq!(last(&log), "on_source_selected 0");
    ui.set_source_catalog_open(false);
    settle();
    // The pinned grid matches the pins; right-click unpins.
    let tiles: Vec<_> = ElementHandle::find_by_element_type_name(&ui, "PinTile").collect();
    assert_eq!(tiles.len(), 2);
    let beta = tiles
        .iter()
        .find(|t| t.accessible_label().as_deref() == Some("Beta"))
        .unwrap();
    beta.mock_single_click(PointerEventButton::Right);
    settle();
    assert_eq!(last(&log), "on_unpin s1");
    // Language chips filter through Rust and show counts.
    press(&ui, "Japanese, 1 titles");
    assert_eq!(last(&log), "on_language_picked 2");
    // Adult sources carry their tag; the switch lives in ⋯.
    assert!(has(&ui, "Night Market, 18+, English"));
    press(&ui, "Browse options");
    let adult = button(&ui, "Show adult sources");
    assert_eq!(adult.accessible_checked(), Some(false));
    adult.invoke_accessible_default_action();
    settle();
    assert_eq!(last(&log), "on_adult_visibility_changed true");
    assert!(ui.get_show_adult());
    // Pin buttons stay on each row.
    press(&ui, "Pin Night Market");
    assert_eq!(last(&log), "on_source_pin_picked s2");
}

fn extensions() {
    let (ui, log) = browse(390.);
    press(&ui, "Extensions");
    assert_eq!(ui.get_browse_tab(), 1);
    assert!(has(&ui, "1.0 · en · 3 sources"));
    ElementHandle::find_by_accessible_label(&ui, "Fixture enabled")
        .next()
        .expect("switch")
        .invoke_accessible_default_action();
    assert_eq!(last(&log), "on_extension_action fixture toggle");
    press(&ui, "Fixture options");
    press(&ui, "Remove");
    assert_eq!(last(&log), "on_extension_remove fixture");
    press(&ui, "Install addon from file");
    assert_eq!(last(&log), "on_install_addon");
}

fn catalogue_and_filters() {
    let (ui, log) = browse(390.);
    let bridge = ui.global::<BrowseBridge>();
    ui.set_source_title("Alpha".into());
    ui.set_source_has_filters(true);
    bridge.set_catalog_latest(true);
    ui.set_source_catalog_open(true);
    // The first page shows placeholders while it loads.
    ui.set_busy(true);
    settle();
    assert!(has(&ui, "Loading titles"));
    ui.set_busy(false);
    ui.set_items(ModelRc::new(VecModel::from(
        (0..9)
            .map(|i| ItemRow {
                title: format!("Title {i}").into(),
                bookmarked: i == 1,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    settle();
    assert!(!has(&ui, "Loading titles"));
    assert_eq!(
        ElementHandle::find_by_element_type_name(&ui, "InLibraryPill").count(),
        1
    );
    // Search from the always-visible field; clearing returns to Popular.
    let field = ElementHandle::find_by_accessible_label(&ui, "Search Alpha")
        .next()
        .expect("search field");
    field.mock_single_click(PointerEventButton::Left);
    settle();
    for ch in "moon".chars() {
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: ch.to_string().into(),
        });
        ui.window().dispatch_event(WindowEvent::KeyReleased {
            text: ch.to_string().into(),
        });
    }
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Return.into(),
    });
    settle();
    assert_eq!(last(&log), "on_search moon");
    assert_eq!(ui.get_source_catalogue_kind(), "search");
    assert!(has(&ui, "Results, 9 titles"));
    press(&ui, "Clear search");
    assert_eq!(last(&log), "on_search");
    assert_eq!(ui.get_source_catalogue_kind(), "popular");
    // Filters: the chip shows the active count and opens the sheet.
    bridge.set_filter_count(2);
    let pills = ModelRc::new(VecModel::from(vec![
        FilterPill {
            label: "Action".into(),
            key: "k0".into(),
            state: 0,
            tristate: true,
        },
        FilterPill {
            label: "Drama".into(),
            key: "k1".into(),
            state: 1,
            tristate: false,
        },
    ]));
    bridge.set_filter_groups(ModelRc::new(VecModel::from(vec![FilterGroup {
        name: "Genres".into(),
        kind: "pills".into(),
        pills,
        ..Default::default()
    }])));
    settle();
    assert!(find(&ui, "Filters, 2 titles").is_some());
    ui.set_source_filters_open(true);
    settle();
    // Tri-state pills step include → exclude; plain pills toggle.
    press(&ui, "Action");
    assert_eq!(last(&log), "on_source_filter_selected k0 1");
    press(&ui, "Drama, included");
    assert_eq!(last(&log), "on_source_filter_changed k1 false");
    press(&ui, "Reset");
    assert_eq!(last(&log), "on_filters_reset");
    press(&ui, "Show results");
    assert_eq!(last(&log), "on_source_filters_applied");
    assert_eq!(ui.get_source_catalogue_kind(), "filtered");
}

fn search_across_sources() {
    let (ui, log) = browse(390.);
    let bridge = ui.global::<BrowseBridge>();
    bridge.set_global_query("moon".into());
    bridge.set_global_sections(ModelRc::new(VecModel::from(vec![
        SearchSection {
            name: "Alpha".into(),
            status: 1,
            count: 12,
            items: ModelRc::new(VecModel::from(vec![ItemRow {
                title: "Moon".into(),
                ..Default::default()
            }])),
            ..Default::default()
        },
        SearchSection {
            name: "Beta".into(),
            status: 0,
            ..Default::default()
        },
        SearchSection {
            name: "Gamma".into(),
            status: 2,
            error: "timed out".into(),
            ..Default::default()
        },
    ])));
    settle();
    assert!(has(&ui, "12 results"));
    assert!(!find(&ui, "Sources").is_some(), "results replace the tabs");
    press(&ui, "Open Alpha with this search");
    assert_eq!(last(&log), "on_global_more 0");
    press(&ui, "Retry Gamma");
    assert_eq!(last(&log), "on_global_retry 2");
    press(&ui, "Back to sources");
    assert_eq!(last(&log), "on_global_search");
}

fn sticky_header_and_layout() {
    for width in [320., 390., 1280., 1920.] {
        let (ui, _) = browse(width);
        for open in [false, true] {
            ui.set_source_catalog_open(open);
            settle();
            for kind in ["SourceRowView", "PinTile", "HeaderIcon"] {
                for e in ElementHandle::find_by_element_type_name(&ui, kind) {
                    let at = e.absolute_position();
                    let size = e.size();
                    assert!(
                        at.x >= -0.5 && at.x + size.width <= width + 0.5,
                        "{kind} overflows {width}: {at:?} {size:?}"
                    );
                    if kind == "HeaderIcon" {
                        assert!(size.height >= 44.);
                    }
                }
            }
        }
    }
    let (ui, _) = browse(390.);
    ui.set_source_rows(ModelRc::new(VecModel::from(
        (0..40)
            .map(|i| source(i, &format!("Source {i}"), false, false))
            .collect::<Vec<_>>(),
    )));
    settle();
    let title = || {
        ElementHandle::find_by_element_type_name(&ui, "PageTitle")
            .find(|e| e.accessible_label().as_deref() == Some("Browse"))
            .unwrap()
    };
    let full = title().size().height;
    // Animations off: the shrink is immediate.
    ui.global::<Anim>().set_enabled(false);
    ElementHandle::find_by_element_id(&ui, "BrowsePage::home-scroll")
        .next()
        .expect("source list scroller")
        .scroll(0., -600.);
    settle();
    assert!(
        title().absolute_position().y < 40.,
        "the header stays pinned"
    );
    assert!(
        title().size().height < full,
        "the title shrinks after scrolling"
    );
    ui.global::<Anim>().set_enabled(true);
}

#[test]
fn browse_page() {
    i_slint_backend_testing::init_no_event_loop();
    sources_list();
    extensions();
    catalogue_and_filters();
    search_across_sources();
    sticky_header_and_layout();
}
