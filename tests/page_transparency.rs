//! A page stored as strips must be indistinguishable from the same page stored
//! whole to everything outside `page_store`: the same pages, IDs, dimensions,
//! layout, anchors, decoded pixels, display windows, captures and thumbnails.
//! Both chapters here go through the production download store and inventory.
use nova_reader::{
    chapter_cache::ChapterCache,
    image_storage::ImageCompression,
    page_inventory::{self, Inventory},
    page_store::{PageImage, SplitPlan, split_form},
    page_thumbnails::PageThumbnails,
    services::Services,
};
use reader_capture::{PageRect, Rect, capture_at_scale_with_decoder};
use reader_domain::{ChapterId, ReadingMode, ResumeAnchor, SeriesId};
use reader_downloads::PageResource;
use reader_engine::{
    DecodeLimits, DisplayDecoder, DisplayOptions, PageDimensions, PageSource, layout_pages,
    resolve_anchor,
};
use reader_sources::CancelToken;
use std::{collections::BTreeMap, io::Cursor, sync::Arc, time::Duration};

const PLAN: SplitPlan = SplitPlan {
    threshold_rows: 1024,
    strip_rows: 256,
};
const LIMITS: DecodeLimits = DecodeLimits {
    max_encoded_bytes: 32 * 1024 * 1024,
    max_width: 16384,
    max_height: 65536,
    max_pixels: 32_000_000,
    max_decoded_bytes: 128 * 1024 * 1024,
};

/// Line-art-like page: a texture that changes on every row, with an optional
/// white margin so border cropping has something to find.
fn art(width: u32, height: u32, margin: bool) -> image::RgbaImage {
    image::RgbaImage::from_fn(width, height, |x, y| {
        if margin && (x < 6 || x >= width - 6 || y < 70 || y >= height - 70) {
            return image::Rgba([255; 4]);
        }
        let n = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503) ^ (y / 7);
        image::Rgba([n as u8, (n >> 8) as u8, (y / 3) as u8, 255])
    })
}
fn png(image: &image::RgbaImage) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}
fn pages() -> Vec<(&'static str, image::RgbaImage)> {
    vec![
        ("tall", art(64, 2600, false)),
        ("short", art(64, 500, false)),
        ("bordered", art(64, 1500, true)),
        ("tallest", art(48, 3333, false)),
    ]
}
/// The page as the cache or a download would store it, losslessly.
fn container(image: &image::RgbaImage) -> Vec<u8> {
    split_form(
        &png(image),
        ImageCompression {
            lossless_inputs: true,
            ..ImageCompression::default()
        },
        Some(PLAN),
        LIMITS,
        32 << 20,
        &|| true,
    )
    .unwrap()
    .expect("tall page splits")
}

struct Fixture {
    _directory: tempfile::TempDir,
    _services: Services,
    whole: Inventory,
    split: Inventory,
}
/// One application directory per process: every test shares the fixture.
fn fixture() -> &'static Fixture {
    static FIXTURE: std::sync::OnceLock<Fixture> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(build)
}
fn build() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::open(&directory.path().join("app")).unwrap();
    let pages = pages();
    let mut inventories = Vec::new();
    for (name, split) in [("whole", false), ("split", true)] {
        let chapter = ChapterId::new(SeriesId::new("removed-addon", "series"), name);
        let resources = pages
            .iter()
            .map(|(id, _)| PageResource {
                id: (*id).into(),
                url: format!("https://example.com/{id}.png"),
                headers: BTreeMap::new(),
            })
            .collect::<Vec<_>>();
        services
            .downloads
            .enqueue(chapter.clone(), resources)
            .unwrap();
        services
            .downloads
            .run(&chapter, |resource| {
                let image = &pages.iter().find(|(id, _)| *id == resource.id).unwrap().1;
                Ok(if split && image.height() > PLAN.threshold_rows {
                    container(image)
                } else {
                    png(image)
                })
            })
            .unwrap();
        inventories
            .push(page_inventory::load(&services, &chapter, &CancelToken::default()).unwrap());
    }
    let split = inventories.pop().unwrap();
    let whole = inventories.pop().unwrap();
    Fixture {
        _directory: directory,
        _services: services,
        whole,
        split,
    }
}
fn read(inventory: &Inventory) -> Vec<PageImage> {
    let cancel = CancelToken::default();
    inventory
        .pages
        .iter()
        .map(|page| page.read(&cancel).unwrap())
        .collect()
}

