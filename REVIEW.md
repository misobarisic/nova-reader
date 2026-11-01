# Review record

The user requested an independent review with `gpt-6-astra` at medium reasoning.
That static review and follow-up inspection reported **no residual demonstrated
P1/P2 issue after fixes**. This is a scoped review result, not certification that
all plan gates are complete or that every platform works.

The resulting fixes include complete visible-window image loading with bounded
prefetch, cancellation of obsolete fetches, explicit retry for failed pages,
64 MiB retained-page accounting, and width-fitted tall-image display tiles capped
at 2,048 rows. Logical resume anchors remain independent of tiles. Download
handling now preserves private Unix file permissions, repairs existing stores,
resumes queued jobs on startup, leaves paused/cancelled jobs stopped, and
serializes application transfers. Source settings have typed validation,
profile-scoped private persistence and UI controls, including masked secret fields.
Android document operations are wired through the activity/JNI bridge.

The decoder still performs bounded full CPU decoding before display resizing and
tiling. Arbitrary oversized stream/tile decoding, measured device memory/frame
budgets, live Manhwa18 operation, platform launch/install acceptance, shared-kit
runtime extraction/Nova compatibility and independently published signed source
distribution remain outside the established acceptance result. See
[implementation status](docs/implementation-status.md).

Final local verification on 31 October 2026 passed:

- Workspace formatting, Clippy for all targets with warnings denied, and desktop workspace check.
- Workspace unit/integration/doc tests, including logical resize/tile geometry and the real-process interrupted-download/offline journey.
- Desktop compilation with update checking omitted.
- Both addon JavaScript fixture suites, deterministic ZIP packaging checks, and 25 Python packaging regressions.
- Linux desktop launch and browse/details/library/tall-page rendering/scroll persistence smoke under Xvfb with software OpenGL.

The first local test link required repairing broken fontconfig/freetype shared-library symlinks in the execution environment; no repository workaround was added.
Android Java/JNI checks passed separately during bridge implementation. Nix evaluation, Windows/Android cross-builds, real devices and complete release artifacts remain unverified locally. Static review and Linux smoke do not substitute for those gates.

## Release setup alignment for 0.1.1

The workflow and packaging recipes now follow Nova's job layout, three parallel
Android ABI builds and separate assembly, Rust caches, runner cleanup, named
steps and original Windows ZIP transfer. Reader identities and dependencies
remain distinct, and releases still require checks on the exact tagged commit.
Cargo-apk2 receives only supported arguments after a locked Cargo fetch; Windows
uses Nova's compiler-closure DLL staging instead of a custom OS-library list.
Configured signing overrides retain their identity, with Nova's Cargo-profile
fallback available. Packaging regressions include the actual Make command line
and repeated read-only DLL staging. The application version is taken from Cargo.

The old Linux release log also exposed an early-closing Debian contents pipe.
The check now consumes the full listing; a real large `.deb` regression passes.
Final workspace formatting, Clippy and tests passed, along with 27 packaging
regressions. Astra 6 medium reviewed the workflow alignment with no P1/P2 blocker.

## Nova tag behavior restored for 0.1.2

The user explicitly requires Nova's workflow behavior. Version 0.1.2 copies
Nova's YAML and preserves its conditions and dependency graph exactly: checks
run only on PR/main; tag builds start directly and publication waits only for
the platform artifacts. The earlier exact-tag check gates are removed.
Only reader identities, absent video subsystems, license files and existing
signing overrides require adaptation. A packaging regression guards this split.

## Android and desktop layouts for 0.1.5

Reader now has separate desktop sidebar and mobile bottom-navigation components.
Android selects the mobile shell at every width; narrow desktop windows can also
use it. Both respect Window system-bar/cutout insets. Shared pages scroll their
headers, filters and content together; mobile filters stack while desktop filters
sit side by side. Actions and fields have 48px touch targets, and chapter/queue
actions occupy their own rows. Reader options expand separately from chapter/page
navigation and scroll in short windows. Back navigation retains the entry section
for details, including chapters opened directly from History/Updates/Downloads.

