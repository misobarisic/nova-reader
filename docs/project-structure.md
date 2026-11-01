# Project structure

Nova Reader is a separate Rust 2024 workspace. UI orchestration is app-owned;
heavy reader subsystems live in leaf crates. It consumes pinned nova-kit storage
and UI utilities without introducing reader-domain dependencies into kit.

| Location | Responsibility |
| --- | --- |
| `build.rs`, `about.toml` | Nova’s copied current-target license/source catalogue generation, zstd-22 embedding recipe and final Android cdylib 16 KiB ELF alignment |
| `src/main.rs`, `src/lib.rs` | Desktop entry, Android entry/lifecycle, CLI routing |
| `src/app.rs` | Worker orchestration, navigation, source/settings selection, visible-page loading, display tiles, lifecycle progress flush |
| `src/services.rs` | Shared UI/test services: install, validated source/date conversion, library, downloads and local import |
| `src/cli.rs`, `src/paths.rs` | Commands, bounded file input, independent data and platform cache directories |
| `src/android_documents.rs`, `android/java/dev/misob/nova/reader/ReaderActivity.java` | Application-context JNI, current-activity SAF and reader status-bar visibility/lifecycle on Java main |
| `src/addon_icons.rs`, `crates/ui/addon-icon.slint` | Verified bundled addon PNGs decoded on workers, retained UI-thread image handles shared by language variants, Sources/Extensions decoration and Kit fallback |
| `src/artwork.rs` | Kit decoded-poster LRU and encoded variants, synchronous cross-page hits, source permits, UI handles, bounded worker and legacy artwork migration |
| `src/chapter_cache.rs` | Kit-backed disposable viewed pages, authorization scopes, chapter-group eviction/active leases and bounded background encoding |
| `src/reader_export.rs` | Original-image validation, screencap encoding, bounded platform save/clipboard delivery and persistent desktop clipboard ownership; `src/app.rs` captures the clean pre-menu viewport with an 8-million-pixel limit and fetches source page bytes |
| `src/export_names.rs` | Portable UTF-8 series folders and bounded title/chapter/page/vertical-position filenames, including duplicate numbering |
| `crates/ui/save-conflict-sheet.slint` | Retained modal Overwrite/Add number/Cancel sheet; `app.slint` owns name/open/choice properties and callbacks |
| `src/chapter_capture.rs` | Bounded source-resolution compositor for visible chapter areas; captures original pixels rather than enlarging a device snapshot |
| `src/capture_settings.rs`, `crates/ui/capture-settings.slint` | Validated device capture preferences and retained More → Settings → Screenshots controls for format (including JPEG XL lossless/lossy), adjacent quality, Device/Chapter screencap resolution, per-series folders and duplicate policy |
| `src/image_storage.rs`, `crates/ui/image-storage-settings.slint` | Durable cache policy and independent JPEG/WebP/AVIF compression; shared Kit cache controls with chapter-first maintenance and separate artwork settings |
| `src/source_catalogs.rs` | Host-owned per-addon catalogue switches under `reader:disabled-catalogs:<JSON package ID>`; filters Browse while preserving direct/history/offline access |
| `src/reading_preferences.rs` | Fallible Nova KV persistence for adult-source visibility and chapter order keyed by full source-local series identity |
| `src/reader_modes.rs`, `docs/reader-modes.md` | Pinned TachiyomiJ2K genre/source classification, Automatic/manual selection, and per-series persisted mode overrides |
| `src/timeline.rs`, `src/source_filters.rs`, `src/source_browser.rs` | Local date groups/provider date parsing, bounded typed filter values and persistent source pins/last-used grouping |
| `src/updates.rs` | Optional reader application release discovery |
| `src/metadata_refresh.rs` | Nova-derived foreground metadata/chapter scheduler, bounded fair batches, persisted per-series deadlines and stale-result admission |
| `src/about.rs` | Nova’s exact-size catalogue decompression, bounded text chunks and source-row ordering/fallbacks; publishes About models from a worker |
| `src/browser.rs` | Nova’s desktop browser launching and Reader Android intent adapter for About source links |
| `src/appearance.rs` | Device appearance/motion preferences, validated edits, immediate application and KV persistence |
| `crates/ui/app.slint` | Shared pages/reader models, safe-area layout, platform/width shell selection, detail origin and translated text |
| `crates/ui/media-models.slint` | Shared series/chapter/timeline/source/extension/filter row models |
| `crates/ui/library-cards.slint`, `crates/ui/display-settings.slint` | Nova poster/list painting and left-anchored progress fill, per-entry context gestures, viewport admission, grid/list switch and persisted display choices |
| `../nova-kit/crates/cache`, `../nova-kit/crates/ui-base/slint/cache-settings-controls.slint` | Actual Nova cache/settings implementations and NumericSetting/cache controls shared by Nova and Reader; app persistence/search/sync adapters remain local |
| `crates/ui/library-search.slint` | Retained Nova search reveal, deferred focus, live filtering and viewport-aware field exposure |
| `crates/ui/entry-menu.slint`, `crates/ui/status-menu.slint` | Per-title Kit action sheet/anchored desktop menu, copied Nova Change status picker, stable-identity category editing and library/refresh actions |
| `crates/ui/detail-hero.slint`, `crates/ui/title-artwork.slint`, `crates/ui/chapter-row.slint` | Nova detail artwork/title/metadata/actions, retained synopsis state, chapter-ready Continue gating, compact chapter rows and accessible action popup |
| `crates/ui/timeline.slint`, `crates/ui/source-list.slint`, `crates/ui/extensions.slint`, `crates/ui/source-filters.slint` | Dated History/Updates, grouped sources with bundled icons, extension management and actual source-defined browse filters |
| `crates/ui/reader-context-menu.slint` | Custom four-action image/screencap menu, stationary-hold/right-click handling, viewport clamping and keyboard navigation; `app.slint` owns capture/action/close callbacks and reader export feedback |
| `crates/ui/navigation.slint` | Nova’s 64px icon rail, flat five-tab bottom bar, icon feedback and keyboard navigation |
| `crates/ui/controls.slint`, `crates/ui/page-header.slint` | Thin Nova Kit control adapters and Nova’s PageHeader; shared palette, typography and accessibility |
| `crates/ui/more.slint` | Retained Nova-style More/Settings landings, nested slide/fade layers, scroll/focus restoration and local import/categories/data/About pages |
| `crates/ui/about.slint`, `crates/ui/about-models.slint`, `crates/ui/about-controls.slint` | Nova’s copied About/source/license viewer with Reader shell routing and shared header adapter |
| `crates/ui/appearance.slint`, `crates/ui/theme-settings.slint`, `crates/ui/settings-row.slint`, `crates/ui/src/appearance.rs` | Nova theme/motion settings controls, responsive setting rows and adapted Nova palette derivation |
| `crates/ui/build.rs` | Kit source aliases, embedded resources, debug layout metadata and narrow Kit dropdown/segmented keyboard/accessibility adaptations |
| `assets/fonts` | Nova’s bundled Roboto faces and Apache notices; More/settings artwork comes directly from Kit |
| `assets/open_source_*.hbs`, `assets/open_source_project_sources.tsv`, `assets/open_source_vendors.txt` | Nova catalogue templates and Reader-specific source/vendor attribution; full local notices are appended by `build.rs` |
| `assets/icons/navigation` | Embedded SVG navigation and reader controls, independent of system font glyph coverage |
| `crates/reader-domain` | Source-local identities, schema-1 state, library/categories, publication/reading status in `src/library_status.rs`, progress/history/bookmarks and backup/restore |
| `crates/reader-sources` | Reader v2 ZIP validation, durable registry, typed profile settings, QuickJS host, HTML/URL capabilities and checked source HTTP |
| `crates/reader-engine` | Dimension probing, bounded PNG/JPEG/WebP decode and shared Kit AVIF delegation, display geometry, resume anchors and decoded cache utility |
| `crates/reader-engine/src/jxl.rs`, `crates/reader-engine/tests/jpeg_xl.rs` | Thin adapter to Kit JPEG XL probing/decoding; Reader retains synthetic RGB/alpha integration fixtures while Kit owns metadata admission, auxiliary-box skipping, tracked grid budget and native row conversion |
| `crates/reader-engine/src/display.rs`, `crates/reader-engine/src/avif.rs` | Width-preserving visible-row display admission/resampling and one-original worker reuse; thin boundary to Kit `nova-image` AVIF/JPEG XL with independent Reader fixture/display/admission regressions |
| `crates/reader-engine/examples/tall_scroll.rs`, `docs/tall-scroll-performance.md` | Deterministic 720×25,000 CPU scroll profile and actual Mihon stable/main rendering comparison |
| Kit `crates/image`, `vendor/ferri-pdf-rav1d` | Shared bounded AVIF/JPEG XL codecs and published 1.1.2 portable decoder; Reader retains only thin adapters, fixture attribution and distribution licenses |
| `crates/reader-downloads` | Durable queue, checksummed page bundles, recovery, quota, source-free offline lookup, local folder/CBZ import, bundle removal and manifest-only status hints |
| `crates/reader-catalogue` | Bounded Keiyoushi protobuf discovery, exact port mappings, signed native catalogue verification and availability states |
| `examples/addons/kagane`, `crates/reader-sources/tests/kagane.rs` | Keiyoushi Kagane v37 native port, 18 languages, compact chapter index, typed filters/settings, public token/one-retry flow, bundled icon and synthetic QuickJS host regressions |
| `examples/addons/manhwa18cc` | Thin Manhwa18 configuration, reusable YiiMadara helper, Apache license/provenance and synthetic parser fixtures |
| `examples/addons/mangafire`, `crates/reader-sources/tests/mangafire.rs` | MangaFire JSON/VRF port, seven languages and typed filters/settings, deterministic parser and actual QuickJS host fixtures; scoped CDN permissions and Apache provenance |
| `addons/` | Versioned, installable native addon ZIP downloads with source links and checksums |
| `examples/addons/neutral-fixture` | Installable network-free fixture, bundled geometric PNGs and 320-page chapter |
| `tests/reading_journey.rs` | Process-boundary install/read/progress/interrupted download/recovery/source-removal/offline journey |
| `tests/ui_navigation.rs`, `tests/mobile_layout.rs` | Headless reader models, phone/landscape/tablet/desktop control bounds, tab taps, accessible activation and back navigation |
| `tests/appearance.rs` | Process-boundary appearance persistence, palette application, invalid edits and theme reset preserving motion preferences |
| `tests/reader_insets.rs` | Real Window safe-area injection verifies edge-to-edge chapter/detail artwork, safe controls/chapter rows, normal-page restoration and stable OSD geometry; uses the already pinned Slint core as a dev dependency |
| `tests/reader_design.rs` | Responsive poster/list/timeline/chapter layouts, source/pin/extension actions, retained catalogue scroll, download-delete actions, tap-versus-pan reader OSD and image-menu targeting, hold cancellation, clamping, keyboard/busy/lifecycle behavior |
| `tests/capture_settings.rs`, `tests/image_storage.rs` | Process-boundary capture/compression persistence, corruption recovery, independent policies and accessible settings controls |
| `tests/reader_modes.rs` | Per-series mode restart persistence, explicit Automatic/legacy migration, source identity isolation and accessible short-viewport mode options |
| `tests/library_navigation.rs`, `tests/source_paging.rs` | Search reveal/focus and retained detail context; automatic viewport paging, short-page fill, Retry and saved catalogue scroll |
| `tests/library_progress.rs` | Phone/desktop grid/list progress geometry from unread through complete, including small left-anchored fills |
| `tests/entry_menu.rs`, `tests/reading_preferences.rs` | Per-title gestures/category identity, durable source visibility/chapter ordering and failed-write behavior |
| `tests/reader_pixel_edges.rs`, `crates/reader-engine/tests` | Physical-pixel shared tile/page edges across fractional scroll/scales, plus AVIF fixtures and decode admission |
| `tests/license_catalog_build.rs`, `.github/scripts/test_package_license_build.py` | Actual generator feature/target/failure/frame-reuse regressions and immutable Nova function/template parity |
| `tests/about.rs` | Retained About/license scrolling, bounded-layout source links, delayed models and keyboard/system Back |
| `tests/image_download_encoding.rs` | Production Services partial-resume encoding: verified pages remain intact, new pages follow compression policy, completed bundles survive policy changes, disabled future writes retain source bytes and offline hashes survive addon removal |
| `tools/test-android-documents.py`, `tests/android-documents` | Production Java/JNI host regression with checked JNI and deterministic main-queue/lifecycle fixtures |
| `android/java/dev/misob/nova/reader/ReaderImageProvider.java`, `tools/test-android-provider-manifest.py` | Android image clipboard URI provider and packaged provider-manifest regression |
| `tools/package-addon.py` | Deterministic manifest-declared reader addon ZIP builder |
| `.github/scripts`, `.github/workflows` | Nova's copied platform packagers and PR/main-check versus tag-build split, required Reader identity/license/signing integration |
| `.github/scripts/test_package_nova_parity.py` | Immutable Nova-source parity for all three packaging scripts, masking only documented metadata/license adaptations and exact AppImage tool pins |
| `.github/scripts/test_package_linux.py`, Kit `crates/image/tests/avif_portability.rs` | Dated AppImage tool/checksum regressions and portable decoder feature-gating/runtime-dispatch regression |
| `android` | Android resources, platform document bridge and intentionally public local debug signing key |

