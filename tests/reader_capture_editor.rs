use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use i_slint_core::{input::TouchPhase, lengths::LogicalPoint, platform::InternalEvent};
use nova_reader::{AppWindow, CaptureCropRect, CaptureSettings, PageRow};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, LogicalSize, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..16 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn button(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing capture action {label}"))
}

fn key(ui: &AppWindow, key: Key) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    settle();
}

fn center(element: &ElementHandle) -> LogicalPosition {
    let point = element.absolute_position();
    let size = element.size();
    LogicalPosition::new(point.x + size.width / 2., point.y + size.height / 2.)
}

fn press(ui: &AppWindow, point: LogicalPosition) {
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position: point,
        button: PointerEventButton::Left,
    });
}

fn release(ui: &AppWindow, point: LogicalPosition) {
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position: point,
        button: PointerEventButton::Left,
    });
    settle();
}

fn canvas_image(height: u32) -> slint::Image {
    slint::Image::from_rgb8(slint::SharedPixelBuffer::new(400, height))
}

fn drag_input(ui: &AppWindow, touch: bool, phase: TouchPhase, point: LogicalPosition) {
    ui.window().dispatch_event(if touch {
        WindowEvent::internal(InternalEvent::Touch {
            id: 1,
            position: LogicalPoint::new(point.x, point.y),
            phase,
        })
    } else {
        match phase {
            TouchPhase::Started => WindowEvent::PointerPressed {
                position: point,
                button: PointerEventButton::Left,
            },
            TouchPhase::Ended => WindowEvent::PointerReleased {
                position: point,
                button: PointerEventButton::Left,
            },
            _ => WindowEvent::PointerMoved { position: point },
        }
    });
}

fn hold_pointer() {
    for _ in 0..20 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(25));
    }
}

fn assert_crop_close(actual: CaptureCropRect, expected: CaptureCropRect) {
    for (actual, expected) in [
        (actual.x, expected.x),
        (actual.y, expected.y),
        (actual.width, expected.width),
        (actual.height, expected.height),
    ] {
        assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
    }
}

#[test]
fn edge_handle_press_and_inward_drag_never_extend_the_selection() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    let actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_capture_editor_action({
        let actions = actions.clone();
        move |action| actions.borrow_mut().push(action.to_string())
    });
    ui.show().unwrap();
    for (width, height) in [(390., 844.), (1120., 780.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        for touch in [false, true] {
            for (label, inward) in [
                ("Resize selection bottom", -1.),
                ("Resize selection bottom left", -1.),
                ("Resize selection bottom right", -1.),
                ("Resize selection top", 1.),
                ("Resize selection top left", 1.),
                ("Resize selection top right", 1.),
            ] {
                ui.set_capture_editor_visible(false);
                ui.set_capture_editor_image(canvas_image(800));
                ui.set_capture_editor_crop(CaptureCropRect {
                    x: 0.1,
                    y: 0.,
                    width: 0.8,
                    height: 1.,
                });
                ui.set_capture_editor_can_extend_above(true);
                ui.set_capture_editor_can_extend_below(true);
                ui.set_capture_editor_visible(true);
                settle();
                let before = ui.get_capture_editor_crop();
                let center = center(&button(&ui, label));
                // Touch the outside half of the 44px target. This is already in
                // the edge-scroll band, without any gesture toward that edge.
                let start = LogicalPosition::new(center.x, center.y - inward * 10.);
                drag_input(&ui, touch, TouchPhase::Started, start);
                hold_pointer();
                assert!(actions.borrow().is_empty(), "stationary {label} at {width}");
                assert_crop_close(ui.get_capture_editor_crop(), before.clone());

                // An inward move still inside the same edge band must shorten the
                // crop, even when the finger pauses before the next input event.
                let inside = LogicalPosition::new(start.x, start.y + inward * 4.);
                drag_input(&ui, touch, TouchPhase::Moved, inside);
                let shortened = ui.get_capture_editor_crop();
                assert!(
                    shortened.height < before.height,
                    "inward {label} at {width}"
                );
                hold_pointer();
                assert_crop_close(ui.get_capture_editor_crop(), shortened.clone());
                assert!(actions.borrow().is_empty(), "inward {label} cannot extend");
                let further = LogicalPosition::new(start.x, start.y + inward * 28.);
                drag_input(&ui, touch, TouchPhase::Moved, further);
                assert!(ui.get_capture_editor_crop().height < shortened.height);
                drag_input(&ui, touch, TouchPhase::Ended, further);
                settle();
                assert!(actions.borrow().is_empty());
            }
        }
    }
}

