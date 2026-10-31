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
