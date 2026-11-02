use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use nova_reader::{AppWindow, PageRow};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, LogicalSize, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..8 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn control(ui: &AppWindow, label: &str, role: AccessibleRole) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(role))
        .unwrap_or_else(|| panic!("missing reader control {label}"))
}

#[test]
fn chapter_page_and_progress_controls_follow_the_effective_mode_while_open() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    ui.set_pages(ModelRc::new(VecModel::from(
        (0..3)
            .map(|index| PageRow {
                page_id: format!("page-{index}").into(),
                ratio: 1.,
                loaded: true,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    let actions = Rc::new(RefCell::new(Vec::new()));
    ui.on_reader_action({
        let actions = actions.clone();
        move |action| {
            actions.borrow_mut().push(match action.as_str() {
                "previous-chapter" => ("chapter", -1),
                "next-chapter" => ("chapter", 1),
                "previous-page" => ("page", -1),
                "next-page" => ("page", 1),
                other => panic!("unexpected reader action {other}"),
            });
        }
    });
    let keyboard_steps = Rc::new(RefCell::new(Vec::new()));
    ui.on_page_step({
        let steps = keyboard_steps.clone();
        move |step| steps.borrow_mut().push(step)
    });
    let seeks = Rc::new(RefCell::new(Vec::new()));
    ui.on_reader_seek({
        let seeks = seeks.clone();
        let weak = ui.as_weak();
        move |value| {
            seeks.borrow_mut().push(value);
            weak.unwrap().set_reader_progress(value);
        }
    });
    ui.show().unwrap();
    for mode in [0, 1, 2, 3] {
        ui.set_reading_mode(mode);
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::RightArrow.into(),
        });
        ui.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::RightArrow.into(),
        });
        assert_eq!(
            keyboard_steps.borrow().last(),
            Some(&(if mode == 2 { -1 } else { 1 }))
        );
    }
    for width in [390., 1120.] {
        ui.window().set_size(LogicalSize::new(width, 844.));
        ui.set_reader_osd(true);
        // The direction is resolved independently from the user's Default or
        // Automatic label and from any chapter list order setting.
        ui.set_reader_mode_label("Default".into());
        for mode in [0, 1, 2, 3, 2, 0] {
            ui.set_reading_mode(mode);
            ui.set_current_page(1);
            ui.set_can_previous(true);
            ui.set_can_next(true);
            ui.set_reader_progress(0.2);
            settle();
            assert!(
                ui.get_reader_osd(),
                "changing effective mode retains controls"
            );
            let previous = control(&ui, "Previous chapter", AccessibleRole::Button);
            let next = control(&ui, "Next chapter", AccessibleRole::Button);
            assert_eq!(
                next.absolute_position().x < previous.absolute_position().x,
                mode == 2,
                "Next points toward the effective reading direction at {width}, mode {mode}"
            );
            previous.invoke_accessible_default_action();
            assert_eq!(actions.borrow().last(), Some(&("chapter", -1)));
            next.invoke_accessible_default_action();
            assert_eq!(actions.borrow().last(), Some(&("chapter", 1)));

            for (can_previous, can_next) in [(false, true), (true, false)] {
                ui.set_can_previous(can_previous);
                ui.set_can_next(can_next);
                settle();
                for (label, enabled, delta) in [
                    ("Previous chapter", can_previous, -1),
                    ("Next chapter", can_next, 1),
                ] {
                    let button = control(&ui, label, AccessibleRole::Button);
                    assert_eq!(button.accessible_enabled(), Some(enabled));
                    let count = actions.borrow().len();
                    button.invoke_accessible_default_action();
                    assert_eq!(actions.borrow().len(), count + usize::from(enabled));
                    if enabled {
                        assert_eq!(actions.borrow().last(), Some(&("chapter", delta)));
                    }
                }
            }

            if width > 700. {
                let previous = control(&ui, "Previous page", AccessibleRole::Button);
                let next = control(&ui, "Next page", AccessibleRole::Button);
                assert_eq!(
                    next.absolute_position().x < previous.absolute_position().x,
                    mode == 2
                );
                previous.invoke_accessible_default_action();
                assert_eq!(actions.borrow().last(), Some(&("page", -1)));
                next.invoke_accessible_default_action();
                assert_eq!(actions.borrow().last(), Some(&("page", 1)));
                ui.set_current_page(0);
                ui.set_scroll_y(0.);
                settle();
                assert_eq!(
                    control(&ui, "Previous page", AccessibleRole::Button).accessible_enabled(),
                    Some(false)
                );
                if mode != 0 {
                    ui.set_current_page(2);
                    settle();
                    assert_eq!(
                        control(&ui, "Next page", AccessibleRole::Button).accessible_enabled(),
                        Some(false)
                    );
                }
            }

            let slider = control(&ui, "Chapter progress", AccessibleRole::Slider);
            let position = slider.absolute_position();
            let size = slider.size();
            let point = LogicalPosition::new(
                position.x + size.width * 0.25,
                position.y + size.height / 2.,
            );
            ui.window().dispatch_event(WindowEvent::PointerPressed {
                position: point,
                button: PointerEventButton::Left,
            });
            ui.window().dispatch_event(WindowEvent::PointerReleased {
                position: point,
                button: PointerEventButton::Left,
            });
            settle();
            let expected = if mode == 2 { 0.75 } else { 0.25 };
            assert!(
                (seeks.borrow().last().unwrap() - expected).abs() < 0.01,
                "physical scrub direction at mode {mode}"
            );
            let before = ui.get_reader_progress();
            slider.invoke_accessible_increment_action();
            assert!(
                (ui.get_reader_progress() - before - 0.001).abs() < 0.0001,
                "accessible increment increases logical progress in every mode"
            );
        }
    }
}

#[test]
fn rtl_progress_paints_completed_pixels_from_the_right_edge() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    use nova_reader::Theme;
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    ui.set_reading_mode(2);
    // Distinct opaque test colors isolate filled/unfilled rail pixels from
    // the dark reader scrim, without depending on a user's chosen palette.
    let mut palette = ui.global::<Theme>().get_current();
    palette.slider_track = slint::Color::from_rgb_u8(30, 45, 60);
    palette.accent_soft = slint::Color::from_rgb_u8(170, 210, 250);
    ui.global::<Theme>().set_current(palette);
    ui.show().unwrap();
    settle();
    ui.set_reader_osd(true);
    for width in [390., 1120.] {
        ui.window().set_size(LogicalSize::new(width, 844.));
        for progress in [0., 0.25, 1.] {
            ui.set_reader_progress(progress);
            settle();
            let slider = control(&ui, "Chapter progress", AccessibleRole::Slider);
            let position = slider.absolute_position();
            let size = slider.size();
            let snapshot = ui.window().take_snapshot().unwrap();
            for fraction in [0.1, 0.5, 0.9] {
                let x = (position.x + size.width * fraction) as usize;
                let y = (position.y + size.height / 2.) as usize;
                let pixel = snapshot.as_slice()[y * snapshot.width() as usize + x];
                let expected = if fraction >= 1. - progress {
                    [170, 210, 250]
                } else {
                    [30, 45, 60]
                };
                assert_eq!(
                    [pixel.r, pixel.g, pixel.b],
                    expected,
                    "RTL rail at {width}px, progress {progress}, position {fraction}"
                );
            }
        }
    }
}