#[test]
fn shortening_into_the_opposite_edge_band_never_scrolls_or_extends() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    ui.on_capture_editor_action(|action| panic!("inward drag extended: {action}"));
    ui.show().unwrap();
    for (width, height) in [(390., 844.), (1120., 780.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        for touch in [false, true] {
            for (label, bottom) in [
                ("Resize selection bottom", true),
                ("Resize selection bottom left", true),
                ("Resize selection bottom right", true),
                ("Resize selection top", false),
                ("Resize selection top left", false),
                ("Resize selection top right", false),
            ] {
                ui.set_capture_editor_visible(false);
                ui.set_capture_editor_image(canvas_image(800));
                ui.set_capture_editor_crop(CaptureCropRect {
                    x: 0.1,
                    y: 0.,
                    width: 0.8,
                    height: 1.,
                });
                ui.set_capture_editor_can_extend_above(true);
                ui.set_capture_editor_can_extend_below(true);
                ui.set_capture_editor_visible(true);
                settle();
                // Give the canvas room to scroll toward the opposite edge:
                // a full-size initial image alone would hide that regression.
                ui.set_capture_editor_image(canvas_image(1600));
                settle();
                if bottom {
                    key(&ui, Key::PageDown);
                    key(&ui, Key::PageDown);
                }
                let canvas = ElementHandle::find_by_element_id(&ui, "CaptureEditor::canvas")
                    .next()
                    .unwrap();
                let artwork =
                    ElementHandle::find_by_element_id(&ui, "CaptureEditor::image-content")
                        .next()
                        .unwrap();
                let before_y = artwork.absolute_position().y;
                let start = center(&button(&ui, label));
                let opposite = LogicalPosition::new(
                    start.x,
                    canvas.absolute_position().y
                        + if bottom {
                            20.
                        } else {
                            canvas.size().height - 20.
                        },
                );
                drag_input(&ui, touch, TouchPhase::Started, start);
                hold_pointer();
                drag_input(&ui, touch, TouchPhase::Moved, opposite);
                let shortened = ui.get_capture_editor_crop();
                assert!(shortened.height < 1., "{label} must shorten");
                hold_pointer();
                assert_crop_close(ui.get_capture_editor_crop(), shortened);
                assert!(
                    (artwork.absolute_position().y - before_y).abs() < 0.01,
                    "{label} at {width}, touch={touch}: opposite edge cannot scroll"
                );
                drag_input(&ui, touch, TouchPhase::Ended, opposite);
                settle();
            }
        }
    }
}

#[test]
fn reversing_an_edge_drag_stops_scrolling_before_another_timer_tick() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    ui.on_capture_editor_action(|action| panic!("unexpected extension: {action}"));
    ui.show().unwrap();
    for (width, height) in [(390., 844.), (1120., 780.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        for touch in [false, true] {
            for above in [false, true] {
                ui.set_capture_editor_visible(false);
                ui.set_capture_editor_image(canvas_image(800));
                ui.set_capture_editor_crop(CaptureCropRect {
                    x: 0.1,
                    y: if above { 0.5 } else { 0. },
                    width: 0.8,
                    height: 0.5,
                });
                ui.set_capture_editor_can_extend_above(true);
                ui.set_capture_editor_can_extend_below(true);
                ui.set_capture_editor_visible(true);
                settle();
                ui.set_capture_editor_image(canvas_image(1600));
                settle();
                if above {
                    key(&ui, Key::PageDown);
                    key(&ui, Key::PageDown);
                }
                let label = if above {
                    "Resize selection top left"
                } else {
                    "Resize selection bottom right"
                };
                let outward = if above { -1. } else { 1. };
                let center = center(&button(&ui, label));
                let start = LogicalPosition::new(center.x, center.y + outward * 10.);
                drag_input(&ui, touch, TouchPhase::Started, start);
                let edge = LogicalPosition::new(start.x, start.y + outward * 10.);
                drag_input(&ui, touch, TouchPhase::Moved, edge);
                let dragged = ui.get_capture_editor_crop();
                for _ in 0..3 {
                    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(25));
                }
                let scrolled = ui.get_capture_editor_crop();
                assert!(
                    scrolled.height > dragged.height,
                    "deliberate edge scroll at {width}"
                );
                let inward = LogicalPosition::new(edge.x, edge.y - outward * 4.);
                drag_input(&ui, touch, TouchPhase::Moved, inward);
                let shortened = ui.get_capture_editor_crop();
                assert!(shortened.height < scrolled.height);
                hold_pointer();
                assert_crop_close(ui.get_capture_editor_crop(), shortened);
                drag_input(&ui, touch, TouchPhase::Ended, inward);
                settle();
            }
        }
    }
}

