# Project structure

Nova Reader is a separate Rust 2024 workspace. UI orchestration is app-owned;
heavy reader subsystems live in leaf crates. It consumes pinned nova-kit storage
and UI utilities without introducing reader-domain dependencies into kit.

| Location | Responsibility |
| --- | --- |
| `build.rs` | Final Android cdylib 16 KiB ELF alignment, preserved when cargo-apk2 overrides Cargo rustflags |
| `src/main.rs`, `src/lib.rs` | Desktop entry, Android entry/lifecycle, CLI routing |
| `src/app.rs` | Worker orchestration, navigation, source/settings selection, visible-page loading, display tiles, lifecycle progress flush |
| `src/services.rs` | Shared UI/test services: install, validated source/date conversion, library, downloads and local import |
| `src/cli.rs`, `src/paths.rs` | Commands, bounded file input, independent data and platform cache directories |
| `src/android_documents.rs`, `android/java/dev/misob/nova/reader/ReaderActivity.java` | Application-context JNI, current-activity SAF and reader status-bar visibility/lifecycle on Java main |
| `src/artwork.rs` | Bounded cover worker, source-checked fetching, disk/CPU/UI caches, viewport cancellation and serialized cache maintenance |
| `src/reader_export.rs` | Original-image validation, screencap encoding, bounded platform save/clipboard delivery and persistent desktop clipboard ownership; `src/app.rs` captures the clean pre-menu viewport with an 8-million-pixel limit and fetches source page bytes |
| `src/capture_settings.rs`, `crates/ui/capture-settings.slint` | Validated device capture preferences and retained More → Settings → Screenshots controls for format, quality and per-series folders |
| `src/image_storage.rs`, `crates/ui/image-storage-settings.slint` | Independent preferences for compression of newly persisted cache/download images; Nova format/quality controls live in Cache settings |
| `src/timeline.rs`, `src/source_filters.rs`, `src/source_browser.rs` | Local date groups/provider date parsing, bounded typed filter values and persistent source pins/last-used grouping |
| `src/updates.rs` | Optional reader application release discovery |
| `src/appearance.rs` | Device appearance/motion preferences, validated edits, immediate application and KV persistence |
| `crates/ui/app.slint` | Shared pages/reader models, safe-area layout, platform/width shell selection, detail origin and translated text |
| `crates/ui/media-models.slint` | Shared series/chapter/timeline/source/extension/filter row models |
| `crates/ui/library-cards.slint`, `crates/ui/display-settings.slint` | Nova poster/list painting, viewport admission, grid/list switch and persisted display choices |
| `crates/ui/detail-hero.slint`, `crates/ui/title-artwork.slint`, `crates/ui/chapter-row.slint` | Nova detail artwork/title/metadata/actions plus compact chapter rows and accessible action popup |
| `crates/ui/timeline.slint`, `crates/ui/source-list.slint`, `crates/ui/extensions.slint`, `crates/ui/source-filters.slint` | Dated History/Updates, grouped sources, extension management and actual source-defined browse filters |
| `crates/ui/reader-context-menu.slint` | Custom four-action image/screencap menu, stationary-hold/right-click handling, viewport clamping and keyboard navigation; `app.slint` owns capture/action/close callbacks and reader export feedback |
| `crates/ui/navigation.slint` | Nova’s 64px icon rail, flat five-tab bottom bar, icon feedback and keyboard navigation |
| `crates/ui/controls.slint`, `crates/ui/page-header.slint` | Thin Nova Kit control adapters and Nova’s PageHeader; shared palette, typography and accessibility |
| `crates/ui/more.slint` | Retained Nova-style More/Settings landings, nested slide/fade layers, scroll/focus restoration and local import/categories/data/About pages |
| `crates/ui/appearance.slint`, `crates/ui/theme-settings.slint`, `crates/ui/settings-row.slint`, `crates/ui/src/appearance.rs` | Nova theme/motion settings controls, responsive setting rows and adapted Nova palette derivation |
| `crates/ui/build.rs` | Kit source aliases, embedded resources, debug layout metadata and narrow Kit dropdown/segmented keyboard/accessibility adaptations |
| `assets/fonts` | Nova’s bundled Roboto faces and Apache notices; More/settings artwork comes directly from Kit |
| `assets/icons/navigation` | Embedded SVG navigation and reader controls, independent of system font glyph coverage |
| `crates/reader-domain` | Source-local identities, schema-1 state, library/categories, progress/history/bookmarks, backup/restore |
| `crates/reader-sources` | Reader v2 ZIP validation, durable registry, typed profile settings, QuickJS host, HTML/URL capabilities and checked source HTTP |
| `crates/reader-engine` | Dimension probing, bounded PNG/JPEG/WebP decode, display downscale, geometry, resume anchors and decoded cache utility |
| `crates/reader-downloads` | Durable queue, checksummed page bundles, recovery, quota, source-free offline lookup, local folder/CBZ import |
| `crates/reader-catalogue` | Bounded Keiyoushi protobuf discovery, exact port mappings, signed native catalogue verification and availability states |
| `examples/addons/manhwa18cc` | Thin Manhwa18 configuration, reusable YiiMadara helper, Apache license/provenance and synthetic parser fixtures |
| `examples/addons/neutral-fixture` | Installable network-free fixture, bundled geometric PNGs and 320-page chapter |
| `tests/reading_journey.rs` | Process-boundary install/read/progress/interrupted download/recovery/source-removal/offline journey |
| `tests/ui_navigation.rs`, `tests/mobile_layout.rs` | Headless reader models, phone/landscape/tablet/desktop control bounds, tab taps, accessible activation and back navigation |
| `tests/appearance.rs` | Process-boundary appearance persistence, palette application, invalid edits and theme reset preserving motion preferences |
| `tests/reader_design.rs` | Responsive poster/list/timeline/chapter layouts, source/pin/extension actions, retained catalogue scroll, tap-versus-pan reader OSD and image-menu targeting, hold cancellation, clamping, keyboard/busy/lifecycle behavior |
| `tests/capture_settings.rs`, `tests/image_storage.rs` | Process-boundary capture/compression persistence, corruption recovery, independent policies and accessible settings controls |
| `tests/image_download_encoding.rs` | Production Services partial-resume encoding: verified pages remain intact, new pages follow compression policy, completed bundles survive policy changes, disabled future writes retain source bytes and offline hashes survive addon removal |
| `tools/test-android-documents.py`, `tests/android-documents` | Production Java/JNI host regression with checked JNI and deterministic main-queue/lifecycle fixtures |
| `android/java/dev/misob/nova/reader/ReaderImageProvider.java`, `tools/test-android-provider-manifest.py` | Android image clipboard URI provider and packaged provider-manifest regression |
| `tools/package-addon.py` | Deterministic manifest-declared reader addon ZIP builder |
| `.github/scripts`, `.github/workflows` | Nova's copied platform packagers and PR/main-check versus tag-build split, required Reader identity/license/signing integration |
| `.github/scripts/test_package_nova_parity.py` | Immutable Nova-source parity for all three packaging scripts, masking only documented app metadata/media-license differences |
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
Screenshots format/quality/folder preferences live separately under
`reader:captures`; changing them preserves original chapter data and metadata.
`reader:image-storage` stores independent cache/download compression switches,
formats and quality for new writes. Preference changes do not rewrite existing
cache files or downloaded chapter bundles.

