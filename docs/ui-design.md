# Nova design reuse

The reference is Nova revision `b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda`,
using the same pinned Nova Kit revision `63141dfc6abd39a247edde1f33ebaeb59dc7229a`.
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
  settings (theme, look and feel, sources, categories and data) and About. It does not advertise
  unsupported video, sync or tracking features.
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
