# Implementation status

This tracks **Nova Reader — Implementation plan**, baseline 31 October 2026.
It describes code present in this repository, not completion of every gate in the
11-page plan. Verification outcomes belong to the current build/review report;
the presence of a test or workflow does not establish that it passed on a device.

| Plan phase | Implemented foundation | Outstanding acceptance/work |
| --- | --- | --- |
| 0 — Prove risky assumptions | Source API/permissions/errors documented; neutral dimensions and 320-page fixtures; bounded dimension-first decode and host source contracts | Live Manhwa catalogue/chapter/image/session smoke; agreed desktop/Android measurements and budgets; true oversized-image streaming/tiled decode |
| 1 — Bootstrap | Independent Reader IDs, Nova’s adaptive rail/bottom tabs, retained More/settings transitions, persisted Theme/Display/Screenshots/Look and feel/Cache pages, safe-area layout and shared Kit controls, pinned kit storage/UI, adapted flake/Makefile/CI/platform packaging | Linux/Android device launch and Windows execution acceptance; complete release artifact smoke and production signing setup |
| 2 — Share addon foundation | Reader-native v2 package/registry, deterministic packaging, bounded QuickJS/HTTP/HTML, typed profile settings/UI, reader DTOs and actual-host fixture tests | Generic kit extraction and shared package tooling; unchanged Nova v1 runtime/package consumer gate; publishing a kit revision before pin updates |
| 3 — Reference reading flow | YiiMadara/Manhwa18 parser and explicit language/NSFW capabilities; neutral install/browse/details/chapters/pages; library reveal search, optimistic details with retained synopsis/scroll, automatic source paging with explicit failure Retry, reading modes and persisted logical resume | Live source availability; device interaction/resume checks; independently published native source package in nova-addons |
| 4 — Usable alpha | Per-entry categories/bookmarks, durable adult visibility and per-series chapter ordering, dated History/Updates, compact chapter actions, source pins/last-used grouping, Sources/Extensions management, source filters and cover cache; manual updates; ordered checksummed bundles; remote chapter download deletion; queue pause/resume/cancel/retry, recovery and queued-job startup resume; local folder/CBZ/ZIP imports; validated metadata backup/restore | Target-platform process-kill/offline/restore acceptance, Android SAF device validation, selectable durable external download destination, configurable quota/retention and source concurrency policies |
| 5 — Discovery/distribution | Bounded gzip/protobuf catalogue adapter, live-derived exact upstream IDs, native port mapping, Ed25519 verification/trust API and sequence floors, discovery states | Published signed catalogue/ZIPs; trust/key-management UI and durable policy; automated download/hash-check/review/activation lifecycle and permission-change UX |
| 6 — Beta/v1 | Platform CI/release scaffolding and focused regression tests | Scheduled Android updates/download policy, background/reboot behavior, sync, external tracking, migration UX, device performance budgets, accessibility and full release rehearsal |

## Reading and offline behavior

The reader offers Webtoon, LTR, RTL and vertical paging, plus Automatic selection
using pinned TachiyomiJ2K genre/source rules. Manual choices persist per series
and override chapter progress; the global fallback remains separate. It loads the full
visible page window plus one prefetch page on each side. Obsolete image fetches
are cancelled; failed pages wait for an explicit Retry. It persists logical page
ID/index plus an intra-page fraction and flushes progress at lifecycle boundaries.
Source list edits resolve a saved page ID first, with a clamped index fallback.
Reflow does not define a new identity. Retained previews upgrade automatically when physical display width grows, including an undersized initial decode; no scroll is required. Reader controls start hidden and tap/click toggles them; Android status-bar visibility follows the OSD. Completion and prefetch are separate.

