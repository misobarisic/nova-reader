//! Reader entry paints immediately and reveals the first actual pixels once.
use nova_reader::{Anim, AppWindow, PageRow};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::time::Duration;
fn advance(ms: u64) {
    for _ in 0..ms / 10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
}
#[test]
fn reader_first_content_fades_once_and_motion_settings_skip_the_fade() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    ui.set_section(6);
    ui.global::<Anim>().set_enabled(true);
    ui.global::<Anim>().set_transitions(true);
    ui.invoke_begin_reader_entry();
    advance(200);
    assert_eq!(
        ui.get_reader_entry_opacity(),
        0.,
        "empty reading canvas does not start a loading animation"
    );
    let rows = ModelRc::new(VecModel::from(vec![PageRow {
        ratio: 1.5,
        ..Default::default()
    }]));
    ui.set_pages(rows.clone());
    advance(200);
    assert_eq!(ui.get_reader_entry_opacity(), 0.);
    let mut first = rows.row_data(0).unwrap();
    first.loaded = true;
    rows.set_row_data(0, first);
    advance(60);
    assert!(ui.get_reader_entry_opacity() > 0. && ui.get_reader_entry_opacity() < 1.);
    advance(200);
    assert_eq!(ui.get_reader_entry_opacity(), 1.);
    let mut first = rows.row_data(0).unwrap();
    first.loaded = false;
    rows.set_row_data(0, first);
    advance(40);
    assert_eq!(
        ui.get_reader_entry_opacity(),
        1.,
        "later page loading does not restart chapter entry"
    );
    for (master, transitions) in [(false, true), (true, false)] {
        ui.global::<Anim>().set_enabled(master);
        ui.global::<Anim>().set_transitions(transitions);
        ui.invoke_begin_reader_entry();
        assert_eq!(ui.get_reader_entry_opacity(), 1.);
    }
}

#[test]
#[ignore = "opt-in native reader entry/ready/error renders"]
fn render_reader_entry_previews() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let ui = AppWindow::new().unwrap();
    ui.global::<Anim>().set_enabled(false);
    ui.set_section(6);
    ui.set_reader_opening(true);
    ui.invoke_begin_reader_entry();
    ui.show().unwrap();
    let image_path = std::env::var("NOVA_READER_ENTRY_PREVIEW_IMAGE").unwrap_or_else(|_| {
        "/workspace/artifacts/reader-preview-little-nemo-1905-10-15.jpg".into()
    });
    let source = image::open(image_path).unwrap().to_rgba8();
    let ratio = source.height() as f32 / source.width() as f32;
    let picture = slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
        source.as_raw(),
        source.width(),
        source.height(),
    ));
    let dir = std::path::PathBuf::from(
        std::env::var("NOVA_READER_ENTRY_PREVIEW_DIR")
            .unwrap_or_else(|_| "/workspace/artifacts/reader-entry-preview".into()),
    );
    std::fs::create_dir_all(&dir).unwrap();
    for (width, height) in [(390, 844), (1120, 780)] {
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));
        ui.set_pages(ModelRc::default());
        ui.set_reader_opening(true);
        ui.set_reader_open_error("".into());
        ui.invoke_begin_reader_entry();
        for state in ["waiting", "ready", "error"] {
            if state == "ready" {
                ui.set_reader_opening(false);
                ui.set_reading_mode(1);
                ui.set_pages(ModelRc::new(VecModel::from(vec![PageRow {
                    loaded: true,
                    ratio,
                    tiles: ModelRc::new(VecModel::from(vec![nova_reader::TileRow {
                        image: picture.clone(),
                        fraction: 1.,
                        right: 1.,
                        bottom: 1.,
                        ..Default::default()
                    }])),
                    ..Default::default()
                }])));
            } else if state == "error" {
                ui.set_pages(ModelRc::default());
                ui.set_reader_opening(true);
                ui.set_reader_open_error("Fixture source is unavailable.".into());
                ui.invoke_begin_reader_entry();
            }
            advance(300);
            let screenshot = ui.window().take_snapshot().unwrap();
            image::save_buffer(
                dir.join(format!("reader-{state}-{width}x{height}.png")),
                screenshot.as_bytes(),
                screenshot.width(),
                screenshot.height(),
                image::ColorType::Rgba8,
            )
            .unwrap();
        }
    }
    std::fs::write(dir.join("preview-source.txt"),"Native production AppWindow software renders; synthetic loading/error states. Public-domain artwork: Winsor McCay, Little Nemo in Slumberland,1905-10-15. https://commons.wikimedia.org/wiki/File:Little_Nemo_1905-10-15.jpg\nNot an Android device capture.\n").unwrap();
}