## State and work boundaries

`DomainStore` stores validated JSON under `reader:state` through `nova-storage`
in the Reader application directory. The internal database filename remains
`nova.redb`; the application directory isolates it from Nova. One directory is
supported per process. Restore validates the replacement before publishing it.
The native metadata backup excludes executable addons, cookies and chapter bytes.

Device appearance preferences live separately under `reader:appearance` in the
same database. Missing values use Nova's defaults; palette/radius/spacing edits
and animation switches apply immediately after a successful save. Theme reset
preserves motion preferences. Portable library backup/restore leaves these
device choices intact. More/Settings route changes publish directly without
loading/deserializing library metadata before starting their transitions.
Library grid/list, sorting and minimum library/browse columns share these device
preferences and survive theme reset. Source pins/last-used source live under
`reader:source-browser`; manual update time is `reader:last-update`. These keys
are independent of portable library backup data.
Screenshots format/quality/resolution/folder preferences live separately under
`reader:captures`; changing them preserves original chapter data and metadata.
The duplicate dropdown stores Ask (default), Overwrite or Do nothing. Ask opens
a nonblocking Overwrite/Add number/Cancel sheet, guarded by the current reader
generation. Clipboard actions bypass naming and duplicate handling. Desktop
saves stage in the destination directory and publish without clobbering unless
overwrite was selected; numbered retries remain race safe. Android API 29+
queries app-owned MediaStore images by exact name and relative path. API 26–28
requests and persists a read/write document-tree grant once, then uses scoped
Nova Reader/series children. Provider overwrites retain a bounded original backup
for rollback; a failed recovery reports an error and retains one private recovery
file rather than accepting further overwrites.
Saved names start with the series title, then padded chapter/page numbers.
Screencaps append the first visible loaded page's original-pixel Y offset.
Filename components are limited to 160 UTF-8 bytes (including duplicate suffix),
series folders to 96 bytes, preserving chapter/page/position suffixes.
Device-resolution screencaps retain the clean pre-menu window snapshot. Chapter
resolution freezes the reader viewport, scroll/pan, displayed page rectangles
and source inputs before opening the menu, then composites visible source pixels
on a worker. The clicked page defines the native pixel scale. Both resolution
modes keep the eight-million-pixel export limit and apply to save and clipboard.
`reader:image-storage` stores independent cache/download compression switches,
formats and quality for new writes, plus a backward-compatible cache policy:
disk caching, separate chapter/artwork limits, decoded artwork memory and future
poster resolution. Preference changes do not rewrite existing cache files or
downloaded chapter bundles. Successful writes publish a thread-safe policy
snapshot before waking workers; hot cache hits never read the database.
Adult-source visibility lives under `reader:show-adult`, defaulting to hidden.
Browse navigation and successful addon installation share a worker snapshot of
filtered sources, all extensions and bundled icons; installation honors the
current adult-source choice immediately. Root source-list refreshes keep the
loading indicator hidden, including visibility-toggle refreshes.
`reader:chapter-order:<serialized SeriesId>` stores descending/ascending chapter
order separately for each full source-local series identity, defaulting to
descending. Both use fallible Nova KV writes; failed saves preserve the prior UI
choice. Corrupt raw values remain intact while defaults are used, until a
successful edit. These device-local preferences are outside portable metadata
backups.