Astra 6 at medium reviewed this change. Its findings about accessible default
actions, direct-chapter back origins, options-panel sizing and scroll reset were
fixed. Regression tests cover actual pointer/default-action activation, disabled
actions, focused Back handling, section scroll reset versus same-page loading,
phone/landscape/tablet layouts and desktop selection. Formatting, workspace check,
Clippy for all targets with warnings denied, and all workspace tests passed.
Linux Xvfb visual checks covered 320px/390px phones, 844px landscape reader options
and the 1120px desktop sidebar. These exercise the mobile Slint layout on Linux;
Android system insets, software keyboard and physical-device interaction still
require device validation. No local Android SDK/NDK/device was available.

## Nova UI reuse for 0.1.6

The reference remains Nova b69a1e5 and the existing pinned Nova Kit revision.
Reader now adapts Nova's 64px icon rail, accent selection circle, flat four-slot
bottom bar, icon feedback and 700px responsive breakpoint. History/Downloads
use desktop shortcut slots and phone More cards. Details/reader cover navigation;
phone child pages hide the bottom bar and wide More children retain the rail.
The More landing, card rows, typography, shared palette, retained scroll and
push/pop behavior follow Nova. Settings/local import/categories/data/About reuse
Kit SettingsCard/SettingsLink, PageHeader, inputs and actions. Nova's prepared
Roboto faces are embedded with their full Apache license in packaged inventories.
See docs/ui-design.md for component/source mapping and the narrow build-time
adaptation of Kit's TV-gated dropdown keyboard/accessibility API.

Astra 6 medium reviewed this change. The dropdown keyboard/accessibility gap and
More scroll/focus restoration findings were fixed. Focused tests verify geometry
at both sides of the breakpoint, actual card/tab activation, child Back behavior,
More scroll preservation, keyboard navigation after pop, dropdown keyboard and
accessible selection, and touch dragging to the twentieth dropdown entry.
Linux Xvfb captures were inspected for phone/desktop More, phone Settings,
320px Browse, desktop Library and wide landscape Library. Physical Android
system-bar/IME/device interaction remains unverified locally; no SDK/NDK/device
is available in this workspace. The release workflow YAML is unchanged.

Final formatting, workspace check, Clippy for all targets with warnings denied,
all workspace tests, and 27 packaging regressions passed. Astra's final review
found no remaining demonstrated P1/P2 issue.

## History root tab for 0.1.7

The user-requested bottom-tab order is Library, Updates, History, Browse, More.
History now keeps bottom navigation visible and Back returns to Library. The
rail uses the same root order, with Downloads before More. The redundant phone
More card is removed, and keyboard focus/selection indices follow the new order.
Nova's theme, shared History glyph and navigation feedback remain in use.
The existing layout regression verifies five ordered tabs at phone widths,
History activation/selection/Back, and More child navigation and scroll state.
Astra 6 medium found no actionable regression; focused layout/navigation tests
passed.
Formatting, workspace check, Clippy for all targets with warnings denied and the
full workspace test suite also passed for the final change.

## Android ELF alignment fix for 0.1.8

The v0.1.5 assembly job (run 41200379600) failed because
`lib/arm64-v8a/libnova_reader.so` had four PT_LOAD segments aligned to 4096 bytes.
The actual uploaded APK was downloaded and inspected; this was its only native
library. The pinned cargo-apk2 `ndk-build2/src/cargo.rs` constructs
CARGO_ENCODED_RUSTFLAGS from environment flags and supplies that to child Cargo,
overriding Reader's configured target rustflags. Versions 0.1.6/0.1.7 retained
that same build configuration.

Root build.rs now emits the 16 KiB maximum-page-size argument on the final
Android cdylib link for every ABI. Host builds emit no link directive. The
packager's ELF, ZIP alignment, payload and signing checks remain intact; the
workflow YAML and signing identity are unchanged. A regression uses real host
Cargo/ELF linking under encoded rustflags, shows the original config is ignored,
and verifies the Android build directive restores compatibility.

