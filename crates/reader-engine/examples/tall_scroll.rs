//! Deterministic CPU profile, without network, disk cache or a GPU.
//! Run with:
//! cargo run --locked -p reader-engine --example tall_scroll \
//!   --config 'profile.dev.package.reader-engine.opt-level=2'
use reader_engine::{
    DISPLAY_TILE_ROWS, DISPLAY_WINDOW_BYTES, DecodeLimits, DisplayDecoder, PageDimensions,
    PageSource, decode_display_window, decode_image,
};
use std::{hint::black_box, io::Cursor, ops::Range, sync::Arc, time::Instant};

/// The same page stored as independent lossless strips, like the app's split
/// pages: decoding a row range touches only the strips it intersects.
struct Strips {
    dimensions: PageDimensions,
    strips: Vec<Vec<u8>>,
}
impl Strips {
    fn new(image: &image::RgbImage) -> Self {
        let strips = (0..image.height().div_ceil(DISPLAY_TILE_ROWS))
            .map(|band| {
                let top = band * DISPLAY_TILE_ROWS;
                let rows = DISPLAY_TILE_ROWS.min(image.height() - top);
                let mut bytes = Cursor::new(Vec::new());
                image::imageops::crop_imm(image, 0, top, image.width(), rows)
                    .to_image()
                    .write_to(&mut bytes, image::ImageFormat::Png)
                    .unwrap();
                bytes.into_inner()
            })
            .collect();
        Self {
            dimensions: PageDimensions {
                width: image.width(),
                height: image.height(),
            },
            strips,
        }
    }
}
impl PageSource for Strips {
    fn dimensions(&self, _: DecodeLimits) -> reader_engine::Result<PageDimensions> {
        Ok(self.dimensions)
    }
    fn encoded_len(&self) -> usize {
        self.strips.iter().map(Vec::len).sum()
    }
    fn row_granularity(&self) -> Option<u32> {
        Some(DISPLAY_TILE_ROWS)
    }
    fn decode_rows(
        &self,
        rows: Range<u32>,
        limits: DecodeLimits,
    ) -> reader_engine::Result<image::RgbaImage> {
        let _ = rows.end;
        decode_image(
            &self.strips[(rows.start / DISPLAY_TILE_ROWS) as usize],
            limits,
        )
    }
    fn decode(&self, limits: DecodeLimits) -> reader_engine::Result<image::RgbaImage> {
        let mut page = image::RgbaImage::new(self.dimensions.width, self.dimensions.height);
        for (band, strip) in self.strips.iter().enumerate() {
            let strip = decode_image(strip, limits)?;
            image::imageops::replace(
                &mut page,
                &strip,
                0,
                i64::from(band as u32 * DISPLAY_TILE_ROWS),
            );
        }
        Ok(page)
    }
}

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
    let strips: Arc<dyn PageSource> = Arc::new(Strips::new(&image));
    drop(image);
    let bytes = Arc::new(encoded.into_inner());
    println!(
        "fixture=RGB PNG 720x25000 encoded={} original_rgba=72000000 strips=13x{DISPLAY_TILE_ROWS} strips_encoded={}",
        bytes.len(),
        strips.encoded_len()
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
        // Split storage: the same windows, decoded band by band.
        let mut banded_decoder = DisplayDecoder::default();
        let mut banded = Vec::new();
        for start in spans {
            let now = Instant::now();
            black_box(
                banded_decoder
                    .decode_window(
                        &strips,
                        limits,
                        width,
                        start,
                        start + 0.06,
                        DISPLAY_WINDOW_BYTES,
                    )
                    .unwrap(),
            );
            banded.push(now.elapsed().as_secs_f64() * 1000.);
        }
        println!("  banded_ms={banded:.2?}");
        println!(
            "  scroll_mean_banded_ms={:.2} cold_banded_ms={:.2}",
            mean(&banded[1..]),
            banded[0]
        );
        // Opening deep in the page (resume position): a fresh decoder per run.
        let jump = |source: &dyn Fn() -> f64| (0..3).map(|_| source()).sum::<f64>() / 3.;
        let whole_jump = jump(&|| {
            let now = Instant::now();
            black_box(
                DisplayDecoder::default()
                    .decode_window(&bytes, limits, width, 0.78, 0.84, DISPLAY_WINDOW_BYTES)
                    .unwrap(),
            );
            now.elapsed().as_secs_f64() * 1000.
        });
        let banded_jump = jump(&|| {
            let now = Instant::now();
            black_box(
                DisplayDecoder::default()
                    .decode_window(&strips, limits, width, 0.78, 0.84, DISPLAY_WINDOW_BYTES)
                    .unwrap(),
            );
            now.elapsed().as_secs_f64() * 1000.
        });
        println!("  jump_cold_whole_ms={whole_jump:.2} jump_cold_banded_ms={banded_jump:.2}");
        let legacy = strip_shifts(&bytes, limits, width, false, None).0;
        let tiles = strip_shifts(&bytes, limits, width, true, None).0;
        let (banded_tiles, entry_waits) = strip_shifts(&strips, limits, width, true, None);
        println!("  strip_shift_banded_tiles_ms={banded_tiles:.2?}");
        println!(
            "  strip_shift_mean_banded_tiles_ms={:.2} bands_decoded_on_entry={entry_waits}",
            mean(&banded_tiles)
        );
        // The same shifts after the idle read-ahead the image worker runs
        // between them (two viewport heights beyond the window edge).
        let (ahead_tiles, entry_waits) = strip_shifts(&strips, limits, width, true, Some(2));
        println!("  strip_shift_banded_read_ahead_ms={ahead_tiles:.2?}");
        println!(
            "  strip_shift_mean_banded_read_ahead_ms={:.2} bands_decoded_on_entry={entry_waits}",
            mean(&ahead_tiles)
        );
        // Continuous scrolling: half a viewport per step through the page.
        for (name, ahead) in [("no read-ahead", None), ("read-ahead", Some(2))] {
            let (times, waits) = continuous_scroll(&strips, limits, width, ahead);
            println!(
                "  continuous_scroll {name}: windows={} mean_window_ms={:.2} max_window_ms={:.2} windows_that_waited_for_a_band={} bands_decoded_on_entry={}",
                times.len(),
                mean(&times),
                times.iter().copied().fold(0., f64::max),
                waits.1,
                waits.0,
            );
        }
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
fn strip_shifts<S: reader_engine::SharedPage>(
    bytes: &S,
    limits: DecodeLimits,
    width: u32,
    tiles: bool,
    read_ahead: Option<u32>,
) -> (Vec<f64>, u64) {
    let mut decoder = DisplayDecoder::default();
    let mut entry_waits = 0;
    let full_height = (25000u64 * u64::from(width) / 720) as f64;
    let step = f64::from(reader_engine::DISPLAY_TILE_ROWS) / full_height;
    let mut previous: Option<reader_engine::DisplayWindowPlan> = None;
    let mut times = Vec::new();
    for i in 0..6 {
        let start = 0.02 + step * f64::from(i);
        let on_demand = decoder.band_stats().on_demand;
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
            entry_waits += decoder.band_stats().on_demand - on_demand;
        }
        previous = Some(plan);
        if let Some(viewports) = read_ahead {
            idle_read_ahead(&mut decoder, bytes, limits, width, plan, viewports);
        }
    }
    (times, entry_waits)
}

