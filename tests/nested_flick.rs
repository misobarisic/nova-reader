//! A horizontal rail inside a vertical list: a mostly sideways drag moves the
//! rail only, and a vertical drag still scrolls the list (see the Flickable
//! note in `vendor/i-slint-core/NOVA_PATCH.md`).
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use nova_reader::{Anim, AppWindow, ShelfEntry, TimelineBridge, TimelineEntry};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, LogicalSize, ModelRc, VecModel};
use std::time::Duration;

fn tick(ui: &AppWindow) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    let _ = ui.window().take_snapshot();
}

fn drag(ui: &AppWindow, from: (f32, f32), step: (f32, f32), steps: usize) {
    let window = ui.window();
    let at = |x: f32, y: f32| LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerPressed {
        position: at(from.0, from.1),
        button: PointerEventButton::Left,
    });
    tick(ui);
    for n in 1..=steps {
        let n = n as f32;
        window.dispatch_event(WindowEvent::PointerMoved {
            position: at(from.0 + step.0 * n, from.1 + step.1 * n),
        });
        tick(ui);
    }
    let end = at(
        from.0 + step.0 * steps as f32,
        from.1 + step.1 * steps as f32,
    );
    window.dispatch_event(WindowEvent::PointerReleased {
        position: end,
        button: PointerEventButton::Left,
    });
    for _ in 0..60 {
        tick(ui);
    }
}

fn card(ui: &AppWindow, title: &str) -> LogicalPosition {
    ElementHandle::find_by_accessible_label(ui, &format!("{title}, Ch. 1, Ready"))
        .next()
        .expect(title)
        .absolute_position()
}

fn history() -> AppWindow {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
        ..Default::default()
    })))
    .unwrap();
    let ui = AppWindow::new().unwrap();
    ui.global::<Anim>().set_enabled(false);
    let bridge = ui.global::<TimelineBridge>();
    bridge.set_shelf(ModelRc::new(VecModel::from(
        (0..6)
            .map(|i| ShelfEntry {
                title: format!("Shelf {i}").into(),
                headline: "Ch. 1".into(),
                detail: "Ready".into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    // A list well taller than the window, so it can scroll.
    let lines: Vec<_> = (0..40)
        .map(|i| TimelineEntry {
            kind: 5,
            index: i,
            y: 312. + i as f32 * 124.,
            height: 116.,
            last: true,
            progress: -1.,
            title: format!("Series {i}").into(),
            ..Default::default()
        })
        .collect();
    bridge.set_list_top(312.);
    bridge.set_content_height(312. + 40. * 124.);
    bridge.set_entries(ModelRc::new(VecModel::from(lines.clone())));
    bridge.set_shown(ModelRc::new(VecModel::from(lines)));
    ui.set_section(2);
    ui.show().unwrap();
    ui.window().set_size(LogicalSize::new(390., 844.));
    for _ in 0..30 {
        tick(&ui);
    }
    ui
}

#[test]
fn rails_keep_sideways_drags_and_lists_keep_vertical_ones() {
    let ui = history();
    let before = card(&ui, "Shelf 1");
    // Mostly sideways with a vertical drift well past the 8px threshold.
    drag(&ui, (before.x + 40., before.y + 60.), (-12., 3.), 12);
    let after = card(&ui, "Shelf 1");
    assert!(
        after.x < before.x - 60.,
        "the rail follows the drag: {before:?} -> {after:?}"
    );
    assert!(
        (after.y - before.y).abs() < 0.5,
        "the list stays put: {before:?} -> {after:?}"
    );
    // A vertical drag on the rail still scrolls the list.
    drag(&ui, (200., after.y + 60.), (1., -12.), 12);
    let scrolled = card(&ui, "Shelf 1");
    assert!(
        scrolled.y < after.y - 60.,
        "the list scrolls: {after:?} -> {scrolled:?}"
    );
}