#[test]
fn inventory_ids_dimensions_layout_anchors_and_pixels_do_not_depend_on_storage() {
    let fixture = fixture();
    let (whole, split) = (read(&fixture.whole), read(&fixture.split));
    // The same pages, in the same order, with the same stable identities.
    let ids = |inventory: &Inventory| {
        inventory
            .pages
            .iter()
            .map(|page| page.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&fixture.whole), ids(&fixture.split));
    assert_eq!(
        ids(&fixture.split),
        ["tall", "short", "bordered", "tallest"]
    );
    // The split chapter really is stored differently: the tall pages decode by strip.
    let bands = |pages: &[PageImage]| {
        pages
            .iter()
            .map(PageSource::row_granularity)
            .collect::<Vec<_>>()
    };
    assert_eq!(bands(&whole), [None; 4]);
    assert_eq!(bands(&split), [Some(256), None, Some(256), Some(256)]);
    // Dimensions, hence ratios, positions and anchors.
    let dimensions = |pages: &[PageImage]| {
        pages
            .iter()
            .map(|page| page.dimensions(LIMITS).unwrap())
            .collect::<Vec<PageDimensions>>()
    };
    assert_eq!(dimensions(&whole), dimensions(&split));
    for mode in [
        ReadingMode::Webtoon,
        ReadingMode::Vertical,
        ReadingMode::LeftToRight,
        ReadingMode::RightToLeft,
    ] {
        let layout = |pages: &[PageImage]| {
            format!(
                "{:?}",
                layout_pages(&dimensions(pages), mode, 390., 800., 12.).unwrap()
            )
        };
        assert_eq!(layout(&whole), layout(&split), "{mode:?}");
    }
    let anchor = ResumeAnchor::new("bordered", 2, 0.37).unwrap();
    let page_ids = ids(&fixture.split);
    assert_eq!(
        resolve_anchor(&anchor, &page_ids).unwrap().page_id,
        resolve_anchor(&anchor, &ids(&fixture.whole))
            .unwrap()
            .page_id
    );
    let layout = layout_pages(&dimensions(&split), ReadingMode::Webtoon, 390., 800., 12.).unwrap();
    let reference =
        layout_pages(&dimensions(&whole), ReadingMode::Webtoon, 390., 800., 12.).unwrap();
    assert_eq!(
        layout.anchor_position(&anchor),
        reference.anchor_position(&anchor)
    );
    assert_eq!(
        layout.visible(0., 1200., 390., 700.),
        reference.visible(0., 1200., 390., 700.)
    );
    // Every pixel, and every row range.
    for (index, (a, b)) in whole.iter().zip(&split).enumerate() {
        assert_eq!(
            a.decode(LIMITS).unwrap(),
            b.decode(LIMITS).unwrap(),
            "page {index}"
        );
        let height = a.dimensions(LIMITS).unwrap().height;
        for rows in [0..height.min(300), 200..height.min(900), height - 1..height] {
            assert_eq!(
                a.decode_rows(rows.clone(), LIMITS).unwrap(),
                b.decode_rows(rows.clone(), LIMITS).unwrap(),
                "page {index} rows {rows:?}"
            );
        }
    }
}

#[test]
fn display_windows_are_identical_for_every_presentation() {
    let fixture = fixture();
    let (whole, split) = (read(&fixture.whole), read(&fixture.split));
    let recipes = [
        DisplayOptions::default(),
        DisplayOptions {
            crop_borders: true,
            ..Default::default()
        },
        DisplayOptions {
            reverse_page: true,
            ..Default::default()
        },
        DisplayOptions {
            crop_borders: true,
            grayscale: true,
            color_filter: true,
            filter_color: 0x2244aa,
            filter_intensity: 30,
            ..Default::default()
        },
    ];
    for (index, (a, b)) in whole.iter().zip(&split).enumerate() {
        for options in recipes {
            for width in [64, 48, 33] {
                for (start, end) in [(0., 1.), (0.2, 0.3), (0.55, 0.56), (0.97, 1.)] {
                    let render = |page: &PageImage| {
                        let mut decoder = DisplayDecoder::default();
                        let presentation = decoder.presentation(page, LIMITS, options).unwrap();
                        let (plan, tiles) = decoder
                            .render_window_tiles(
                                page,
                                LIMITS,
                                width,
                                start,
                                end,
                                1 << 24,
                                options,
                                |renderer| {
                                    // Awkward piece sizes straddle every strip seam.
                                    let plan = renderer.plan();
                                    let mut tiles = Vec::new();
                                    let mut top = plan.first_row;
                                    while top < plan.first_row + plan.rows {
                                        let rows = (plan.first_row + plan.rows - top).min(211);
                                        let mut out =
                                            vec![0; plan.width as usize * rows as usize * 4];
                                        renderer.render_rect(top, rows, 0, plan.width, &mut out)?;
                                        tiles.extend(out);
                                        top += rows;
                                    }
                                    Ok(tiles)
                                },
                            )
                            .unwrap();
                        (presentation, plan, tiles)
                    };
                    assert_eq!(
                        render(a),
                        render(b),
                        "page {index} {options:?} width {width} {start}..{end}"
                    );
                }
            }
        }
    }
}

