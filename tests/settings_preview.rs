//! Opt-in software snapshots of More → Settings pages compiled in their own
//! crates, checking that each sizes to its content inside More.
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use nova_reader::{Anim, AppWindow};
use slint::{ComponentHandle, LogicalSize};
use std::time::Duration;

fn settle() {
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

#[test]
#[ignore = "manual production UI previews; writes PNGs to NOVA_READER_PREVIEW_DIR"]
fn settings_page_previews() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
        ..Default::default()
    })))
    .unwrap();
    let dir = std::path::PathBuf::from(
        std::env::var("NOVA_READER_PREVIEW_DIR")
            .unwrap_or_else(|_| "target/settings-preview".into()),
    );
    std::fs::create_dir_all(&dir).unwrap();
    for (name, section) in [
        ("cache", 17),
        ("theme", 14),
        ("sync", 20),
        ("screenshots", 18),
    ] {
        let ui = AppWindow::new().unwrap();
        ui.global::<Anim>().set_enabled(false);
        ui.show().unwrap();
        ui.window().set_size(LogicalSize::new(390., 844.));
        ui.set_section(section);
        settle();
        // Scroll to the end: the page's full height must be reachable.
        for scroll in ElementHandle::find_by_element_id(&ui, "MorePage::detail-scroll") {
            scroll.scroll(0., -100_000.);
        }
        settle();
        let pixels = ui.window().take_snapshot().unwrap();
        let path = dir.join(format!("{name}-end.png"));
        image::save_buffer(
            &path,
            pixels.as_bytes(),
            pixels.width(),
            pixels.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
    // The Settings list itself, with each row's icon; desktop also shows
    // More's own list beside it.
    for (width, height) in [(390u32, 1100u32), (1280, 900)] {
        let ui = AppWindow::new().unwrap();
        ui.global::<Anim>().set_enabled(false);
        ui.show().unwrap();
        ui.window()
            .set_size(LogicalSize::new(width as f32, height as f32));
        ui.set_section(10);
        settle();
        let pixels = ui.window().take_snapshot().unwrap();
        image::save_buffer(
            dir.join(format!("settings-list-{width}.png")),
            pixels.as_bytes(),
            pixels.width(),
            pixels.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}
