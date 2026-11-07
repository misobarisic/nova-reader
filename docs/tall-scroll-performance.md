# Tall-page scrolling

The 720×25,000 case has 18 million pixels: one RGBA original occupies
72,000,000 bytes (68.7 MiB). Encoded-image disk caching removes repeat network
requests, but does not remove decompression, resampling, tile copies or GPU uploads.

## What Mihon actually does

Reviewed stable **v0.21.3** (`feadad5e1e2bf638be6e2ca317d11657b09d4596`)
and main **`e588e04359fe1058206a2cbac1b01018ad787359`**. Webtoon rendering uses
strips in both revisions; it does not use the paged viewer's subsampling view.

- [Stable ReaderPageImageView](https://github.com/mihonapp/mihon/blob/v0.21.3/app/src/main/java/eu/kanade/tachiyomi/ui/reader/viewer/ReaderPageImageView.kt)
  selects `WebtoonStripImageView` for nonanimated webtoon pages. Its Coil request
  disables Coil memory/disk caching because the reader manages page lifetime.
- [Stable ImageDecoder](https://github.com/mihonapp/mihon/blob/v0.21.3/app/src/main/java/eu/kanade/tachiyomi/data/coil/ImageDecoder.kt)
  decodes a page once, creates immutable strips of about 4 MiB, then frees the
  full decoded bitmap. The strip-width budget is 128 MiB per page, capped at
  4,096 pixels wide, and attempts to retain at least the view width. A semaphore
  serializes strip decoding.
- [Stable WebtoonStripImageView](https://github.com/mihonapp/mihon/blob/v0.21.3/app/src/main/java/eu/kanade/tachiyomi/ui/reader/viewer/webtoon/WebtoonStripImageView.kt)
  retains the strips for the attached page and only draws strips intersecting
  the canvas clip. Immutable strips reach the GPU when drawn. Shared scaled
  boundaries round to integer pixels. Detached pages above 64 MiB release their
  strips and decode again when reattached.
- [Main ImageDecoder](https://github.com/mihonapp/mihon/blob/e588e04359fe1058206a2cbac1b01018ad787359/app/src/main/java/eu/kanade/tachiyomi/data/coil/ImageDecoder.kt)
  retains that strip strategy while decoding a native frame with sampling and
  copying row bands, avoiding a second full Java bitmap. Its comment explicitly
  explains that per-strip regions would decode preceding rows repeatedly.
- [Stable ChapterCache](https://github.com/mihonapp/mihon/blob/v0.21.3/app/src/main/java/eu/kanade/tachiyomi/data/cache/ChapterCache.kt)
  uses a separate 100 MiB disk LRU for encoded images and page lists.
  [Main ChapterCache](https://github.com/mihonapp/mihon/blob/e588e04359fe1058206a2cbac1b01018ad787359/app/src/main/java/mihon/data/cache/ChapterCache.kt)
  uses separate TTL caches: page lists for one hour and images for one day.
  These disk policies are distinct from strip rendering and GPU lifetime.

## Reader's focused change

Previously every shifted row window called `decode_image` again, even when
the encoded response remained in memory. The app also copied every replacement
tile into Slint pixel buffers on its UI event loop.

`DisplayDecoder` now reuses one decoded original for repeated windows of the
same encoded `Arc<Vec<u8>>`. It drops that original before decoding a different
response. Authentication/cache-scope changes produce different response Arcs,
and changed decode limits require new admission. `clear()` releases retained
pixels and the response when reader/source lifecycle changes invalidate them,
including while the image worker is idle. Whole native-size outputs take
ownership of their original allocation. Window/model limits, native width,
global resampling coordinates and physical-pixel shared drawing edges remain
the same. Bilinear row selection now runs once per output row; a regression
compares all output bytes with the previous per-pixel formula.

The image worker prepares Slint pixel buffers before publishing a completion.
Windows align to a global 2,048-row strip grid when the existing budget permits;
tight budgets keep an exact admitted window covering all visible rows. An exact
encoded-response identity, output width/full height and strip rectangle allow
overlapping image handles and their existing Slint delegates to survive through
in-place `VecModel` edge insertions/removals. Only entering strips receive new
pixel buffers. An evicted or replaced backing window rejects a stale completion
and retries without blanking current art. Unchanged dimensions skip chapter-wide
ratio/position notifications and scroll restoration. Ordinary covered scroll ticks
also skip navigation labels, chapter ID cloning and image task/set work. Output
admission reserves space for both the row window and prepared buffers beside
the retained original, and the worker waits for UI acknowledgement before
preparing another completion. Idle lifecycle checks clear the original and
encoded response when the reader closes or its generation/source is invalidated.

The previous 600 ms progress timer also committed the domain store synchronously
on the Slint event loop, making periodic stalls possible even when no image
window changed. `src/progress_checkpoint.rs` now coalesces latest chapter
positions on a storage worker. Timer handoff is nonblocking; explicit navigation,
shutdown and lifecycle flushes use a durability barrier after that handoff. Tests
cover busy storage, failed writes/retry, final pending work and actual process
restart persistence. This removes storage commits from ordinary scrolling, while
physical-device frame measurements remain necessary to quantify the result.

Unlike Mihon, Reader retains a bounded row window rather than every strip of an
attached page. This avoids expanding the current 32 MiB retained model budget.
The worker still extracts a full admitted CPU row window before preparing only
new strip buffers. Offscreen page decoding waits while a visible long page has
not reached its final admitted window, including tasks queued before dimensions
were known. This keeps speculative prefetch from evicting the worker's one
original; final windows and short pages retain normal next-page prefetch.
Renderer cache reuse depends on preserving repeated-item
identity as well as image identity: recreating delegates can re-upload dynamic
images. Slow-scroll regressions assert unchanged pixel allocations/model identity,
no reset or overlap row-change events and at most one entering 2,048-row strip
(four 512-row upload rects) per window shift for a 720×25,000 page. These are deterministic work/retention
checks; Android GPU frame times still require physical-device measurements.

## Strip-shift rendering

A viewport shift used to render the whole admitted window (about three
2,048-row strips) and then copy only the entering strip into its upload buffer.
The worker now computes the surviving-strip `reuse` before decoding, and
`DisplayDecoder::render_window_tiles` hands a `WindowRenderer` to the app, which
samples only the entering rectangles straight into their `SharedPixelBuffer`s.
Resampling uses an x-tap table, 8-bit fixed-point weights and a two-row
horizontal cache, so adjacent output rows share horizontally interpolated source
rows. Pixels at a shared row are still identical between windows and tiles.
Fixed point can differ from the old f64 formula by one level per channel; tests
bound that difference.

The `tall_scroll` example's strip-shift case (same machine, alternating runs of
the old and new binaries, five one-strip shifts, 512 px output) went from about
109 ms per shift to about 5 ms (6 ms for a shift that actually admits a strip;
some shifts admit none and now cost nothing). Native-width shifts went from
1.7 ms to 0.5 ms.

## Upload granularity

Strips still align to the 2,048-row grid, but the Slint tile items are 512-row
sub-rects of each strip (`DISPLAY_UPLOAD_ROWS`), so a mid-scroll GPU upload is a
quarter of the size: a 720 px wide upload drops from 5.9 MB to 1.5 MB and a
1,080 px wide one from 8.8 MB to 2.2 MB. Sub-rects sit on one global grid, so
overlapping windows still share identical rects and their delegates. The seam
logic in `app.slint` is per tile and unchanged. Only upload sizes and tile
counts were measured; frame times need a physical device.

## Retention and read-ahead

Pages that leave the one-page-either-side window used to be dropped at once,
so a short back-and-forth scroll decoded them again. Outside a reverse handoff
they now stay retained, and a completion that needs the model budget reclaims
the farthest page outside the read-ahead window first (visible work may reclaim
any non-visible page, as before). Read-ahead follows the scroll direction
(`continuity_backwards`): about two viewport heights, one to four pages, with
one page kept behind. Pages whose width or display recipe is stale are still
dropped immediately, and `defers_prefetch`, `handoff_budgets` and the 32 MiB
model budget are unchanged. A scripted live-worker walk (down 3, up 2, down 2
pages of 512x768) went from 10 decodes with 4 repeats to 8 decodes with none.

## Split pages (banded decoding)

A tall page can be stored as independently encoded 2,048-row strips (see
`docs/project-structure.md`, `page_store`). The engine sees only the
`PageSource` trait: dimensions, an optional `row_granularity`, and
`decode_rows`. `DisplayDecoder` decodes a source that reports a granularity
smaller than the page one band at a time and keeps the bands in an LRU capped
at 64 MiB (and at what the decoded budget leaves after the output window).
`WindowRenderer::render_rect` loads only the bands its rows touch; a rectangle
that straddles a seam samples both neighbours through the same resampler as a
whole decode, so a banded window is byte-identical to the whole-page window.
Rotation, spread splitting and reversal need the whole page and decode it
whole. Border cropping scans the bands once in order (`CropScan`), which
equals whole-page detection.

The `tall_scroll` example generates the same 720x25,000 page as 13 lossless
PNG strips and runs the same windows against both layouts. Same host and
command as below, mean of three runs. Host scheduling moves single values by
up to about 25% (the 720 px banded jump ranged 84-104 ms, the 512 px whole-page
later-window mean 26-39 ms). Scroll means exclude the first window.

| Output width | Layout | First window | Mean later window | Jump to 78% (fresh) | One-strip shift |
| --- | --- | --- | --- | --- | --- |
| 720 px | whole page, decoded once and retained | 211 ms | 2.4 ms | 219 ms | 0.7 ms |
| 720 px | strips, banded | 52 ms | 28 ms | 91 ms | 12.8 ms |
| 512 px | whole page, decoded once and retained | 231 ms | 31 ms | 251 ms | 6.3 ms |
| 512 px | strips, banded | 106 ms | 48 ms | 126 ms | 22.0 ms |

Opening a tall page, or jumping into its middle, now costs the strips that are
actually shown instead of the whole page: about 4x less at the first window and
about 2.4x less for a jump at native width (2x at 512 px). The price is paid while scrolling: a
whole retained original makes later windows nearly free, whereas each strip
that enters now decodes once (about 13 ms per strip at 720 px). Scrolling
through a page costs the same decode work in total as before; it is spread
over the scroll rather than paid up front, and the page is never held as a
72 MB original. Strips here are lossless PNG; the app's cache stores them in the
cache format (WebP by default), whose per-strip decode cost is measured in the
app (see "Split storage in the app" below).

## Split storage in the app

The cache stores a page taller than 4,096 rows as 2,048-row strips in the
cache's format and quality (WebP 85 by default; see `page_store`). A page taller
than 16,383 rows could never be a single WebP, so such pages stayed in their
source encoding. `tests/split_decode_cost.rs` (ignored; it prints timings)
measures the same 720x25,000 line-art page as the cache would hold it, with the
engine's `tall_scroll` windows. Same host, mean of three runs, release-like
dependencies (`--config 'profile.dev.package.nova-reader.opt-level=2'`):

| Output width | Cache entry | First window | Mean later window | Jump to 78% (fresh) | One-strip shift |
| --- | --- | --- | --- | --- | --- |
| 720 px | whole source PNG (what a page over 8 MP stored before) | 220 ms | 5.3 ms | 230 ms | 0.7 ms |
| 720 px | 13 WebP 85 strips | 103 ms | 52 ms | 163 ms | 27 ms |
| 512 px | whole source PNG | 236 ms | 26 ms | 250 ms | 6.2 ms |
| 512 px | 13 WebP 85 strips | 183 ms | 67 ms | 251 ms | 39 ms |

What the numbers say, without rounding in the strips' favour:

- Opening a page, or resuming deep inside it, costs the strips shown instead of
  the whole page: about 2.1x less at native width, about 1.3x less at 512 px,
  and nothing gained for a jump at 512 px on this fixture.
- Scrolling gets dearer per window. The whole-page path decodes once and then
  serves windows from a retained 72 MB original (0.7-6 ms per strip); strips
  decode each strip as it enters (27-39 ms), off the UI thread and ahead of the
  viewport by the usual 2,048-row prefetch. The 72 MB original is gone: the
  decoded footprint is at most the 64 MiB band cache and in practice a few
  bands.
- This fixture is the cheap case for the baseline: line art makes a tiny PNG
  (1.8 MB) that decodes fast, while the same pixels as WebP 85 are 4.2 MB. A
  photographic or JPEG source is the other way round (WebP is smaller than the
  source), and decoding a JPEG or lossy WebP whole is far slower than PNG.
- Writing the strips takes 2.7 s here (decode once, encode 13 strips), on the
  cache writer thread at prefetch priority behind the viewer.
- Lossy WebP strips are decoded with libwebp, which is linked already for
  encoding. The pure-Rust decoder behind `decode_image` made the same strips
  about three times slower (first window about 300 ms, one-strip shift about
  77 ms at 720 px) and would have turned the first column of the table into a
  loss.

## Reproduce the CPU profile

```sh
cargo run --locked -p reader-engine --example tall_scroll \
  --config 'profile.dev.package.reader-engine.opt-level=2'
```

The example generates a deterministic RGB PNG with thin black strokes at
720×25,000 (1,839,479 encoded bytes), then measures eight windows at native width
720 and downsampled width 512. Each viewport spans 6% of the original, with the
production prefetch rows and limits. The stateless and cached loops run
separately so a cached original does not overlap the baseline's decode phase.
The first cached window is cold; reported scroll means exclude that first window.
The engine and its image dependencies are optimized; the dev-profile label
printed by Cargo describes the overall profile.

The profile runs on an x86-64 Xeon Platinum 8573C host. It measures CPU decoding
and row extraction/resampling only. It does not measure Android frame times,
GPU uploads, renderer memory or physical-device acceptance.

Before global strip alignment/reuse, one optimized run produced these times. Host scheduling affects these
values; they are evidence for the repeated work removed, not device guarantees.
Both loops use the current bit-exact row resampler, so the comparison isolates
decoded-original reuse.

| Output width | Stateless scroll mean | Cached scroll mean | Cached cold window |
| --- | --- | --- | --- |
| 720 px, native width | 111.42 ms | 1.69 ms | 108.10 ms |
| 512 px, downsampled | 264.66 ms | 136.34 ms | 273.13 ms |

Native-width scrolling removes almost all repeated decode work. Downsampling
still spends substantial time resampling every row window. Texture upload
costs require a separate Android measurement, as do overlap reuse or smaller
upload batches.

That earlier native-width profile's largest output was 16,119,360 bytes; aligned
windows can retain more rows within the same admission limits. Its retained
original plus output occupies 88,119,360 bytes, below the 134,217,728-byte phase
limit. Native decoding of RGB uses at most the existing 54,000,000-byte decoder
output plus 72,000,000-byte RGBA conversion admission. An RGBA-encoded source of
these dimensions still exceeds that unchanged two-buffer decoder admission.
Display-model references, renderer memory and encoded responses have separate
budgets; 128 MiB does not describe total process memory.

Tests use allocation identity, exact pixels, response replacement, stricter
limits and phase admission; there are no timing thresholds in CI.
