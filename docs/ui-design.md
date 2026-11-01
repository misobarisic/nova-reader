# Nova design reuse

The reference is Nova revision `b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda`,
with shared cache, codecs and controls from Nova Kit revision
`138f29e33d4f7d8ae9204b6bd6118eb458740f70`.
Reader adapts destinations and reader content, rather than defining a second
navigation or settings design system.

- `navigation.slint` adapts Nova's `sidenav.slint` and `bottomnav.slint`:
  64px icon rail, 56px item pitch, immediate 48px accent circle, and shared
  icon-only `NavFeedback`. The narrow bar is 68px high, has five equal slots,
  24px icons and 12px labels, and selects with accent color without a tile fill.
  Both bars consume gap taps and support keyboard and accessible activation.
  Nova's transparent-bar option releases the layout slot and draws its existing
  32px dark fade above the tabs. Scroll content reserves end space so the final
  row can move clear of the controls; opaque mode reserves the full 68px slot.
- The same 700px breakpoint selects the compact shell on Android and desktop.
  Wide Android windows therefore use the rail, as Nova does. Details and the
  reader cover navigation; narrow child pages hide the bottom bar. Desktop
  More/settings children retain the stationary rail.
- Root destinations are Library, Updates, History, Browse and More, in that
  order as requested. Downloads occupies the desktop shortcut slot before More
  and is a More card on phones. History keeps the bottom bar visible and Back
  returns to Library.
- `page-header.slint` is adapted directly from Nova's `PageHeader`, with the
  same typography and back-button placement. Page gutters are 18px.
- Detail entry copies Nova's 40ms deferred entrance and shared
  `Anim.transition_fade`, applied to the artwork, chapter body and toolbar
  together. It replays for a new title or a return from reading on both desktop
  and Android. Disabling animations or page transitions shows the detail
  immediately, including when a fade is already running.
- `controls.slint` only adapts Reader's API/accessibility to Nova Kit's
  `PillButton`, `AddonIconButton`, `SettingsInput`, `ToggleSwitch`, `Dropdown`,
  `SettingsLink` and `SettingsBackButton`. Cards and scroll views are imported
  directly from Kit. Painting, hover, focus, spacing, radii and motion use
  Kit's `Theme`, `SettingsStyle` and `Anim` globals.
- `more.slint` uses Nova's retained landing / sliding child pattern. Its landing
  viewport stays stable when external bottom navigation hides, preserving the
  scroll position on child return with deferred scroll/focus restoration after
  layout publication. Settings has its own retained landing and independently
  animated leaf layer, preserving its offset on Back. Entry targets are staged
  after mounting; outgoing layers survive the reverse transition. Disabling
  motion selects explicit endpoints even during an interrupted transition.
  Supported destinations are local import,
  settings (theme, display, look and feel, sources, categories, data, cache and sync) and About. It does not advertise
  unsupported video or tracking features.
- `theme-settings.slint` adapts Nova's Theme and Look and feel modules with Kit
  cards, segmented controls, swatches and toggles. `settings-row.slint` preserves
  Nova's side-by-side/stacked measurement without its app-specific sync/search
  bridges. Eight accents, three background styles, card/button radius, card
  spacing and transparent navigation apply live. Motion offers Nova's master,
  page-transition, hover and navigation-feedback switches. Palette derivation
  comes from Nova's Rust implementation. Device preferences persist independently
  of portable library metadata; theme reset preserves motion choices.
- Roboto Regular/Bold and the Library/Browse/More/Updates/Downloads artwork are
  reused from Nova. Fonts and SVGs are embedded into packaged builds. Font
  provenance and the full Apache license live under `assets/fonts`; font
  notices also accompany each platform dependency license inventory.

The pinned Kit dropdown gates keyboard input on TV mode and keeps its popup
method private. `crates/ui/build.rs` reads that exact dependency source and
applies narrow, asserted transformations into `OUT_DIR`: enable existing
keyboard handling on all platforms, expose popup opening, and provide accessible
option activation. All its visual implementation remains upstream code. A
changed upstream contract fails compilation rather than silently losing these
adaptations. No files in the dependency checkout are changed.
The same build step extracts the pinned Kit `Segmented` implementation and adds
only its missing per-option accessible action. Theme switches use two-way UI
draft values and roll back to saved preferences if persistence fails.

