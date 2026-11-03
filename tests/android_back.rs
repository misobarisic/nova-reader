//! Android's backend finishes the Activity when system Back is rejected.
//! These tests cover the real Window dispatch result, without an exit callback.
use nova_reader::AppWindow;
use slint::platform::{Key, WindowEvent, WindowEventDispatchResult};
use slint::{ComponentHandle, LogicalSize};
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}
fn press(ui: &AppWindow, key: Key) -> WindowEventDispatchResult {
    let text: slint::SharedString = key.into();
    let result = ui
        .window()
        .dispatch_event_with_result(WindowEvent::KeyPressed { text: text.clone() })
        .unwrap();
    let released = ui
        .window()
        .dispatch_event_with_result(WindowEvent::KeyReleased { text })
        .unwrap();
    if result == WindowEventDispatchResult::Rejected {
        assert_eq!(
            released,
            WindowEventDispatchResult::Rejected,
            "Android's native fallback requires both Back events to be unhandled"
        );
    }
    settle();
    result
}

#[test]
fn android_system_back_yields_only_from_library_root_after_flushing() {
    i_slint_backend_testing::init_no_event_loop();
    for (width, height) in [(390., 844.), (1120., 780.)] {
        let ui = AppWindow::new().unwrap();
        let flushes = Rc::new(Cell::new(0));
        ui.on_flush_progress({
            let flushes = flushes.clone();
            move || flushes.set(flushes.get() + 1)
        });
        let routes = Rc::new(Cell::new(0));
        ui.on_navigate({
            let weak = ui.as_weak();
            let routes = routes.clone();
            move |section| {
                routes.set(routes.get() + 1);
                weak.unwrap().set_section(section);
            }
        });
        ui.show().unwrap();
        ui.window().set_size(LogicalSize::new(width, height));
        settle();
        assert_eq!(press(&ui, Key::Back), WindowEventDispatchResult::Rejected);
        assert_eq!(
            flushes.get(),
            1,
            "durable flush occurs before native Activity.finish fallback"
        );
        assert_eq!(
            routes.get(),
            0,
            "root system Back must not navigate Library to itself"
        );

        ui.invoke_focus_library_search();
        settle();
        assert_eq!(
            press(&ui, Key::Back),
            WindowEventDispatchResult::Accepted,
            "first Back closes Library search"
        );
        assert_eq!(flushes.get(), 1);
        assert_eq!(routes.get(), 0);
        assert_eq!(
            press(&ui, Key::Back),
            WindowEventDispatchResult::Rejected,
            "second Back reaches native exit after search closes"
        );
        assert_eq!(flushes.get(), 2);

        ui.set_entry_menu_visible(true);
        settle();
        assert_eq!(
            press(&ui, Key::Back),
            WindowEventDispatchResult::Accepted,
            "entry menu dismissal has priority over root exit"
        );
        assert!(!ui.get_entry_menu_visible());
        assert_eq!(flushes.get(), 2);

        ui.set_section(5);
        settle();
        assert_eq!(
            press(&ui, Key::Back),
            WindowEventDispatchResult::Accepted,
            "detail Back returns to Library"
        );
        assert_eq!(ui.get_section(), 0);
        assert_eq!(routes.get(), 1);
        assert_eq!(flushes.get(), 2);
        assert_eq!(press(&ui, Key::Back), WindowEventDispatchResult::Rejected);
        assert_eq!(flushes.get(), 3);

        for key in [Key::Escape, Key::Backspace] {
            assert_eq!(
                press(&ui, key),
                WindowEventDispatchResult::Accepted,
                "desktop escape/backspace retain existing navigation behavior"
            );
        }
        assert_eq!(flushes.get(), 3);
    }
}
