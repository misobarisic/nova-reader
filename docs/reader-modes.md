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
workers finish. J2K's normal global fallback is RTL; Reader preserves its existing
global preference instead. Reader's explicit Automatic option honors an LTR
classification directly, rather than reproducing J2K's one-time suppression of
implicit LTR when migrating unset viewer flags with a non-RTL global default.

## Persistence and priority

Reader keeps actual renderer mode indices 0–3 (Webtoon, LTR, RTL, vertical).
Selector index **4** is Automatic. The option also displays the effective mode or
indicates that a manual mode is saved for this series.

A versioned `reader:mode-choice:<JSON SeriesId>` KV record stores either Automatic
or a manual mode. Both source ID and local series ID participate, including for
series outside the library. Manual choices override every chapter's old saved
mode. Chapter progress retains logical anchors, not authority to overwrite the
series choice. Existing explicit per-library preferences migrate to manual
records; global defaults and chapter progress never become manual overrides.
Selecting Automatic writes an explicit record, so a legacy library preference
cannot revive on a later open.

Changes publish only after the fallible durable write succeeds. On failure the
current mode and saved selection remain unchanged. Chapter-open completion
reloads the latest choice to respect a manual change made while source metadata
was loading. Image work cannot modify the choice. These records are app-local
metadata, like chapter order; they are not included in the current domain-only
native backup schema.

Unreadable or future-version preference records remain untouched and use
Automatic for the current session, allowing the reader to open and a deliberate
new selection to repair the setting. Failure to migrate an already durable
library preference keeps using that library value; it does not block offline
reading. Explicit user writes remain fallible and never publish on failure.
