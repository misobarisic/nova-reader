//! Opt-in software snapshots of the production Library, including its pinned
//! category header. The cover images are deterministic colored fixtures.
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use nova_reader::{Anim, AppWindow, Appearance, ItemRow};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};
use std::{path::Path, time::Duration};

fn settle() {
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}
fn element(ui: &AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(ui, id)
        .next()
        .unwrap_or_else(|| panic!("missing {id}"))
}
fn category(ui: &AppWindow) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, "Reading")
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
        .expect("production Reading category")
}
fn cover(index: usize) -> slint::Image {
    let palette = [
        (32u8, 105u8, 110u8),
        (158, 66, 48),
        (73, 70, 133),
        (112, 106, 46),
        (54, 99, 153),
        (141, 69, 112),
    ];
    let (r, g, b) = palette[index % palette.len()];
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(160, 240);
    for (i, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
        let x = i % 160;
        let y = i / 160;
        let band = ((x + y / 2 + index * 19) / 32) % 3;
        let tint = if band == 1 { 38 } else { 0 };
        *pixel = slint::Rgba8Pixel::new(
            r.saturating_add(tint),
            g.saturating_add(tint),
            b.saturating_add(tint),
            255,
        );
    }
    slint::Image::from_rgba8(pixels)
}
fn snapshot(ui: &AppWindow, dir: &Path, label: &str, width: u32, height: u32) {
    let pixels = ui
        .window()
        .take_snapshot()
        .expect("native software Library pixels");
    let output = dir.join(format!("library-{label}-{width}x{height}.png"));
    image::save_buffer(
        &output,
        pixels.as_bytes(),
        pixels.width(),
        pixels.height(),
        image::ColorType::Rgba8,
    )
    .expect("save native Library preview");
    eprintln!("Library preview: {}", output.display());
}

#[test]
#[ignore = "manual production UI previews; writes PNGs to NOVA_READER_PREVIEW_DIR"]
fn library_native_phone_and_desktop_sticky_header_previews() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
        ..Default::default()
    })))
    .unwrap();
    let dir = std::path::PathBuf::from(
        std::env::var("NOVA_READER_PREVIEW_DIR")
            .unwrap_or_else(|_| "/workspace/artifacts/library-preview".into()),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let titles = [
        "The wandering atlas",
        "Night orchard",
        "Letters from the sea",
        "Paper constellations",
        "The last observatory",
        "A quieter season",
    ];
    for (width, height) in [(390u32, 844u32), (1120, 780)] {
        let ui = AppWindow::new().unwrap();
        ui.global::<Anim>().set_enabled(false);
        ui.global::<Appearance>().set_library_list_view(false);
        ui.global::<Appearance>().set_library_min_cols(2);
        ui.set_heading("Your library".into());
        ui.set_caption("A home for your next chapter.".into());
        ui.set_categories(ModelRc::new(VecModel::from(
            [
                "All",
                "Reading",
                "Completed",
                "Plan to read",
                "On hold",
                "Favorites",
                "Weekend reads",
            ]
            .map(Into::into)
            .to_vec(),
        )));
        ui.set_category_index(1);
        ui.set_items(ModelRc::new(VecModel::from(
            (0..72)
                .map(|i| ItemRow {
                    title: format!("{} {}", titles[i % titles.len()], i + 1).into(),
                    badge: "Reading".into(),
                    subtitle: "New chapters available".into(),
                    artwork_key: format!("preview-{i}").into(),
                    series_key: format!("preview-series-{i}").into(),
                    completed: (i % 11 + 1) as i32,
                    total: 24,
                    poster: cover(i),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        )));
        ui.show().unwrap();
        ui.window()
            .set_size(LogicalSize::new(width as f32, height as f32));
        settle();
        snapshot(&ui, &dir, "top", width, height);
        element(&ui, "LibraryPage::lib-scroll").scroll(0., -1100.);
        settle();
        let pinned_y = category(&ui).absolute_position().y;
        let grid_y = element(&ui, "LibraryPage::library-grid")
            .absolute_position()
            .y;
        snapshot(&ui, &dir, "scrolled", width, height);
        element(&ui, "LibraryPage::lib-scroll").scroll(0., -317.);
        settle();
        assert!(
            (category(&ui).absolute_position().y - pinned_y).abs() < 0.5,
            "category lane stays pinned while native grid scrolls"
        );
        assert!(
            element(&ui, "LibraryPage::library-grid")
                .absolute_position()
                .y
                < grid_y - 100.
        );
        snapshot(&ui, &dir, "scrolled-further", width, height);
    }
    std::fs::write(dir.join("preview-source.txt"),
        "Production AppWindow software renderer, 390x844 and 1120x780. Deterministic colored fixture cover images and fictional series metadata; no UI mockups. Top, scrolled and scrolled-further captures exercise the real Nova LibraryScroll/header overlay. Not an Android hardware capture.\n").unwrap();
}
