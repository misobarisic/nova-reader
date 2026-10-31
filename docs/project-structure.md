# Project structure

Nova Reader is a separate Rust 2024 workspace. UI orchestration is app-owned;
heavy reader subsystems live in leaf crates. It consumes pinned nova-kit storage
and UI utilities without introducing reader-domain dependencies into kit.

| Location | Responsibility |
| --- | --- |
| `src/main.rs`, `src/lib.rs` | Desktop entry, Android entry/lifecycle, CLI routing |
| `src/app.rs` | Worker orchestration, navigation, source/settings selection, visible-page loading, display tiles, lifecycle progress flush |
| `src/services.rs` | Shared UI/test services: install, metadata conversion, library, downloads and local import |
| `src/cli.rs`, `src/paths.rs` | Commands, bounded file input, independent application directory |
| `src/android_documents.rs`, `android/java/dev/misob/nova/reader/ReaderActivity.java` | Android SAF document/tree import, staged backup export and JNI callbacks |
| `src/updates.rs` | Optional reader application release discovery |
| `crates/ui` | Slint application window, responsive navigation, reader/page models, application version binding and translated text |
| `crates/reader-domain` | Source-local identities, schema-1 state, library/categories, progress/history/bookmarks, backup/restore |
| `crates/reader-sources` | Reader v2 ZIP validation, durable registry, typed profile settings, QuickJS host, HTML/URL capabilities and checked source HTTP |
| `crates/reader-engine` | Dimension probing, bounded PNG/JPEG/WebP decode, display downscale, geometry, resume anchors and decoded cache utility |
| `crates/reader-downloads` | Durable queue, checksummed page bundles, recovery, quota, source-free offline lookup, local folder/CBZ import |
| `crates/reader-catalogue` | Bounded Keiyoushi protobuf discovery, exact port mappings, signed native catalogue verification and availability states |
| `examples/addons/manhwa18cc` | Thin Manhwa18 configuration, reusable YiiMadara helper, Apache license/provenance and synthetic parser fixtures |
| `examples/addons/neutral-fixture` | Installable network-free fixture, bundled geometric PNGs and 320-page chapter |
| `tests/reading_journey.rs` | Process-boundary install/read/progress/interrupted download/recovery/source-removal/offline journey |
| `tests/ui_navigation.rs` | Headless Slint navigation and reader model checks |
| `tools/package-addon.py` | Deterministic manifest-declared reader addon ZIP builder |
| `.github/scripts`, `.github/workflows` | Nova's parallel Android build/assembly, compiler-closure Windows staging, Linux packaging, Rust caches, license inventory and Nova’s PR/main-check versus tag-build split |
| `android` | Android resources, platform document bridge and intentionally public local debug signing key |

## State and work boundaries

`DomainStore` stores validated JSON under `reader:state` through `nova-storage`
in the Reader application directory. The internal database filename remains
`nova.redb`; the application directory isolates it from Nova. One directory is
supported per process. Restore validates the replacement before publishing it.
The native metadata backup excludes executable addons, cookies and chapter bytes.

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
and reflow. Device GPU overhead and peak total process memory remain unmeasured.

The reader source runtime currently belongs to this repository. Moving its
generic envelope/runtime into nova-kit requires shared API design and Nova v1
consumer regressions; this workspace does not claim that extraction is complete.
Source examples are intended for independent future publication in nova-addons.

## Where to look

- Static review scope and outcome: [review record](../REVIEW.md).
- Run/build/install and CLI: [README](../README.md), [packaging](packaging.md).
- Source manifest, HTTP permissions, limits and error contract: [source API](source-api.md).
- Manhwa routes, languages, parser tests and upstream license: [reference source](manhwa18.md).
- Discovery, actual upstream IDs and signing trust API: [catalogues](catalogues.md).
- Implemented behavior versus outstanding plan gates: [implementation status](implementation-status.md).
