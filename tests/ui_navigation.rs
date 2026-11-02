use nova_reader::{AppWindow, ItemRow, PageRow};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

#[test]
fn responsive_navigation_and_virtual_reader_models() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_items(ModelRc::new(VecModel::from(vec![ItemRow {
        title: "Fixture".into(),
        subtitle: "Six pages".into(),
        badge: "Unread".into(),
        bookmarked: false,
        ..ItemRow::default()
    }])));
    ui.on_navigate({
        let weak = ui.as_weak();
        move |section| weak.unwrap().set_section(section)
    });
    for section in [0, 1, 2, 3, 4, 5, 7, 0] {
        ui.invoke_navigate(section);
        assert_eq!(ui.get_section(), section);
    }
    ui.window().set_size(slint::LogicalSize::new(390., 720.));
    ui.set_pages(ModelRc::new(VecModel::from(
        (0..320)
            .map(|i| PageRow {
                page_id: i.to_string().into(),
                image: slint::Image::default(),
                tiles: ModelRc::default(),
                ratio: 1.5,
                position: i as f32 * 1.5,
                loaded: false,
                preview: false,
                error: "".into(),
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_total_ratio(480.);
    ui.set_section(6);
    ui.set_current_page(319);
    assert_eq!(ui.get_pages().row_count(), 320);
    assert_eq!(ui.get_current_page(), 319);
    // Geometry may hold hundreds of placeholders while image ownership remains
    // empty until the worker admits the visible/prefetch window.
    assert!(ui.get_pages().iter().all(|p| !p.loaded));
}