Formatting, workspace check, Clippy for all targets with warnings denied, the
full workspace tests, and all 28 packaging regressions passed locally. Astra 6
medium found no review blockers and independently ran the Android packaging
suite successfully. Host ELF regression is separate from actual Android APK
assembly and device acceptance; those results belong to the new release run.

## Nova packaging parity restored for 0.1.9

The prior claim of copying Nova's release setup was too broad: the YAML job graph
matched, but Reader-only release gates remained in the invoked packagers. The
ELF gate absent from Nova was the gate that failed v0.1.5. All three platform
packagers are now re-copied from Nova b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda,
with only Reader identities/artifacts, its existing Android version-code policy,
absent media inputs and required license inventory integration adapted.
Nova's merge, payload, ABI, ZIP alignment, manifest and signer checks remain.
Reader-only video/ELF release checks, Linux ldd/contents/extraction preflights and
the Windows replacement bundle assertion are removed. Make's build/run recipes
also follow Nova; the extra preflight fetch is removed. Existing private signing
overrides are unchanged. No check dependency or new tag check is added.

The Android build alignment fix remains. Its real-ELF test helper is now confined
to the development test suite instead of being part of release assembly. Parity
tests compare all three scripts to canonical digests derived from the immutable
Nova source, masking only the documented necessary app adaptations. Astra 6
medium independently compared those digests to the actual git-show reference
and found no P1/P2 blocker. All 28 packaging tests, shell syntax, formatting,
Clippy for all targets, workspace check and all workspace tests passed locally.
Actual release assembly remains a separate CI result.

## Android document picker bridge fix for 0.1.10

The screenshot's generic Java exception exposed an incorrect context assumption.
android-activity 0.6.1 init.rs obtains get_application() and initializes
ndk-context with an Application global reference. Reader passed that object to
requestDocument as an Activity. The bridge now uses Application only for class
loading/cache access, and Java resolves the current weak ReaderActivity on the
main queue, following Nova's foreground-activity pattern. The live reference is
published before NativeActivity startup and an old instance cannot clear a newer
instance during recreation.

The Rust bridge explicitly registers its result method on the app class and
reports Throwable.toString while clearing pending exceptions and deleting local
refs, so failed launches can be retried. The launch signature has no Activity
parameter. Picker/copy limits and the release workflow are unchanged.

Production Java compiled against Android API 35. The committed host regression
runs production Java and Rust JNI in a real JVM with -Xcheck:jni, using a small
framework fixture and a foreign Java thread for queued callbacks. It passed all
four imports, backup export launch, cancellation, activity replacement before
launch, absent activity, exception cause/cleanup/retry, and the existing busy/
single-use/stale-result test. The JNI harness also passed Clippy with warnings
denied. Workspace formatting, all-target Clippy, workspace check/tests and all
28 packaging regressions passed. Astra 6 medium found no P1/P2 issue. These are
bridge and compilation checks, not physical-device/document-provider acceptance.

## Theme and More/settings motion for 0.1.11

Astra 6 at medium investigated the differences against Nova b69a1e5. Reader
had no theme preference page/backend, and one shared child boolean kept its
animation targets unchanged on Settings → Categories/Data and Back. Shared
child scrolling also reset the Settings landing. Pure More/settings routes
additionally deserialized the complete library before publishing navigation.

Theme and Look and feel now adapt Nova's modules, Kit controls and original
palette derivation. Device preferences persist under `reader:appearance`,
independently of portable library metadata. Retained nested layers, deferred
entry and scroll/focus restoration use Nova's transition pattern and timings;
More/settings route changes publish without rebuilding library models.
Transparent navigation uses Nova's overlay/fade and scroll end-space pattern.

Astra's follow-up identified lost toggle bindings, missing accessible actions,
unfinished animations reviving after master-off, and malformed cosmetic JSON
blocking startup. Two-way draft/rollback state, narrowly adapted Kit segmented
actions, explicit animation cancellation/endpoints and recoverable defaults
address these. Astra's re-review reported no residual P1/P2 issue; it did not
independently execute tests. The rapid motion regression publishes the off
event before re-enabling one millisecond later, within the old transition.