#[test]
fn captures_of_whole_and_split_pages_match() {
    let fixture = fixture();
    let (whole, split) = (read(&fixture.whole), read(&fixture.split));
    let capture = |pages: &[PageImage], options: DisplayOptions, scale: Option<f64>| {
        // The tall page and its neighbour, with the viewport across a strip seam.
        let first = pages[0].dimensions(LIMITS).unwrap();
        let second = pages[1].dimensions(LIMITS).unwrap();
        let heights = [
            f64::from(first.height) * 64. / f64::from(first.width),
            f64::from(second.height) * 64. / f64::from(second.width),
        ];
        let layout = [
            PageRect {
                index: 0,
                rect: Rect {
                    x: 0.,
                    y: 0.,
                    width: 64.,
                    height: heights[0],
                },
            },
            PageRect {
                index: 1,
                rect: Rect {
                    x: 0.,
                    y: heights[0],
                    width: 64.,
                    height: heights[1],
                },
            },
        ];
        let viewport = Rect {
            x: 0.,
            y: heights[0] - 300.,
            width: 64.,
            height: 640.,
        };
        capture_at_scale_with_decoder(
            viewport,
            0,
            &layout,
            [9, 8, 7, 255],
            options,
            scale,
            128 * 1024 * 1024,
            |index| Ok(pages[index].clone()),
            |_, page, limits| page.decode(limits).map(Arc::new),
        )
        .unwrap()
    };
    for options in [
        DisplayOptions::default(),
        DisplayOptions {
            crop_borders: true,
            ..Default::default()
        },
    ] {
        for scale in [None, Some(1.), Some(0.5)] {
            let a = capture(&whole, options, scale);
            let b = capture(&split, options, scale);
            assert_eq!((a.width, a.height), (b.width, b.height));
            assert!(a.pixels == b.pixels, "{options:?} {scale:?}");
            assert_eq!(a.source_start, b.source_start);
        }
    }
}

#[test]
fn thumbnails_of_whole_and_split_pages_match() {
    let fixture = fixture();
    let mut results = Vec::new();
    for inventory in [&fixture.whole, &fixture.split] {
        let mut per_mode = Vec::new();
        for gallery in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let cache = Arc::new(ChapterCache::new(directory.path().join("cache")));
            let (sender, receiver) = std::sync::mpsc::channel();
            let service = PageThumbnails::new(
                cache,
                Arc::new(move |completion| {
                    let _ = sender.send(completion);
                }),
            );
            let pages = Arc::new(inventory.pages.to_vec());
            if gallery {
                service.replace_inventory_for_gallery(1, pages);
            } else {
                service.replace_inventory(1, pages);
            }
            service.viewport(1, 0, 3, 1);
            let mut seen = BTreeMap::new();
            while seen.len() < 4 {
                let completion = receiver.recv_timeout(Duration::from_secs(60)).unwrap();
                let thumbnail = completion.result.as_ref().unwrap();
                seen.insert(
                    completion.index,
                    (
                        completion.dimensions.map(|d| (d.width, d.height)),
                        completion.top_preview,
                        thumbnail.pixels.width(),
                        thumbnail.pixels.height(),
                        thumbnail.pixels.as_bytes().to_vec(),
                    ),
                );
            }
            service.cancel();
            per_mode.push(seen);
        }
        results.push(per_mode);
    }
    for (mode, (whole, split)) in results[0].iter().zip(&results[1]).enumerate() {
        assert_eq!(whole.len(), 4);
        for index in 0..4 {
            assert!(whole[&index] == split[&index], "mode {mode} page {index}");
        }
    }
    // The tall pages really did take the top-preview path in page mode.
    assert!(results[1][0][&0].1 && (0..4).all(|i| !results[1][1][&i].1));
}
