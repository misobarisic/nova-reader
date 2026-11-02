use i_slint_backend_testing::{AccessibleRole, ElementHandle, mock_elapsed_time};
use nova_reader::{Anim, AppWindow, DownloadCleanupSettings, EntryCategoryRow};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};
fn categories(ui: &AppWindow) {
    ui.global::<DownloadCleanupSettings>()
        .set_categories(ModelRc::new(VecModel::from(vec![
            EntryCategoryRow {
                id: "stable-id".into(),
                label: "Favorites".into(),
                checked: true,
            },
            EntryCategoryRow {
                id: "second-id".into(),
                label: "Keep for later".into(),
                checked: false,
            },
            EntryCategoryRow {
                id: "long-id".into(),
                label: "A longer category name".into(),
                checked: false,
            },
        ])));
}
#[test]
fn cleanup_controls_accessible_on_phone_and_desktop_use_stable_ids() {
    i_slint_backend_testing::init_no_event_loop();
    for width in [390., 1120.] {
        let ui = AppWindow::new().unwrap();
        ui.global::<Anim>().set_enabled(false);
        ui.window().set_size(slint::LogicalSize::new(width, 844.));
        ui.set_section(21);
        categories(&ui);
        let calls = Rc::new(RefCell::new(Vec::new()));
        ui.global::<DownloadCleanupSettings>().on_changed({
            let calls = calls.clone();
            move |key| calls.borrow_mut().push(key.to_string())
        });
        ui.show().unwrap();
        mock_elapsed_time(Duration::from_millis(100));
        ElementHandle::find_by_accessible_label(&ui, "Auto-delete read chapters")
            .find(|e| e.accessible_role() == Some(AccessibleRole::Checkbox))
            .unwrap()
            .invoke_accessible_default_action();
        ElementHandle::find_by_accessible_label(&ui, "Favorites")
            .next()
            .unwrap()
            .invoke_accessible_default_action();
        assert_eq!(*calls.borrow(), ["", "stable-id"]);
    }
}
#[test]
#[ignore = "opt-in native settings screenshots"]
fn render_cleanup_settings_previews() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let ui = AppWindow::new().unwrap();
    ui.global::<Anim>().set_enabled(false);
    ui.set_section(21);
    categories(&ui);
    ui.global::<DownloadCleanupSettings>().set_enabled(true);
    ui.show().unwrap();
    let dir = std::path::PathBuf::from("/workspace/artifacts/download-cleanup-preview");
    std::fs::create_dir_all(&dir).unwrap();
    for (width, height) in [(390, 844), (1120, 780)] {
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));
        mock_elapsed_time(Duration::from_millis(100));
        let shot = ui.window().take_snapshot().unwrap();
        image::save_buffer(
            dir.join(format!("cleanup-{width}x{height}.png")),
            shot.as_bytes(),
            shot.width(),
            shot.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}
