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
