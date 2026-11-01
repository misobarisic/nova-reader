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
    }
}