#[test]
fn border_drag_survives_async_extension_without_moving_the_finger_or_opposite_edge() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    let requests = Rc::new(RefCell::new(Vec::new()));
    ui.on_capture_editor_action({
        let weak = ui.as_weak();
        let requests = requests.clone();
        move |action| {
            if action == "extend-above" || action == "extend-below" {
                requests.borrow_mut().push(action.to_string());
                weak.unwrap().set_capture_editor_busy(true);
            }
        }
    });
    ui.show().unwrap();
    for (width, height) in [(390., 844.), (1120., 780.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        for touch in [false, true] {
            for (label, opposite, above) in [
                ("Resize selection top", "Resize selection bottom", true),
                (
                    "Resize selection top left",
                    "Resize selection bottom left",
                    true,
                ),
                (
                    "Resize selection top right",
                    "Resize selection bottom right",
                    true,
                ),
                ("Resize selection bottom", "Resize selection top", false),
                (
                    "Resize selection bottom left",
                    "Resize selection top left",
                    false,
                ),
                (
                    "Resize selection bottom right",
                    "Resize selection top right",
                    false,
                ),
            ] {
                ui.set_capture_editor_visible(false);
                ui.set_capture_editor_busy(false);
                ui.set_capture_editor_image(canvas_image(800));
                ui.set_capture_editor_crop(CaptureCropRect {
                    x: 0.1,
                    y: 0.,
                    width: 0.8,
                    height: 1.,
                });
                ui.set_capture_editor_can_extend_above(true);
                ui.set_capture_editor_can_extend_below(true);
                ui.set_capture_editor_visible(true);
                requests.borrow_mut().clear();
                settle();
                let handle = button(&ui, label);
                assert!(handle.size().width >= 48. && handle.size().height >= 48.);
                let mut start = center(&handle);
                if label == "Resize selection top" || label == "Resize selection bottom" {
                    assert!(
                        handle.size().width > 48.,
                        "the border is draggable away from the central knob"
                    );
                    start.x = handle.absolute_position().x + 10.;
                }
                let canvas = ElementHandle::find_by_element_id(&ui, "CaptureEditor::canvas")
                    .next()
                    .unwrap();
                let edge = LogicalPosition::new(
                    start.x,
                    canvas.absolute_position().y
                        + if above { 8. } else { canvas.size().height - 8. },
                );
                let original_opposite = center(&button(&ui, opposite)).y;
                drag_input(&ui, touch, TouchPhase::Started, start);
                drag_input(&ui, touch, TouchPhase::Moved, edge);
                for _ in 0..80 {
                    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(25));
                    if ui.get_capture_editor_busy() {
                        break;
                    }
                }
                assert_eq!(
                    &*requests.borrow(),
                    &[if above {
                        "extend-above"
                    } else {
                        "extend-below"
                    }]
                );
                let before = ui.get_capture_editor_crop();
                // Reverse slightly while the backend is loading. The held
                // finger remains authoritative when the larger canvas arrives.
                let inward = if above { 1. } else { -1. };
                let pointer = LogicalPosition::new(start.x, start.y + inward * 4.);
                drag_input(&ui, touch, TouchPhase::Moved, pointer);
                assert_crop_close(ui.get_capture_editor_crop(), before.clone());
                let projected = if above {
                    CaptureCropRect {
                        y: 0.,
                        height: (1. + before.y + before.height) / 2.,
                        ..before.clone()
                    }
                } else {
                    CaptureCropRect {
                        y: before.y / 2.,
                        height: 1. - before.y / 2.,
                        ..before.clone()
                    }
                };
                ui.set_capture_editor_busy(false);
                ui.set_capture_editor_crop(projected);
                ui.set_capture_editor_image(canvas_image(1600));
                settle();
                assert!(
                    (center(&button(&ui, label)).y - pointer.y).abs() < 1.,
                    "{label}, width={width}, touch={touch}: border must stay under the held pointer"
                );
                assert!(
                    (center(&button(&ui, opposite)).y - original_opposite).abs() < 1.,
                    "opposite source edge must not jump"
                );
                let extended = ui.get_capture_editor_crop();
                hold_pointer();
                assert_crop_close(ui.get_capture_editor_crop(), extended.clone());
                let further = LogicalPosition::new(pointer.x, pointer.y + inward * 20.);
                drag_input(&ui, touch, TouchPhase::Moved, further);
                assert!(
                    ui.get_capture_editor_crop().height < extended.height,
                    "the same gesture can shorten after loading"
                );
                drag_input(&ui, touch, TouchPhase::Ended, further);
                settle();
            }
        }
    }
}

