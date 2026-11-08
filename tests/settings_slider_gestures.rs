//! A horizontal slider drag must keep its settings document stationary.
use std::{cell::Cell, rc::Rc, time::Duration};

use i_slint_backend_testing::{AccessibleRole, ElementHandle, mock_elapsed_time};
use i_slint_core::{input::TouchPhase, lengths::LogicalPoint, platform::InternalEvent};
use nova_reader::{Anim, AppWindow, ReaderSettings};
use slint::{
    ComponentHandle, LogicalPosition, LogicalSize,
    platform::{PointerEventButton, WindowEvent},
};

fn advance(ms: u64) {
    for _ in 0..ms / 10 {
        mock_elapsed_time(Duration::from_millis(10));
    }
}
fn touch(ui: &AppWindow, x: f32, y: f32, phase: TouchPhase) {
    ui.window()
        .dispatch_event(WindowEvent::internal(InternalEvent::Touch {
            id: 1,
            position: LogicalPoint::new(x, y),
            phase,
        }));
}
fn element(ui: &AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(ui, id)
        .next()
        .unwrap_or_else(|| panic!("missing {id}"))
}
fn filters(ui: &AppWindow) {
    ElementHandle::find_by_accessible_label(ui, "Color")
        .find(|item| item.accessible_role() == Some(AccessibleRole::Tab))
        .expect("reader settings Color tab")
        .invoke_accessible_default_action();
    advance(400);
}
fn vertical_swipe(ui: &AppWindow, x: f32, y: f32) {
    touch(ui, x, y, TouchPhase::Started);
    advance(30);
    for step in 1..=12 {
        touch(ui, x, y - step as f32 * 8., TouchPhase::Moved);
        advance(20);
    }
    touch(ui, x, y - 96., TouchPhase::Ended);
    advance(400);
}

#[test]
fn reader_settings_slider_owns_diagonal_touch_and_mouse_drags_then_scroll_resumes() {
    i_slint_backend_testing::init_no_event_loop();
    for (section, width) in [(19, 390.), (6, 390.), (19, 1200.), (6, 1200.)] {
        for scenario in ["touch", "mouse", "cancel", "close"] {
            if scenario == "close" && section != 6 {
                continue;
            }
            let ui = AppWindow::new().unwrap();
            ui.global::<Anim>().set_enabled(false);
            ui.set_section(section);
            ui.set_touch_menus(scenario != "mouse");
            ui.global::<ReaderSettings>().set_custom_brightness(true);
            ui.global::<ReaderSettings>().set_color_filter(true);
            let edits = Rc::new(Cell::new(0));
            ui.global::<ReaderSettings>().on_settings_changed({
                let weak = ui.as_weak();
                let edits = edits.clone();
                move |key, value| {
                    if key == "brightness" {
                        edits.set(edits.get() + 1);
                        if let Some(ui) = weak.upgrade() {
                            ui.global::<ReaderSettings>().set_brightness(value);
                        }
                    }
                }
            });
            ui.show().unwrap();
            ui.window().set_size(LogicalSize::new(width, 700.));
            advance(400);
            let open = || {
                ui.set_reader_osd(true);
                advance(200);
                ElementHandle::find_by_accessible_label(&ui, "Settings")
                    .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
                    .expect("OSD settings action")
                    .invoke_accessible_default_action();
                advance(400);
            };
            if section == 6 {
                open();
            }
            filters(&ui);
            let content = element(&ui, "ReaderSettingsForm::settings-content");
            let scroll = element(&ui, "ReaderSettingsForm::settings-scroll");
            let slider = ElementHandle::find_by_accessible_label(&ui, "Brightness")
                .find(|item| item.accessible_role() == Some(AccessibleRole::Slider))
                .expect("brightness slider");
            let p = slider.absolute_position();
            let size = slider.size();
            let start = LogicalPosition::new(p.x + size.width * 0.15, p.y + size.height / 2.);
            let original_y = content.absolute_position().y;
            if scenario == "mouse" {
                ui.window()
                    .dispatch_event(WindowEvent::PointerMoved { position: start });
                ui.window().dispatch_event(WindowEvent::PointerPressed {
                    position: start,
                    button: PointerEventButton::Left,
                });
            } else {
                touch(&ui, start.x, start.y, TouchPhase::Started);
            }
            advance(200);
            for step in 1..=10 {
                let position = LogicalPosition::new(
                    start.x + size.width * 0.07 * step as f32,
                    start.y - step as f32 * 3.,
                );
                if scenario == "mouse" {
                    ui.window()
                        .dispatch_event(WindowEvent::PointerMoved { position });
                } else {
                    touch(&ui, position.x, position.y, TouchPhase::Moved);
                }
                advance(20);
                assert!(
                    (content.absolute_position().y - original_y).abs() < 0.5,
                    "slider drag cannot pan settings: {scenario} section={section} width={width}"
                );
            }
            assert!(edits.get() > 0, "gesture actually adjusts the slider");
            let end = LogicalPosition::new(start.x + size.width, start.y - 50.);
            if scenario == "mouse" {
                ui.window().dispatch_event(WindowEvent::PointerReleased {
                    position: end,
                    button: PointerEventButton::Left,
                });
            } else {
                if scenario == "close" {
                    ElementHandle::find_by_accessible_label(&ui, "Close reader settings")
                        .next()
                        .expect("close reader settings")
                        .invoke_accessible_default_action();
                    advance(200);
                }
                touch(
                    &ui,
                    end.x,
                    end.y,
                    if scenario == "touch" {
                        TouchPhase::Ended
                    } else {
                        TouchPhase::Cancelled
                    },
                );
            }
            advance(400);
            if scenario == "close" {
                open();
                filters(&ui);
            }
            ui.set_touch_menus(true);
            let viewport = scroll.absolute_position();
            let before_swipe = content.absolute_position().y;
            vertical_swipe(
                &ui,
                viewport.x + 8.,
                viewport.y + scroll.size().height * 0.85,
            );
            assert!(
                content.absolute_position().y < before_swipe - 10.,
                "scroll resumes: {scenario} section={section} width={width}, before={before_swipe}, after={}",
                content.absolute_position().y
            );
        }
    }
}

