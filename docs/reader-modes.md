# Reader mode selection

Automatic classification is adapted from TachiyomiJ2K, not an image-aspect
heuristic. The immutable upstream is
[Jays2Kings/tachiyomiJ2K, b83defe42b2ac9f460667bda3a42fdd9d12e767e](https://github.com/Jays2Kings/tachiyomiJ2K/tree/b83defe42b2ac9f460667bda3a42fdd9d12e767e).

Reference paths at that revision:

- `app/src/main/java/eu/kanade/tachiyomi/data/database/models/Manga.kt`,
  `defaultReaderType` and tag/source helpers, lines 166–239.
- `app/src/main/java/eu/kanade/tachiyomi/ui/reader/ReaderViewModel.kt`,
  `getMangaReadingMode` / `setMangaReadingMode`, lines 730–778.
- `app/src/main/java/eu/kanade/tachiyomi/ui/reader/settings/ReadingModeType.kt`.
- `app/src/main/java/eu/kanade/tachiyomi/data/preference/PreferencesHelper.kt:172`.

The project README credits **Copyright 2015 Javier Tomás** and licenses the work
under **Apache-2.0**. The Rust adaptation retains attribution in
`src/reader_modes.rs`; the applicable license and provenance accompany the
application's packaged third-party notices. No upstream scripts are executed.

## Rules and adaptations

Tags are split on commas, trimmed and lowercased. Webtoon takes priority for
`long strip`, `manhwa`, `манхва`, `kr`, `korean…`, or a tag containing `webtoon`.
A source name containing `webtoon`, `manhwa`, or `toonily` also selects Webtoon,
except when manhua/comic tags exclude that source hint.

LTR follows for `chinese`, `manhua`, `english…`, or `comic` tags, or the upstream
comic-source name list (Gunnerkrigg, Dilbert, Cyanide, xkcd, ComicExtra,
Read Comics Online, ReadComicOnline), or a manhua source, subject to upstream
manga-tag exclusions. Tapas is deliberately excluded from the source-only LTR
rule. Explicit Webtoon tags still win. The implementation preserves the pinned
rule distinctions between classification tags and source exclusions, including
its mixed-case `zh-Hans`/`zh-Hant` exclusion literals after lowercase normalization.
Source names come from source descriptors, never guessed from image URLs.

No width/height threshold exists in this upstream classifier. Reader therefore
makes no new aspect-ratio guesses: unknown metadata uses its established global
fallback. This is resolved before page image decode and does not change as image
workers finish. J2K's normal global fallback is RTL. Reader explicitly resolves recognized
manga/Japanese tags to RTL after the Webtoon/LTR priorities, while preserving
its established fallback for unknown metadata. Reader's explicit Automatic option honors an LTR
classification directly, rather than reproducing J2K's one-time suppression of
implicit LTR when migrating unset viewer flags with a non-RTL global default.

## Persistence and priority

Reader keeps actual renderer mode indices 0–3 (Webtoon, LTR, RTL, vertical).
Selector index **4** remains explicit Automatic, **5** is long strip with gaps,
and **6** is Default. Default inherits Settings → Reader's default mode, whose
initial value is Automatic. Automatic remains independently selectable even
when the global default is manual. The panel displays the resolved mode.

A versioned `reader:mode-choice:<JSON SeriesId>` KV record stores Default, explicit Automatic, a manual mode or long strip with gaps. Both source ID and local series ID participate, including for
series outside the library. Manual choices override every chapter's old saved
mode. Chapter progress retains logical anchors, not authority to overwrite the
series choice. Existing explicit per-library preferences migrate to manual
records; global defaults and chapter progress never become manual overrides.
A missing record now selects Default. Existing explicit Automatic records stay
Automatic, so changing the global default does not silently change them.
Selecting Automatic writes an explicit record, so a legacy library preference
cannot revive on a later open.

Changes publish only after the fallible durable write succeeds. On failure the
current mode and saved selection remain unchanged. Chapter-open completion
reloads the latest choice to respect a manual change made while source metadata
was loading. Image work cannot modify the choice. These records synchronize as
source-scoped portable settings, including Default resets; they remain outside
the current domain-only native backup schema. Chapter ordering stays local.

Unreadable or future-version preference records remain untouched and use
Default for the current session, allowing the reader to open and a deliberate
new selection to repair the setting. Failure to migrate an already durable
library preference keeps using that library value; it does not block offline
reading. Explicit user writes remain fallible and never publish on failure.

## Reader settings

`src/reader_settings.rs` keeps global defaults under `reader:reader-settings` and
series overrides under `reader:reader-settings-series:<JSON SeriesId>`. Both
source and local series ID participate. The UI uses cached snapshots while
scrolling; edits publish only after storage succeeds. Layout and orientation
changes in the in-reader panel belong to that series; the Reader page in More
edits global defaults. The per-series reset restores layout/orientation
inheritance without changing the selected reading mode. Reading mode and each
paged/Webtoon layout field synchronize independently, preserving explicit false,
zero and `null` (follow-global) decisions. Orientation is deliberately device-local.
The scope badge opens the same shared-value/device-override controls in More and
inside the reader. General and Filters edit global defaults even from the reader;
brightness and hardware/power policy remain local, while portable appearance and
filter preferences synchronize. Incoming changes refresh loaded series and the
active reading mode without publishing received fields back to peers.

Tap regions port Mihon v0.21.3 at
`feadad5e1e2bf638be6e2ca317d11657b09d4596` under Apache-2.0. Pager/Webtoon
defaults, RTL actions, inversion and disabled navigation follow the actual
upstream region maps. Desktop left clicks use those same regions; right click
and stationary touch hold keep image actions. Double taps cancel the pending
single, and pans cancel both taps and holds.

On Android orientation uses the activity's requested orientation, while desktop
orientation constrains the reading viewport and reverse portrait rotates the
display. Fullscreen and keep-awake apply only while reading. On Android,
**Fullscreen** hides both the status bar and the navigation bar for the whole
reader session, like Nova's player; the bars stay hidden when the OSD is shown,
a swipe reveals them transiently, and both are restored on leaving the reader.
Native desktop idle inhibition runs on a coalescing worker and releases on
leaving the reader.

Display crop/split/rotation and tint/grayscale/inversion operate on bounded
windows from the immutable source. Source-resolution screencaps use the same
geometry/filter mappings; saving an original page preserves original bytes.

Chapter buttons preserve the current mode and start the destination at **1×**.
**Previous chapter** enters
the last image (the last spread half when split), or the exact trailing chapter
viewport in Webtoon; **Next chapter** enters the first image/start. They prepare
the destination in the existing reader and wait for real geometry/pixels before
entering it. Opening through Detail or History still resumes the
chapter's saved position.
An active zoom settles the old source position, then returns to 1× before chapter
positioning. Opening Pages cancels a pending chapter change and reconciles the
actual settled source position without writing progress or history or changing
completion flags. Back restores that same position, pan and zoom.
In collection viewers, group buttons return to the selected group's gallery grid.

Ordinary Webtoon gestures stop at loaded page and decoded strip edges while
the next overlapping strip loads. Completion keeps the admitted source point
and does not replay a large gesture. Explicit seek/resume may select a loading
position immediately. Chapter separators and transition captions are 96px high.


## Desktop webtoon sizing

Webtoon and Long strip with gaps use a centered column capped at **720 logical
pixels** on desktop by default. **Desktop webtoon width** in the Webtoon layout
form adjusts this to 320–1600 logical pixels, independently of transient zoom.
It is a global, device-local preference even when changed inside a series: a
laptop's comfortable column should not narrow an Android phone's reader.
Android keeps its screen-width fit; narrow desktop windows are also respected.
Side padding (0–25% on each side) is applied **after** the column cap. Previously
it was applied to the window before the 1100px cap, making padding edits
ineffective on sufficiently wide windows. Zoom then multiplies the fitted width.
Paged modes retain their own 1100px cap and fit-to-screen-height behavior.

## Suwayomi WebUI comparison

Source-level comparison against Suwayomi-WebUI revision
[`29ff36a0d49120f42ef9195b999f356058e5f677`](https://github.com/Suwayomi/Suwayomi-WebUI/tree/29ff36a0d49120f42ef9195b999f356058e5f677),
not a visual/browser review or a claim of full feature parity. No upstream UI
code is copied for this change.

| Area | Suwayomi WebUI | Nova Reader |
| --- | --- | --- |
| Layout modes | Single page, double page, continuous vertical, continuous horizontal, Webtoon | LTR/RTL/vertical paged, seamless Webtoon, long strip with gaps |
| Direction | Separate LTR/RTL setting | Direction is part of the paged mode choice |
| Automatic/default | Auto-Webtoon option and layout profiles; per-series default flags | Metadata-driven Automatic plus explicit Default inheritance |
| Scaling | Width, height, screen, original, plus stretch | Paged contain-fit; Webtoon width-fit; source-anchored zoom |
| Width/gaps | Optional 10–100% reader width; configurable page gap | Persistent desktop pixel column cap plus 0–25% side padding; seamless or fixed-gap strips |
| Desktop quick UI | Mode, spread offset, scale, direction, auto-scroll, Settings | Bottom OSD with mode, chapter/progress/navigation actions and a More settings panel |
| Settings UI | Tabbed dialog, per-setting reset/default controls | Shared global/in-reader form with Reading mode, General and Filters tabs; per-series layout/orientation reset |
| Storage scope | Server/global and manga metadata, active-device prefixes and layout profiles | Peer-sync field records for portable preferences; explicit device-local controls for hardware/layout-specific choices |

Suwayomi's useful distinction is **layout versus scale versus direction**. Its
double-page spreads and continuous-horizontal layout are not equivalent to
Nova's wide-page splitting or LTR pager, and are not implemented by this sizing
fix. Likewise, Nova's vertical pager is not Suwayomi's continuous-vertical
scrolling mode. The focused improvement here is making desktop strips readable
without browser/app zoom workarounds, not introducing additional renderers.

References:

- [Mode/scale enums and global/manga settings](https://github.com/Suwayomi/Suwayomi-WebUI/blob/29ff36a0d49120f42ef9195b999f356058e5f677/src/features/reader/Reader.types.ts)
- [Width control and its 10–100% range](https://github.com/Suwayomi/Suwayomi-WebUI/blob/29ff36a0d49120f42ef9195b999f356058e5f677/src/features/reader/settings/layout/components/ReaderSettingWidth.tsx)
- [Layout form and individual reset controls](https://github.com/Suwayomi/Suwayomi-WebUI/blob/29ff36a0d49120f42ef9195b999f356058e5f677/src/features/reader/settings/layout/ReaderLayoutSettings.tsx)
- [Desktop quick settings](https://github.com/Suwayomi/Suwayomi-WebUI/blob/29ff36a0d49120f42ef9195b999f356058e5f677/src/features/reader/overlay/navigation/desktop/quick-settings/ReaderNavBarDesktopQuickSettings.tsx)
- [Settings dialog](https://github.com/Suwayomi/Suwayomi-WebUI/blob/29ff36a0d49120f42ef9195b999f356058e5f677/src/features/reader/settings/screens/ReaderSettings.tsx)
- [Metadata, inheritance flags and device prefixes](https://github.com/Suwayomi/Suwayomi-WebUI/blob/29ff36a0d49120f42ef9195b999f356058e5f677/src/features/reader/settings/ReaderSettingsMetadata.ts)
