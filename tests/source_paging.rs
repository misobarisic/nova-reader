use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{AppWindow, ItemRow};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel, platform::PointerEventButton};
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}
fn rows(count: usize) -> ModelRc<ItemRow> {
    ModelRc::new(VecModel::from(
        (0..count)
            .map(|index| ItemRow {
                title: format!("Source title {index}").into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ))
}
fn catalogue(width: f32, height: f32) -> (AppWindow, Rc<Cell<usize>>) {
    let ui = AppWindow::new().unwrap();
    ui.window().set_size(LogicalSize::new(width, height));
    ui.set_section(3);
    ui.set_source_catalog_open(true);
    let requests = Rc::new(Cell::new(0));
    ui.on_load_more({
        let requests = requests.clone();
        let weak = ui.as_weak();
        move || {
            requests.set(requests.get() + 1);
            let ui = weak.unwrap();
            ui.set_catalogue_page_error("".into());
            ui.set_busy(true);
        }
    });
    (ui, requests)
}

#[test]
fn source_pages_follow_viewport_demand_without_concurrency_or_scroll_reset() {
    i_slint_backend_testing::init_no_event_loop();
    for (width, height) in [(390., 844.), (1120., 780.), (640., 240.)] {
        let (ui, requests) = catalogue(width, height);
        ui.set_items(rows(100));
        ui.set_has_more(true);
        ui.show().unwrap();
        settle();
        assert_eq!(requests.get(), 0, "long grids wait for the viewport");
        let scroll = ElementHandle::find_by_element_id(&ui, "BrowsePage::catalog-scroll")
            .next()
            .unwrap();
        scroll.scroll(0., -100_000.);
        settle();
        assert_eq!(requests.get(), 1, "reaching the end requests one page");
        settle();
        assert_eq!(requests.get(), 1, "a pending request cannot overlap");
        let position = ui.get_catalogue_scroll_y();
        assert!(position < -100.);
        ui.set_items(rows(180));
        ui.set_busy(false);
        settle();
        assert!((ui.get_catalogue_scroll_y() - position).abs() < 1.);
        assert_eq!(requests.get(), 1, "appended rows move the end away");
        scroll.scroll(0., -100_000.);
        settle();
        assert_eq!(requests.get(), 2);
        ui.set_has_more(false);
        ui.set_busy(false);
        settle();
        assert_eq!(requests.get(), 2, "source end-of-list stops paging");
        assert!(
            ElementHandle::find_by_accessible_label(&ui, "Load more")
                .next()
                .is_none()
        );
        ui.hide().unwrap();
    }
}

#[test]
fn short_pages_fill_automatically_and_failed_pages_wait_for_explicit_retry() {
    i_slint_backend_testing::init_no_event_loop();
    let (ui, requests) = catalogue(1120., 780.);
    ui.set_items(rows(1));
    ui.set_has_more(true);
    ui.set_status("Background download completed".into());
    ui.show().unwrap();
    settle();
    assert_eq!(
        requests.get(),
        1,
        "short pages fill despite unrelated status"
    );
    ui.set_items(rows(2));
    ui.set_busy(false);
    settle();
    assert_eq!(requests.get(), 2, "successful short pages keep filling");
    ui.set_busy(false);
    ui.set_catalogue_page_error("Page failed".into());
    settle();
    assert_eq!(
        requests.get(),
        2,
        "failed requests never retry automatically"
    );
    let retry = ElementHandle::find_by_accessible_label(&ui, "Retry")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap();
    retry.mock_single_click(PointerEventButton::Left);
    settle();
    assert_eq!(requests.get(), 3);
    ui.set_has_more(false);
    ui.set_busy(false);
    settle();
    assert_eq!(requests.get(), 3);
    ui.set_has_more(true);
    ui.set_source_catalog_open(false);
    settle();
    assert_eq!(requests.get(), 3, "the source management page never pages");
    ui.set_source_catalog_open(true);
    ui.set_section(5);
    settle();
    assert_eq!(requests.get(), 3, "other destinations never page");
    ui.hide().unwrap();
}