#[test]
fn releasing_during_extension_preserves_the_selected_source_and_keyboard_edges_can_extend() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    let requests = Rc::new(RefCell::new(Vec::new()));
    ui.on_capture_editor_action({
        let weak = ui.as_weak();
        let requests = requests.clone();
        move |action| {
            requests.borrow_mut().push(action.to_string());
            if action == "extend-above" || action == "extend-below" {
                weak.unwrap().set_capture_editor_busy(true);
            }
        }
    });
    for above in [true, false] {
        ui.set_capture_editor_visible(false);
        ui.set_capture_editor_busy(false);
        ui.set_capture_editor_image(canvas_image(800));
        ui.set_capture_editor_crop(CaptureCropRect {
            x: 0.1,
            y: 0.,
            width: 0.8,
            height: 1.,
        });
        ui.set_capture_editor_can_extend_above(true);
        ui.set_capture_editor_can_extend_below(true);
        ui.set_capture_editor_visible(true);
        settle();
        let label = if above {
            "Resize selection top"
        } else {
            "Resize selection bottom"
        };
        let start = center(&button(&ui, label));
        let canvas = ElementHandle::find_by_element_id(&ui, "CaptureEditor::canvas")
            .next()
            .unwrap();
        let edge = LogicalPosition::new(
            start.x,
            canvas.absolute_position().y + if above { 8. } else { canvas.size().height - 8. },
        );
        press(&ui, start);
        ui.window()
            .dispatch_event(WindowEvent::PointerMoved { position: edge });
        for _ in 0..80 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(25));
            if ui.get_capture_editor_busy() {
                break;
            }
        }
        assert!(ui.get_capture_editor_busy());
        release(&ui, edge);
        let selected_top = center(&button(&ui, "Resize selection top")).y;
        let selected_bottom = center(&button(&ui, "Resize selection bottom")).y;
        ui.set_capture_editor_busy(false);
        ui.set_capture_editor_crop(CaptureCropRect {
            x: 0.1,
            y: 0.,
            width: 0.8,
            height: 1.,
        });
        ui.set_capture_editor_image(canvas_image(1600));
        settle();
        assert!((center(&button(&ui, "Resize selection top")).y - selected_top).abs() < 1.);
        assert!((center(&button(&ui, "Resize selection bottom")).y - selected_bottom).abs() < 1.);
        // Source coverage is unchanged even though normalized coordinates now
        // refer to an enlarged canvas. Late publication cannot select a chunk.
        assert!((ui.get_capture_editor_crop().height - 0.5).abs() < 0.001);
    }
    ui.set_capture_editor_visible(false);
    ui.set_capture_editor_busy(false);
    ui.set_capture_editor_image(canvas_image(800));
    ui.set_capture_editor_crop(CaptureCropRect {
        x: 0.1,
        y: 0.,
        width: 0.8,
        height: 1.,
    });
    ui.set_capture_editor_visible(true);
    settle();
    requests.borrow_mut().clear();
    button(&ui, "Resize selection bottom").invoke_accessible_default_action();
    key(&ui, Key::DownArrow);
    assert_eq!(&*requests.borrow(), &["extend-below"]);
    for label in ["Extend above", "Extend below"] {
        assert!(
            ElementHandle::find_by_accessible_label(&ui, label)
                .next()
                .is_none()
        );
    }
}