`reader:mode-choice:<serialized SeriesId>` stores a versioned Automatic/manual
choice independently of library membership. A manual mode overrides chapter
progress modes; explicit library preferences migrate once. Automatic classifies
genres and source names using the pinned J2K rules before image decoding, with
Reader's established global fallback. Saving a series choice never changes that
global default. Details and adaptations are in [reader modes](reader-modes.md).

Fetched series posters belong to `cache_dir()/posters`, never the data directory.
Desktop uses the platform cache base plus `nova-reader`; Android uses
`Context.getCacheDir()`. `NOVA_READER_CACHE_DIR` overrides the cache root for tests
or deliberate configuration, independently of `NOVA_READER_DATA_DIR`. Disk files
use source/series/URL identity plus opaque authorization scope for encoded reuse. The worker admits 32 requests, rejects
encoded covers above 8 MiB and limits source decode to 8 million pixels/72 MiB
with display images at most 460×690. Nova Kit owns the decoded-pixel LRU
(128 MiB by default), encoded variant container and incremental disk GC.
The old 32 MiB/256-file artwork quota is replaced by an independent 1 GiB
default disk budget, using Nova's shared limit presets.
UI images share the LRU pixel buffers; visible/selected artwork and up to 48
additional recently used cards retain UI handles. Model hydration and request
cache hits reuse pixels immediately across Browse, Library, Updates, History
and details, without disk access or decode. Read-only source-availability proofs
revoke hits when a source is disabled, removed or updated, without retaining addon
archives. The single worker permits one decoded completion awaiting UI
acknowledgement; queued requests carry metadata, not image buffers. Active decode
allocations are separate from the retained-pixel budget.
The transport's existing 32 MiB response cap precedes the artwork admission cap.
Viewport changes cancel obsolete work. Chapter usage/clear is the primary Cache
page action; the artwork footer has separate usage/clear. Worker maintenance
uses Kit’s batched usage index and grouped eviction. Both actions affect owned
temporary files only; explicit downloads/imports and library data remain data.
The memory setting controls the artwork LRU; reader display-window/model
budgets remain separately bounded as described below.

