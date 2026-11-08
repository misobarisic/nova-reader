use i_slint_backend_testing::{ElementHandle, mock_elapsed_time};
use nova_reader::{Anim, AppWindow, ImageLimitSettings};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};

#[test]
fn image_limit_drafts_apply_together_and_reset_is_separate() {
    i_slint_backend_testing::init_no_event_loop();
    for width in [390., 1120.] {
        let ui = AppWindow::new().unwrap();
        ui.global::<Anim>().set_enabled(false);
        ui.window().set_size(slint::LogicalSize::new(width, 844.));
        ui.set_section(22);
        let settings = ui.global::<ImageLimitSettings>();
        settings.set_encoded_mib("64".into());
        settings.set_width_pixels("32768".into());
        settings.set_height_pixels("131072".into());
        settings.set_megapixels("64".into());
        settings.set_decoded_mib("512".into());
        let saves = Rc::new(RefCell::new(Vec::new()));
        settings.on_save({
            let saves = saves.clone();
            move |a, b, c, d, e| {
                saves
                    .borrow_mut()
                    .push([a, b, c, d, e].map(|s| s.to_string()))
            }
        });
        let resets = Rc::new(RefCell::new(0));
        settings.on_reset({
            let resets = resets.clone();
            move || *resets.borrow_mut() += 1
        });
        ui.show().unwrap();
        mock_elapsed_time(Duration::from_millis(100));
        assert!(saves.borrow().is_empty());
        ElementHandle::find_by_accessible_label(&ui, "Apply limits")
            .next()
            .unwrap()
            .invoke_accessible_default_action();
        assert_eq!(*saves.borrow(), [["64", "32768", "131072", "64", "512"]]);
        ElementHandle::find_by_accessible_label(&ui, "Restore defaults")
            .next()
            .unwrap()
            .invoke_accessible_default_action();
        assert_eq!(*resets.borrow(), 1);
        assert_eq!(saves.borrow().len(), 1);
    }
}

#[test]
#[ignore = "opt-in native settings screenshots"]
fn render_image_limit_previews() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let ui = AppWindow::new().unwrap();
    ui.global::<Anim>().set_enabled(false);
    ui.set_section(22);
    ui.show().unwrap();
    let dir = std::path::PathBuf::from("/workspace/artifacts/image-limits-preview");
    std::fs::create_dir_all(&dir).unwrap();
    for (width, height) in [(390, 844), (1120, 780)] {
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));
        mock_elapsed_time(Duration::from_millis(100));
        let shot = ui.window().take_snapshot().unwrap();
        image::save_buffer(
            dir.join(format!("image-limits-{width}x{height}.png")),
            shot.as_bytes(),
            shot.width(),
            shot.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}

#[test]
fn settings_headers_begin_at_the_top_on_phone_and_desktop() {
    i_slint_backend_testing::init_no_event_loop();
    for width in [390., 1120.] {
        let ui = AppWindow::new().unwrap();
        ui.global::<Anim>().set_enabled(false);
        ui.window().set_size(slint::LogicalSize::new(width, 844.));
        ui.show().unwrap();
        for (section, title) in [
            (10, "Settings"),
            (11, "Categories"),
            (12, "Data"),
            (14, "Theme"),
            (15, "Look and feel"),
            (16, "Display"),
            (17, "Cache"),
            (18, "Screenshots & saved images"),
            (19, "Reader"),
            (20, "Sync"),
            (21, "Downloads"),
            (22, "Image limits"),
        ] {
            ui.set_section(section);
            mock_elapsed_time(Duration::from_millis(100));
            let positions: Vec<_> = ElementHandle::find_by_accessible_label(&ui, title)
                .map(|e| e.absolute_position().y)
                .collect();
            assert!(
                positions.iter().any(|y| (0. ..100.).contains(y)),
                "{title} at {width}px: {positions:?}"
            );
        }
    }
}