Coverage includes actual pointer/accessibility activation and retained switch
reset, separate-process restart/palette/reset behavior, corrupt preference
recovery, intermediate nested motion, interrupted Back, rapid motion toggles,
retained More/Settings offsets and responsive layouts. Xvfb renders of Theme,
Settings and More were inspected at phone/desktop sizes. Android frame times
and touch/system-inset behavior still require physical-device verification.
Release workflow and platform packaging scripts are unchanged.

Final local formatting, all-target Clippy with warnings denied, workspace check
and workspace tests passed. The packaging suite passed with 28 tests reported
and one platform-specific skip. These checks precede the release tag; tag builds
retain Nova's existing compile/package-only workflow.

## Collection, chapter and cache redesign for 0.1.12

The user requested Nova's library cards/display and detail header, plus the
supplied Mihon chapter, History, Updates and Browse structures. Reader now
adapts Nova's poster/list geometry, progress/captions, responsive sizing and
artwork/title/synopsis/action presentation. Source rows group last-used, pinned
and language variants; Extensions manages packages. Provider-defined filters
keep labels separate from actual request values. Detail catalogue Back retains
source ownership, query/filter/pagination and scroll; a new source invalidates
old requests and snapshots before any unavailable-source early return.

Chapter OSD starts hidden and overlays the unchanged viewport on tap/click;
dragging remains scrolling. Android status-bar visibility follows the OSD,
including focus/recreation and API 29/35 paths. The first-image blur investigation
found that a small initial decode was permanently considered retained. Requested
physical-pixel quality is now tracked separately, stale previews upgrade without
scrolling in all modes, and replacement accounting excludes the previous image.
The detail header action says `Continue chapter X`; label and action share the same
resume target selection and preserve fractional chapter strings.
History poster clicks open series details, while the title/chapter text opens
the recorded chapter. Pointer regressions cover both targets; chapter download
and options controls have separate touch targets with an 8px gap.

Series posters use the independent platform cache on desktop and Android,
never application data. The bounded worker handles source checks, viewport
cancellation, hashed encoded files, CPU/UI limits, usage and serialized clearing.
Cache maintenance only removes owned poster files and leaves downloads/imports
and metadata intact. A running Linux fixture created its poster under
`XDG_CACHE_HOME/nova-reader/posters`, with none under its separate data directory.

Astra 6 medium independently reviewed the production changes and follow-up
fixes. Findings included discarded provider dates, catalogue ownership/failed
pagination, keyboard access and obsolete cover work. The final recheck found no
residual P1/P2 issue. Source/date/value tests, cache limits/clear/cancellation,
same-anchor quality upgrades, source/extension actions, catalogue return/scroll,
responsive layouts and OSD tap-versus-pan regressions cover the changes.
Host JNI tests and Clippy pass; full workspace tests, formatting, all-target
Clippy, workspace check and packaging regressions pass. Phone/desktop renders
with neutral sample covers are preview artifacts, not Android device captures.
Android APK/device performance and release assembly remain separate CI/device
results. Release workflow retains Nova's copied tag build behavior.


### Reader image context menu

Right-click and stationary long-press open the four requested image actions.
Short taps still toggle the OSD, and dragging cancels the hold gesture. Original
image actions preserve the source encoding; screencaps use a bounded clean
viewport snapshot taken before the menu appears. Encoding, source fetches,
saving and clipboard delivery run off the UI thread, with visible results.
Desktop clipboard ownership remains alive for subsequent paste requests.
Saved captures use Pictures/Nova Reader, with optional series folders. Android
29+ uses MediaStore; older Android versions use the document picker. The image
clipboard uses a private,
read-only content provider with per-URI grants and bounded cache retention.
The pinned cargo-apk2 manifest schema is extended to emit this provider; the
release workflow conditions and job dependencies are unchanged.

Screenshot settings adapt Nova's format/quality/folder controls. PNG and lossless
WebP preserve pixels; JPEG and lossy WebP use separate quality values. Original
chapter image saves retain their source format; generated screencaps use the
selected encoder. Folder grouping applies to both save actions. Clipboard actions
do not create an additional saved gallery image.