Online pages belong to `cache_dir()/viewed-chapters`. The shared Kit container,
atomic writes, batched usage index and grouped GC retain them across reader
sessions. Its independent disk budget defaults to 1 GiB, with active-chapter
leases protecting pages while reading; chapter pressure cannot evict posters.
Keys include full chapter/page identity and an opaque source/profile/settings,
request and cookie scope. Every RAM/disk hit revalidates source permissions;
unknown custom transports opt out of reusable page caching. Background encoding
preserves dimensions, retains original bytes when unsupported/over budget, and
has a 32 MiB active-plus-pending encoded-byte limit. Namespace epochs prevent
cleared or cancelled work from repopulating files. Explicit downloads keep their
own manifests and are never removed by disposable-cache maintenance.

`addons/` contains registry state, immutable content-addressed packages and its
writer lock. Package version history rejects changed bytes at the same identity
and version. Packages are app-wide; source HTTP sessions are in-memory and
source/profile scoped. Profile switching must cancel old work. Typed settings
live under `addons/settings/<profile-sha256>/<addon-id>.json`, separately from
metadata backups. Boolean/text/select/secret values are schema-validated; on Unix
settings directories use 0700 and atomic settings files use 0600. The settings UI
masks secrets. These file permissions do not imply encryption at rest.