#[test]
fn source_compression_slider_owns_diagonal_drags_and_releases_scroll() {
    use nova_reader::SourceCompressionSettings;
    i_slint_backend_testing::init_no_event_loop();
    for width in [390., 1120.] {
        for scenario in ["touch", "mouse", "cancel"] {
            let ui = AppWindow::new().unwrap();
            ui.global::<Anim>().set_enabled(false);
            ui.set_section(8);
            ui.set_heading("Source settings".into());
            ui.set_touch_menus(scenario != "mouse");
            let settings = ui.global::<SourceCompressionSettings>();
            settings.set_use_global(false);
            let edits = Rc::new(Cell::new(0));
            settings.on_settings_changed({
                let edits = edits.clone();
                move |key, _| {
                    if key == "cache_quality" {
                        edits.set(edits.get() + 1);
                    }
                }
            });
            ui.show().unwrap();
            ui.window().set_size(LogicalSize::new(width, 844.));
            advance(300);
            let content = element(&ui, "SourceSettingsPage::source-settings-content");
            let slider = ElementHandle::find_by_accessible_label(&ui, "Cache quality")
                .find(|e| e.accessible_role() == Some(AccessibleRole::Slider))
                .unwrap();
            let p = slider.absolute_position();
            let size = slider.size();
            let start = LogicalPosition::new(p.x + size.width * 0.15, p.y + size.height / 2.);
            let original_y = content.absolute_position().y;
            if scenario == "mouse" {
                ui.window()
                    .dispatch_event(WindowEvent::PointerMoved { position: start });
                ui.window().dispatch_event(WindowEvent::PointerPressed {
                    position: start,
                    button: PointerEventButton::Left,
                });
            } else {
                touch(&ui, start.x, start.y, TouchPhase::Started);
            }
            advance(100);
            for step in 1..=10 {
                let position = LogicalPosition::new(
                    start.x + size.width * 0.07 * step as f32,
                    start.y - step as f32 * 3.,
                );
                if scenario == "mouse" {
                    ui.window()
                        .dispatch_event(WindowEvent::PointerMoved { position });
                } else {
                    touch(&ui, position.x, position.y, TouchPhase::Moved);
                }
                advance(20);
                assert!(
                    (content.absolute_position().y - original_y).abs() < 0.5,
                    "source slider pans document: {width} {scenario}"
                );
            }
            assert!(edits.get() > 0);
            let end = LogicalPosition::new(start.x + size.width, start.y - 50.);
            if scenario == "mouse" {
                ui.window().dispatch_event(WindowEvent::PointerReleased {
                    position: end,
                    button: PointerEventButton::Left,
                });
            } else {
                touch(
                    &ui,
                    end.x,
                    end.y,
                    if scenario == "cancel" {
                        TouchPhase::Cancelled
                    } else {
                        TouchPhase::Ended
                    },
                );
            }
            advance(300);
            ui.set_touch_menus(true);
            let before = content.absolute_position().y;
            vertical_swipe(&ui, content.absolute_position().x + 8., 680.);
            assert!(
                content.absolute_position().y < before - 10.,
                "source scrolling must resume: {width} {scenario}"
            );
        }
    }
}
