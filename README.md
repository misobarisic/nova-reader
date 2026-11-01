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
It also controls JPEG/WebP/AVIF compression of newly cached images and new offline downloads;
existing files stay unchanged. **Screenshots** controls capture format (including
AVIF), quality, screencap resolution, optional series folders and duplicate handling. Screencaps can
use device pixels or the original chapter image's pixel scale for the visible area.
These device preferences are saved separately from library backups.
Browse groups sources by last used, pins and language; Extensions manages installed
packages. Chapters open with controls hidden; tap or click to show them.
Automatic reading mode follows [TachiyomiJ2K's genre/source rules](docs/reader-modes.md).
A manual choice is saved for that series across chapters and restarts.
Right-click or hold an image to save/copy its original or the visible screencap.
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

In Browse, select **Neutral reader fixtures**, open the series, add it to the
library, and choose a chapter. Fixtures include portrait, wide, tall, mixed and
320-page chapters. Reader controls offer Webtoon, left-to-right, right-to-left
and vertical paging. Download a chapter before going offline. Restarting retains
the logical page and intra-page position; completed downloads remain readable
when their source is removed. Queued transfers resume on launch; paused and
cancelled jobs wait for an explicit action. Transfers run serially in the app.

Browse installs local addon ZIPs and selects source/language variants. A newly
selected ZIP is explicitly enabled by the application. Permission-expanding
updates require re-enabling. Source Settings renders declared boolean, text,
select and secret fields; values are stored per source/profile outside backups. Adult sources are hidden until **Show adult sources**
is selected; that choice and each series’ chapter order survive restarts. The
reference [Manhwa18.cc package](docs/manhwa18.md) is explicitly NSFW and exposes English, Korean and all-language variants. Its parser has
synthetic fixtures; live website operation has not been verified. Browser
challenge handoff is not implemented.

The [MangaFire port](docs/mangafire.md) provides seven languages, search/filters,
chapter and volume listings through a separately installable native addon.
It is marked as a mixed/adult source and defaults to Safe content.

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
npm ci --ignore-scripts --prefix examples/addons/manhwa18cc/tests
npm test --prefix examples/addons/manhwa18cc/tests
node examples/addons/mangafire/tests/parser.test.mjs
python3 -B -m unittest discover -s .github/scripts -p 'test_package_*.py'
```

`make check` includes formatting, Clippy, workspace check/tests, addon tests and
packaging regressions. These are verification commands, not a claim that a
particular checkout or platform passed. See [packaging](docs/packaging.md) for
cross checks, Android signing, artifact names and release workflow. Release
configuration targets Android ARM64/ARMv7/x86_64/universal APKs, Linux Debian and
AppImage, and a Windows x86_64 ZIP; installation and device acceptance remain
separate from compile/archive checks.

The [static review record](REVIEW.md) distinguishes review findings from build
and target-device verification. See [project structure](docs/project-structure.md), [source API and limits](docs/source-api.md),
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
