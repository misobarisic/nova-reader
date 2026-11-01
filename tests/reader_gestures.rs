//! Real touch input verifies reader tap/hold arbitration inside its Flickable.
use i_slint_backend_testing::mock_elapsed_time;
use i_slint_core::{input::TouchPhase, lengths::LogicalPoint, platform::InternalEvent};
use nova_reader::{AppWindow, PageRow, ReaderSettings, TileRow};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, LogicalSize, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn advance(ms: u64) {
    // Small steps also run Flickable's delayed delivery before later timers.
    for _ in 0..ms / 10 {
        mock_elapsed_time(Duration::from_millis(10));
    }
    mock_elapsed_time(Duration::from_millis(ms % 10));
}

fn touch(ui: &AppWindow, x: f32, y: f32, phase: TouchPhase) {
    ui.window()
        .dispatch_event(WindowEvent::internal(InternalEvent::Touch {
            id: 1,
            position: LogicalPoint::new(x, y),
            phase,
        }));
}

fn tap(ui: &AppWindow, x: f32, y: f32) {
    touch(ui, x, y, TouchPhase::Started);
    advance(50);
    touch(ui, x, y, TouchPhase::Ended);
}

fn right_click(ui: &AppWindow, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Right,
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Right,
    });
}

#[test]
fn reader_distinguishes_single_double_drag_hold_and_right_click() {
    i_slint_backend_testing::init_no_event_loop();
    for (width, height) in [(390., 844.), (1200., 800.)] {
        let ui = AppWindow::new().unwrap();
        ui.global::<ReaderSettings>().set_long_press_actions(true);
        let singles = Rc::new(RefCell::new(Vec::new()));
        let doubles = Rc::new(RefCell::new(Vec::new()));
        ui.on_reader_tap({
            let singles = singles.clone();
            move |x, y| singles.borrow_mut().push((x, y))
        });
        ui.on_reader_double_tap({
            let doubles = doubles.clone();
            move |x, y| doubles.borrow_mut().push((x, y))
        });
        let image = slint::Image::from_rgb8(slint::SharedPixelBuffer::new(2, 6));
        ui.set_pages(ModelRc::new(VecModel::from(vec![PageRow {
            page_id: "gesture-page".into(),
            ratio: 3.,
            loaded: true,
            tiles: ModelRc::new(VecModel::from(vec![TileRow {
                image,
                fraction: 1.,
                ..Default::default()
            }])),
            ..Default::default()
        }])));
        ui.set_reading_mode(0);
        ui.set_total_ratio(3.);
        ui.set_section(6);
        ui.set_reader_double_tap_enabled(false);
        ui.show().unwrap();
        ui.window().set_size(LogicalSize::new(width, height));
        advance(400);
        let (x, y) = (width / 2., height / 2.);

        tap(&ui, x, y);
        assert_eq!(
            singles.borrow().len(),
            1,
            "single remains immediate when zoom is disabled"
        );
        let first = singles.borrow()[0];
        assert!((first.0 - 0.5).abs() < 0.02 && (first.1 - 0.5).abs() < 0.02);
        ui.set_reader_double_tap_enabled(true);
        advance(20);
        tap(&ui, x, y);
        advance(200);
        assert_eq!(singles.borrow().len(), 1, "wait for a possible double tap");
        advance(60);
        assert_eq!(singles.borrow().len(), 2);

        tap(&ui, x, y);
        advance(80);
        tap(&ui, x + 4., y + 3.);
        advance(300);
        assert_eq!(
            doubles.borrow().len(),
            1,
            "nearby stationary taps zoom once"
        );
        assert_eq!(
            singles.borrow().len(),
            2,
            "double taps never dispatch a control tap"
        );
        assert!(!ui.get_reader_osd(), "double tap never flashes the OSD");

        tap(&ui, x, y);
        advance(30);
        touch(&ui, x, y, TouchPhase::Started);
        advance(50);
        touch(&ui, x, y - 120., TouchPhase::Moved);
        advance(100);
        touch(&ui, x, y - 180., TouchPhase::Moved);
        touch(&ui, x, y - 180., TouchPhase::Ended);
        advance(400);
        assert_eq!(
            singles.borrow().len(),
            2,
            "a pan cancels pending taps even before delayed press delivery"
        );
        assert_eq!(doubles.borrow().len(), 1);
        assert!(
            ui.get_scroll_y() < 0.,
            "the image still yields to Flickable"
        );

        // A second press that becomes a hold must also discard the first tap.
        tap(&ui, x, y);
        advance(30);
        touch(&ui, x, y, TouchPhase::Started);
        advance(700);
        assert!(ui.get_reader_context_visible());
        touch(&ui, x, y, TouchPhase::Ended);
        advance(300);
        assert_eq!(
            singles.borrow().len(),
            2,
            "hold and release consume both pending taps"
        );
        ui.invoke_close_reader_context();
        advance(50);

        ui.global::<ReaderSettings>().set_long_press_actions(false);
        advance(20);
        touch(&ui, x, y, TouchPhase::Started);
        advance(800);
        assert!(
            !ui.get_reader_context_visible(),
            "long press can be disabled"
        );
        touch(&ui, x, y, TouchPhase::Ended);
        advance(300);
        assert_eq!(singles.borrow().len(), 3);
        right_click(&ui, x, y);
        assert!(
            ui.get_reader_context_visible(),
            "right click remains independent of long press"
        );
        ui.invoke_close_reader_context();
        advance(50);

        tap(&ui, x, y);
        touch(&ui, x, y, TouchPhase::Started);
        advance(150);
        // Exercise the TouchArea cancellation delivered by Flickable/pointer
        // exit. Slint 1.18's raw single-finger TouchPhase::Cancelled synthesizes
        // a release first, so it cannot represent this cancellation contract.
        ui.window().dispatch_event(WindowEvent::PointerExited);
        touch(&ui, x, y, TouchPhase::Cancelled);
        advance(400);
        assert_eq!(
            singles.borrow().len(),
            3,
            "cancelled gestures discard pending taps"
        );
        assert_eq!(doubles.borrow().len(), 1);
    }
}