Fetched series posters belong to `cache_dir()/posters`, never the data directory.
Desktop uses the platform cache base plus `nova-reader`; Android uses
`Context.getCacheDir()`. `NOVA_READER_CACHE_DIR` overrides the cache root for tests
or deliberate configuration, independently of `NOVA_READER_DATA_DIR`. Disk files
use hashes of source/series/URL identity. The worker admits 32 requests, rejects
encoded covers above 8 MiB, limits source decode to 8 million pixels/72 MiB,
and retains 12 MiB CPU, 32 MiB disk/256 files and 32 MiB shared UI artwork.
The transport's existing 32 MiB response cap precedes the artwork admission cap.
Viewport changes cancel obsolete work; cache clear runs on the worker and clears
only owned poster files and CPU/UI artwork. Downloads/imports remain data.

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
event loop updates UI models. Navigation work uses cancellation/generation checks.
The reader covers the complete visible page window plus one prefetch page on
each side, cancels obsolete fetches and leaves failed pages waiting for Retry.
Retained decoded/display pages share a 64 MiB accounting budget. Width-fitted
display images become textures with at most 2,048 rows; original CPU decoding is
still bounded full-image decoding. Prefetch does not mark a chapter read. Resume
stores logical page ID/index and intra-page offset, independent of display tiles
and reflow. Retained pages upgrade at a larger physical-pixel width without discarding their preview or logical anchor; decode targets use 128px buckets capped at 2,048px. Requested quality is tracked separately from source texture width to avoid repeated decoding of narrow originals. Device GPU overhead and peak total process memory remain unmeasured.

The reader source runtime currently belongs to this repository. Moving its
generic envelope/runtime into nova-kit requires shared API design and Nova v1
consumer regressions; this workspace does not claim that extraction is complete.
Source examples are intended for independent future publication in nova-addons.

## Where to look

- Nova design reuse and destination behavior: [UI design](ui-design.md).
- Cover cache placement/maintenance: `src/paths.rs`, `src/artwork.rs`, More → Settings → Cache.
- Series/chapter layouts, timeline/source groups and reader controls: `crates/ui/library-cards.slint`, `crates/ui/detail-hero.slint`, `crates/ui/timeline.slint`, `crates/ui/source-list.slint`, `tests/reader_design.rs`.
- Reader image/screencap actions and export settings: `crates/ui/reader-context-menu.slint`, `src/reader_export.rs`, `src/capture_settings.rs`, More → Settings → Screenshots.
- New cache/download image compression: `src/image_storage.rs`, `crates/ui/image-storage-settings.slint`, More → Settings → Cache.
- Appearance controls, palette and device persistence: `crates/ui/theme-settings.slint`, `crates/ui/src/appearance.rs`, `src/appearance.rs`.
- Android picker context, callbacks and host JVM regression: [Android documents](android-documents.md).
- Android/desktop shells and touch layout checks: `crates/ui/navigation.slint`, `crates/ui/app.slint`, `tests/mobile_layout.rs`.
- Static review scope and outcome: [review record](../REVIEW.md).
- Run/build/install and CLI: [README](../README.md), [packaging](packaging.md).
- Source manifest, HTTP permissions, limits and error contract: [source API](source-api.md).
- Manhwa routes, languages, parser tests and upstream license: [reference source](manhwa18.md).
- Discovery, actual upstream IDs and signing trust API: [catalogues](catalogues.md).
- Implemented behavior versus outstanding plan gates: [implementation status](implementation-status.md).