`chapters/` holds queue state and ordered chapter bundles. Verified page files
and atomically published completion metadata determine offline availability.
Incomplete work remains incomplete across restart; source removal does not remove
library entries, progress or completed bundles. The current service configures an
8 GiB chapter store quota. Unix download directories/files use 0700/0600, with
existing-store permission repair on open. Durable queued jobs resume at launch;
paused/cancelled jobs remain stopped. The app serializes chapter transfers; the
store retains a two-active-jobs-per-source admission cap.

Workers fetch metadata and images and decode admitted images. Only the Slint
event loop updates UI models. Download status hints come from bounded manifests
on a coalesced worker and are cached in the view; rendering never hashes page
payloads. Opening offline chapters and recovery still verify stored bytes. Navigation work uses cancellation/generation checks.
The reader covers the complete visible page window plus one prefetch page on
each side, cancels obsolete fetches and leaves failed pages waiting for Retry.
Retained display/model pixel buffers use 32 MiB, with another 32 MiB reserved
within their 64 MiB accounting budget for replacement references. Display output
uses bounded visible-row windows at the requested physical width, rather than
shrinking tall-page width to fit a full-page texture. Each window is capped at
32 MiB, reduced to a share of the 32 MiB model budget when multiple pages are visible,
with up to 2,048 prefetched rows on either side when the budget permits. Refresh
starts before the window boundary, with up to 512 rows of advance coverage; the
threshold contracts when the byte budget leaves less margin. Individual
textures contain at most 2,048 × 2,048 pixels. Physical width targets use 128px buckets
capped at the 16,384px decoder width ceiling; source pixels are never upscaled.
The worker's `DisplayDecoder` retains one admitted original while extracting
successive row windows from the same encoded response. Source response identity
uses the encoded `Arc`, so replacement responses and changed source/authentication
scopes cannot reuse old pixels. A different response releases that original
before decoding; a full native-size display transfers ownership instead of
retaining duplicate original pixels. This remains full CPU decoding, rather than
region/streaming decoding or a total process-memory bound. Decoder/native
conversion and original-RGBA-plus-window-output phases are admitted separately
within 128 MiB each. The worker also keeps a 32 MiB generation-scoped, memory-only
encoded-page cache; a miss can temporarily add another 32 MiB fetched response.
Replacement model references are released before constructing new Slint buffers;
the reserve covers a previous image still held by the renderer during replacement.
Prefetch does not mark a chapter read. Resume stores logical page ID/index and
intra-page offset, independent of row windows, texture tiles and reflow.
Retained previews upgrade when physical-pixel width grows without losing their
logical anchor. Shared drawing edges snap in physical pixel coordinates;
logical positions remain continuous. Device GPU overhead and peak total process
memory remain unmeasured.

