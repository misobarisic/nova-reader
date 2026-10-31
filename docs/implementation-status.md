# Implementation status

This tracks **Nova Reader — Implementation plan**, baseline 31 October 2026.
It describes code present in this repository, not completion of every gate in the
11-page plan. Verification outcomes belong to the current build/review report;
the presence of a test or workflow does not establish that it passed on a device.

| Plan phase | Implemented foundation | Outstanding acceptance/work |
| --- | --- | --- |
| 0 — Prove risky assumptions | Source API/permissions/errors documented; neutral dimensions and 320-page fixtures; bounded dimension-first decode and host source contracts | Live Manhwa catalogue/chapter/image/session smoke; agreed desktop/Android measurements and budgets; true oversized-image streaming/tiled decode |
| 1 — Bootstrap | Independent Reader IDs, Nova’s adaptive rail/bottom tabs and More/settings cards, safe-area layout and shared Kit controls, pinned kit storage/UI, adapted flake/Makefile/CI/platform packaging | Linux/Android device launch and Windows execution acceptance; complete release artifact smoke and production signing setup |
| 2 — Share addon foundation | Reader-native v2 package/registry, deterministic packaging, bounded QuickJS/HTTP/HTML, typed profile settings/UI, reader DTOs and actual-host fixture tests | Generic kit extraction and shared package tooling; unchanged Nova v1 runtime/package consumer gate; publishing a kit revision before pin updates |
| 3 — Reference reading flow | YiiMadara/Manhwa18 parser and explicit language/NSFW capabilities; neutral install/browse/details/chapters/pages; library, reading modes and persisted logical resume | Live source availability; device interaction/resume checks; independently published native source package in nova-addons |
| 4 — Usable alpha | Categories/bookmarks/history models and UI actions; manual updates; ordered checksummed bundles; queue pause/resume/cancel/retry, recovery and queued-job startup resume; local folder/CBZ/ZIP imports; validated metadata backup/restore | Target-platform process-kill/offline/restore acceptance, Android SAF device validation, selectable durable external download destination, configurable quota/retention and source concurrency policies |
| 5 — Discovery/distribution | Bounded gzip/protobuf catalogue adapter, live-derived exact upstream IDs, native port mapping, Ed25519 verification/trust API and sequence floors, discovery states | Published signed catalogue/ZIPs; trust/key-management UI and durable policy; automated download/hash-check/review/activation lifecycle and permission-change UX |
| 6 — Beta/v1 | Platform CI/release scaffolding and focused regression tests | Scheduled Android updates/download policy, background/reboot behavior, sync, external tracking, migration UX, device performance budgets, accessibility and full release rehearsal |

## Reading and offline behavior

The reader offers Webtoon, LTR, RTL and vertical paging. It loads the full
visible page window plus one prefetch page on each side. Obsolete image fetches
are cancelled; failed pages wait for an explicit Retry. It persists logical page
ID/index plus an intra-page fraction and flushes progress at lifecycle boundaries.
Source list edits resolve a saved page ID first, with a clamped index fallback.
Reflow does not define a new identity. Completion and prefetch are separate.

PNG, JPEG and WebP originals are supported within explicit limits: default
32 MiB encoded input, width 16,384, height 65,536, 32 million pixels and 128 MiB
CPU allocation admission. Native decode output and RGBA conversion are admitted
together. Display resizing also reserves its output budget. These are rejection
and retention limits, not measured device budgets. The current decoder performs
a bounded full decode before display downscale; it cannot stream arbitrary tall
originals or claim that post-decode resizing reduces peak decode allocation.
Display images are fitted to reading width and split into textures of at most
2,048 rows so tall-page lettering is not reduced to a single short texture.
Logical anchors remain independent of these tiles. Retained decoded/display
pages have an enforced 64 MiB accounting budget, with the active page prioritized
over distant retained pages. This does not measure driver overhead or remove the
bounded full CPU decode. Actual GPU residency/frame times and total process
memory still need target-device measurement. Some oversized images deliberately fail rather than exhausting
memory.

Chapter downloads use verified individual pages and atomic completion manifests.
The store recovers interrupted jobs, retries missing/corrupt pages and refuses
false completion. Queued jobs automatically resume on app startup; paused and
cancelled jobs do not. The application serializes transfers, while the store
retains a two-active-jobs-per-source cap. Unix stores use private 0700 directories
and 0600 files, repairing existing store permissions on open. Completed bundles open without a source installed. Changing
or expiring page resources can be refreshed before retry. The service currently
uses app-private storage and an 8 GiB quota; user-selected destination/retention
policy is not a completed cross-platform feature.

Android document bridge code is present for source ZIP and CBZ/ZIP import, image
folder selection, metadata backup import/export and bounded staging into private
storage. Provider behavior, lifecycle cancellation and actual device permissions
still need device checks. This does not establish a persistent external download
destination or background download service.

The metadata JSON backup restores library, categories, reading preferences,
progress, history, bookmarks and source descriptors. It omits downloaded/imported
chapter bytes, executable packages and sessions. Restoring with missing sources
retains records; it does not reinstall code or recover omitted chapter files.

## Source and distribution limits

The reference Manhwa18 package preserves pinned upstream routes, raw-language
policy, apostrophe normalization, filter precedence and document order. English,
Korean and all-language variants are explicit; adult sources require the Browse
visibility setting. Synthetic parser and actual-host tests do not prove live
support. Browser challenge resolution/session transfer remains unavailable.

Source Settings exposes validated boolean, text, select and secret fields from
the addon schema. Values are scoped to source/profile and persisted separately
from metadata backups, using Unix 0700 directories and 0600 files. Secrets are
masked in UI; permissions are access controls, not encryption at rest. No claim
is made that every source needs or declares settings.

The native package validator atomically preserves a previous valid installation
on validation failure, keeps immutable version hashes and invalidates stale source
handles. The app explicitly enables a new user-selected ZIP; permission-expanding
updates remain disabled until re-enabled. Native catalogue verification is a
library API. It does not itself install archives, create trust or run automatic
updates. Keiyoushi APK/JAR catalogue entries are discovery records, not executable
Nova sources.

## Verification entry points

The user-requested `gpt-6-astra` review at medium reasoning found no residual
demonstrated P1/P2 issue after fixes; see [review record](../REVIEW.md). Static
review is recorded separately from execution. Local workspace checks, Clippy,
tests (including geometry/tiles and the process-boundary journey), addon/parser
and packaging regressions passed. A Linux desktop Xvfb smoke also covered
browse, details, library add, tall rendering and scroll persistence. See the
[verification record](../REVIEW.md) for scope and remaining platform checks.

Run focused checks before broad workspace checks:

```sh
node examples/addons/neutral-fixture/tests/fixture.test.mjs
npm ci --ignore-scripts --prefix examples/addons/manhwa18cc/tests
npm test --prefix examples/addons/manhwa18cc/tests
python3 -B -m unittest discover -s .github/scripts -p 'test_package_*.py'
cargo test --locked -p reader-sources
cargo test --locked -p reader-domain -p reader-engine -p reader-downloads -p reader-catalogue
cargo test --locked --test reading_journey
cargo test --locked --test ui_navigation
cargo fmt --all -- --check
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```

The journey test has real child-process boundaries and an intentional download
interruption, then checks durable position, recovered pages, source removal and
offline decode. It shares application service paths with the UI. It is an
automated service journey, not a device touch/keyboard or rendering benchmark.
CI also defines Windows GNU and Android ARM64 compile checks. A configured check
is not evidence of a successful run; release installation, execution and signing
acceptance remain separate gates. See [packaging](packaging.md).