Cache and offline-download compression have independent JPEG/WebP and quality
choices copied from Nova's image-cache controls. They affect future writes only:
cache hits and verified partial/completed download pages retain their exact
bytes. Encoding preserves source dimensions. Images outside the selected
codec's limits retain their source encoding and still pass the normal image
validation before download publication. Defaults match Nova's WebP85 cache
compression; offline download compression is opt-in.

Final local verification passed workspace tests, formatting, all-target Clippy
with warnings denied, workspace check, root regressions for direct-History
capture naming, and packaging regressions (28 reported, one platform skip).
The running Linux/Xvfb app also exercised all four context actions: original
600x900 PNG bytes were saved unchanged, a clean 390x844 WebP screencap honored
format/series-folder preferences, and separate clipboard clients read the
original and viewport image dimensions after the callbacks completed. Android
JNI/Clippy, production Java against API35, MediaStore rollback/legacy picker
fixtures, and real patched manifest serialization passed. Astra 6 medium's final
static recheck reported no remaining P1/P2 findings. APK assembly and physical
Android interaction remain separate CI/device validation.


## System-bar layout and download deletion for 0.1.13

The black reader strip had two causes: the native window did not establish
edge-to-edge/cutout layout, and the shared page wrapper reserved top insets even
when status icons were hidden. Reader now follows Nova's normal transparent-bar
window policy, clears inherited fullscreen flags when restoring normal pages,
and keeps chapter artwork independent of top/side control insets. Detail uses
Nova's original fixed status-bar scrim and safe toolbar/hero/chapter gutters.

Delete download is available in remote chapter actions and Downloads. The
operation captures a stable chapter identity before background removal, keeps
metadata/progress/history, protects local imports, and refuses active writers.

Verification passed actual Window-inset regressions (portrait/status cutout,
landscape side cutout, safe detail/chapter controls, unchanged OSD viewport and
normal-page restoration), download action/callback and data-preservation tests,
all workspace tests, formatting, all-target Clippy with warnings denied,
workspace check and packaging regressions (28 reported, one platform skip).
Android JNI/Clippy and production Java compilation against API35 passed with
API26/28/29/30/35 window fixtures, including inherited fullscreen flags,
attachment retry and recreation. Astra 6 medium's final recheck reported no
residual P1/P2 findings. Physical-device rendering remains unverified locally.

## Collection, image quality and source additions (0.1.14)

Astra 6 medium independently reviewed this change set and its follow-up fixes,
with no residual P1/P2 findings. Findings fixed during review included AVIF
metadata-count allocation, keyboard category-row visibility, stale reader-worker
indices, premature completion with partial image windows, and false challenge
classification of legitimate MangaFire JSON text.

Tall pages retain native reading width through bounded visible-row windows and
2048×2048 texture tiles. Shared physical drawing edges remove fractional-scroll
seams. The retained model allowance is 32 MiB with 32 MiB replacement headroom;
encoded chapter bytes have a separate 32 MiB generation-scoped cache. Original
CPU decoding remains bounded full-image decoding. Posters now share Nova's
128 MiB decoded LRU and retain visible artwork plus 48 recent UI images.

Other reviewed behavior includes Nova search reveal, retained detail scroll and
synopsis, immediate detail presentation while chapters load, automatic catalogue
paging with explicit Retry, stable per-entry menus/categories, durable adult
visibility and chapter order, and J2K-derived Automatic reading mode with
per-series manual overrides. Mode choices use full source-local identities,
override historical chapter modes, and publish only after successful saves.

AVIF supports bounded 8-bit still decode and screenshot/cache/download encoding.
Screencaps offer Device or Chapter resolution; native captures use original
source pixels and frozen geometry, with an aligned source-pixel grid rather than
an enlarged device bitmap. Full applicable adapter/J2K notices accompany desktop
and Android distributions. MangaFire is a separately installable native port
with seven exact upstream language mappings and a deterministic licensed ZIP.

Verification on 1 November 2026 passed:

- Workspace formatting, all-target Clippy with warnings denied, workspace check,
  desktop check without update checking, and all workspace tests/doc tests.
