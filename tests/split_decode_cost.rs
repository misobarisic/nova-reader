//! Decode-cost report for tall pages stored whole and as the cache's strips
//! (WebP 85, 2,048 rows). Not a regression test: it prints timings, and is
//! ignored so CI never depends on host speed. Reproduce with
//!
//! cargo test --locked --test split_decode_cost -- --ignored --nocapture \
//!   --config 'profile.dev.package.nova-reader.opt-level=2' \
//!   --config 'profile.dev.package.reader-engine.opt-level=2'
//!
//! (the cargo options go before `--`).
use nova_reader::{
    image_storage::ImageCompression,
    page_store::{PageImage, SplitPlan, split_form},
};
use reader_engine::{DISPLAY_TILE_ROWS, DISPLAY_WINDOW_BYTES, DecodeLimits, DisplayDecoder};
use std::{hint::black_box, io::Cursor, time::Instant};

fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.
}
fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

#[test]
#[ignore = "timing report"]
fn report_decode_cost_of_whole_and_split_cached_pages() {
    let (width, height) = (720u32, 25_000u32);
    // The same deterministic line art as the tall_scroll example.
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        if x.is_multiple_of(9) || y.is_multiple_of(13) || (x % 9 == 6 && y % 13 < 7) {
            image::Rgb([0, 0, 0])
        } else {
            image::Rgb([255, 255, 255])
        }
    });
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).unwrap();
    let png = png.into_inner();
    drop(image);
    let limits = DecodeLimits::default();
    let compression = ImageCompression::cache_default();
    let plan = SplitPlan {
        threshold_rows: 4096,
        strip_rows: DISPLAY_TILE_ROWS,
    };
    let started = Instant::now();
    let container = split_form(&png, compression, Some(plan), limits, 32 << 20, &|| true)
        .unwrap()
        .unwrap();
    let write_ms = ms(started);
    println!(
        "fixture 720x25000 source_png={} bytes; cache strips (WebP 85)={} bytes, written in {write_ms:.0} ms \
         (a whole WebP cannot hold 25,000 rows: the format stops at 16,383)",
        png.len(),
        container.len(),
    );
    let layouts = [
        (
            "whole source PNG (today's >8 MP cache entry)",
            PageImage::from_encoded(png),
        ),
        ("WebP 85 strips", PageImage::from_stored(container).unwrap()),
    ];
    for output_width in [720, 512] {
        let spans = [0., 0.13, 0.26, 0.39, 0.52, 0.65, 0.78, 0.94];
        println!("output width {output_width}");
        for (name, page) in &layouts {
            let mut decoder = DisplayDecoder::default();
            let mut windows = Vec::new();
            for start in spans {
                let started = Instant::now();
                black_box(
                    decoder
                        .decode_window(
                            page,
                            limits,
                            output_width,
                            start,
                            start + 0.06,
                            DISPLAY_WINDOW_BYTES,
                        )
                        .unwrap(),
                );
                windows.push(ms(started));
            }
            let jumps = (0..3)
                .map(|_| {
                    let started = Instant::now();
                    black_box(
                        DisplayDecoder::default()
                            .decode_window(
                                page,
                                limits,
                                output_width,
                                0.78,
                                0.84,
                                DISPLAY_WINDOW_BYTES,
                            )
                            .unwrap(),
                    );
                    ms(started)
                })
                .collect::<Vec<_>>();
            // One strip of scroll at a time, rendering only the entering strip.
            let full_height = (25_000u64 * u64::from(output_width) / 720) as f64;
            let step = f64::from(DISPLAY_TILE_ROWS) / full_height;
            let mut decoder = DisplayDecoder::default();
            let mut previous: Option<reader_engine::DisplayWindowPlan> = None;
            let mut shifts = Vec::new();
            for i in 0..6 {
                let start = 0.02 + step * f64::from(i);
                let started = Instant::now();
                let (plan, ()) = decoder
                    .render_window_tiles(
                        page,
                        limits,
                        output_width,
                        start,
                        start + 0.06,
                        DISPLAY_WINDOW_BYTES,
                        reader_engine::DisplayOptions::default(),
                        |renderer| {
                            let plan = renderer.plan();
                            let mut top = plan.first_row;
                            while top < plan.first_row + plan.rows {
                                let bottom = ((top / DISPLAY_TILE_ROWS + 1) * DISPLAY_TILE_ROWS)
                                    .min(plan.first_row + plan.rows);
                                if previous.is_none_or(|p| {
                                    top < p.first_row || bottom > p.first_row + p.rows
                                }) {
                                    let mut out =
                                        vec![
                                            0u8;
                                            (bottom - top) as usize * plan.width as usize * 4
                                        ];
                                    renderer.render_rect(
                                        top,
                                        bottom - top,
                                        0,
                                        plan.width,
                                        &mut out,
                                    )?;
                                    black_box(out);
                                }
                                top = bottom;
                            }
                            Ok(())
                        },
                    )
                    .unwrap();
                if previous.is_some() {
                    shifts.push(ms(started));
                }
                previous = Some(plan);
            }
            println!(
                "  {name}: first window {:.1} ms, mean later window {:.1} ms, jump to 78% {:.1} ms, one-strip shift {:.1} ms",
                windows[0],
                mean(&windows[1..]),
                mean(&jumps),
                mean(&shifts)
            );
        }
    }
}