The reader source runtime currently belongs to this repository. Moving its
generic envelope/runtime into nova-kit requires shared API design and Nova v1
consumer regressions; this workspace does not claim that extraction is complete.
Source examples are intended for independent future publication in nova-addons.

## Where to look

- Nova design reuse and destination behavior: [UI design](ui-design.md).
- Delete a downloaded chapter: `src/app.rs` (`delete-download`), `crates/reader-downloads/src/lib.rs` (`remove`), chapter actions and More → Downloads. Local imports are protected.
- Viewed chapter cache, grouped eviction and pins: `src/chapter_cache.rs`, `src/app.rs`, Kit `nova-cache::gc`/`blob`.
- Cache quotas, disk master switch and worker snapshots: `src/image_storage.rs`, `crates/ui/image-storage-settings.slint`, `tests/image_storage.rs`.
- Shared Nova settings controls: Kit `slint/cache-settings-controls.slint`; Nova keeps SettingsRow/search/sync/keyboard wrappers.
- Cover cache placement, immediate reuse and source revocation: `src/paths.rs`, `src/artwork.rs`, More → Settings → Cache.
- Addon icons and source filters: `src/addon_icons.rs`, `crates/ui/addon-icon.slint`, `src/source_filters.rs`, `crates/reader-sources/src/package.rs`; optional `manifest.icon` must name a declared PNG asset. Multiple/include/exclude groups use existing Nova setting rows with typed JSON output.
- Kagane port and JPEG XL pages: `examples/addons/kagane`, `crates/reader-sources/tests/kagane.rs`, `crates/reader-engine/src/jxl.rs`.
- Shared cache implementation/settings: Kit `crates/cache` and `crates/ui-base/slint/cache-settings-controls.slint`; Reader `src/chapter_cache.rs`, `src/image_storage.rs` and `tests/image_storage.rs`.
- Library search, retained details and per-title actions: `crates/ui/library-search.slint`, `crates/ui/entry-menu.slint`, `src/app.rs`, `tests/library_navigation.rs`, `tests/entry_menu.rs`.
- Automatic source paging and Retry: `src/app.rs` (`browse`/`complete_browse`), `crates/ui/app.slint`, `tests/source_paging.rs`.
- Automatic/manual reader mode and per-series overrides: `src/reader_modes.rs`, `docs/reader-modes.md`, `tests/reader_modes.rs`.
- Adult visibility and per-series chapter order: `src/reading_preferences.rs`, `tests/reading_preferences.rs`.
- AVIF image admission/encoding: `crates/reader-engine/src/avif.rs`, `src/reader_export.rs`, `src/image_storage.rs`.
- Stable ARMv7 AVIF build fix and provenance: Kit `vendor/ferri-pdf-rav1d/PATCHES.md`; AppImage download pins: `docs/packaging.md`.
- Tall-page row windows and display admission: `crates/reader-engine/src/display.rs`, `src/app.rs` (`request_images`).
- Tall-page CPU performance and Mihon comparison: `docs/tall-scroll-performance.md`, `crates/reader-engine/examples/tall_scroll.rs`.
- Reader tile seams and physical-pixel edges: `crates/ui/app.slint`, `tests/reader_pixel_edges.rs`.
- Series/chapter layouts, timeline/source groups and reader controls: `crates/ui/library-cards.slint`, `crates/ui/detail-hero.slint`, `crates/ui/timeline.slint`, `crates/ui/source-list.slint`, `tests/reader_design.rs`.
- Left-anchored library reading progress: `crates/ui/library-cards.slint`, `tests/library_progress.rs`.
- Image filenames and duplicate handling: `src/export_names.rs`, `src/reader_export.rs`, `src/android_documents.rs`, `crates/ui/save-conflict-sheet.slint`.
- Reader image/screencap actions and export settings: `crates/ui/reader-context-menu.slint`, `src/reader_export.rs`, `src/chapter_capture.rs`, `src/capture_settings.rs`, More → Settings → Screenshots.
- New cache/download image compression: `src/image_storage.rs`, `crates/ui/image-storage-settings.slint`, More → Settings → Cache.
- Appearance controls, palette and device persistence: `crates/ui/theme-settings.slint`, `crates/ui/src/appearance.rs`, `src/appearance.rs`.
- Android picker context, callbacks and host JVM regression: [Android documents](android-documents.md).
- Android/desktop shells and touch layout checks: `crates/ui/navigation.slint`, `crates/ui/app.slint`, `tests/mobile_layout.rs`.
- Static review scope and outcome: [review record](../REVIEW.md).
- Build-time license catalogue and source inventory: `build.rs`, `about.toml`, `assets/open_source_*`, `tests/license_catalog_build.rs`.
- About license/source viewer: `src/about.rs`, `crates/ui/about.slint`, `tests/about.rs`.
- Run/build/install and CLI: [README](../README.md), [packaging](packaging.md).
- Source manifest, HTTP permissions, limits and error contract: [source API](source-api.md).
- Manhwa routes, languages, parser tests and upstream license: [reference source](manhwa18.md).
- MangaFire VRF/JSON API, seven languages, filters and upstream provenance: [MangaFire source](mangafire.md).
- Discovery, actual upstream IDs and signing trust API: [catalogues](catalogues.md).
- Implemented behavior versus outstanding plan gates: [implementation status](implementation-status.md).