`tests/mobile_layout.rs` checks the breakpoint, rail/bar geometry, responsive
bounds, actual tab/card activation, More/Settings Back and retained scroll,
intermediate nested push/pop positions, interrupted/master-off transitions,
reader options, and keyboard/accessibility selection in the shared dropdown.
`tests/appearance.rs` checks saving and reloading in separate processes, actual
palette/geometry changes, rejected invalid edits and reset semantics.
Linux rendering smoke is separate from Android system-inset, keyboard and
physical-device acceptance.


The collection and detail views also reuse Nova b69a1e5. `library-cards.slint`
adapts its 2:3 poster/list geometry, captions, left-anchored progress rail, hover lift and
responsive 160px/230px sizing; `title-artwork.slint` is Nova's original component.
`detail-hero.slint` adapts the edge-to-edge artwork, baked palette scrims,
responsive title/metadata, expandable synopsis and primary/library actions.
Display settings retain the selected layout and minimum column counts.
The progress fill copies Nova's explicit zero x/y origin, so even one read
chapter fills from the left in grid and list layouts. The synopsis uses Nova's
12px drag guard and has no focus outline or focus-triggered scroll jump; tap,
keyboard and accessible expansion remain available. Details open immediately
while metadata and chapters load without a loading label or spinner.
Library search reuses Nova’s retained 58px reveal and Kit `SearchField`. The
search action or Ctrl+F exposes and focuses it, edits filter immediately, and
Escape/Back closes it and clears the query. Deferred focus and viewport
correction keep the field reachable in short windows and when layout changes.

Library cards open a per-title menu on right-click or stationary hold; the detail
toolbar’s three-dot action opens the same stable-identity target. Narrow layouts
use Kit’s `MenuSheet`; desktop actions stay anchored to the selected title.
Categories, refresh and add/remove operate on that title, with category assignment
kept inside its menu. The Library toolbar no longer assigns categories through
a global three-dot action. Dragging cancels the hold.

Details publish the selected title, poster and known metadata before remote
chapter loading finishes. Loading/error feedback sits below the hero; Continue
is disabled until chapter rows are available. Opening a different series resets
the detail viewport and synopsis; returning from its reader restores the retained
scroll offset and expanded/collapsed synopsis.

The supplied Mihon references define the compact chapter and dated activity
structure: unread dot, chapter/date/scanlator, trailing download/options, and
Continue chapter X; History/Updates use covers, date groups and remove/download actions.
Nova supplies colors, radii, typography, actions and the existing five-tab bar.
History posters open the series detail page; the text area opens the recorded
chapter. Unread chapter rows show an accent dot and bright title; read rows
remove the dot and dim the title. Download/options actions retain an 8px gap.
Browse separates Sources and Extensions. Source rows group Last used/Pinned/
language, with independent Latest/pin actions; a source opens Popular/Latest,
search, provider-defined filters and a poster gallery. Catalogue Back retains
its stable source identity, result/query/pagination/filter context and scroll.
Source changes clear the old snapshot before a request can fail.
Source galleries request the next page near the viewport end and continue
filling a short viewport until the source finishes. Only one page request runs
at a time; successful appends preserve scroll and deduplicate full series IDs.
A failed page or a page claiming more results without new titles pauses automatic
loading and exposes Retry for that same page. Adult-source visibility and each
series’ chapter order persist through Nova KV; failed writes keep the saved
choice rather than publishing an unsaved change.

Chapter controls are hidden on entry. A tap/click reveals their overlay without
resizing the reading viewport; a drag remains a scroll gesture. On Android the
status bar follows OSD visibility and its desired state survives recreation and
focus changes. Status-bar navigation does not alter the navigation bar. Host
API 29/35 regressions verify Java/JNI transitions separately from device testing.

`reader-context-menu.slint` adds the floating image menu: right-click or a
stationary 600ms press on a loaded page offers Save image, Save screencap, Copy
image to clipboard and Copy screencap to clipboard. Its slate panel, lavender
line icons, rounded corners, divider and focus/hover colors use Nova tokens.
Dragging cancels the hold and continues scrolling; a completed hold consumes
its release so it cannot reveal reader controls. Context Menu / Shift+F10 targets
the current visible page; arrows/Tab and Enter operate the four actions. Outside
click, Escape, Back and navigation dismiss it. The panel clamps to safe viewport
bounds and scrolls within unusually short viewports.

