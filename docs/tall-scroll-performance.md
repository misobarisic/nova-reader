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

The image worker now prepares the Slint pixel buffers before publishing a
completion. The UI event loop creates image handles and updates models. Output
admission reserves space for both the row window and prepared buffers beside
the retained original, and the worker waits for UI acknowledgement before
preparing another completion. Idle lifecycle checks clear the original and
encoded response when the reader closes or its generation/source is invalidated.

Unlike Mihon, Reader retains a bounded row window rather than every strip of an
attached page. This avoids expanding the current 32 MiB retained model budget.
Replacing a window can still replace overlapping GPU textures. Stable tile reuse
would require a separate implementation and device measurements.

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

One optimized run produced the following times. Host scheduling affects these
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

The native-width profile's largest output is 16,119,360 bytes. Its retained
original plus output occupies 88,119,360 bytes, below the 134,217,728-byte phase
limit. Native decoding of RGB uses at most the existing 54,000,000-byte decoder
output plus 72,000,000-byte RGBA conversion admission. An RGBA-encoded source of
these dimensions still exceeds that unchanged two-buffer decoder admission.
Display-model references, renderer memory and encoded responses have separate
budgets; 128 MiB does not describe total process memory.

Tests use allocation identity, exact pixels, response replacement, stricter
limits and phase admission; there are no timing thresholds in CI.