- Both existing addon fixture suites, MangaFire Node/actual QuickJS/catalogue
  fixtures, and 28 packaging regressions (one environment-specific test skipped).
- Native 720×20000 lettering, viewport/zoom/DPR coverage, encoded-byte reuse,
  partial-window completion, 2D texture reconstruction and physical-pixel seams.
- Preference restart/migration/corruption/save-failure checks, short-window mode
  options, stable category targeting, search/detail restoration and poster reuse.
- Six source-resolution compositor tests; actual Linux software-OpenGL smoke of
  collection/details/menu/settings and clean saved captures: Device 390×844,
  Chapter 626×1355 for the same viewport of a 600-pixel-wide fixture.
- AVIF offline/source-removal download roundtrip, real PNG/AVIF desktop clipboard
  roundtrips, 698 process-isolated corrupt-input cases, Android Java/JNI AVIF
  save/provider tests, and license inventories for Linux, Windows and three
  Android targets.

Physical Android rendering, Windows execution and total device memory/frame
budgets remain unverified locally. MangaFire's safe API metadata probes succeeded
through the session proxy, but its actual Reader-host live smoke hit a DNS
preflight deadline; live image downloading/decoding remains unverified. No
captcha solving is claimed. The copied Nova workflow/tag-build split and
platform packaging scripts are unchanged; release builds remain the CI gate.

## Release build fixes (0.1.15)

The 0.1.14 tag run failed in two places: stable ARMv7 compilation rejected the
AVIF decoder's unconditional nightly feature attribute (E0554), and Linux
packaging rejected a changed upstream `continuous` linuxdeploy asset. ARM64,
Android x86_64 and Windows had built successfully.

The complete published ferri-pdf-rav1d 1.1.2 source is now patched locally with
only two feature predicates: ARM/RISC-V runtime feature-detection attributes are
assembly-only, matching their existing implementations. Reader retains portable
8-bit AVIF, stable Rust and the decoder's malformed-input hardening. Original
licenses, archive checksum and source history remain in the vendor directory.
Independent inventory comparison retained all 176 published files, with only
those predicates and appended provenance notes differing. Linux tool downloads
now use verified dated release URLs rather than moving continuous assets; exact
pins and the unchanged fail-closed verifier have regression tests.

Astra 6 medium reviewed the fixes and independent source/tool comparisons with
no P1/P2 findings. Local verification passed workspace formatting, all-target
Clippy, workspace check/tests, actual stable ARMv7 encoder/decoder compilation,
three portability regressions, 698 isolated malformed-input probes, all 30
packaging regressions and license inventories for all five release targets.
The production Linux script also built/extracted both Debian and AppImage
formats and executed their CLI using the existing local debug binary. That
Debian 13 smoke is not an Ubuntu 22.04 release artifact or device acceptance.
The copied workflow job graph and tag-build conditions are unchanged; the new
release run remains the final platform build/assembly/publication gate.

## Nova license/About reuse and UI corrections (0.1.16)

The 0.1.15 release completed all Linux, Windows and Android builds, APK assembly
and publication successfully. Its stable ARMv7 fix and pinned AppImage tools
remain unchanged.

The license flow now copies Nova b69a1e5's actual build-script generator,
cargo-about policy/templates, zstd-22 recipe, bounded runtime decompressor and
About source/license viewer. Reader adapts only its identity, active features,
local notices, source rows and existing shell/platform browser integration.
Each build renders only its current target's inventory; the redundant release
inventory steps and obsolete sidecar files are removed. Tag-only release builds
and separate main/PR checks retain their existing job graph and conditions.
Generator regressions cover feature/target arguments, tool/license failures,
complete notices, frame checksums and unchanged output reuse; immutable-source
tests check the copied Nova code and resources.

Settings now labels Device/Chapter capture sizing as Screencap resolution.
The detail synopsis copies Nova's 12px tap/drag guard without a focus outline
or focus-triggered scrolling. Detail metadata fetching shows no loading label
or spinner. Library progress uses Nova's explicit left/top fill origin in grid
and list layouts, including the one-read-chapter case. MangaFire's installable
ZIP is published in `addons/` with its source and checksum.