Reader cache/download format indices append JPEG XL lossless (3) and lossy (4),
with a per-group `jxl_lossless_jpeg` flag defaulting false for older saved settings.
Both groups use Nova's dropdown. The quality control remains active for lossy
JXL; opting into JPEG transcoding bypasses it only for supported original JPEGs.
Artwork resizing produces PNG before compression, so resized covers use normal
encoding. All three writers delegate JPEG coefficient conversion to Kit.

Addon settings list all declared source/language catalogue entries separately from addon-defined configuration. Host switches default to enabled, persist independently by package/source ID, and affect Browse inventory rather than whole-addon authority. Hidden entries remain available in the settings list for re-enabling.

Android touch feedback binds `AppWindow.touch-menus` to Kit's `ControlInput`,
matching Nova. The real platform sets it independently of window width, so wide
Android layouts suppress card hover lift too, while narrow desktop windows keep
mouse hover. Rows, navigation, hero actions and context sheets use Kit's exact
`ControlTouchArea.feedback-active` policy; Flickable cancellation ends touch
feedback. Reader image/card gestures keep their existing drag/hold cancellation.

Library follows Nova’s control order: category rail (the default tab is “All”),
then sorting and the grid/list/search icons. Search retains its existing narrow
and wide reveal placement and keyboard viewport handling.

