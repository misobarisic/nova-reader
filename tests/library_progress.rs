use i_slint_backend_testing::ElementHandle;
use nova_reader::{AppWindow, Appearance, ItemRow};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};
use std::time::Duration;

fn settle() {
    for _ in 0..24 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

#[test]
fn library_progress_starts_at_left_in_grid_and_list() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.show().unwrap();
    for (width, height) in [(320., 640.), (390., 844.), (1120., 800.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        for list_view in [false, true] {
            ui.global::<Appearance>().set_library_list_view(list_view);
            for completed in [0, 1, 25, 50, 80] {
                ui.set_items(ModelRc::new(VecModel::from(vec![ItemRow {
                    title: "Progress fixture".into(),
                    completed,
                    total: 50,
                    ..Default::default()
                }])));
                settle();
                if completed == 0 {
                    assert!(
                        ElementHandle::find_by_element_id(
                            &ui,
                            "LibraryCard::library_progress_track"
                        )
                        .next()
                        .is_none(),
                        "unread titles have no empty progress rail"
                    );
                    continue;
                }
                let track =
                    ElementHandle::find_by_element_id(&ui, "LibraryCard::library_progress_track")
                        .next()
                        .expect("visible progress track");
                let fill =
                    ElementHandle::find_by_element_id(&ui, "LibraryCard::library_progress_fill")
                        .next()
                        .expect("progress fill");
                let origin = track.absolute_position();
                let start = fill.absolute_position();
                assert!(track.size().width > 0.);
                assert!(
                    (start.x - origin.x).abs() < 0.01 && (start.y - origin.y).abs() < 0.01,
                    "{width}px list={list_view}, {completed}/50 starts at {start:?}, track {origin:?}"
                );
                let fraction = (completed as f32 / 50.).min(1.);
                assert!((fill.size().width - track.size().width * fraction).abs() < 0.01);
                assert_eq!(fill.size().height, track.size().height);
            }
        }
    }
}
