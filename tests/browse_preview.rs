//! Opt-in software snapshots of Browse: the source list, extensions, a
//! catalogue with its filter sheet, and results across sources.
use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use nova_reader::{
    Anim, AppWindow, BrowseBridge, ExtensionRow, FilterGroup, FilterPill, ItemRow, SearchSection,
    SourceRow,
};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};
use std::time::Duration;

fn settle() {
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn cover(index: usize) -> slint::Image {
    let palette = [
        (59u8, 42u8, 107u8),
        (122, 46, 46),
        (28, 59, 47),
        (74, 29, 79),
        (15, 58, 74),
    ];
    let (r, g, b) = palette[index % palette.len()];
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(120, 180);
    for (i, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
        let tint = ((i % 120 + i / 120) / 40 % 2 * 40) as u8;
        *pixel = slint::Rgba8Pixel::new(r.saturating_add(tint), g.saturating_add(tint), b, 255);
    }
    slint::Image::from_rgba8(pixels)
}

fn source(i: i32, name: &str, language: &str, code: &str, adult: bool, pinned: bool) -> SourceRow {
    SourceRow {
        id: format!("s{i}").into(),
        name: name.into(),
        language: language.into(),
        code: code.into(),
        extension: format!("{name} extension").into(),
        enabled: true,
        latest: i % 2 == 0,
        pinned,
        adult,
        index: i,
        ..Default::default()
    }
}

fn fill(ui: &AppWindow) {
    let bridge = ui.global::<BrowseBridge>();
    let all = vec![
        source(0, "MangaDex", "English", "EN", false, true),
        source(1, "Comick", "English", "EN", false, true),
        source(2, "Raw Kingdom", "Japanese", "JA", true, false),
        source(3, "Every Scan", "All languages", "ALL", false, true),
        source(4, "Tachi Index", "Korean", "KO", false, false),
    ];
    ui.set_source_rows(ModelRc::new(VecModel::from(all.clone())));
    bridge.set_pinned(ModelRc::new(VecModel::from(
        all.iter().filter(|s| s.pinned).cloned().collect::<Vec<_>>(),
    )));
    bridge.set_has_last_used(true);
    bridge.set_last_used(all[0].clone());
    bridge.set_last_used_detail("English · opened 2 h ago".into());
    bridge.set_languages(ModelRc::new(VecModel::from(
        ["All", "English", "Japanese", "Korean"]
            .map(Into::into)
            .to_vec(),
    )));
    bridge.set_language_counts(ModelRc::new(VecModel::from(vec![5, 2, 1, 1])));
    bridge.set_pinned_count(3);
    bridge.set_attention_count(1);
    ui.set_extensions(ModelRc::new(VecModel::from(vec![
        ExtensionRow {
            name: "MangaDex".into(),
            version: "1.4.2".into(),
            language: "en".into(),
            enabled: true,
            sources: 1,
            id: "a".into(),
            ..Default::default()
        },
        ExtensionRow {
            name: "Raw Kingdom".into(),
            version: "0.9.0".into(),
            language: "ja".into(),
            enabled: false,
            sources: 2,
            id: "b".into(),
            ..Default::default()
        },
    ])));
    ui.set_source_title("MangaDex".into());
    bridge.set_catalog_language("English".into());
    bridge.set_catalog_latest(true);
    ui.set_source_has_filters(true);
    bridge.set_filter_count(2);
    let pills = |labels: &[&str], tristate: bool| {
        ModelRc::new(VecModel::from(
            labels
                .iter()
                .enumerate()
                .map(|(i, l)| FilterPill {
                    label: (*l).into(),
                    key: format!("k{i}").into(),
                    state: (i % 3) as i32,
                    tristate,
                })
                .collect::<Vec<_>>(),
        ))
    };
    bridge.set_filter_groups(ModelRc::new(VecModel::from(vec![
        FilterGroup {
            name: "Status".into(),
            kind: "pills".into(),
            pills: pills(&["Ongoing", "Completed", "Hiatus"], false),
            ..Default::default()
        },
        FilterGroup {
            name: "Genres".into(),
            kind: "pills".into(),
            pills: pills(
                &[
                    "Action",
                    "Adventure",
                    "Comedy",
                    "Drama",
                    "Fantasy",
                    "Mystery",
                    "Romance",
                    "Slice of life",
                ],
                true,
            ),
            ..Default::default()
        },
    ])));
    ui.set_items(ModelRc::new(VecModel::from(
        (0..12)
            .map(|i| ItemRow {
                title: format!("A long series title number {i}").into(),
                detail: if i % 2 == 0 {
                    "Ongoing · 120 chapters"
                } else {
                    "Completed"
                }
                .into(),
                list_detail: "Ongoing · 120 chapters".into(),
                bookmarked: i % 4 == 1,
                poster: cover(i),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    bridge.set_global_sections(ModelRc::new(VecModel::from(vec![
        SearchSection {
            name: "MangaDex".into(),
            status: 1,
            count: 14,
            items: ModelRc::new(VecModel::from(
                (0..6)
                    .map(|i| ItemRow {
                        title: format!("Result {i}").into(),
                        poster: cover(i),
                        bookmarked: i == 2,
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            )),
            ..Default::default()
        },
        SearchSection {
            name: "Comick".into(),
            status: 0,
            ..Default::default()
        },
        SearchSection {
            name: "Every Scan".into(),
            status: 2,
            error: "timed out".into(),
            ..Default::default()
        },
    ])));
}

#[test]
#[ignore = "manual production UI previews; writes PNGs to NOVA_READER_PREVIEW_DIR"]
fn browse_previews() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
        ..Default::default()
    })))
    .unwrap();
    let dir = std::path::PathBuf::from(
        std::env::var("NOVA_READER_PREVIEW_DIR").unwrap_or_else(|_| "target/browse-preview".into()),
    );
    std::fs::create_dir_all(&dir).unwrap();
    for (width, height) in [(390u32, 844u32), (1280, 800)] {
        for name in ["sources", "extensions", "catalogue", "filters", "search"] {
            let ui = AppWindow::new().unwrap();
            ui.global::<Anim>().set_enabled(false);
            fill(&ui);
            ui.set_section(3);
            match name {
                "extensions" => ui.set_browse_tab(1),
                "catalogue" => ui.set_source_catalog_open(true),
                "filters" => {
                    ui.set_source_catalog_open(true);
                    ui.set_source_filters_open(true);
                }
                "search" => ui.global::<BrowseBridge>().set_global_query("moon".into()),
                _ => {}
            }
            ui.show().unwrap();
            ui.window()
                .set_size(LogicalSize::new(width as f32, height as f32));
            settle();
            let pixels = ui.window().take_snapshot().unwrap();
            let path = dir.join(format!("{name}-{width}.png"));
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
}