`reader-domain/src/library_status.rs` adapts Nova's actual `WatchStatus` and
progress bucket decision order to Reading, Plan to read, Completed, Dropped and On hold.
LibraryEntry.reading_status defaults to Auto for old metadata and participates in
backups. Manual pins override the derived bucket without changing chapter
progress/history; Back to automatic restores derivation. Built-in filters appear
ahead of user categories. Library/detail title menus share Change status sheets.

`crates/ui/status-menu.slint` is copied directly from Nova, with the host import,
reading labels and omission of its video-only New episodes choice adapted. The
action codes match Nova; both library and detail context menus use this picker.
Reading and Completed may also be pinned explicitly, without modifying chapter progress.


`src/metadata_refresh.rs` adapts Nova's saved-episode scheduler: check each minute,
sequential batches of four at five-minute intervals, recently read titles every
30 minutes and older titles every six hours. Half the budget protects overdue
recent titles; the rest rotates through older/unvisited entries. Only foreground
Library/Updates/History run work; reading, detail, settings, minimization, Android
pause and foreground requests cancel it. User-pinned Reading remains eligible;
Plan to read, Completed, On hold and Dropped pins opt out. Automatic caught-up
entries remain eligible. Deadlines persist as `reader:metadata-refresh:<SeriesId JSON>` in Nova KV, including bounded failure retry and manual-refresh cooldowns.
Fetch and publication are separate: source revocation, removal/status changes,
empty results and a newer foreground metadata baseline reject stale results.
Progress, history and disappeared chapters survive successful merges.

`Series.publication_status` now retains normalized addon publication state
(Unknown/Ongoing/Completed/OnHiatus/Cancelled), defaulting Unknown for old data.
Automatic Completed requires source Completed plus every known chapter read;
ongoing/unknown caught-up entries stay Reading. Explicit Completed overrides the
source/progress decision. It is independent of the user's `reading_status` pin.


Capture format indices append JPEG XL lossless/lossy at 5/6, preserving 0–4.
`CapturePreferences.jxl_quality` defaults to 85 for older `reader:captures` values,
persists independently of JPEG/WebP/AVIF and applies only to lossy JPEG XL. Shared
Kit encoding produces `.jxl` / `image/jxl` screencaps at either capture resolution;
Reader decode admission is checked before delivery. Original page exports keep
source bytes. Screenshot quality sits immediately after format. Cache settings
follow chapter cache → cache compression → artwork/memory → downloads; each
compression quality row directly follows its format, before optional transcode.

Artwork cache usage/clear/status are owned by `ImageStorageSettingsPage` beside
artwork disk/resolution/memory controls, before downloads. `MorePage` forwards
its existing artwork usage/status/busy properties and clear callback into that
page; chapter maintenance retains its independent callback and busy state.
`tests/capture_settings.rs::render_capture_and_cache_settings_previews` is an
opt-in software-rendered phone/desktop capture helper for reviewing these pages.