PNG, JPEG, WebP and 8-bit still AVIF originals are supported within explicit limits: default
32 MiB encoded input, width 16,384, height 65,536, 32 million pixels and 128 MiB
CPU allocation admission. Native decode output and RGBA conversion are admitted
together. Display resizing also reserves its output budget. These are rejection
and retention limits, not measured device budgets. The current decoder performs
a bounded full decode before display downscale; it cannot stream arbitrary tall
originals or claim that post-decode resizing reduces peak decode allocation.
Display output retains bounded visible-row windows at the requested physical
width, keeping small lettering at the available source resolution rather than
shrinking a whole tall page to meet a texture budget. Each window admits at most
32 MiB, reduced when multiple pages share the 32 MiB model budget,
and up to 2,048 neighboring rows on either side. Texture tiles contain at most
2,048 × 2,048 pixels. Physical width targets use 128px buckets up to 16,384px, without
upscaling beyond source width. Retained display/model pixels use 32 MiB,
with another 32 MiB reserved for image replacements inside a 64 MiB accounting
budget. The active page is prioritized over distant pages. Logical anchors do not
depend on window/tile boundaries. Shared physical-pixel drawing edges prevent
independent rounding gaps across fractional scroll and device scales, while
logical positions remain unrounded.

The CPU decoder still fully decodes each admitted original before extracting a
window, and window changes may repeat that decode. Display windowing does not
remove this transient allocation, measure driver overhead or establish a total
process-memory bound. A separate 32 MiB worker cache retains encoded page bytes
for the current generation; a cache miss may temporarily hold another 32 MiB
response. Actual GPU residency/frame times and total process memory
still need target-device measurement. Some oversized images deliberately fail
rather than exhausting memory.

Right-click and stationary hold open original-image and viewport-screencap
save/clipboard actions. Capture format, quality and series-folder preferences
follow Nova, including an AVIF choice with its own quality preference. The
pure-Rust AVIF path accepts 8-bit still images and rejects animation, multiple
frames, 10/12-bit samples and rotation/mirror/crop transforms. Its admission
checks include padded codec working frames, RGBA output and encoded copies;
these conservative checks do not measure total process memory. Android 29+
saves use MediaStore; older Android uses the document picker. Screencaps exclude the menu/OSD and are bounded to eight million pixels.
Device resolution is the default; Chapter resolution composites the visible
viewport from original images at the clicked page's native pixel scale, with
frozen scroll/pan and page geometry. Both save and clipboard honor this setting.

Chapter and detail artwork extend beneath system-bar/cutout areas. Reader OSD
controls keep independent safe insets; normal pages restore Nova's transparent
status-bar layout and detail scrim. Host tests cover layout and platform calls;
physical Android rendering remains a device acceptance check.

Chapter downloads use verified individual pages and atomic completion manifests.
The store recovers interrupted jobs, retries missing/corrupt pages and refuses
false completion. Remote chapter downloads can be deleted from chapter actions
or Downloads without removing library metadata, progress or other chapters;
local imports remain protected. Status hints refresh off the UI thread without
hashing page files. Optional JPEG/WebP/AVIF compression applies only to newly fetched
pages, preserves dimensions and keeps original bytes when the chosen codec
cannot preserve the image. AVIF and WebP preserve transparency; JPEG conversion
keeps transparent originals. Existing verified pages remain unchanged.
Queued jobs automatically resume on app startup; paused and
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

## Collection and detail behavior

Library search reveals/focuses Nova’s search field and filters on edits. Each
library entry has a long-press/right-click action menu; detail toolbar actions
share the same full series identity, including category assignment. Details
show selected metadata before chapter requests finish, retain their scroll and
expanded synopsis across reader return, and keep Continue disabled without
chapters. Source galleries append near the viewport end, deduplicate series
identities and pause failed or non-advancing pages until explicit Retry.
Adult visibility and chapter order persist through fallible Nova KV writes
outside portable backups; order keys include the full source-local series ID.

Poster reuse is shared across Browse, Library, Updates, History and details:
one 128 MiB decoded-pixel LRU backs UI handles, with visible/selected artwork
plus up to 48 additional recent UI cards. Source availability invalidates cache
hits on disable/remove/update. The existing 8 MiB encoded, 8-million-pixel/72 MiB
source decode and 32 MiB/256-file disk limits remain; active decode allocations
are separate from retained pixels. These are bounds, not device measurements.

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