Before showing the menu, the UI hides the OSD and export feedback, then calls
`reader-context-opening(page)` synchronously so the backend can capture a clean
viewport. An action hides the menu, calls `reader-context-action(page, action)`,
then calls `reader-context-closed()` to release the cached capture. Cancellation
also calls the close callback. Export work disables the four actions and exposes
progress; success/error feedback appears in a reader toast for four seconds.
More → Settings → Screenshots is a retained settings leaf with Nova rows and
controls for format, encoding quality, screencap resolution and separate series folders. Its
`CaptureSettings` global publishes validated device preferences; they persist
under `reader:captures`, independently of library backups and chapter bytes.
Device resolution remains the default. Chapter resolution captures the same
visible chapter area at the clicked page's source-pixel scale, using original
images and frozen viewer geometry. This applies to save and clipboard actions,
supports scrolling, panning and paged letterboxing, and retains the eight-million-
pixel export limit. A capture above that limit reports an error rather than
silently reducing the selected source resolution.
Cache settings also expose independent switches, JPEG/WebP/AVIF format and quality
for new cache images and new downloads. Their `ImageStorageSettings` global and
`reader:image-storage` preferences govern future writes; existing files are
preserved when a preference changes. Screenshot export format remains separate.

Image cache maintenance consumes Nova's actual implementation moved to Kit:
processed variants, decoded LRU, batched usage index and incremental GC. Cache
settings reuse the extracted Nova controls and choices. Online chapter pages
and posters occupy separate namespaces under the platform cache root on both
desktop and Android, each with a default 1 GiB disk budget. Chapter controls and
maintenance appear first. Older chapter groups are evicted while the active
chapter is protected; chapter activity cannot evict artwork. Clearing either
cache preserves library data and downloaded/imported chapters.
Poster cache hits publish immediately across collection/detail routes. One
configurable decoded-pixel LRU (128 MiB default) backs UI images, with visible artwork plus up to
48 extra recently used UI cards retained; source access revocation invalidates
cached hits. Poster resolution defaults to Original, and chapter cache encoding
always preserves native dimensions. Preference edits affect future writes.
Worker decode memory is additional to that retained-pixel budget.

Screenshots and cache/download compression also offer AVIF. The reader accepts
8-bit still AVIF through its pure-Rust codec path; animated/multiframe, high-bit
depth and transformed AVIF variants are rejected. AVIF/WebP preserve alpha;
JPEG storage conversion keeps original bytes for transparent input. Lossy capture
formats keep separate quality preferences, with AVIF defaulting to 85. Changing
compression preferences never rewrites existing files.

Reader drawing rounds shared page/tile edges in physical pixel coordinates,
including fractional scroll, device scale and window origin. Adjacent tiles
share an edge instead of independently rounding their heights; logical page
positions and resume anchors remain unrounded. Tall-page display retains
bounded visible row windows at the needed width, with neighboring rows admitted
within the output budget; it does not squeeze the full page into a narrow
texture. The source still passes a bounded full CPU decode before windowing.

The detail header action displays `Continue chapter X`; there is no floating
bottom-right duplicate. Its label and action
share the same resume selection (most recent unfinished chapter, then first
unread, then the first chapter); fractional chapter identifiers stay strings.

Reader Options offers Automatic alongside the four manual reading modes. Its
status displays the detected effective mode or a saved manual choice. Automatic
uses the pinned J2K genre/source rules before image decoding; manual choices
persist per full series ID across chapters and restarts. Choosing Automatic
explicitly restores detection, and failed saves preserve the previous choice.
See [reader mode selection](reader-modes.md) for upstream rules and adaptations.

## System bars and chapter downloads (0.1.13)

ReaderActivity follows Nova `crates/player/src/external.rs:523,666–732` for
normal edge-to-edge layout: transparent status bar, cleared inherited fullscreen
and translucent-status flags, and stable decor fitting. Active reading hides
only the status bar. The chapter canvas reaches beneath top/side cutouts while
OSD controls keep their own safe insets, so OSD changes leave its viewport fixed.
Normal pages retain their safe area. Detail artwork reaches the window edges,
with Nova's original `assets/status-bar-scrim.svg`, default gradient strength,
safe toolbar spacing and safe hero/chapter-list gutters.

Remote chapter rows offer Delete download from their trailing action and menu;
Downloads exposes the same operation. Local imports retain their sole copy.
Removal targets the chapter bundle, keeping series metadata and reading history.


Sources and Extensions use `addon-icon.slint`: a 40px source-colored image with
Nova Kit's extension icon as fallback and the existing disabled opacity. Labels,
focus, pin/latest hit targets and extension action cards retain their current
Nova typography/spacing. Icons are bundled package assets, fully decoded on a
worker and retained on the UI thread; language variants share the same image.
Multiple source filters reuse Nova setting switches and Any/Include/Exclude
dropdowns. No new palette or platform navigation is introduced.
