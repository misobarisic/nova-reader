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