#[test]
fn selection_editor_resizes_with_pointer_and_keyboard_and_owns_reader_input() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    let actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_capture_editor_action({
        let actions = actions.clone();
        move |action| actions.borrow_mut().push(action.to_string())
    });
    let changes = Rc::new(RefCell::new(Vec::new()));
    ui.on_capture_editor_selection_changed({
        let changes = changes.clone();
        move |crop| changes.borrow_mut().push(crop)
    });
    ui.on_page_step(|_| panic!("crop editor must consume page navigation"));
    ui.on_reader_action(|_| panic!("crop editor must consume reader shortcuts"));
    ui.show().unwrap();

    for (width, height) in [(390., 844.), (1120., 780.)] {
        ui.set_capture_editor_visible(false);
        ui.window().set_size(LogicalSize::new(width, height));
        ui.set_capture_editor_image(canvas_image(800));
        ui.set_capture_editor_crop(CaptureCropRect {
            x: 0.1,
            y: 0.12,
            width: 0.8,
            height: 0.7,
        });
        ui.set_capture_editor_preview_mode(false);
        ui.set_capture_editor_copy(false);
        ui.set_capture_editor_can_extend_above(true);
        ui.set_capture_editor_can_extend_below(true);
        ui.set_capture_editor_visible(true);
        settle();
        for label in [
            "Cancel",
            "Preview",
            "Save",
            "Resize selection top left",
            "Resize selection top",
            "Resize selection top right",
            "Resize selection right",
            "Resize selection bottom right",
            "Resize selection bottom",
            "Resize selection bottom left",
            "Resize selection left",
        ] {
            let control = button(&ui, label);
            let position = control.absolute_position();
            let size = control.size();
            assert!(size.width >= 44. && size.height >= 44., "{label}");
            assert!(position.x >= -0.5 && position.y >= -0.5, "{label}");
            assert!(position.x + size.width <= width + 0.5, "{label}");
            assert!(position.y + size.height <= height + 0.5, "{label}");
        }

        for removed in ["Extend above", "Extend below"] {
            assert!(
                ElementHandle::find_by_accessible_label(&ui, removed)
                    .next()
                    .is_none()
            );
        }

        let start = center(&button(&ui, "Resize selection left"));
        let end = LogicalPosition::new(start.x + 22., start.y);
        press(&ui, start);
        ui.window()
            .dispatch_event(WindowEvent::PointerMoved { position: end });
        release(&ui, end);
        let dragged = ui.get_capture_editor_crop();
        assert!(dragged.x > 0.15, "left side follows pointer at {width}");
        assert!((dragged.x + dragged.width - 0.9).abs() < 0.001);
        assert!((dragged.height - 0.7).abs() < 0.001);

        button(&ui, "Resize selection bottom right").invoke_accessible_default_action();
        key(&ui, Key::LeftArrow);
        key(&ui, Key::UpArrow);
        let resized = ui.get_capture_editor_crop();
        assert!((resized.width - (dragged.width - 0.01)).abs() < 0.001);
        assert!((resized.height - (dragged.height - 0.01)).abs() < 0.001);
        assert!(!changes.borrow().is_empty());
        assert!(!ui.get_reader_osd());
        key(&ui, Key::PageDown);

        button(&ui, "Preview").mock_single_click(PointerEventButton::Left);
        assert_eq!(actions.borrow().last().unwrap(), "preview");
        ui.set_capture_editor_preview_mode(true);
        settle();
        button(&ui, "Adjust selection");
        assert!(
            ElementHandle::find_by_accessible_label(&ui, "Resize selection left")
                .next()
                .is_none()
        );
        key(&ui, Key::Escape);
        assert_eq!(actions.borrow().last().unwrap(), "adjust");
        ui.set_capture_editor_preview_mode(false);
        ui.set_capture_editor_copy(true);
        settle();
        button(&ui, "Copy").invoke_accessible_default_action();
        assert_eq!(actions.borrow().last().unwrap(), "confirm");
        ui.set_capture_editor_busy(true);
        settle();
        let count = actions.borrow().len();
        button(&ui, "Copy").invoke_accessible_default_action();
        assert_eq!(actions.borrow().len(), count, "busy export is guarded");
        key(&ui, Key::Escape);
        assert_eq!(actions.borrow().last().unwrap(), "cancel");
        ui.set_capture_editor_busy(false);
    }
}