Astra 6 medium reviewed the copied license flow and final UI corrections with
no residual P1/P2 findings. Verification passed formatting, all-target workspace
Clippy with warnings denied, workspace check and all workspace tests/doc tests,
including actual synopsis pointer/drag/keyboard/accessibility and rendered-edge
checks, busy-detail error/Continue behavior, and phone/desktop progress geometry.
All 33 packaging regressions passed without skips. The Android API 35 Java/JNI
browser bridge harness and Clippy passed; the actual Linux About/source/license
viewer was rendered locally. Physical Android acceptance remains unverified.

### Shared cache, AVIF and tall-page integration

Final integration review is performed by Codex, following the user's request
to stop further Astra reviews. Nova's actual cache implementation, settings
controls and compatibility tests now live in Kit. Nova retains transport,
profile/sync and UI bridge adapters; Reader retains source authorization,
bounded admission and event-loop model ownership. Kit is pinned by full commit
in both consumers. Kit's Linux Nix and native Windows CI passed, including the
shared AVIF implementation.

Reader online pages use disposable platform-cache storage, separate from data
and explicit downloads. Chapter groups have a separate 1 GiB default budget,
active leases and bounded admission; artwork has its own 1 GiB default and
128 MiB decoded-pixel budget. Source/profile/settings/request/cookie scopes
guard memory, disk and queued publication. Clear generations, guarded atomic
writes and bounded background reservations prevent stale work repopulating
files. Large or unsupported cache transforms preserve original image bytes and
dimensions. Preference changes do not re-encode existing Reader cache files.

The 720×25,000 profile exposed repeated full decoding and event-loop pixel
copies. A worker-scoped original is now reused across windows, pixel buffers
are prepared off the UI thread, and one pending UI completion bounds handoff
memory. Idle invalidation releases retained source pixels. Optimized host CPU
window time improved from 111.42 ms to 1.69 ms at native width; GPU and Android
frame times were not measured. See `docs/tall-scroll-performance.md` for exact
Mihon source references, reproducible measurements and remaining resampling cost.

Kit's `nova-image` owns the unchanged Reader AVIF decoder and bit-identical
encoder; the complete 176-file stable scalar decoder moved with its licenses.
Reader uses thin wrappers. Nova adds AVIF cache/source-logo and screenshot
support with independent quality 85, retaining existing JPEG/WebP settings and
format indices. Shared host codec tests, 698 isolated corrupt inputs and an
actual stable ARMv7 codec build passed. Kit's Nix shell copies Nova's locked
inputs; all three host derivations evaluated and Linux CI builds/tests passed.


### Saved-image names and duplicate handling

Exports use portable series-title/chapter/page names. Chapter and page numbers
have four-digit minimum padding; screencaps append the first visible loaded
page's native-pixel Y coordinate, frozen with the pre-menu viewport. The shared
name builder preserves position suffixes within 160 UTF-8 filename bytes and
96 UTF-8 folder bytes, including Unicode truncation and Windows reserved names.
Optional folders organize exports alphabetically by series. Clipboard delivery
retains its existing image behavior and bypasses file conflicts.

The persisted Screenshots dropdown offers Ask (default), Overwrite and Do
nothing. Ask opens a retained Nova Kit modal with Overwrite, Add number and
Cancel. Closing it, changing reader generations, cancelling Android activity
work or leaving the app cancels the pending choice. Only one encoded export is
retained while awaiting a reply. Desktop saves stage and fsync in the destination
directory, publish without replacing existing files and retry numbered names
without clobbering; explicit overwrite uses atomic file replacement.

Android API 29+ resolves only app-owned exact-name/path MediaStore entries and
rejects unexpected provider renaming. API 26–28 obtains a persisted, scoped
folder grant. Existing Android images are backed up before provider overwrite;
write, close and commit failures restore their bytes and visibility in the host
fixtures. Permanently failing provider recovery reports the failure and retains
one bounded private backup, blocking subsequent overwrites. It cannot promise
filesystem atomicity for external document providers.

