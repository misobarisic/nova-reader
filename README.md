# Nova Reader

A Rust and Slint manga/webtoon reader derived from Nova, with a reader-specific
application identity, native JavaScript sources, local chapters, durable downloads
and reading progress. This implementation follows **Nova Reader — Implementation
plan** (planning baseline 31 October 2026). It is a working development tree;
[implementation status](docs/implementation-status.md) records the outstanding
acceptance gates.

The interface follows Nova's design: an adaptive icon rail or five-tab bottom
bar, shared Nova Kit controls, and More/settings cards. Local imports are under
**More → Local chapters**; categories and backups are under **More → Settings**.
**More → Settings → Theme** offers Nova's accents, background styles, card/button
styling and transparent bottom bar. **Display** controls poster/list layouts and
grid columns; **Look and feel** controls animations. **Cache** controls separate
chapter-page and artwork caches, disk limits, decoded artwork memory and poster
resolution. Clearing either cache preserves explicit downloads and library data.
It also controls JPEG/WebP/AVIF/JPEG XL compression of newly cached images and new offline downloads;
both use one choice per format, quality, and a separate option to preserve lossless
PNG/WebP inputs when encoding to WebP or JPEG XL. AVIF encoding remains lossy.
Existing files stay unchanged. **Screenshots** controls capture format (including
AVIF and JPEG XL lossless/lossy), quality, screencap resolution, optional series folders and duplicate handling. Screencaps can
use device pixels or the original chapter image's pixel scale for the visible area.
Normal **Save screencap** and **Copy screencap** open a selection editor with
draggable edges/corners, **Extend above / Extend below**, preview and confirmation.
The **Show simple screencap actions** switch also offers immediate viewport
capture actions; it defaults on. Turning it off leaves selection mode available.
These device preferences are saved separately from library backups.
Browse groups sources by last used, pins and language; Extensions manages installed
packages. Sources and Extensions show bundled addon icons, with a fallback for
older packages. [Installable addons](https://github.com/misobarisic/nova-addons#nova-reader-chapter-and-page-sources)
are maintained exclusively in nova-addons, including their source code, tests,
licenses and installable ZIPs. Reader retains its host/runtime, catalogue compatibility metadata and neutral
offline addon fixtures. JPEG XL chapter pages are supported. Chapters open with controls hidden;
tap or click to show them.
Compatible JPEG XL pages show bounded previews as bytes arrive, then replace them
with the completed image. Previews never count as completed pages or enter caches.
Ctrl+wheel on desktop and pinch on Android zoom up to 8×. While zoomed in, taps
show or hide controls without turning pages; double tap still switches fit/2×.
Automatic reading mode follows [TachiyomiJ2K's genre/source rules](docs/reader-modes.md).
A manual choice is saved for that series across chapters and restarts.
Right-click or hold an image to save/copy its original or a selected screencap.
Reader chapter/page controls and progress follow the effective reading direction:
Webtoon/LTR/vertical use Next on the right, RTL uses Next on the left.
Unfinished chapter rows show the last saved page beside the date. Marking a
chapter read/unread through its menu changes completion without creating a
History visit or replacing an actual visit's timestamp.
Chapter artwork fills the hidden status-bar area; showing the OSD restores the
bar and keeps controls clear of cutouts. Delete remote chapter downloads from
the chapter actions or Downloads; reading progress stays intact.
Saved images go to Pictures/Nova Reader; Android 26–28 asks for a folder once.
Names sort by title, chapter and page, for example
`The Café Terrace and Its Goddesses - Ch. 0001 - P. 0017.jpg`.
Screencaps append a vertical source-pixel offset such as ` - Y012480`.
Names stay within 160 UTF-8 bytes; optional series folders stay within 96.
**If a file exists** defaults to **Ask**, offering Overwrite, Add number or Cancel.
Settings can instead automatically Overwrite or Do nothing.
Series posters and pages viewed online use the platform cache directory on desktop
and Android, separately from library data and explicit downloads. Chapter eviction
groups older pages by chapter and protects the active chapter; its separate budget
keeps reading activity from flushing posters. Both disk budgets default to 1 GiB.
`NOVA_READER_CACHE_DIR` can override the cache root.
See [UI design reuse](docs/ui-design.md) for the source/component mapping.

## Run a neutral reading flow

Use the pinned Nix development shell, or install a Rust toolchain and the desktop
libraries specified in [flake.nix](flake.nix), plus `cargo-about` 0.9.1 for
Nova’s build-time About catalogue:

```sh
nix develop
cargo build --locked
python3 tools/package-addon.py examples/addons/neutral-fixture /tmp/neutral-fixture.zip
cargo run --locked -- install-addon /tmp/neutral-fixture.zip
cargo run --locked
```

The default shell includes Node.js/npm for addon parser fixtures in `make check`.
The app runs addons through embedded QuickJS and needs no Node.js at runtime.
The default, Android and Windows shells include `cargo-sweep` for cleaning unused
Cargo build artifacts.

Linux addon installation uses Nova's native ZIP picker on a dedicated worker,
independently of catalogue loading. The Nix shell exposes the dynamically loaded
D-Bus library for the desktop portal and includes Zenity as the fallback. After
updating the flake, re-enter `nix develop` before launching Reader. Non-Nix builds
need the desktop's file-chooser portal or Zenity available at runtime.

In Browse, select **Neutral reader fixtures**, open the series, add it to the
library, and choose a chapter. Fixtures include portrait, wide, tall, mixed and
320-page chapters. Reader controls offer Webtoon, left-to-right, right-to-left
and vertical paging. Download a chapter before going offline. Restarting retains
the logical page and intra-page position; completed downloads remain readable
when their source is removed. Queued transfers resume on launch; paused and
cancelled jobs wait for an explicit action. Transfers run serially in the app.
On Android, active transfers use Nova's foreground download service, wake lock
and progress notification. Android time limits and force-stop still apply;
interrupted queues resume when Reader opens again. Failed writes and interrupted
transfers are reconciled without leaving uncommitted page files behind.

Browse installs local addon ZIPs and selects source/language variants. A newly
selected ZIP is explicitly enabled by the application. Permission-expanding
updates require re-enabling. Source Settings renders declared boolean, text,
select and secret fields; values are stored per source/profile outside backups. Adult sources are hidden until **Show adult sources**
is selected; that choice and each series’ chapter order survive restarts. The
source implementations and their live-site limits are documented in
[nova-addons](https://github.com/misobarisic/nova-addons). Browser challenge
handoff is not implemented by Reader.

Library search reveals a live filter. Long-press or right-click a library title,
or use the detail toolbar menu, for that title’s categories and library actions.
Details retain their scroll and expanded synopsis on reader return; source
galleries load further results as you scroll and show Retry after a failed page.

PNG, JPEG, WebP and 8-bit still AVIF images are supported within bounded decode
limits. Animated, high-bit-depth or transformed AVIF variants are rejected.

The reader loads the full visible page window plus one neighboring page on each
side, cancels obsolete image requests and waits for Retry after a page failure.
Retained display/model pixels use 32 MiB plus 32 MiB replacement headroom. Tall-page output
uses bounded visible-row windows at reading width, split into textures of at
most 2,048 × 2,048 pixels; logical position stays independent of those boundaries.
Original decoding remains bounded full-image decoding and can repeat as the
window moves; arbitrary oversized stream decoding is pending.

The application uses an independent data directory selected with the operating
system's application-directory API. Linux defaults to
`$XDG_DATA_HOME/nova-reader` (usually `~/.local/share/nova-reader`). Set
`NOVA_READER_DATA_DIR` before launching to use a chosen directory; for example,
`NOVA_READER_DATA_DIR=/tmp/reader-demo cargo run --locked`. Android uses
app-private storage. Keep the same directory across launches to resume.

The CLI also supports local imports and metadata backups:

```sh
cargo run --locked -- list-sources
cargo run --locked -- import /path/to/chapter.cbz
cargo run --locked -- import-folder /path/to/images
cargo run --locked -- backup /path/to/reader-backup.json
cargo run --locked -- restore /path/to/reader-backup.json
cargo run --locked -- catalogue-keiyoushi
```

Run `cargo run --locked -- --help` for the complete command list. ZIP/CBZ imports
use natural filename order. Backups contain library metadata, categories,
preferences, progress, history, bookmarks and source descriptors. They exclude
chapter files, executable addon ZIPs and network credentials. Restore replaces
metadata; reinstall required addons separately and retain/copy downloaded chapter
files separately. Imported or downloaded content is not embedded in the JSON.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
cargo test --locked --test reading_journey
cargo test --locked --test ui_navigation
cargo test --locked --test mobile_layout
node examples/addons/neutral-fixture/tests/fixture.test.mjs
python3 -B -m unittest discover -s .github/scripts -p 'test_package_*.py'
```

`make check` includes formatting, Clippy, workspace check/tests, addon tests and
neutral-fixture packaging regressions. Real source parser and native host tests
run in nova-addons. These are verification commands, not a claim that a
particular checkout or platform passed. See [packaging](docs/packaging.md) for
cross checks, Android signing, artifact names and release workflow. Release
configuration targets Android ARM64/ARMv7/x86_64/universal APKs, Linux Debian and
AppImage, and a Windows x86_64 ZIP; installation and device acceptance remain
separate from compile/archive checks.

See [project structure](docs/project-structure.md), [source API and limits](docs/source-api.md),
[catalogue trust/discovery](docs/catalogues.md) and [remaining gates](docs/implementation-status.md).

## Attribution and licensing

Nova Reader is GPL-3.0-or-later; see [LICENSE](LICENSE) and
[third-party notices](THIRD_PARTY_NOTICES.md). The foundation derives from Nova
`b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda`. Shared storage, cache, codecs and UI
come from nova-kit `138f29e33d4f7d8ae9204b6bd6118eb458740f70`. Generic addon runtime
extraction into kit and Nova consumer compatibility are still pending.
Slint carries its own license alternatives; dependency license texts are
generated for the actual build target/features and embedded into About using
Nova’s copied build-time catalogue. The Manhwa18/YiiMadara port retains its
upstream Apache-2.0 license and provenance. Neutral fixture material is CC0-1.0.

## Cross-device sync

More → Settings → Sync offers device pairing with expiring invitation codes/QR,
confirmation prompts, peer management, Sync now, automatic exchange, local network
discovery and a read-only chapter/state inspector. Android also offers best-effort
background sync. Reader devices pair with other Reader devices; Nova’s video
library uses a separate protocol and identity.

Library entries, categories, chapter progress/read actions, history, bookmarks,
portable settings and source packages synchronize as individual records. Settings
show Nova’s scope indicators and a shared/local comparison; device overrides keep
a chosen value local until released. Chapter files, paths and credentials remain
on their device. Received addons require local permission approval when needed.

Production addon sources and tag-built downloads for both apps are maintained in
[`misobarisic/nova-addons`](https://github.com/misobarisic/nova-addons): existing
`addons/` targets Nova and `reader-addons/` targets Nova Reader. Reader downloads
use the `nova-reader-` filename prefix and the `nova-reader1` manifest contract.
Runtime fixtures and existing download URLs remain available in this repository.
