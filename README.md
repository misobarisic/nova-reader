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
grid columns; **Look and feel** controls animations. **Cache** shows usage and
clears temporary poster images without deleting downloaded chapters. It also controls
JPEG/WebP compression of newly cached artwork and new offline downloads;
existing files stay unchanged. **Screenshots** controls capture format, quality
and optional series folders.
These device preferences are saved separately from library backups.
Browse groups sources by last used, pins and language; Extensions manages installed
packages. Chapters open with controls hidden; tap or click to show them.
Right-click or hold an image to save/copy its original or the visible screencap.
Saved images go to Pictures/Nova Reader; Android 26–28 uses a destination picker.
Series posters use the platform cache directory on desktop and Android, separately
from library data and explicit downloads. `NOVA_READER_CACHE_DIR` can override it.
See [UI design reuse](docs/ui-design.md) for the source/component mapping.

## Run a neutral reading flow

Use the pinned Nix development shell, or install a Rust toolchain and the desktop
libraries specified in [flake.nix](flake.nix):

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
is selected. The reference [Manhwa18.cc package](docs/manhwa18.md) is explicitly
NSFW and exposes English, Korean and all-language variants. Its parser has
synthetic fixtures; live website operation has not been verified. Browser
challenge handoff is not implemented.

The reader loads the full visible page window plus one neighboring page on each
side, cancels obsolete image requests and waits for Retry after a page failure.
Retained decoded/display pages have a 64 MiB budget. Admitted tall images are
width-fitted and split into display textures of at most 2,048 rows, preserving
logical position independently of texture boundaries. Original decoding remains
bounded full-image decoding; arbitrary oversized stream decoding is pending.

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
`b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda`. Shared storage/UI remain pinned to
nova-kit `63141dfc6abd39a247edde1f33ebaeb59dc7229a`; this is the inherited Nova
pin, not the newer kit revision listed in the plan. Generic addon runtime
extraction into kit and Nova consumer compatibility are still pending.
Slint carries its own license alternatives; dependency license texts are
produced separately by release tooling. The Manhwa18/YiiMadara port retains its
upstream Apache-2.0 license and provenance. Neutral fixture material is CC0-1.0.