#[test]
fn selection_handle_auto_scrolls_and_requests_both_chapter_extensions() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    let actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_capture_editor_action({
        let actions = actions.clone();
        let weak = ui.as_weak();
        move |action| {
            actions.borrow_mut().push(action.to_string());
            // Production refresh owns the gesture while composing an expanded
            // canvas. Acknowledging that transition also bounds each request.
            if action == "extend-above" || action == "extend-below" {
                weak.unwrap().set_capture_editor_busy(true);
            }
        }
    });
    ui.show().unwrap();
    for (width, height) in [(390., 844.), (1120., 780.)] {
        ui.set_capture_editor_visible(false);
        ui.set_capture_editor_busy(false);
        ui.window().set_size(LogicalSize::new(width, height));
        ui.set_capture_editor_image(canvas_image(800));
        ui.set_capture_editor_crop(CaptureCropRect {
            x: 0.1,
            y: 0.,
            width: 0.8,
            height: 0.5,
        });
        ui.set_capture_editor_can_extend_above(true);
        ui.set_capture_editor_can_extend_below(true);
        ui.set_capture_editor_visible(true);
        settle();

        // A second canvas is twice as tall, as after a backend extension. Its
        // scale stays stable, and dragging the bottom edge pans the canvas.
        ui.set_capture_editor_image(canvas_image(1600));
        settle();
        let start = center(&button(&ui, "Resize selection bottom right"));
        let edge = LogicalPosition::new(start.x, height - 90.);
        press(&ui, start);
        ui.window()
            .dispatch_event(WindowEvent::PointerMoved { position: edge });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(25));
        let first = ui.get_capture_editor_crop();
        for _ in 0..12 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(25));
        }
        let scrolled = ui.get_capture_editor_crop();
        assert!(
            scrolled.height > first.height + 0.05,
            "edge drag scrolls at {width}"
        );
        for _ in 0..72 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(25));
        }
        release(&ui, edge);
        assert!(
            actions
                .borrow()
                .iter()
                .any(|action| action == "extend-below")
        );
        ui.set_capture_editor_image(canvas_image(2400));
        ui.set_capture_editor_busy(false);
        settle();

        // Return to the top of the expanded canvas before locating its
        // clipped handle, then drag into the previous chapter area.
        key(&ui, Key::PageUp);
        key(&ui, Key::PageUp);
        button(&ui, "Resize selection top left").invoke_accessible_default_action();
        let start = center(&button(&ui, "Resize selection top left"));
        let edge = LogicalPosition::new(start.x, 65.);
        press(&ui, start);
        ui.window()
            .dispatch_event(WindowEvent::PointerMoved { position: edge });
        for _ in 0..20 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(25));
        }
        release(&ui, edge);
        assert!(
            actions
                .borrow()
                .iter()
                .any(|action| action == "extend-above")
        );
        let crop = ui.get_capture_editor_crop();
        assert!(crop.x >= 0. && crop.y >= 0.);
        assert!(crop.x + crop.width <= 1.001 && crop.y + crop.height <= 1.001);
        actions.borrow_mut().clear();
    }
}

#[test]
fn simple_screencap_setting_hides_both_immediate_actions() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_pages(ModelRc::new(VecModel::from(vec![PageRow {
        loaded: true,
        page_id: "page-1".into(),
        ratio: 2.,
        ..Default::default()
    }])));
    ui.set_section(6);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    ui.invoke_open_reader_context(0, 10., 10.);
    settle();
    button(&ui, "Save image (simple)");
    button(&ui, "Save screencap (simple)");
    button(&ui, "Copy screencap (simple)");
    let grouped_actions = [
        "Save image (simple)",
        "Save image…",
        "Save screencap (simple)",
        "Save screencap…",
        "Copy image to clipboard",
        "Copy screencap (simple)",
        "Copy screencap",
    ];
    let positions = grouped_actions.map(|label| center(&button(&ui, label)).y);
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "simple actions precede their corresponding normal actions"
    );
    for pair in [(0, 1), (1, 2), (2, 3), (4, 5), (5, 6)] {
        assert!((positions[pair.1] - positions[pair.0] - 54.).abs() < 0.5);
    }
    assert!(
        positions[4] - positions[3] >= 62.,
        "the divider separates save from copy, keeping each simple/normal pair together"
    );
    // The simple save's subtitle says what it will write.
    ui.set_reader_context_save_detail("Cached · WebP".into());
    settle();
    assert_eq!(
        button(&ui, "Save image (simple)")
            .accessible_description()
            .as_deref(),
        Some("Cached · WebP")
    );
    ui.global::<CaptureSettings>().set_simple_screencaps(false);
    settle();
    for label in [
        "Save image (simple)",
        "Save screencap (simple)",
        "Copy screencap (simple)",
    ] {
        assert!(
            ElementHandle::find_by_accessible_label(&ui, label)
                .next()
                .is_none()
        );
    }
    for label in [
        "Save image…",
        "Copy image to clipboard",
        "Save screencap…",
        "Copy screencap",
    ] {
        button(&ui, label);
    }
}