/// What the image worker does while the viewer is idle: decode the bands
/// beyond the window edge, nearest first.
fn idle_read_ahead<S: reader_engine::SharedPage>(
    decoder: &mut DisplayDecoder,
    source: &S,
    limits: DecodeLimits,
    width: u32,
    window: reader_engine::DisplayWindowPlan,
    viewports: u32,
) {
    // A viewport spans 6% of the page.
    let viewport_rows = (f64::from(window.full_height) * 0.06) as u32;
    while decoder
        .read_ahead(
            source,
            limits,
            reader_engine::ReadAhead {
                width,
                options: reader_engine::DisplayOptions::default(),
                window,
                backwards: false,
                depth_rows: viewport_rows * viewports,
                max_bytes: 32 * 1024 * 1024,
            },
        )
        .unwrap()
        == reader_engine::ReadAheadStep::Decoded
    {}
}

/// Scroll the whole page half a viewport at a time, rendering only the tiles
/// that enter each window. Returns every window's time and, as
/// (bands decoded while a window waited, windows that had to wait).
fn continuous_scroll<S: reader_engine::SharedPage>(
    source: &S,
    limits: DecodeLimits,
    width: u32,
    read_ahead: Option<u32>,
) -> (Vec<f64>, (u64, usize)) {
    let mut decoder = DisplayDecoder::default();
    let mut previous: Option<reader_engine::DisplayWindowPlan> = None;
    let mut times = Vec::new();
    let mut waited = 0;
    let mut stalled = 0;
    let mut start = 0.;
    while start + 0.06 <= 1. {
        let on_demand = decoder.band_stats().on_demand;
        let now = Instant::now();
        let (plan, buffers) = decoder
            .render_window_tiles(
                source,
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
                        let bottom = ((top / DISPLAY_TILE_ROWS + 1) * DISPLAY_TILE_ROWS)
                            .min(plan.first_row + plan.rows);
                        if previous
                            .is_none_or(|p| top < p.first_row || bottom > p.first_row + p.rows)
                        {
                            let mut buffer =
                                vec![0u8; (bottom - top) as usize * plan.width as usize * 4];
                            renderer.render_rect(top, bottom - top, 0, plan.width, &mut buffer)?;
                            buffers.push(buffer);
                        }
                        top = bottom;
                    }
                    Ok(buffers)
                },
            )
            .unwrap();
        black_box(buffers);
        if previous.is_some() {
            let elapsed = now.elapsed().as_secs_f64() * 1000.;
            let new = decoder.band_stats().on_demand - on_demand;
            // Windows that render nothing (no strip entered) cost nothing.
            if previous != Some(plan) {
                times.push(elapsed);
                waited += new;
                stalled += usize::from(new > 0);
            }
        }
        previous = Some(plan);
        if let Some(viewports) = read_ahead {
            idle_read_ahead(&mut decoder, source, limits, width, plan, viewports);
        }
        start += 0.03;
    }
    (times, (waited, stalled))
}
