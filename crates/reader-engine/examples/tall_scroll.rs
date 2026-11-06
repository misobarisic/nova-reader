//! Deterministic CPU profile, without network, disk cache or a GPU.
//! Run with:
//! cargo run --locked -p reader-engine --example tall_scroll \
//!   --config 'profile.dev.package.reader-engine.opt-level=2'
use reader_engine::{DISPLAY_WINDOW_BYTES, DecodeLimits, DisplayDecoder, decode_display_window};
use std::{hint::black_box, io::Cursor, sync::Arc, time::Instant};

fn main() {
    let image = image::RgbImage::from_fn(720, 25000, |x, y| {
        if x.is_multiple_of(9) || y.is_multiple_of(13) || (x % 9 == 6 && y % 13 < 7) {
            image::Rgb([0, 0, 0])
        } else {
            image::Rgb([255, 255, 255])
        }
    });
    let mut encoded = Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    drop(image);
    let bytes = Arc::new(encoded.into_inner());
    println!(
        "fixture=RGB PNG 720x25000 encoded={} original_rgba=72000000",
        bytes.len()
    );
    for width in [720, 512] {
        let limits = DecodeLimits::default();
        let spans = [0., 0.13, 0.26, 0.39, 0.52, 0.65, 0.78, 0.94];
        let mut old = Vec::new();
        let mut cached = Vec::new();
        let mut output_bytes = 0;
        for start in spans {
            let now = Instant::now();
            let window = decode_display_window(
                &bytes,
                limits,
                width,
                start,
                start + 0.06,
                DISPLAY_WINDOW_BYTES,
            )
            .unwrap();
            output_bytes = output_bytes.max(window.plan.byte_len());
            old.push(now.elapsed().as_secs_f64() * 1000.);
            black_box(window);
        }
        let mut decoder = DisplayDecoder::default();
        for start in spans {
            let now = Instant::now();
            black_box(
                decoder
                    .decode_window(
                        &bytes,
                        limits,
                        width,
                        start,
                        start + 0.06,
                        DISPLAY_WINDOW_BYTES,
                    )
                    .unwrap(),
            );
            cached.push(now.elapsed().as_secs_f64() * 1000.);
        }
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        println!(
            "width={width} windows={} max_output={output_bytes} decoder_and_output={} limit={}",
            spans.len(),
            72000000 + output_bytes,
            limits.max_decoded_bytes
        );
        println!("  old_ms={old:.2?}");
        println!("  new_ms={cached:.2?}");
        println!(
            "  scroll_mean_old_ms={:.2} scroll_mean_new_ms={:.2} cold_new_ms={:.2}",
            mean(&old[1..]),
            mean(&cached[1..]),
            cached[0]
        );
        let legacy = strip_shifts(&bytes, limits, width, false);
        let tiles = strip_shifts(&bytes, limits, width, true);
        println!("  strip_shift_legacy_ms={legacy:.2?}");
        println!("  strip_shift_tiles_ms={tiles:.2?}");
        println!(
            "  strip_shift_mean_legacy_ms={:.2} strip_shift_mean_tiles_ms={:.2}",
            mean(&legacy),
            mean(&tiles)
        );
    }
}

/// One-strip shifts as the image worker sees them: every request moves the
/// viewport by one 2,048-row strip, so one strip enters and one leaves. The
/// legacy path renders the whole admitted window, then copies the entering
/// strips into tile buffers; the tile path renders only the entering strips
/// straight into their buffers.
fn strip_shifts(bytes: &Arc<Vec<u8>>, limits: DecodeLimits, width: u32, tiles: bool) -> Vec<f64> {
    let mut decoder = DisplayDecoder::default();
    let full_height = (25000u64 * u64::from(width) / 720) as f64;
    let step = f64::from(reader_engine::DISPLAY_TILE_ROWS) / full_height;
    let mut previous: Option<reader_engine::DisplayWindowPlan> = None;
    let mut times = Vec::new();
    for i in 0..6 {
        let start = 0.02 + step * f64::from(i);
        let now = Instant::now();
        let entering =
            |top: u32, bottom: u32, previous: Option<reader_engine::DisplayWindowPlan>| {
                previous.is_none_or(|p| top < p.first_row || bottom > p.first_row + p.rows)
            };
        let plan = if tiles {
            let (plan, buffers) = decoder
                .render_window_tiles(
                    bytes,
                    limits,
                    width,
                    start,
                    start + 0.06,
                    DISPLAY_WINDOW_BYTES,
                    reader_engine::DisplayOptions::default(),
                    |renderer| {
                        let plan = renderer.plan();
                        let mut buffers = Vec::new();
                        let mut top = plan.first_row;
                        while top < plan.first_row + plan.rows {
                            let bottom = ((top / reader_engine::DISPLAY_TILE_ROWS + 1)
                                * reader_engine::DISPLAY_TILE_ROWS)
                                .min(plan.first_row + plan.rows);
                            if entering(top, bottom, previous) {
                                let mut buffer =
                                    vec![0u8; (bottom - top) as usize * plan.width as usize * 4];
                                renderer.render_rect(
                                    top,
                                    bottom - top,
                                    0,
                                    plan.width,
                                    &mut buffer,
                                )?;
                                buffers.push(buffer);
                            }
                            top = bottom;
                        }
                        Ok(buffers)
                    },
                )
                .unwrap();
            black_box(buffers);
            plan
        } else {
            let window = decoder
                .decode_window(
                    bytes,
                    limits,
                    width,
                    start,
                    start + 0.06,
                    DISPLAY_WINDOW_BYTES,
                )
                .unwrap();
            let plan = window.plan;
            let stride = plan.width as usize * 4;
            let mut top = plan.first_row;
            while top < plan.first_row + plan.rows {
                let bottom = ((top / reader_engine::DISPLAY_TILE_ROWS + 1)
                    * reader_engine::DISPLAY_TILE_ROWS)
                    .min(plan.first_row + plan.rows);
                if entering(top, bottom, previous) {
                    let from = (top - plan.first_row) as usize * stride;
                    let to = (bottom - plan.first_row) as usize * stride;
                    black_box(window.image.as_raw()[from..to].to_vec());
                }
                top = bottom;
            }
            plan
        };
        if previous.is_some() {
            times.push(now.elapsed().as_secs_f64() * 1000.);
        }
        previous = Some(plan);
    }
    times
}