#[test]
fn crop_canvas_dims_only_the_four_regions_outside_the_selection() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(200, 400);
    pixels
        .make_mut_slice()
        .fill(slint::Rgb8Pixel::new(100, 200, 240));
    ui.set_capture_editor_image(slint::Image::from_rgb8(pixels));
    ui.set_capture_editor_crop(CaptureCropRect {
        x: 0.2,
        y: 0.2,
        width: 0.6,
        height: 0.6,
    });
    ui.show().unwrap();
    for (width, height) in [(390., 844.), (1120., 780.)] {
        ui.window().set_size(LogicalSize::new(width, height));
        ui.set_capture_editor_visible(true);
        settle();
        let canvas = ElementHandle::find_by_element_id(&ui, "CaptureEditor::image-content")
            .next()
            .unwrap();
        let origin = canvas.absolute_position();
        let size = canvas.size();
        let snapshot = ui.window().take_snapshot().unwrap();
        let pixel = |x: f32, y: f32| {
            let x = (origin.x + size.width * x) as usize;
            let y = (origin.y + size.height * y) as usize;
            let p = snapshot.as_slice()[y * snapshot.width() as usize + x];
            [p.r, p.g, p.b]
        };
        assert_eq!(
            pixel(0.5, 0.5),
            [100, 200, 240],
            "selected pixels remain clear"
        );
        for (x, y) in [(0.5, 0.1), (0.5, 0.9), (0.1, 0.5), (0.9, 0.5)] {
            assert_eq!(pixel(x, y), [40, 80, 96], "outside dim region at {x}, {y}");
        }
        ui.set_capture_editor_visible(false);
    }
}

#[test]
#[ignore = "writes native phone and desktop capture editor previews for visual review"]
fn render_capture_editor_previews() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let storage = tempfile::tempdir().unwrap();
    nova_storage::init_at(storage.path());
    let ui = AppWindow::new().unwrap();
    nova_reader::appearance::init(&ui).unwrap();
    let source = std::env::var("NOVA_READER_CAPTURE_PREVIEW_IMAGE").unwrap_or_else(|_| {
        "/workspace/artifacts/reader-preview-little-nemo-1905-10-15.jpg".into()
    });
    let decoded = image::open(source).unwrap().to_rgb8();
    let image = slint::Image::from_rgb8(slint::SharedPixelBuffer::clone_from_slice(
        decoded.as_raw(),
        decoded.width(),
        decoded.height(),
    ));
    ui.set_section(6);
    ui.set_capture_editor_image(image.clone());
    ui.set_capture_editor_crop(CaptureCropRect {
        x: 0.08,
        y: 0.15,
        width: 0.84,
        height: 0.7,
    });
    ui.set_capture_editor_can_extend_above(true);
    ui.set_capture_editor_can_extend_below(true);
    ui.show().unwrap();
    let output = std::path::PathBuf::from(
        std::env::var("NOVA_READER_CAPTURE_PREVIEW_DIR")
            .unwrap_or_else(|_| "/workspace/artifacts/reader-capture-border-preview".into()),
    );
    std::fs::create_dir_all(&output).unwrap();
    for (width, height) in [(390, 844), (1120, 780)] {
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));
        ui.set_capture_editor_visible(true);
        settle();
        let snapshot = ui.window().take_snapshot().unwrap();
        image::save_buffer(
            output.join(format!("capture-editor-{width}x{height}.png")),
            snapshot.as_bytes(),
            snapshot.width(),
            snapshot.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
        ui.set_capture_editor_visible(false);
    }
}
