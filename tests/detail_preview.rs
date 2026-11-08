//! Opt-in software snapshots of the series detail screen on a phone and a
//! desktop, with a tall cover fixture.
use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use nova_reader::{Anim, AppWindow, ItemRow};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};
use std::time::Duration;

fn settle() {
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

/// A 9:16 cover: a warm sky over a dark band, so crops are easy to spot.
fn cover(width: u32, height: u32) -> slint::Image {
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
    for (i, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
        let y = i as u32 / width;
        let x = i as u32 % width;
        let t = y as f32 / height as f32;
        let stripe = ((x + y) / 24) % 2 == 0;
        *pixel = if t < 0.6 {
            slint::Rgba8Pixel::new(
                (200. - t * 120.) as u8,
                (90. + t * 40.) as u8,
                if stripe { 120 } else { 90 },
                255,
            )
        } else {
            slint::Rgba8Pixel::new(30, 40, if stripe { 80 } else { 60 }, 255)
        };
    }
    slint::Image::from_rgba8(pixels)
}

#[test]
#[ignore = "manual production UI previews; writes PNGs to NOVA_READER_PREVIEW_DIR"]
fn detail_previews() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
        ..Default::default()
    })))
    .unwrap();
    let dir = std::path::PathBuf::from(
        std::env::var("NOVA_READER_PREVIEW_DIR").unwrap_or_else(|_| "target/detail-preview".into()),
    );
    std::fs::create_dir_all(&dir).unwrap();
    for (width, height) in [(390u32, 844u32), (1280, 800), (1600, 1000)] {
        let ui = AppWindow::new().unwrap();
        ui.global::<Anim>().set_enabled(false);
        ui.set_items(ModelRc::new(VecModel::from(
            (1..=24)
                .rev()
                .map(|n| ItemRow {
                    title: format!("Chapter {n}").into(),
                    number: n.to_string().into(),
                    short_title: format!("Chapter {n}").into(),
                    timestamp: "2 Nov".into(),
                    read: n < 12,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        )));
        ui.set_series_title("Moonlit Cartographer".into());
        ui.set_series_metadata("Ongoing · MangaDex · 24 chapters".into());
        ui.set_series_genres(ModelRc::new(VecModel::from(
            ["Adventure", "Fantasy", "Mystery", "Drama"]
                .map(Into::into)
                .to_vec(),
        )));
        ui.set_series_description("A mapmaker who charts coastlines by moonlight finds an island that is never in the same place twice, and a crew that will pay anything to reach it before the tide turns.".into());
        ui.set_resume_chapter("12".into());
        // What `artwork::set_series_poster` publishes: the cover and its
        // few-pixel copy.
        ui.set_series_poster(cover(360, 640));
        ui.set_series_backdrop(cover(12, 21));
        ui.set_section(5);
        ui.show().unwrap();
        ui.window()
            .set_size(LogicalSize::new(width as f32, height as f32));
        settle();
        let pixels = ui.window().take_snapshot().unwrap();
        let path = dir.join(format!("detail-{width}.png"));
        image::save_buffer(
            &path,
            pixels.as_bytes(),
            pixels.width(),
            pixels.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}
