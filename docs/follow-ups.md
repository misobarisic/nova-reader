# Follow-ups

Deferred work that has been designed or discussed but is not yet scheduled.
Each entry records why it was deferred and what it would unlock.

## Lossless JPEG crop for tall-page strips

**Status:** deferred. It needs a new native dependency, such as libjpeg-turbo's
lossless transform (`tjTransform` / jpegtran), mozjpeg, or an equivalent
DCT-domain crop in nova-kit. The current dependency tree has none:
`zune-jpeg` only decodes.

**Context:**
- Tall pages in the chapter cache, and optionally in downloads, can be stored
  as 2,048-row strips, so that scrolling decodes only the rows it needs.
- With compression off, strips must be pixel-exact.
- PNG and lossless WebP split losslessly.
- JPEG currently stays whole, because it cannot be cut without re-encoding.
- Reversible JPEG XL (`transcode_jpeg_to_jxl`) only works on a whole JPEG, so
  it cannot produce per-strip files.

**What it would do:** crop a JPEG at 16-row MCU boundaries (2,048 is a
multiple of 16) without re-encoding, giving JPEG strips that are bit-exact
slices of the original's coefficient data.

**What it would unlock:**
- exact strips for JPEG webtoons, the most common source format;
- per-strip reversible JPEG XL when "Losslessly transcode JPEG" is on;
- exporting a split JPEG page as its original bytes rather than a rebuilt PNG.

**Constraints:**
- The dependency must build for Linux, Windows GNU and Android (arm64, armv7,
  x86_64) under the flake.
- Its license must be compatible with `about.toml`.
- Output must be verified pixel-identical to decoding the uncut original.