The final Java/JNI harness compiles against the real Android API 35 jar and
includes the actual Android Rust export adapter body. Collision choices,
rollback, stale replies, activity replacement, exact Unicode names, 96-byte
folder acceptance and over-limit rejection pass, together with JNI Clippy.
These are host checks; physical Android acceptance remains unverified.

Final Reader validation passed formatting, required workspace Clippy with
warnings denied, workspace check and all workspace tests/doc tests (173 passed;
five intentional child-process/X11/expensive-profile test entries ignored).
All 33 packaging regressions ran without skips. Neutral, Manhwa18cc and MangaFire
addon fixture suites passed. Nova's workspace Clippy/check, configuration/media,
serial software-OpenGL screenshot, headless settings and persistence/sync checks
passed before its shared-cache/AVIF commit was pushed.
The desktop distribution feature set (`--no-default-features --features desktop`)
also passed its locked check and target-specific license generation.


## Addon icons and Kagane (0.1.17)

Codex reviewed this change directly. Optional manifest icons are local declared
PNG assets, constrained to 256 KiB and 512×512 before installation. Workers
fully decode and prepare 64px thumbnails; only the UI thread creates Slint image
handles. Matching digests reuse handles across Browse visits, language variants
share package icons, and refreshes discard removed/replaced decoration. A 512-icon
cap bounds retained pixels to 8 MiB. Disabled packages require no script/network
execution to display their icons. Kit supplies the iconless/invalid-pixel fallback.

Manhwa18cc and MangaFire 1.0.1 include unchanged upstream icons and notices.
Kagane 1.0.0 follows Keiyoushi v37 at immutable revision
1dfd0bcf25b48cb5bd42d9191918a29072c67a61. Native QuickJS fixtures exercise all
capability variants, Safe search defaults, posted filters, compact column-order
chapter mapping, fractional identities, chronological metadata, page ordering,
public integrity tokens, cancellation, rejected cache hosts and incomplete
chapter indices. Rejected image access refreshes once through declared
resolvePage; the refreshed request is checked again and cannot recurse forever.
The narrow integrity header is treated as sensitive for redirects.

Reader now supports the JPEG XL page format observed in Kagane manifests using
jxl-oxide 0.12.6 with Rayon disabled. Dimension/encoded/page budgets and the
codec's grid allocation tracker bound admitted still pages. The adapter reserves
RGBA/row output before rendering, streams rows directly, skips unrelated
compressed container metadata and rejects custom ICC, previews, animation, CMYK
and extra channels. This is decoding, not JPEG XL encoding. Original page save,
clipboard, offline bundles and local import retain their original bytes; normal
new-write cache/download compression can transcode supported JXL input. Neutral
lossless RGB/alpha fixtures validate exact pixels, truncation and low budgets;
a boxed fixture verifies auxiliary Brotli metadata is never inflated.

Multiple and tristate source filters now render through existing Nova setting
controls, preserving provider values separately from labels. Options use internal
keys that cannot collide with provider IDs. Bounds remain explicit, including
512 choices per multiple group and 1024 rendered options total. This also makes
MangaFire's existing genre/content/type filters usable in Reader.

Independent live HTTP probes verified Safe Kagane search/details/chapter index
and original/datasaver manifests on 2026-11-01. Both used JXL on kstatic.to. These
probes fetched no chapter image bytes. Physical Android and live image decoding
remain unverified; host/compile tests do not establish those acceptance gates.

The actual Reader-transport live metadata smoke was attempted separately and
failed at its first search with `Network("DNS deadline")` in this workspace.
Successful independent HTTP probes are not reported as a native live session.

Final validation passed formatting, all-target workspace Clippy with warnings
denied, locked workspace check, all workspace tests/doc tests and 33 packaging
regressions without skips. The actual Android Java/JNI save/clipboard/import
harness and Clippy passed, including original JXL bytes on API 26/35. The exact
production JXL adapter compiled for ARMv7 in an isolated pure-codec harness.
The full local ARMv7 engine check was blocked by the missing Android NDK C linker
in the existing storage dependency; APK/whole-app validation remains in release
CI. Reference addon parsers and deterministic package/source-byte checks passed.
