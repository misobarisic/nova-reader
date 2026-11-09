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

## Chapter cache: clean shutdown and the batched usage index

**Status:** deferred. `chapter_cache::tests::grouped_chapter_eviction_preserves_active_chapter_posters_and_downloads`
fails since the nova-kit bump to `01de53b` (`df98627`).

**Context:**
- Kit's image cache now writes its usage index (`.image-usage-v1.json`: cached
  files, sizes, last-used times) at most every two minutes
  (`nova_cache::gc::BATCH_INTERVAL`) instead of on every touch. Images and
  pages are still written immediately.
- When the cache's `gc::Worker` (held as `ChapterCache::_gc`) is dropped, Kit
  writes a final index marked `clean_shutdown`. Without that mark the next
  start rescans the cache directory to rebuild the records.
- The failing test drops its seeded cache and expects the index on disk, but
  finds none: the final write did not happen there. Either the test keeps the
  cache alive (only the test needs updating) or the drop path does not flush.

**Impact today:**
- Normal quit: nothing lost, if the worker is dropped.
- Crash or Android kill: up to two minutes of last-used times are lost, and
  the next start pays for a directory rescan. An image viewed just before the
  kill can look older and be evicted sooner. Active chapters are unaffected:
  eviction skips anything used within the last two minutes and active leases.
- If the app never drops `ChapterCache` on exit, every start pays the rescan.
  This costs speed, not data.

**What to do:**
- Confirm whether the app drops the chapter cache (and its `gc::Worker`) on
  orderly exit on desktop and Android; if not, drop it explicitly before the
  process ends.
- Update the test for batched writes: wait for the worker's clean flush on
  drop, or seed the index through Kit rather than reading the file directly.

## History as a reading diary: one visit per chapter per day

**Status:** deferred, direction agreed (option B below). Retention and the
cross-device delete behaviour still need deciding.

**Today:** `reader_domain::HistoryEntry { chapter, read_at }` keeps one entry
per chapter. `ReaderState::set_progress` replaces it on every progress update,
so re-reading a chapter moves it to today and its earlier read disappears:
reading chapters 80–87 on Monday and chapter 85 again on Wednesday leaves
Monday's session showing 80–84 and 86–87 only. Sync stores one `history`
record per chapter. Resume, read state and Updates come from progress, not
history.

**Options considered:**
- A. Keep the last read only (today).
- B. One visit per (chapter, local day) holding the first and last time that
  day: past days stay intact, re-reads appear on their own day, and session
  time ranges stay true. Chosen.
- C. Every open as an event: complete but noisy, large and heavy to sync.
- D. One entry with `first_read_at`, `last_read_at` and a read count: cheap,
  but History still only shows the latest day. B can carry these numbers.

**What B involves:**
- Domain: visits keyed by chapter and local day; reading again the same day
  updates that day's last time only. A schema version bump migrates each
  existing entry to one visit on its day. `validate` keeps rejecting visits to
  unknown chapters.
- Retention: a cap (12 months or a fixed number of visits), to decide.
- Sync: `history` records keyed by chapter and day, so devices' visits merge.
  Older builds do not understand them: either write both forms for a release
  or require updated peers. The checkpoint rule that stops an old checkpoint
  undoing a clear (`src/sync.rs`) moves to per-visit identities.
- Removal: "Remove session" deletes that day's visits for the series;
  removing a chapter from history and Clear history delete all its visits.
  Whether removal propagates to other devices as it does today is open.
- Pages: `src/timeline_page.rs` builds History facts from visits rather than
  from the chapter's single `read_at`; the detail screen can show "first read
  3 Nov · read 3×".

## Reader handoff tests are timing-sensitive under full-suite load

**Status:** deferred. The live image-worker tests in
`src/reader_handoff_tests.rs` (for example
`live_worker_large_reverse_and_forward_gestures_preserve_covered_anchor` and
`live_image_worker_resumes_midchapter_and_crosses_two_reverse_boundaries`)
occasionally fail in `cargo test --workspace` and pass when run alone. They
drive a real worker thread in a subprocess and wait on decode progress, so a
busy machine can change what has been decoded when an assertion runs.

**What to do:** wait on the condition each assertion depends on (decoded rows
or the anchor) instead of a fixed number of pumps, or give these tests a
dedicated serial runner.
