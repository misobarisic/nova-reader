# Follow-ups

Deferred work that has been designed or discussed but is not yet scheduled.
Each entry records why it was deferred and what it would unlock.

## Lossless JPEG crop for tall-page strips

**Status:** deferred. It needs a new native dependency, such as libjpeg-turbo's
lossless transform (`tjTransform` / jpegtran), mozjpeg, or an equivalent
DCT-domain crop in nova-kit. The current dependency tree has none:
`zune-jpeg` only decodes.

**Context:**
- Tall pages in the chapter cache, and optionally in downloads, are stored as
  independently encoded 2,048-row strips (`page_store`). Strips exist only when
  that store's compression is on, and each is encoded in the store's format and
  quality, so a JPEG source is re-encoded like any other page.
- There is no pixel-exact mode: with compression off a page is stored as
  fetched, whole.
- Reversible JPEG XL (`transcode_jpeg_to_jxl`) only works on a whole JPEG, so
  it cannot produce per-strip files; a JPEG transcoded that way stays whole.

**What it would do:** crop a JPEG at 16-row MCU boundaries (2,048 is a
multiple of 16) without re-encoding, giving JPEG strips that are bit-exact
slices of the original's coefficient data.

**What it would unlock:**
- exact strips for JPEG webtoons, the most common source format (an opt-in
  exact mode, which does not exist today);
- per-strip reversible JPEG XL when "Losslessly transcode JPEG" is on;
- exporting a split JPEG page as its original bytes rather than a rebuilt PNG.

**Constraints:**
- The dependency must build for Linux, Windows GNU and Android (arm64, armv7,
  x86_64) under the flake.
- Its license must be compatible with `about.toml`.
- Output must be verified pixel-identical to decoding the uncut original.
