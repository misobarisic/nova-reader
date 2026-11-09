# Project structure

Nova Reader is a separate Rust 2024 workspace. UI orchestration is app-owned;
heavy reader subsystems live in leaf crates. It consumes pinned nova-kit storage
and UI utilities without introducing reader-domain dependencies into kit.

| Location | Responsibility |
| --- | --- |
| `build.rs`, `about.toml` | Nova’s copied current-target license/source catalogue generation, zstd-22 embedding recipe and final Android cdylib 16 KiB ELF alignment |
| `src/main.rs`, `src/lib.rs` | Desktop entry, Android entry/lifecycle, CLI routing |
| `src/app.rs` | Worker orchestration, navigation, source/settings selection, visible-page loading, display tiles, lifecycle progress flush |
| `src/reader_open.rs`, `tests/reader_entry.rs` | Immediate chapter route from detail/History, cancellable worker-only resume/metadata/offline preparation, in-reader retry, and first-content motion policy |
| `src/reader_progressive.rs` | Current-page JPEG XL streaming feed, provisional source geometry/tiles, bounded preview mailbox, EOF validation and nonce-guarded cleanup; final loading/progress/cache remain owned by the ordinary image worker |
| `src/services.rs` | Shared UI/test services: install, validated source/date conversion, library, downloads and local import |
| `src/downloads.rs`, `crates/ui/downloads.slint`, `crates/ui/download-controls.slint` | Completed chapters grouped by series, responsive Downloaded/Queue panes, metadata-only status projection, shared bounded poster cache, stable-ID selection/actions and durable queue reordering and a confirmation sheet before deletes Look: a live `QueueCard` at the top (the queue's first row, its progress, the summary, Pause all/Resume all; on a phone only with the Downloaded tab), `TabChip` Downloaded/Queue chips with counts, All/Unread chips, and each title with its open chapters drawn as one card (`DownloadEntry.last` closes a group) with 64×96 covers. |
| `src/cli.rs`, `src/paths.rs` | Commands, bounded file input, independent data and platform cache directories |
| `src/android_documents.rs`, `android/java/dev/misob/nova/reader/ReaderActivity.java` | Application-context JNI, current-activity SAF and reader system-bars (status and navigation) visibility/lifecycle on Java main |
| `src/android_downloads.rs`, `src/download_background.rs`, `android/java/dev/misob/nova/reader/ReaderBackgroundService.java` | The download foreground service: the Application context, class lookup, exception clearing and the start/retry/throttle state machine are Kit `nova-android` (the Java service is rendered from its template, `tests/android_java.rs` checks the committed copy); Reader supplies queue leases, independent notification progress, wake lock, resumed-only permission prompt and error-safe native calls |
| `src/addon_icons.rs`, `crates/ui/addon-icon.slint` | Verified bundled addon PNGs decoded on workers, retained UI-thread image handles shared by language variants, Sources/Extensions decoration and Kit fallback |
| `src/artwork.rs` | Kit decoded-poster LRU and encoded variants, synchronous cross-page hits, source permits, UI handles, four bounded fetchers, serialized display decoding, independent bounded cache encoding, and legacy artwork migration |
| `src/categories.rs` | Device-unique ids for new library categories (device id plus a local sequence starting at 1), shared by the category page and backup import |
| `src/chapter_cache.rs` | Kit-backed disposable viewed pages and thumbnail derivatives sharing one disk namespace/quota, authorization scopes, chapter-group eviction/active leases, a directory-scoped Kit `gc::Marker` for the cleanup schedule, filtered thumbnail clear and bounded background encoding; reads return `PageImage` and entries are checked with `page_store::inspect_stored`, so a tall page the writer stored as strips is still one entry |
| `src/page_store.rs`, `src/page_store/strips.rs`, `tests/page_store_boundary.rs`, `tests/page_transparency.rs`, `tests/split_decode_cost.rs` | The stored-page boundary. Opaque `PageImage` (`from_encoded` for network/import bytes, which are never interpreted as a stored layout; `from_stored` for cache/download files) implements the engine's `PageSource` and exposes `dimensions`, `decode`, `decode_rows`, `export`, identity (`ptr_eq`/`downgrade`) and, for the stores that write files, `stored_bytes`. `storage_form`/`split_form` produce what gets written, `inspect_stored`/`validate_stored` check entries and committed downloads, `check_fetched` rejects a source response carrying the container header. A tall page may be stored as one file of independently encoded row strips (magic `NRSTRIP\0`, FNV-checked header, strip table); that container, its magic and its strip types are private to `strips.rs`, so page counters, IDs, anchors, ratios, dimensions, inventory, thumbnails, captures and the capture editor see only dimensions and decoded rows. `tests/page_store_boundary.rs` greps the sources for leaks of the container and for stored pages decoded outside the boundary, and a `compile_fail` doctest on the module proves the strip types are unreachable; `tests/page_transparency.rs` compares whole and split storage end to end (inventory, dimensions, layout, anchors, pixels, display windows, captures, thumbnails); `tests/split_decode_cost.rs` is the ignored decode-cost report. Lossy WebP strips are decoded with libwebp (several times faster than the engine's pure-Rust decoder) |
| `src/page_inventory.rs`, `tests/page_inventory.rs` | Reader-free stable page/resource inventory; source-free downloaded/imported metadata and selection-time original integrity checks |
| `src/page_overview.rs`, `src/gallery_presentation.rs` | Shared overview orchestration, reader anchor restoration, atomic focal gallery density changes, gallery-first entry/groups, explicit metadata and full-resolution selection; native controller regressions live beside these modules |
| `src/page_thumbnails.rs`, `src/decode_admission.rs` | Lazy viewport-bounded thumbnail worker, mode-specific derivatives, adaptive gallery pixel leases/reflow recycling, direct progressive JXL prefixes and shared foreground-first heavy decode admission (`try_acquire` takes the decoder without waiting, for read-ahead); service/admission regressions live beside the modules |
| `src/thumbnail_cache_settings.rs` | Thumbnail usage/clear callbacks; invalidates live work and UI references while preserving originals and downloads |
| `src/reader_settings.rs`, `src/reader_settings_ui.rs` | Durable global reader defaults, device-local desktop webtoon column width, source-local series layout/orientation overrides, cached effective snapshots and fallible UI rollback |
| `src/reader_controls.rs`, `src/reader_navigation.rs` | Nova-style OSD actions, source-anchored 0.5×–8× zoom/seek, motion-setting-aware double-tap animation, zoomed tap-zone bypass, Mihon tap maps, mode inheritance and Android/desktop preference projection |
| `src/reader_continuity.rs`, `src/reader_handoff_tests.rs` | Demand-prioritized bidirectional chapter preparation on one physical metadata worker, per-chapter source ownership/progress, anchor-preserving attachment, visible-segment retention and deterministic reverse-demand scheduling/admission regressions |
| `src/reader_chapter_navigation.rs`, `src/reader_scroll_admission.rs` | Stable chapter-button first/last viewport intent, retained-model entry after trusted destination pixels, ordinary Webtoon decoded-range edge admission/overlapping replacement demand and the page read-ahead depth (about two viewport heights in Webtoon, two pages in paged modes while they fit the model budget) |
| `src/reader_band_read_ahead.rs`, `src/reader_read_ahead_tests.rs` | Idle work of the image worker between tasks, one small step at a time and only with an empty queue: decode the next bands of the split page being scrolled (`Reader::band_read_ahead` plans it from the published window, direction and `defers_prefetch`/handoff gates; the engine's `DisplayDecoder::read_ahead` decodes one band) and fetch, without decoding, the encoded bytes of up to three pages past the decode window (cancelled when visible work is queued, failed pages left to their own task). Tests drive the real worker and count bands decoded on entry |
| `src/reader_desktop.rs` | Nova-derived latest-state native keep-awake worker; Linux portal/screensaver, Windows execution state and macOS caffeinate lifetime |
| `crates/ui/reader-osd.slint`, `crates/ui/reader-settings.slint` | Responsive bottom OSD, mounted for the whole reader and driven by `shown` (gradients fade with `Anim.player_fade`; Back, the top-right group and the bottom rail slide to their nearest physical edge with `Anim.player_slide`; the private `OsdInput` global drops taps, focus and accessibility as soon as it hides), with fixed 44px chapter buttons and a single expanding bottom progress slider, aligned compact actions labeled with the effective mode, one More action per layout, animated phone-sheet/desktop-panel controls and shared global settings form under More → Settings → Reader. The settings sheet opens at half height on phones (handle taps/drags expand, collapse or close; desktop keeps the anchored full panel) with a series · chapter subtitle; its Layout · Display · Color tabs have a sliding underline and fading content, scope labels (This series / All series), mode tiles, tap-zone diagrams drawn from `src/reader_navigation.rs`'s grids, background swatches, colour presets derived from the stored colour (no extra setting), slider blocks and switch tiles; `KeyedBlock` keeps every `setting-key` searchable with its sync badge. Covered by `tests/reader_settings_sheet.rs` |
| `src/progress_checkpoint.rs` | One background checkpoint writer, latest offsets coalesced per chapter, failure retry and ordered durable navigation/lifecycle barriers; the 600 ms UI timer only queues progress |
| `src/reader_export.rs` | Original-image validation, screencap encoding, bounded platform delivery and persistent desktop clipboard ownership; reader/selection guards extend through actual Android clipboard dispatch. Save/Copy image keeps a whole page's original bytes; a page stored as strips is reassembled and encoded with the screenshot format and quality (PNG, with a status notice, when that format cannot hold it; the desktop clipboard gets RGBA). `from_page_converted` re-encodes any page for a converted save; `page_facts`, `format_name`, `format_limit` and `output_label` describe sources and outputs for Save image without decoding. The one pixel-to-storage encoder (`reencode_storage_pixels`) serves whole pages and strips |
| `src/export_names.rs` | Portable UTF-8 series folders and bounded title/chapter/page/vertical-position filenames, including duplicate numbering |
| `crates/ui/action-confirm-sheet.slint` | The one confirmation sheet (MenuSheet pattern) for clear history, backup import and recovery deletion (`BackupBridge.confirm_open`), download deletes (`download-confirm-open`), remove addon, remove from library, mark all read/unread, clear positions and chapter-row delete; `app.slint` captures the pending call in `ask-confirm` and dispatches it from `resolve-action-confirm` only on confirm |
| `crates/ui/save-conflict-sheet.slint`, `crates/ui/action-confirm-sheet.slint` | Retained modal Overwrite/Add number/Cancel and destructive-confirm dialogs, both thin wrappers over nova-kit's `ChoiceDialog` (`@NovaUiBase/choice-dialog.slint`; centered popup-surface panel with the menus' scale-and-fade entrance and Escape/arrow keys; `choice(i)` reports the row, `labels.length` for cancel); `app.slint` owns name/open/choice properties and callbacks |
| `crates/reader-capture`, `src/chapter_capture.rs` | Leaf bounded source compositor and source-coordinate selection/revision tickets, generic over any engine `PageSource` (a whole encoded response or a `PageImage`); a page stored in bands that is painted without crop/split/rotation is decoded only for the bands under the capture (`DecodedRows`, `band_aligned`); app re-export supports Chapter or frozen Device scale |
| `src/reader_capture_editor.rs`, `crates/ui/capture-editor.slint` | Source-anchored draggable crop borders/handles, dimmed initial context, continuous outward extension, bounded worker previews, Preview/Adjust/Save/Copy, stale-result guards and selected-position naming |
| `src/reader_capture_sources.rs` | Generation/profile-scoped original-source reuse for captures, weak viewer provenance, bounded shared session bytes, a decoded-original LRU keyed by exact response and holding whole pages or the bands/rows a capture decoded (reserved with the encoded retention) and cancellation/permission revalidation; compressed display variants are excluded |
| `src/download_cleanup.rs`, `crates/ui/download-cleanup-settings.slint`, `tests/download_cleanup_settings.rs` | Device-local opt-in read-completion retention, stable category exclusions, post-commit manual/checkpoint cleanup and native settings previews |
| `src/capture_settings.rs`, `crates/ui/capture-settings.slint` | More → Settings → Screenshots & saved images: simple Save image source (`save_source`: page as it is / prefer cached copy) and `convert_saves` (both device-only, not synced), the `simple_screencaps` switch that now shows every simple image action, capture format/quality, Device/Chapter resolution, folders and duplicates |
| `src/reader_save_image.rs`, `crates/ui/save-image-sheet.slint`, `crates/ui/save-output.slint`, `tests/save_image_sheet.rs` | Save image sources (original from source, downloaded copy, cached copy via disk-only `ChapterCache::read_cached`, imported file) and output (keep or convert): simple Save image resolves a `Plan` from the settings and its menu subtitle; Save image… probes sources on disk, opens `SaveImageSheet` and runs exactly the chosen source, with no fallback. `SaveOutputPanel` is the one format/quality control shared with the screencap editor |
| `src/image_limits.rs`, `src/image_limits_ui.rs`, `crates/ui/image-limit-settings.slint` | Device-local encoded bytes, dimensions, pixels and decoded-memory admission, durable validation/reset and operation snapshots with stale-publication guards |
| `src/image_storage.rs`, `crates/ui/image-storage-settings.slint` | Durable cache policy, unified codecs and independent input-aware lossless WebP/JXL cache/download compression; per-store tall-page strip settings (`split_tall_pages`, `split_threshold_rows`, `strip_rows`) inside the `cache`/`downloads` objects, with store-specific defaults for files saved before they existed; shared Kit cache controls with chapter-first maintenance and separate artwork settings |
| `crates/ui/source-settings.slint` | Addon settings page with slider-owned Flickable gestures, keyboard focus reveal and existing catalogue/custom-field callbacks |
| `src/source_compression.rs`, `crates/ui/source-compression-settings.slint`, `tests/source_compression.rs` | Host-owned per-addon global/explicit cache and download compression, worker snapshots and addon sync projection; budgets remain device-wide |
| `src/source_catalogs.rs` | Host-owned per-addon catalogue switches under `reader:disabled-catalogs:<JSON package ID>`; filters Browse while preserving direct/history/offline access |
| `src/backup/`, `src/backup_actions.rs`, `crates/ui/backup.slint`, `tests/backup_page.rs` | Full-device export/import ([backups](backups.md)): `.nova-reader-backup` ZIP on Kit `nova-backup` (`archive.rs` binds the schema; `components.rs` captures, validates and materializes records; `merge.rs` Merge/Replace/undo; `plan.rs` mapping, preview and revisions; `apply.rs` one staged transaction, recovery receipts and undo; `files.rs` stored download/local-import pages; `legacy.rs` metadata JSON), the Data page (section 12) and its `BackupBridge`, CLI `backup`/`verify-backup`/`restore`/`undo-restore` |
| `src/profiles.rs`, `src/profile_actions.rs`, `crates/reader-domain/src/profile.rs`, `crates/ui/profiles.slint`, `tests/profiles.rs`, `tests/profiles_ui.rs` | Per-user profiles ([profiles](profiles.md)): directory, record validation, listing, creation, atomic retirement and cleanup of retired profiles in the `profiles`/`profile_deletions` sync domains are Kit `nova_sync::profiles::directory` (`profiles.rs` supplies its `Host`: the source registry's per-profile settings, and keeps ordering, the selection and the deletion preconditions), device-local selection, the personal-key whitelist and active pointer, legacy migration, switching/deletion on the UI thread and the Profiles page (section 24) |
| `src/content_presentation.rs`, `tests/content_presentation.rs`, `tests/presentation_sync.rs` | Versioned `reader:content-presentation:<JSON SeriesId>` records, synced per entry through the `reader_presentation` domain; explicit Default/Reader/Gallery choice and last addon hint, captured before domain conversion and retained for offline use |
| `src/reading_preferences.rs` | Fallible Nova KV persistence for adult-source visibility and chapter order keyed by full source-local series identity |
| `src/reader_modes.rs`, `docs/reader-modes.md` | Pinned TachiyomiJ2K classification, Default/global inheritance, explicit Automatic/manual/gapped selection, and durable per-series mode overrides |
| `src/timeline.rs`, `src/source_filters.rs`, `src/source_browser.rs` | Local date groups and day keys/provider date parsing; bounded typed filter values plus the filter sheet's groups, active count and Reset to the source's defaults; persistent source pins, last used (with `last_used_at`) and the language chip, split by `sections()` into Last used, Pinned and All sources |
| `src/updates.rs` | Optional reader application release discovery |
| `src/metadata_refresh.rs` | Nova-derived foreground metadata/chapter scheduler, bounded fair batches, persisted per-series deadlines and stale-result admission |
| `src/about.rs` | Nova’s exact-size catalogue decompression, bounded text chunks and source-row ordering/fallbacks; publishes About models from a worker |
| `src/browser.rs` | Nova’s desktop browser launching and Reader Android intent adapter for About source links |
| `src/sync_settings.rs` | Field-level global/per-series portable settings records, atomic persistence, durable device override baselines, actual Nova scope indicators/modal data and effective reader reapply |
| `src/sync_ui.rs`, `crates/ui-shared/sync-bridge.slint`, `crates/ui-shared/sync-host.slint`, `crates/ui/sync-host.slint` (the `@NovaSyncHost` file Kit's sync page imports, re-exporting the shared globals), `crates/ui/sync-settings.slint` | Shared Kit Settings → Sync page (section 20), worker-owned startup/shutdown/projection, pairing/invites/QR/peer status and chapter inspector; preserves the UI event-loop boundary |
| `src/sync_addons.rs` | Per-package desired sync state, hash-verified paired-device addon transfers, nonsecret settings/catalogue switches and permission-confirmation state |
| `src/android_sync.rs`, `android/java/dev/misob/nova/reader/ReaderSync*.java`, `QrScanActivity.java`, `tools/test-android-sync.py` | Android periodic/headless sync leases (headless files-dir bootstrap, class lookup and the pending QR-scan slot come from Kit `nova-android`), generation-scoped cancellation, network recovery, clipboard and Camera2 invite scanning; Java/JNI regressions |
| `src/desktop_dialogs.rs` | Nova’s dedicated native ZIP picker worker and picker-only busy guard, plus `pick_path` for local-import and backup/restore dialogs (worker thread, one at a time); bounded bytes return to the original window before Reader starts installation |
| `src/appearance.rs` | Device appearance/motion preferences, validated edits, immediate application and KV persistence |
| `crates/ui/app.slint` | Shared pages/reader models, safe-area layout, platform/width shell selection, detail origin, Nova's deferred detail-entry fade and translated text |
| `crates/ui-shared/media-models.slint` | Shared series/chapter/timeline/source/extension/filter row models (in the shared library so page crates use the same types) |
| `crates/ui/library-cards.slint`, `crates/ui/display-settings.slint` | Nova poster/list painting and left-anchored progress fill shown after a chapter is read, per-entry context gestures, viewport admission, grid/list switch and persisted display choices. `library-style` (the library only; Browse keeps the framed card) draws frameless covers with an unread or "+N" badge, a download badge, a 3px progress bar for partly read titles, and a two-line title and one detail line on the cover over a dark fade (the card is the cover alone), three columns on phones, and 94px list rows with "status · source · N downloaded" |
| `../nova-kit/crates/cache`, `../nova-kit/crates/ui-base/slint/cache-settings-controls.slint` | Actual Nova cache/settings implementations and NumericSetting/cache controls shared by Nova and Reader; app persistence/search/sync adapters remain local |
| `src/library_navigation.rs`, `crates/ui/library-page.slint` | Category picks move the chip at once and build the filtered cards on a worker (`library_view::project`, `SeriesFields` is the `Send` card data) from `View.rendered`, the state the last render drew, so no DB read; `PROJECTION_GENERATION` drops superseded picks. Device-local `reader:library-navigation:v1` last selected stable category; session-only per-category SeriesId/row-fraction anchors and Nova's pinned sibling header/native LibraryScroll. The header ("Library", "N titles", search) scrolls away; the pinned toolbar holds Kit's tonal counted category chips, the sort pill (one icon, flipped for ascending) with `LibrarySortMenu`, and the view switch |
| `src/library_view.rs`, `tests/library_refresh.rs` | Library sorts (Recently added, Title, Unread, Last read, Latest chapter) in their natural direction, reversed by `reader:appearance/library_sort_reversed` (titles with nothing to sort by stay last), per-category title counts ignoring search, and the grid/list detail lines ("Ch. X of Y", "Caught up", "Completed", "N chapters") |
| `crates/ui/library-search.slint` | Retained Nova search reveal, deferred focus, live filtering and viewport-aware field exposure |
| `crates/ui/entry-menu.slint`, `crates/ui/status-menu.slint` | Per-title action dialog (nova-kit's `ActionDialog`, `MenuItem` from `@NovaUiBase/menu-item.slint`, re-exported by `chapter-menu.slint`: titled, scrolling, popup-style panel with the menus' entrance) on phones, anchored growing desktop panel, Change status picker (also an `ActionDialog`), stable-identity category editing, series mark-all read/unread, series clear-all chapter positions and library/refresh actions, Default/Reader/Gallery presentation and collection Metadata/Download/thumbnail-density (`density-smaller`/`density-larger`, handled in `app.slint`) actions |
| `crates/ui/detail-hero.slint`, `crates/ui/title-artwork.slint` (Kit `TitleArtwork` re-export), `crates/ui/chapter-row.slint` | Nova detail artwork/title/metadata/actions, retained synopsis state, chapter-ready action gating and backend `resume-action-label` for Start/Continue/Read again, 68px chapter rows (38px number column with a new-chapter dot, a `DownloadRing` per download state, an animated icon ⋯ menu in `crates/ui/chapter-menu.slint`, read state as text colour plus accessible description, bookmark mark, status-led details line, reading progress bar) and accessible action popup; the list header shows chapter and unread counts and scrolling count chips (All, Unread, Downloaded, Bookmarked) On desktop the hero shows the whole cover as a card at its own aspect (160px, 200px from 1200px) beside the text, over a blurred wash of it (`series-backdrop`: a 12px-wide copy `artwork::set_series_poster` makes, scaled up smoothly), with a button-sized primary action; phones keep the edge-to-edge cover. From 1000px the chapter list shows two chapters to a row (left then right, so neighbours stay side by side), placing each row by its rank among the shown rows (`detail-rank-*`, published with the filter counts). Previews: `tests/detail_preview.rs` |
| `src/chapter_news.rs` | The "new" chapter dot: the profile-local `reader:seen-chapters:<JSON SeriesId>` record of chapter IDs the detail screen has shown (not synced or backed up). A visit compares against it once and keeps the answer through the reader and page overview; the first visit of a title marks nothing |
| `crates/ui/title-menu.slint`, `src/app.rs::publish_title_menu` | The title menu, opened by the detail screen's ⋯ and by a hold or right-click on a Library title (`open-title-menu`, anchored at the button or the pointer, with the chapter menu's grow motion): a summary line, Refresh/Status/Categories/Layout tiles, Delete read downloads (keeps bookmarked chapters), mark-all and position actions, add/remove, and sub-pages for Download (next chapter, next 5, all unread, everything via `entry_action` `download-next:N`/`download-unread`/`download-all`), Status (the manual statuses and Automatic, `status:N`), Categories (ticks stay open, `category:ID`) and Layout (`presentation-*`). `title-menu-requested` makes Rust publish the summary, download counts, current status (`title-menu-status`), layout choice and category ticks for that title. `EntryMenu` remains for the page overview's ⋯ |
| `src/downloads.rs::publish_detail_queue` | The detail screen's download card: this title's queued/running/paused/failed jobs from the loaded manifests (running chapter and percent, counts with failed in the destructive colour, a progress bar), republished on every poll; Pause all / Resume act on this title's jobs only (`detail_queue_action`) |
| `src/chapter_selection.rs`, `crates/ui/chapter-selection-bar.slint` | Multi-chapter selection on the detail screen: kept in the view by `ChapterId` (`View::chapter_selection`) so refreshes keep it; a row hold or the header's Select starts it, rows become checkboxes, and the sliding bar runs Select all (current filter), Mark read, Download and Delete downloads (one confirmation) with one refresh. Back leaves selection before the title |
| `src/chapter_batch.rs` | Multi-chapter actions in natural reading order: chapters before one (Mark previous as read) and which to download next (from a chapter, next unread, all unread, all), skipping chapters on disk or already waiting |
| `src/chapter_list.rs`, `tests/detail_chapter_list.rs` | Chapter row text: `short_title` (drops a repeated "Chapter N" prefix), the leading status (downloading/queued/paused/failed, then "Page X") and the read fraction, drawn only for downloaded chapters whose page list gives the length |
| `crates/ui-timeline/`, `crates/ui/timeline-page.slint`, `crates/ui/timeline-host.slint`, `crates/ui-shared/timeline-bridge.slint`, `src/timeline_groups.rs`, `src/timeline_page.rs`, `tests/timeline_page.rs` | Updates "Grouped" and History "Combined" as their own crate: a pinned header whose title shrinks once scrolled, day headings that stick below it, and a virtualised list of positioned lines (`TimelineEntry`: day, group header, chapter, show more, actions, single, timeline item). `timeline_groups.rs` groups the dated rows by local day then series (Updates peek at 3 chapters, History sessions collapse) and computes each line's position; on wide screens (`TimelineBridge.columns`, bound by `TimelineHost` at 960px, outside the mobile UI) `two_columns` puts Updates' cards into the shorter of two columns under full-width day headings, with the day's "Mark read" beside the date, and `two_column_days` puts whole History days side by side so each day stays one unbroken history with its own sticky heading; Rust keeps the whole list in `TimelineBridge.entries` and gives the page only the lines within 800px of the viewport in `shown` (the page reports its position every 400px through `window-changed`; scrolled windows keep their rows, so cost no longer grows with the list); re-rendering the same list patches the model in place (`patch`: the common head and tail keep their rows and slide, the changed middle is inserted and fades in), while a different list (section, filter, search, view) fades in (`TimelineEntry.column`: 0 full width, 1 left, 2 right); `timeline_page.rs` builds the facts from `render`'s rows, publishes lines, chips, the History shelf and refresh progress, and runs group/day Mark read, Download all, Read first, Resume and Remove session. Grouping (`updates_grouping`), History view (`history_view`) and the hidden shelf (`history_shelf_hidden`) are device-only appearance keys; expansion lives in the view only. Updates also keeps read chapters uploaded within 30 days |
| `crates/ui-browse/`, `crates/ui/browse-page.slint`, `crates/ui/browse-host.slint`, `crates/ui-shared/browse-bridge.slint`, `src/browse_page.rs`, `tests/browse_page.rs` | Browse "Refresh" as its own crate. The window's Browse properties (`source-rows`, `source-catalog-open`, `source-query`, …) alias `BrowseBridge` where they are declared, so Rust keeps its setters and paging/filters/back navigation keep their paths. The page has the pinned header, Search all sources with its scope, Sources/Extensions tabs, the Last used card, Pinned grid (hold or right-click unpins), language chips, extension switches and ⋯, the catalogue header/search/chips with a grid/list switch, loading placeholders, an In library badge, and the filter sheet (pills include/exclude where the source is tri-state). `browse_page.rs` publishes those sections and runs search across sources (pinned or every enabled source, at most 4 at a time, cancelled by a new query, per-source failures and retry, › opens the catalogue searching). Addon updates are not built (nothing checks for them) Browse fills the page width; on wide screens (960px, outside the mobile UI) source and extension rows go two to a row and pinned tiles fit as many columns as the width allows; from 700px the Sources | Extensions switch keeps a fixed 320px width and the Last used card puts fixed-size Popular/Latest buttons at its end A catalogue slides in over the source list and back out, the Sources | Extensions highlight slides between tabs, and the home list fades in when its tab or search results change |
| `crates/ui-settings-storage/`, `crates/ui-settings-capture/`, `crates/ui-settings-sync/`, `crates/ui-settings-look/`, `crates/ui/settings-section-host.slint`, `crates/ui-shared/settings-pages.slint`, `crates/ui-shared/settings-globals.slint`, `tests/settings_preview.rs` | More → Settings sub-pages compiled in four crates (cache, image limits and download cleanup; screenshots; sync and data; theme, motion and display), one `…Module` per page. More's detail column mounts the open page with one `SettingsSectionHost`; each module reports its measured height in `SettingsPages.content-height` (a ComponentContainer does not pass on its child's layout) and reads `interactive`/`narrow`/cache state from it. The pages' globals (`ImageStorageSettings`, `ImageLimitSettings`, `DownloadCleanupSettings`, `CaptureSettings`, `BackupBridge`, `ReaderSyncSettings`, `SyncContent`) live in the shared library so the window, Rust and the pages share one copy. Reader defaults (`ReaderSettingsForm`), Categories, Profiles and About stay in the window |
| `crates/ui/page-menu.slint` | The ⋯ menu (`PageMenuItem` rows: labels, checks, actions, destructive actions, switches; scrolls long lists), header icon buttons and the shrinking `PageTitle` shared by Updates, History and Browse. `PageMetrics` holds the header geometry shared with Library (18px inset, 52px title row, 32px/28px title) that Updates, History and Browse use |
| `crates/ui/reader-context-menu.slint` | Save image…/Save screencap…/copy actions plus optional `(simple)` actions (Save image (simple) carries a subtitle of what it writes), mounted while in the reader and driven by `shown` (grows from the press point with `transform-scale`/`transform-origin`, `Anim.player_slide` in and `Anim.player_fast` out, no input while closing), stationary-hold/right-click handling, clamping, keyboard navigation and export feedback |
| `vendor/i-slint-core`, `tests/nested_flick.rs`, `tests/flick_velocity.rs` | Patched Slint core (`NOVA_PATCH.md`): pinch deltas, aborted two-finger taps, short-window fling velocity, and Flickables claiming a drag only along the axis it leans towards, so horizontal rails inside vertical lists keep sideways flicks |
| `crates/ui/reader-pager.slint`, `tests/reader_pager.rs` | Touch/mouse gallery dragging and wheel paging in reading order, adjacent-page motion and bounded settling; zoomed artwork yields to image panning |
| `crates/ui/reader-zoom.slint` | Native pinch scale/moving-midpoint lifecycle and remaining-finger guard; `app.slint` forwards `reader-zoom-by` and `reader-transform` into reader controls, while image interactions consume actual Ctrl+wheel modifiers |
| `crates/ui/page-overview.slint`, `crates/ui/page-thumbnail.slint`, `tests/page_overview.rs` | Shared row-virtualized overview with labeled manga pages or edge-to-edge square gallery tiles (2px gutters, dark placeholder, motion-aware 150ms fade-in, inset current ring and full-tile keyboard ring, broken-image glyph), a collection header floating over a top scrim (the hand-virtualized grid Flickable pads its content by the header height so images start below it and scroll under it; anchor/restore fractions are measured from the pad) that hides while scrolling down and returns on scroll up/top-edge/keyboard, group chips (up to 8 groups, dropdown beyond), a trailing fast scroller with an "Image N of M" bubble for collections over ~3 screens, stable-ID selection, RTL columns, focal pinch/Ctrl+wheel density and logical viewport-anchor restoration; native interaction and opt-in raster previews (`render_gallery_snapshots` writes before/after PNGs) Chapter pages use the shared `PageHeader` (title wraps on phones) with the options button on the right, then one slim row: page count, a Current page chip and the three-step `SizeSwitch` (small, medium, large thumbnails). Collections sit on the theme's canvas colour, with the floating header on the shared 18px inset. |
| `crates/ui/navigation.slint` | Nova’s 64px icon rail, flat five-tab bottom bar, icon feedback and keyboard navigation |
| `crates/ui/controls.slint`, `crates/ui/page-header.slint` | Thin Nova Kit control adapters and Reader's own `PageHeader` (the shared `PageMetrics` header: 52px row, 32/28px title, plain back arrow on the 18px inset; same properties as Kit's, used by More, Settings, Downloads, source settings and page previews); shared palette, typography and accessibility |
| `crates/ui/more.slint` | Retained Nova-style More/Settings landings, nested slide/fade layers, scroll/focus restoration and local import/categories/data/About pages |
| `crates/ui/about.slint`, `crates/ui/about-models.slint`, `crates/ui/about-controls.slint` | Nova’s copied About/source/license viewer with Reader shell routing and shared header adapter |
| `crates/ui-shared/appearance.slint`, `crates/ui/theme-settings.slint`, `crates/ui/settings-row.slint` (Kit `SettingsRow` re-export), `crates/ui/src/appearance.rs` | Nova theme/motion settings controls, responsive setting rows and adapted Nova palette derivation |
| `crates/ui-shared/` (`shared.slint`, `links = "novauishared"`) | The `NovaUiShared` compiled Slint library: Kit host globals (`Theme`, `Anim`, `SettingsStyle`, …), `SheetItem`, the settings sync-badge global (`SettingsSync`, Rust `ReaderSettingsSync`; never rename a library global on import, or the generator makes a second copy), `Appearance`, the row models, `PageMenuItem` and each page's bridge global (`LibraryBridge`, `TimelineBridge`, `BrowseBridge`). Page crates and `crates/ui` import it as `@NovaUiShared` so every compiled module shares one copy; Rust names re-export from `nova_reader_ui` |
| `crates/ui-library/`, `crates/ui/library-host.slint`, `crates/ui/src/page_factories.rs`, `crates/ui/src/window.rs` | The library page compiled as its own crate (`LibraryModule` binds `LibraryPage` to `LibraryBridge`). `LibraryHost` in `app.slint` binds window state to the bridge and mounts the page with a `ComponentContainer`; `page_factories.rs` installs the factory and relays bridge callbacks to the window's callbacks; `window.rs` wraps the generated `AppWindow` so `AppWindow::new()` installs factories and `global::<…>()` keeps working. Edits to `library-page.slint` and `library-search.slint` rebuild only this crate; `library-cards.slint` is shared with the Browse crate |
| `vendor/i-slint-compiler` | Nova's patched Slint 1.18 compiler (compiled-root embedding, cross-library aliases), wired through `[patch.crates-io]` so Slint can be compiled in separate library crates |
| `crates/ui/build.rs`, `crates/ui/build-support.rs` | Shared compiler configuration for every UI crate (Kit aliases, experimental module builds, embedded resources and debug layout metadata); Kit `MenuSheet`, `Dropdown` and `Segmented` are imported unmodified (`ReaderSelect` turns on `keyboard-input-enabled`), and `SettingSyncInfo` comes from Kit `sync-types.slint` |
| `vendor/i-slint-core`, `vendor/i-slint-core/NOVA_PATCH.md` | Pinned Slint 1.18.0 registry source with raw-pinch/lift corrections and a minimum Flickable velocity observation window, source regressions and retained upstream licenses; selected through the root Cargo patch |
| `assets/fonts` | Nova’s bundled Roboto faces and Apache notices; More/settings artwork comes directly from Kit |
| `assets/open_source_*.hbs`, `assets/open_source_project_sources.tsv`, `assets/open_source_vendors.txt` | Nova catalogue templates and Reader-specific source/vendor attribution; full local notices are appended by `build.rs` |
| `assets/icons/navigation` | Embedded SVG navigation and reader controls, independent of system font glyph coverage |
| `assets/icons/settings` | Reader's own More/Settings row icons in Kit's menu-icon style (reader, screenshots, backup, image limits, folder, profiles, archive file); `more.slint` passes them as `icon:` and uses Kit's `icon-kind` set for the rest, one distinct icon per row |
| `crates/reader-domain` | Source-local identities, schema-1 state, library/categories, publication/reading status in `src/library_status.rs`, progress/history/bookmarks, legacy metadata `backup_json`/`restore_json` (restore keeps the device's installed-source list), `exclusive` reads under the serial lock and the serialized atomic persistence hook |
| `src/sync.rs` | Reader per-identity adapter on Kit `nova-sync`: default `reader` namespace, metadata, library, category memberships, resume, manual reading actions, history and bookmarks, plus the per-entry `reader_presentation` domain owned by `content_presentation.rs`; atomic `reader:state`/record/baseline persistence and remote echo guard |
| `crates/reader-sources` | Reader v2 registry, profile settings, QuickJS and checked HTTP; exact-origin/name cookie permissions, isolated session generations/cache scopes, verified peer archives and parse-only native validation. Package ZIPs are opened through Kit `nova_storage::zip` (raw central-directory count, duplicate/unsafe-path/symlink rejection, size limits), registry files are written with `nova_storage::fs::atomic_write`, and the public-address filter and Android TLS configuration come from Kit `nova-net` |
| `crates/reader-engine` | Dimension probing, bounded PNG/JPEG/WebP decode and shared Kit AVIF delegation, display geometry, resume anchors and decoded cache utility |
| `crates/reader-engine/src/jxl.rs`, `crates/reader-engine/tests/jpeg_xl.rs` | Thin adapter to Kit JPEG XL probing/decoding; Reader retains synthetic RGB/alpha integration fixtures while Kit owns metadata admission, auxiliary-box skipping, tracked grid budget and native row conversion |
| `crates/reader-engine/src/display.rs`, `crates/reader-engine/src/source.rs`, `crates/reader-engine/src/avif.rs` | Width-preserving visible-row display admission/resampling (x-tap fixed-point bilinear, `WindowRenderer` tile-direct rendering of only entering strips) and one-original worker reuse; `PageSource` is the engine's strip-agnostic page boundary (dimensions, `decode_rows`, optional `row_granularity`) and `DisplayDecoder` keeps a band cache for sources that report one (trimmed to 32 MiB, farthest from the last render first), sampling rows across band seams with the same resampler, and `read_ahead` decodes the bands beyond a published window one at a time (`BandStats` separates bands a render waited for from bands read ahead); thin boundary to Kit `nova-image` AVIF/JPEG XL with independent Reader fixture/display/admission regressions |
| `crates/reader-engine/examples/tall_scroll.rs`, `docs/tall-scroll-performance.md` | Deterministic 720×25,000 CPU scroll profile and actual Mihon stable/main rendering comparison |
| Kit `crates/image`, `vendor/ferri-pdf-rav1d` | Shared bounded AVIF/JPEG XL codecs and published 1.1.2 portable decoder; Reader retains only thin adapters, fixture attribution and distribution licenses |
| `crates/reader-downloads` | Durable queue, checksummed page bundles, owned-file crash recovery, panic-safe active admission, quota, source-free offline lookup, local folder/CBZ import (CBZ archives open through Kit `nova_storage::zip`: raw central-directory record count and duplicate rejection before the zip crate's name index is trusted), a free-space reserve checked before every page commit (`set_free_space_probe` for tests), bundle removal and manifest-only status/progress hints; an installable page validator (`set_page_validator`) lets the app accept the layouts it writes, defaulting to "must decode as an image" |
| `crates/reader-catalogue` | Bounded Keiyoushi protobuf discovery (Kit `nova-net` HTTPS-only redirect policy, Android TLS configuration and capped reads), exact port mappings, signed native catalogue verification and availability states |
| `addons/README.md` | Installation instructions and external nova-addons link; no real site sources or ZIPs are bundled |
| `examples/addons/neutral-fixture` | Installable network-free fixture, bundled geometric PNGs and 320-page chapter |
| External `nova-addons/reader-addons`, `reader-tests` and tagged releases | Real site implementations, parser/QuickJS tests and licensing; installable ZIPs are generated on tags and published in GitHub Releases |
| `tests/reading_journey.rs` | Process-boundary install/read/progress/interrupted download/recovery/source-removal/offline journey |
| `tests/ui_navigation.rs`, `tests/mobile_layout.rs` | Headless reader models, phone/landscape/tablet/desktop control bounds, tab taps, accessible activation and back navigation |
| `tests/appearance.rs` | Process-boundary appearance persistence, palette application, invalid edits and theme reset preserving motion preferences |
| `tests/reader_insets.rs` | Real Window safe-area injection verifies edge-to-edge chapter/detail artwork, safe controls/chapter rows, normal-page restoration and stable OSD geometry; uses the already pinned Slint core as a dev dependency |
| `tests/reader_design.rs` | Responsive poster/list/timeline/chapter layouts, source/pin/extension actions, retained catalogue scroll, download-delete actions, tap-versus-pan reader OSD and image-menu targeting, hold cancellation, clamping, keyboard/busy/lifecycle behavior |
| `tests/downloads.rs` | Phone/desktop saved-chapter and queue controls, stable-ID selection/reordering, filters, empty states and opt-in production AppWindow preview captures |
| `tests/capture_settings.rs`, `tests/image_storage.rs` | Process-boundary capture/compression persistence, corruption recovery, independent policies and accessible settings controls |
| `tests/reader_capture_editor.rs`, `crates/reader-capture/src/selection.rs` | Phone/desktop handle, keyboard, edge-extension, menu visibility and source-selection/epoch regressions; ignored native screenshot harness |
| `tests/reader_mode_controls.rs` | Effective-mode chapter/page actions, RTL progress pointer/keyboard/accessibility behavior and right-origin progress rendering |
| `crates/reader-sources/tests/cookies.rs`, `.github/scripts/test_package_cookie_permissions.py` | Cookie permissions, isolated/concurrent sessions, cache identity, update re-enabling and native/Python manifest parity |
| `tests/reader_settings.rs`, `tests/reader_gestures.rs`, `src/reader_preview.rs` | Process-restart defaults/overrides and validation, actual touch gesture cancellation, and optional production phone/desktop raster previews (including the 96px chapter separator via `READER_UI_PREVIEW_BOUNDARIES_ONLY=1`) |
| `tests/flick_velocity.rs` | Workspace adapter compiles the actual vendored private velocity estimator/tests with upstream core/Euclid types; rejects under-observed input bursts while retaining ordinary fling cadence |
| `tests/reader_modes.rs` | Per-series mode restart persistence, explicit Automatic/legacy migration, source identity isolation and accessible short-viewport mode options |
| `tests/library_navigation.rs`, `tests/library_preview.rs`, `tests/android_back.rs`, `tests/source_paging.rs` | Pinned Library categories, stable row restoration, search/focus and retained detail context; native Library previews; automatic viewport paging, short-page fill, Retry and saved catalogue scroll |
| `tests/library_progress.rs` | Phone/desktop grid/list progress geometry from unread through complete, including small left-anchored fills |
| `tests/entry_menu.rs`, `tests/reading_preferences.rs` | Per-title gestures/category identity, durable source visibility/chapter ordering and failed-write behavior |
| `tests/reader_pixel_edges.rs`, `crates/reader-engine/tests` | Physical-pixel shared tile/page edges across fractional scroll/scales, plus AVIF fixtures and decode admission |
| `tests/license_catalog_build.rs`, `.github/scripts/test_package_license_build.py` | Actual generator feature/target/failure/frame-reuse regressions and immutable Nova function/template parity |
| `tests/about.rs` | Retained About/license scrolling, bounded-layout source links, delayed models and keyboard/system Back |
| `tests/image_download_encoding.rs` | Production Services partial-resume encoding: verified pages remain intact, new pages follow compression policy, completed bundles survive policy changes, disabled future writes retain source bytes and offline hashes survive addon removal |
| `tools/test-android-documents.py`, `tests/android-documents` | Production Java/JNI host regression with checked JNI and deterministic main-queue/lifecycle fixtures |
| `android/java/dev/misob/nova/reader/ReaderImageProvider.java`, `tools/test-android-provider-manifest.py` | Android image clipboard URI provider and packaged provider-manifest regression |
| `tools/package-addon.py` | Deterministic manifest-declared reader addon ZIP builder |
| `.github/scripts`, `.github/workflows` | Nova's copied platform packagers and PR/main-check versus tag-build split, required Reader identity/license/signing integration |
| `.github/scripts/test_package_nova_parity.py` | Immutable Nova-source parity for all three packaging scripts, masking only documented metadata/license adaptations and exact AppImage tool pins |
| `.github/scripts/test_package_linux.py`, Kit `crates/image/tests/avif_portability.rs` | Dated AppImage tool/checksum regressions and portable decoder feature-gating/runtime-dispatch regression |
| `android` | Android resources, platform document bridge and intentionally public local debug signing key |
| `flake.nix`, `Makefile` | Pinned desktop/Android/Windows Cargo environments with cargo-sweep; Node/npm in the default shell support addon fixtures in `make check`, while runtime addons use QuickJS |

## State and work boundaries

`DomainStore` stores validated JSON under `reader:state` (physically
`profile:<id>:reader:state` for the selected profile; every personal key is
profile-scoped by the whitelist in `reader_domain::profile`, see
[profiles](profiles.md)) through `nova-storage` in the Reader application directory. The internal database filename remains
`nova.redb`; the application directory isolates it from Nova. One directory is
supported per process. Restore validates the replacement before publishing it.
A full backup ([backups](backups.md)) carries profiles, addon packages and configuration (credentials in plaintext), device settings and, only when chosen, finished chapter files; it never carries cookies, sync identity or the installed-source list.

Device appearance preferences live separately under `reader:appearance` in the
same database. Missing values use Nova's defaults; palette/radius/spacing edits
and animation switches apply immediately after a successful save. Theme reset
preserves motion preferences. Portable library backup/restore leaves these
device choices intact. More/Settings route changes publish directly without
loading/deserializing library metadata before starting their transitions.
Library grid/list, sorting and minimum library/browse columns share these device
preferences and survive theme reset; so do Updates grouping, the History view and
the hidden History shelf, which are never synced. Source pins, the last-used
source and when it was opened (`last_used_at`), and the All sources language chip
live under `reader:source-browser`; manual update time is `reader:last-update`. These keys
are independent of portable library backup data.
Screenshots format/quality/resolution/folder preferences live separately under
`reader:captures`; changing them preserves original chapter data and metadata.

`reader:download-cleanup` stores a device-local, default-off retention switch and
stable excluded category IDs. More → Settings → Downloads uses Nova’s cleanup
row and category pill selection pattern. A local unread-to-read transition from
manual actions or durable reader checkpoints can remove only complete, inactive
remote bundles, under the download transfer lock. Any excluded membership keeps
the chapter; local imports are always kept. Enabling the switch, restarting and
incoming sync never sweep historical downloads. Deleted category IDs are pruned
on the next deliberate policy edit. Cleanup never changes progress or History.
The duplicate dropdown stores Ask (default), Overwrite or Do nothing. Ask opens
a nonblocking Overwrite/Add number/Cancel sheet, guarded by the current reader
generation. Clipboard actions bypass naming and duplicate handling. Desktop PNG
screencap Copy moves validated RGBA directly to the native clipboard worker;
Save and Android still encode PNG, and other formats keep their codec semantics. Desktop
saves stage in the destination directory and publish without clobbering unless
overwrite was selected; numbered retries remain race safe. Android API 29+
queries app-owned MediaStore images by exact name and relative path. API 26–28
requests and persists a read/write document-tree grant once, then uses scoped
Nova Reader/series children. Provider overwrites retain a bounded original backup
for rollback; a failed recovery reports an error and retains one private recovery
file rather than accepting further overwrites.
Saved names start with the series title, then padded chapter/page numbers.
Screencaps append an original-pixel Y offset. Selection exports use the first
exported source row on the first intersecting page; simple Device captures use
the first visible loaded page's offset.
Filename components are limited to 160 UTF-8 bytes (including duplicate suffix),
series folders to 96 bytes, preserving chapter/page/position suffixes.
Normal screencap save/copy opens the selection editor. `simple_screencaps` in
`reader:captures` defaults true and exposes immediate `(simple)` actions, including
Save image (simple); turning it off leaves Save image… and the selection actions.
The selection editor's Output panel picks a saved screencap's format and quality,
starting from the Screenshots settings each time. Simple Device captures retain
the clean pre-menu viewport snapshot. Desktop simple and selection screencaps
bound the initial export to visible loaded artwork, excluding exterior side/top/
bottom gutters while retaining internal page gaps and source black pixels.
Android retains its existing frame policy. Original-page saves stay unchanged;
`src/app.rs::media_capture_viewport` and the native capture-editor regressions
cover desktop strip/page cropping for Device and Chapter exports.
The editor freezes chapter identity,
scroll/pan, display transforms and logical rectangles, including unloaded pages,
before selection. Crop borders and handles edit source-anchored bounds across
the current chapter. The initial canvas reveals a bounded dimmed context margin
while preserving the original crop; outward drags extend the canvas without
automatically selecting a whole segment. Previews serialize decoding and retain at most one
million pixels; controls never enter exports. Advanced Device exports recompose
source pixels at frozen logical DPR; Chapter resolution uses native source scale.
Save and clipboard retain eight-million-pixel output/128 MiB working limits.
Preview/selection revisions and reader generations guard delayed publication;
filenames use the first intersecting page and actual selected vertical offset.
Capture sessions reuse validated original encoded bytes across preparation,
Preview and Save/Copy, up to 16 MiB within the compositor working allowance.
Viewer references are weak and generation-scoped; disk display/compression
variants never acquire original-source provenance. Every reuse revalidates the
source/profile scope and cancellation. Tight-memory captures drop retention and
retry with the original working allowance rather than reducing image quality.
`reader:image-limits` stores device-local encoded MiB, width/height, megapixels
and decoded MiB caps. Startup installs the validated policy before workers;
saves publish only after persistence. Decode defaults remain unchanged, while
local imports, source transfers, progressive previews, captures and reader
workers use operation snapshots. A policy revision discards stale reader
results; poster edits invalidate transient admissions and retry failed posters.
Poster fetches retain their independent 8 MiB ceiling. Encoded/display/capture
aggregate memory and cache quotas remain separate from image admission.

`reader:source-compression:<JSON package ID>` stores optional addon compression overrides.
Absence follows current global cache/download compression; explicit overrides include
independent enabled switches, formats, quality and input-aware lossless options.
Addon sync carries them outside the JavaScript settings schema. Explicit override
sync requires Reader 0.1.29 or newer on receiving peers: Reader 0.1.28 rejects
the entire addon descriptor when its new `compression` field is present. Global
inheritance omits that field and preserves the legacy wire shape; newer Reader
versions also accept older descriptors without it. Workers resolve policies by
stable package ID; existing cache files and downloads stay unchanged.

Tall pages can be stored as strips (`page_store`). The cache and downloads each
carry `split_tall_pages`, `split_threshold_rows` (default 4096) and `strip_rows`
(default 2048, a multiple of 16) inside their `reader:image-storage` objects, so
they sync field by field and take the usual device-local override; they are not
part of the device-only `policy`. The cache splits by default; downloads do not.
Splitting happens only when that store's compression is on (and, for the cache,
the disk cache too), each strip uses the store's format and quality, and pages
the codec rules keep whole (high-depth lossless PNG, a lossless JPEG transcode)
or that cannot be split are stored as before. The toggle is shown disabled, with
its saved choice kept, while a prerequisite is off. Splitting applies to pages
written afterwards: existing cache entries are replaced only when evicted, and a
completed download is never rewritten. A file saved before these fields existed
reads with each store's own default (cache on, downloads off); per-addon
compression overrides choose a codec but not where pages split. Older versions
of Reader discard split cache entries as malformed and cannot open split
downloads. The lossless JPEG crop that would make JPEG strips exact is deferred
(`docs/follow-ups.md`).

`reader:image-storage` stores independent cache/download compression switches,
formats, quality and separate `lossless_inputs` policies for new writes, plus backward-compatible cache budgets:
disk caching, separate chapter/artwork limits, decoded artwork memory and future
poster resolution. Preference changes do not rewrite existing cache files or
downloaded chapter bundles. Successful writes publish a thread-safe policy
snapshot before waking workers; hot cache hits never read the database.
Adult-source visibility lives under `reader:show-adult`, defaulting to hidden.
Browse navigation and successful addon installation share a worker snapshot of
filtered sources, all extensions and bundled icons; installation honors the
current adult-source choice immediately. Root source-list refreshes keep the
loading indicator hidden, including visibility-toggle refreshes.
`reader:chapter-order:<serialized SeriesId>` stores descending/ascending chapter
order separately for each full source-local series identity, defaulting to
descending. Both use fallible Nova KV writes; failed saves preserve the prior UI
choice. Corrupt raw values remain intact while defaults are used, until a
successful edit. These device-local preferences are outside portable metadata
backups.

`reader:content-presentation:<JSON SeriesId>` stores schema-version-1 records
with separate Default/Reader/Gallery user preference and last-known addon hint.
The complete source-local series identity scopes each record. Explicit user
choices win over the hint; Default uses Gallery only for `"gallery"`, otherwise
Reader. Source browse/search/details capture hints before domain conversion;
metadata refresh cannot replace the user's choice. Only a full details response
may withdraw a hint: listing rows (`Services::series_listing`) merely add one,
because catalogues often omit the field from listings. The records survive
restart and support offline entry, and remain outside portable backups. They do
not change reading-mode settings, IDs or download paths.

The same choice and hint are synced per entry in the `reader_presentation`
domain (`profile:reader:reader_presentation`), keyed by the JSON `SeriesId`, with
value `{"schema_version":1,"user":"default|reader|gallery","hint":"gallery"|"reader"|null}`.
It is a separate domain because `reader_domain::Series` rejects unknown fields,
so adding `presentation` to the synced series would make 0.1.35 peers refuse it;
0.1.35 peers store and relay the unknown domain unchanged. `ReaderPolicy` validates
key and value (known tokens, 64 KiB cap; newer schema versions are retained but not
applied). Writers commit the KV record and account record in one Kit transaction;
projection (`content_presentation::apply_remote`, inside the sync persistence hook)
writes KV, then refreshes the in-memory snapshots. Choices recorded before sync
existed are seeded once; untouched defaults gain no authority. Projection
re-renders cards and details through `refresh_synced` but never closes an open
reader: the next open uses the synced value.

`reader:mode-choice:<serialized SeriesId>` stores a versioned Automatic/manual
choice independently of library membership. A manual mode overrides chapter
progress modes; explicit library preferences migrate once. Automatic classifies
genres and source names using the pinned J2K rules before image decoding, with
Reader's established global fallback. Saving a series choice never changes that
global default. Details and adaptations are in [reader modes](reader-modes.md).

Fetched series posters belong to `cache_dir()/posters`, never the data directory.
Desktop uses the platform cache base plus `nova-reader`; Android uses
`Context.getCacheDir()`. `NOVA_READER_CACHE_DIR` overrides the cache root for tests
or deliberate configuration, independently of `NOVA_READER_DATA_DIR`. Disk files
use source/series/URL identity plus opaque authorization scope for encoded reuse. The worker admits 32 requests, rejects
encoded covers above 8 MiB and limits source decode to 8 million pixels/72 MiB
with display images at most 460×690. Nova Kit owns the decoded-pixel LRU
(128 MiB by default), encoded variant container and incremental disk GC.
The old 32 MiB/256-file artwork quota is replaced by an independent 1 GiB
default disk budget, using Nova's shared limit presets.
UI images share the LRU pixel buffers; visible/selected artwork and up to 48
additional recently used cards retain UI handles. Model hydration and request
cache hits reuse pixels immediately across Browse, Library, Updates, History
and details, without disk access or decode. Read-only source-availability proofs
revoke hits when a source is disabled, removed or updated, without retaining addon
archives. Four workers overlap cover requests, each capped at 8 MiB by the
native source transport. Its per-session HTTP pools retain at most eight clients
keyed by validated host/public DNS address set and reuse
connections only after fresh DNS validation; changed address sets and session
resets use fresh pools, with cookies frozen per request in Rust. One display decoder keeps full-size decode allocations
serialized, and each fetch worker may retain one small decoded completion until
UI acknowledgment. A separate cache worker performs optional encoding and
maintenance; its pending originals are bounded to 16 MiB/32 jobs, plus one active
8 MiB original. New cache encodes wait for visible requests to drain, and an
already active encoder never holds the display decoder lock. Its codec allocation
is separate from the one display decoder and retained-pixel LRU. Full queues drop
optional cache writes rather than delaying the grid. Cancellation, source/profile
proofs and generation checks still guard publication and disk writes.
Viewport changes cancel obsolete work. Chapter usage/clear is the primary Cache
page action; the artwork footer has separate usage/clear. Worker maintenance
uses Kit’s batched usage index and grouped eviction. Both actions affect owned
temporary files only; explicit downloads/imports and library data remain data.
The memory setting controls the artwork LRU; reader display-window/model
budgets remain separately bounded as described below.

Online pages belong to `cache_dir()/viewed-chapters`. The shared Kit container,
atomic writes, batched usage index and grouped GC retain them across reader
sessions. Its independent disk budget defaults to 1 GiB, with active-chapter
leases protecting pages while reading; chapter pressure cannot evict posters.
Keys include full chapter/page identity and an opaque source/profile/settings,
request and cookie scope. Every RAM/disk hit revalidates source permissions;
unknown custom transports opt out of reusable page caching. Background encoding
preserves dimensions, retains original bytes when unsupported/over budget, and
has a 32 MiB active-plus-pending encoded-byte limit. Namespace epochs prevent
cleared or cancelled work from repopulating files. Explicit downloads keep their
own manifests and are never removed by disposable-cache maintenance.

Thumbnail derivatives share this same viewed-chapter container, chapter groups
and disk quota; they have no separate disk allowance. Thumbnail usage and clear
under More → Settings → Cache filter derivative keys, preserving original viewed
pages and explicit downloads/imports. Whole chapter-cache maintenance includes
both kinds. Clear invalidates pending work and live Slint references before
removing files.

Page inventory loading reads metadata without constructing a Reader or writing
progress/History. The Controller creates its single thumbnail service only when
an overview opens. Requests cover visible rows plus one neighboring row on each
side, with one fetch/decode worker. Shared heavy-decode admission prefers the
fullscreen viewer, then visible thumbnails, then prefetch; an active decode is
non-preemptible. Live thumbnail leases include service, mailbox and UI ownership
within 8 MiB, with another 8 MiB reserved for the renderer's previous frame;
source and heavy-decode allocations remain additional and image-policy bounded.
Gallery derivatives use center-cropped squares up to 320px, adaptively reduced
to fit the admitted tiles within the live pixel budget. Their cache recipe is
separate from the existing manga derivative keys; manga pages retain whole-page
or marked tall-page top previews. Gallery density reflow first releases UI image
handles and completion leases, then revokes the old worker epoch and recycles
loaded pixels at the new fair share before fetching newly visible tiles. This
also avoids refetching loaded tiles when disk caching is disabled.
Tile errors stay local; authentication,
challenge, rate-limit and unavailable-source errors pause that source until Retry.
Generation, source authorization, image-policy revision and cache invalidation
guard publication. Local/remote progressive JXL uses a bounded prefix directly,
without re-encoding and without assuming a fixed fraction of the original bytes.
Prefixes belong only to disposable thumbnail derivatives; full-resolution entry
still verifies original availability/integrity. Thumbnail work neither creates a
chapter download nor marks it offline or read.

`addons/` contains registry state, immutable content-addressed packages and its
writer lock. Package version history rejects changed bytes at the same identity
and version. Packages are app-wide; source HTTP sessions are in-memory and
source/profile scoped. Profile switching must cancel old work. Typed settings
live under `addons/settings/<profile-sha256>/<addon-id>.json`, separately from
the sync store. Boolean/text/select/secret values are schema-validated; on Unix
settings directories use 0700 and atomic settings files use 0600. The settings UI
masks secrets. These file permissions do not imply encryption at rest.

`permissions.cookies` grants exact canonical HTTPS origins and individual cookie
names. `nova.cookies.set` constructs Secure, host-only, Path=/ cookies in Rust;
scripts cannot read jars or Set-Cookie headers, and raw Cookie headers stay
blocked. Settings changes rotate in-memory session generations while invocations
retain their matching settings/session snapshots. Old requests cannot write into
the new jar or reuse its image-cache partition. Cookie permission expansion
requires re-enabling locally and never grants execution through sync.

`chapters/` holds queue state and ordered chapter bundles. Verified page files
and atomically published completion metadata determine offline availability.
Incomplete work remains incomplete across restart; source removal does not remove
library entries, progress or completed bundles. The current service configures an
8 GiB chapter store quota. Unix download directories/files use 0700/0600, with
existing-store permission repair on open. Durable queued jobs resume at launch;
paused/cancelled jobs remain stopped. The app serializes chapter transfers; the
store retains a two-active-jobs-per-source admission cap.

Android downloads acquire Nova's `dataSync` foreground service before metadata
work begins. A process-local queue lease survives navigation and Activity
replacement; an independent one-second worker updates generic chapter/page
progress without depending on Slint frames. Last-lease completion, failure or
unwinding stops the service and releases its wake lock. The Java service and
notification-permission method are byte-equivalent to Nova at
`ee3877808764d70362a6e0b442e79e5128317abf` after Reader identity renaming.
The JNI port additionally clears Java exceptions and retries failed promotion
on the next active update. Android 15's data-sync timeout remains in force;
force-stop/process death still requires reopening Reader to resume the queue.

Recovery removes only generated pending/uncommitted/corrupt page names inside
owned hash-named job directories. It preserves verified resumable pages and
unrelated files, rejects symlink trees, and keeps quota accounting aligned with
remaining payloads. Interrupted initial-manifest publication and page-rename /
manifest-commit crash windows are covered by real killed-process tests.
`DownloadStore::progress` reads bounded atomic manifests without hashing pages.
`DownloadStore::snapshot` supplies lightweight job metadata to the Downloads page.
`move_before` persists queue order only for inactive queued jobs; `run_next` claims
the next admissible job under that same lock, so a reorder affects the next
dispatch. Batch preparation and transfers share one serialized worker path. Transient
source failures (5xx, 408, 429, timeouts, connection resets) retry in place with
bounded backoff (2s, 6s, 15s; rate limits wait five times longer) before the job
is left Failed for a manual Retry; cancel/pause/shutdown interrupt the sleep.
Completed chapters and partial/failed queue jobs remain separate. Download actions
use full chapter identities; deleting bundles preserves reading progress and
History, and local imports are protected. Detail polling patches transfer fields
in place to preserve open menus, read colors and scroll position.
The service Java/API/JNI regressions live in `tools/test-android-background.py`
and `tests/android-background/`; default packaging checks also verify its pinned
Nova provenance and required manifest permissions without an Android SDK.

Workers fetch metadata and images and decode admitted images. Only the Slint
event loop updates UI models. Download status hints come from bounded manifests
on a coalesced worker and are cached in the view; rendering never hashes page
payloads. Opening offline chapters and recovery still verify stored bytes. Navigation work uses cancellation/generation checks.
The reader covers the complete visible page window plus one prefetch page on
each side and cancels obsolete fetches. A blocked reverse gesture keeps a stable
chapter/page demand separate from the admitted resume anchor. While it waits,
the visible pages and demanded previous tail replace ordinary neighbor speculation.
Retained display/model pixel buffers use 32 MiB, with another 32 MiB reserved
within their 64 MiB accounting budget for replacement references. Display output
uses bounded visible-row windows at the requested physical width, rather than
shrinking tall-page width to fit a full-page texture. Each window is capped at
32 MiB, reduced to a share of the 32 MiB model budget when multiple pages are visible,
with 4,096 total prefetch rows and global 2,048-row strip alignment when the
budget permits; tight budgets retain an unaligned window covering every visible
row. Matching response identity and output dimensions retain overlapping Slint
images and repeated delegates through in-place tile-model changes; workers only
prepare buffers for entering strips. Unchanged dimensions avoid chapter-wide
geometry writes and scroll restoration. Refresh starts before the window boundary,
with up to 512 rows of advance coverage; the
threshold contracts when the byte budget leaves less margin. Individual
textures contain at most 2,048 × 2,048 pixels. Physical width targets use 128px buckets
capped at 16,384 physical output pixels independently of configurable image-input
limits; source pixels are never upscaled
in CPU buffers, while raster tiles fill the full phone viewport at zoom 1.
The worker's `DisplayDecoder` retains one admitted original while extracting
successive row windows from the same encoded response. Source response identity
uses the encoded `Arc`, so replacement responses and changed source/authentication
scopes cannot reuse old pixels. A different response releases that original
before decoding; a full native-size display transfers ownership instead of
retaining duplicate original pixels. Offscreen decode/prefetch waits until visible
long pages reach their final window, preserving that one-original cache across
scrolling. Reverse boundary demand instead reserves at most 16 MiB for the
previous tail from the hard 32 MiB model allowance, using trusted effective
presentation dimensions to preserve each visible page's physical-width row
coverage. Visible overscan shrinks first; only its publication releases the
previous-tail phase. Unknown visible geometry defers that phase until a visible
completion explicitly restarts scheduling. Queued tasks and publications
recheck current generation, stable identity, phase and per-page allocation.
Demand survives tail completion until crossing, reversal or explicit navigation.
If a visible replacement fails, its surviving retained bytes become a pinned
allocation floor and the previous tail uses the remaining headroom. Zero legal
tail rows run normal display admission and expose a terminal limit error.
This remains full CPU decoding, rather than
region/streaming decoding or a total process-memory bound. Decoder/native
conversion and original-RGBA-plus-window-output phases are admitted separately
within the device decoder allowance (128 MiB by default). Eight-bit RGB/gray
images decode into the final RGBA allocation and expand backward in place,
avoiding a second full-size conversion buffer. The worker also keeps a 32 MiB generation-scoped, memory-only
encoded-page cache; a miss can temporarily add a fetched response up to the
configured per-image byte limit. Larger responses are not retained in that cache.
Replacement model references are released before constructing new Slint buffers;
the reserve covers a previous image still held by the renderer during replacement.
Prefetch does not mark a chapter read. Transient page-image failures (5xx, 408,
429, timeouts and connection resets) retry automatically with bounded backoff
(0.8s, 2.5s, 6s, 15s; rate limits wait five times longer) so a source blip no
longer requires leaving and reopening the chapter. Auth, not-found, cancelled,
unsupported/corrupt images and admission-limit errors never retry. Retries are
scoped by stable chapter/page identity and deadline: surviving pages keep their
remaining backoff and consumed attempts across prepend/prune, and overview return
rebinds suspended deadlines. Removed/replaced readers and pruned pages discard
old tickets; exhausted/permanent failures stay terminal. Success resets the
per-page budget.
Resume stores logical page ID/index and
intra-page offset, independent of row windows, texture tiles and reflow.
Automatic in-place chapter crossings enter the destination's first image rather
than restoring its old resume. Subsequent checkpoints and exit flush replace
that previous saved position with this visit's position, even when earlier;
resume is not a maximum/farthest-page register. Explicit chapter opens still
restore the latest saved anchor.

The existing library long-press/right-click and detail top-right title menus
expose **Mark all read** / **Mark all unread**. The batch targets the full
source-local series, drops saved chapter pages (read state is not a reading
position), preserves actual History, and uses manual completion sync records;
marking read respects device download cleanup.
The same menus expose **Clear all chapter positions**: one durable local edit
clears every saved resume in the series (reopening starts at image one), leaving
read/unread state, History, bookmarks and other source-local identities untouched. It needs no
chapter-list refresh because it only rewrites existing resume records; the
changed resume values publish through the normal snapshot prepare.
Chapter three-dot menus also expose **Clear progress**: pending checkpoints are
drained before removing the saved page, preserving read status, History and
bookmarks. Every chapter that becomes read (manually or by finishing it) keeps
its read flag but loses its saved page, so the detail list shows no "Page: N"
marker and reopening starts at image one. The one exception is live reading
partway through an already-read chapter (for example scrolling back up into
it, `Reader::keeps_place_in_read_chapter`): it stays read but stores the place
reached, so History reopens there; Detail's "Read again" still restarts it
(`View::reread`), and reaching the last page or end clears it again. Cleared resumes use a non-empty
sentinel anchor, so older peers accept and open them at page one; the existing
resume sync record is reused, not a new domain.
Controller regressions live beside `src/reader_continuity.rs`; native menu
reachability/dispatch lives in `tests/entry_menu.rs`.
Retained previews upgrade when physical-pixel width grows without losing their
logical anchor. Shared drawing edges snap in physical pixel coordinates;
logical positions remain continuous. Device GPU overhead and peak total process
memory remain unmeasured.

The reader source runtime currently belongs to this repository. Moving its
generic envelope/runtime into nova-kit requires shared API design and Nova v1
consumer regressions; this workspace does not claim that extraction is complete.
Reader retains only its neutral executable addon fixture; catalogue compatibility
metadata and its host regressions remain in `reader-catalogue`. Real site implementations,
source-specific Node/native tests and notices live exclusively in
`nova-addons/reader-addons/` and `nova-addons/reader-tests/`. Tagged releases
generate all installable ZIPs; Reader does not depend on a sibling checkout.

## Where to look

- Nova design reuse and destination behavior: [UI design](ui-design.md).
- Delete a downloaded chapter: `src/app.rs` (`delete-download`), `crates/reader-downloads/src/lib.rs` (`remove`), chapter actions and More → Downloads. Local imports are protected.
- Saved chapter groups, bulk selection and live queue: `src/downloads.rs`, `crates/ui/downloads.slint`, `tests/downloads.rs`; durable dispatch/reordering: `DownloadStore::run_next` / `move_before` and `Services::run_next_download`.
- Background download service, notifications and interruption cleanup: `src/android_downloads.rs`, `src/download_background.rs`, `crates/reader-downloads/src/lib.rs`, `tools/test-android-background.py`.
- Viewed chapter cache, grouped eviction and pins: `src/chapter_cache.rs`, `src/app.rs`, Kit `nova-cache::gc`/`blob`.
- Tall pages stored as strips (what is written, how it is read back, why nothing else can tell): `src/page_store.rs`, `src/page_store/strips.rs`, engine `PageSource`/band cache in `crates/reader-engine/src/{source,display}.rs`, cache writes in `src/chapter_cache.rs::compress`, download writes in `src/services.rs::fetch_download_page`, settings in `src/image_storage.rs`, tests `tests/page_store_boundary.rs` and `tests/page_transparency.rs`.
- Image admission, device persistence/reset and live policy regressions: `src/image_limits.rs`, `tests/image_limits_backend.rs`; source streaming bounds: `crates/reader-sources/tests/streaming.rs`.
- Cache quotas, disk master switch and worker snapshots: `src/image_storage.rs`, `crates/ui/image-storage-settings.slint`, `tests/image_storage.rs`.
- Shared Nova settings controls: Kit `slint/cache-settings-controls.slint`; Nova keeps SettingsRow/search/sync/keyboard wrappers.
- Immediate chapter entry, worker preparation and first-content fade: `src/reader_open.rs`, `tests/reader_entry.rs`, `crates/ui/app.slint`.
- Cover cache placement, concurrent fetches and source revocation: `src/paths.rs`, `src/artwork.rs`, More → Settings → Cache.
- Real site addons, their tests and tagged release downloads: [nova-addons](https://github.com/misobarisic/nova-addons). Reader keeps only `examples/addons/neutral-fixture`.
- Progressive JPEG XL reader previews and strict final handoff: `src/reader_progressive.rs`, `src/app.rs::image_worker`, `src/chapter_cache.rs::read_with_chunks`; neutral streamed-EOF regressions live with the progressive module.
- Addon icons and source filters: `src/addon_icons.rs`, `crates/ui/addon-icon.slint`, `src/source_filters.rs`, `crates/reader-sources/src/package.rs`; optional `manifest.icon` must name a declared PNG asset. Multiple/include/exclude groups show as pills in Browse's filter sheet (`FilterSet::groups`) and keep their typed JSON output.
- Shared cache implementation/settings: Kit `crates/cache` and `crates/ui-base/slint/cache-settings-controls.slint`; Reader `src/chapter_cache.rs`, `src/image_storage.rs` and `tests/image_storage.rs`.
- Library location, pinned controls, search and per-title actions: `src/library_navigation.rs`, `crates/ui/library-page.slint`, `crates/ui/library-search.slint`, `crates/ui/entry-menu.slint`, `tests/library_navigation.rs`, `tests/entry_menu.rs`.
- Automatic source paging and Retry: `src/app.rs` (`browse`/`complete_browse`), `crates/ui/app.slint`, `tests/source_paging.rs`.
- Backup, import, merge/replace, undo and the Data page: `docs/backups.md`, `src/backup/`, `src/backup_actions.rs`, `crates/ui/backup.slint`.
- Profiles (identity, key scoping, switching, deletion, picker): `docs/profiles.md`, `src/profiles.rs`, `src/profile_actions.rs`, `crates/reader-domain/src/profile.rs`, `crates/ui/profiles.slint`.
- Deferred, designed-but-unscheduled work (e.g. the lossless JPEG crop for tall-page strips): `docs/follow-ups.md`.
- Per-entry Reader/Gallery preferences and optional addon hints (local KV plus the `reader_presentation` sync domain): `src/content_presentation.rs`, `src/sync.rs`, `src/services.rs::Services::{series,series_listing}`, `tests/content_presentation.rs`, `tests/presentation_sync.rs`.
- Automatic/manual reader mode and per-series overrides: `src/reader_modes.rs`, `docs/reader-modes.md`, `tests/reader_modes.rs`.
- Effective-direction chapter/page controls and progress slider: `crates/ui/reader-osd.slint`, `tests/reader_mode_controls.rs`.
- Paged gallery dragging, zoom/pan and fitted image geometry: `crates/ui/reader-pager.slint`, `crates/ui/app.slint`, `src/reader_controls.rs`, `tests/reader_pager.rs`.
- Shared page/image overview grid, focal gallery density changes (`overview-zoom-requested`), reader return/resize anchors and native previews: `src/page_overview.rs`, `crates/ui/app.slint`, `crates/ui/page-overview.slint`, `crates/ui/page-thumbnail.slint`, `tests/page_overview.rs`.
- Gallery-first routing, groups, explicit Metadata/Download and collection progress: `src/gallery_presentation.rs`, `src/reader_open.rs`, `src/reader_controls.rs`.
- Reader-free inventory and selection-time original integrity: `src/page_inventory.rs`, `DownloadStore::page_inventory`, `tests/page_inventory.rs`.
- Lazy thumbnail admission, gallery square recipes/adaptive reflow recycling, direct progressive JXL prefixes and pixel leases: `src/page_thumbnails.rs`, `src/decode_admission.rs`, `crates/reader-sources/src/lib.rs::Source::fetch_page_until`.
- Shared chapter/thumbnail disk quota and independent thumbnail clear: `src/chapter_cache.rs`, `src/thumbnail_cache_settings.rs`, `crates/ui/image-storage-settings.slint`.
- Desktop Ctrl+wheel, Android pinch, source-point anchoring and magnified tap behavior: `crates/ui/reader-zoom.slint`, `crates/ui/reader-context-menu.slint`, `src/reader_controls.rs` (native production-controller regression).
- Pinned Slint pinch/lift corrections, Flickable velocity observation window and provenance: `vendor/i-slint-core/NOVA_PATCH.md`, `vendor/i-slint-core/input.rs`, `vendor/i-slint-core/items/flickable/data_ringbuffer.rs`; real reader touch regression: `tests/reader_gestures.rs`.
- Unfinished chapter page labels and manual read/unread History behavior: `src/app.rs` (`render`, `toggle_chapter_read`), `tests/reader_design.rs` and app unit tests.
- Adult visibility and per-series chapter order: `src/reading_preferences.rs`, `tests/reading_preferences.rs`.
- AVIF image admission/encoding: `crates/reader-engine/src/avif.rs`, `src/reader_export.rs`, `src/image_storage.rs`.
- Stable ARMv7 AVIF build fix and provenance: Kit `vendor/ferri-pdf-rav1d/PATCHES.md`; AppImage download pins: `docs/packaging.md`.
- Tall-page row windows, bounded boundary/entry demand and display admission: `crates/reader-engine/src/display.rs`, `src/app.rs` (`request_images`), `src/reader_scroll_admission.rs`, `src/reader_handoff_tests.rs`.
- Read-ahead (band decoding of split pages while idle, encoded fetch ahead, page depth in paged modes, next-chapter metadata lead): `src/reader_band_read_ahead.rs`, `src/reader_scroll_admission.rs` (`read_ahead_pages`), `src/reader_continuity.rs` (`near_continuation`), `src/app.rs` (`image_worker`), measurements in `docs/tall-scroll-performance.md`.
- Explicit previous-last/next-first chapter entry: `src/reader_chapter_navigation.rs`; normal Detail/History resume remains `src/reader_open.rs`.
- Tall-page CPU performance and Mihon comparison: `docs/tall-scroll-performance.md`, `crates/reader-engine/examples/tall_scroll.rs`, `tests/split_decode_cost.rs`.
- Reader tile seams and physical-pixel edges: `crates/ui/app.slint`, `tests/reader_pixel_edges.rs`.
- Series/chapter layouts and reader controls: `crates/ui/library-cards.slint`, `crates/ui/detail-hero.slint`, `tests/reader_design.rs`.
- Updates/History grouping, sticky headers and the History shelf: `src/timeline_groups.rs`, `src/timeline_page.rs`, `crates/ui/timeline-page.slint`, `tests/timeline_page.rs`, previews in `tests/timeline_preview.rs`.
- Browse sources, catalogue, filter sheet and search across sources: `src/browse_page.rs`, `src/source_browser.rs`, `crates/ui/browse-page.slint`, `tests/browse_page.rs`, previews in `tests/browse_preview.rs`.
- Left-anchored library reading progress: `crates/ui/library-cards.slint`, `tests/library_progress.rs`.
- Image filenames and duplicate handling: `src/export_names.rs`, `src/reader_export.rs`, `src/android_documents.rs`, `crates/ui/save-conflict-sheet.slint`.
- Download retention/excluded categories: `src/download_cleanup.rs`, `crates/ui/download-cleanup-settings.slint`, More → Settings → Downloads; native preview/accessibility tests in `tests/download_cleanup_settings.rs`.
- Reader image/screencap actions and export settings: `crates/ui/reader-context-menu.slint`, `crates/ui/capture-editor.slint`, `src/reader_capture_editor.rs`, `src/reader_export.rs`, `crates/reader-capture`, `src/capture_settings.rs`, More → Settings → Screenshots & saved images.
- Save image sources/output, the Save image… sheet and the simple save subtitle: `src/reader_save_image.rs`, `crates/ui/save-image-sheet.slint`, `crates/ui/save-output.slint`, `tests/save_image_sheet.rs`.
- Reader OSD and context menu show/hide motion: `crates/ui/reader-osd.slint`, `crates/ui/reader-context-menu.slint`, `tests/reader_motion.rs`.
- Addon settings scrolling, catalogue and custom fields: `crates/ui/source-settings.slint`; `tests/settings_slider_gestures.rs`.
- Per-addon compression inheritance and switches: `src/source_compression.rs`, `crates/ui/source-compression-settings.slint`, source/extension Settings.
- New cache/download image compression and independent input-aware lossless policies: `src/image_storage.rs`, `crates/ui/image-storage-settings.slint`, More → Settings → Cache.
- Appearance controls, palette and device persistence: `crates/ui/theme-settings.slint`, `crates/ui/src/appearance.rs`, `src/appearance.rs`.
- Android picker context, callbacks and host JVM regression: [Android documents](android-documents.md).
- Android/desktop shells and touch layout checks: `crates/ui/navigation.slint`, `crates/ui/app.slint`, `tests/mobile_layout.rs`.
- Build-time license catalogue and source inventory: `build.rs`, `about.toml`, `assets/open_source_*`, `tests/license_catalog_build.rs`.
- About license/source viewer: `src/about.rs`, `crates/ui/about.slint`, `tests/about.rs`.
- Run/build/install and CLI: [README](../README.md), [packaging](packaging.md).
- Source manifest, HTTP permissions, limits and error contract: [source API](source-api.md).
- Cookie grants and session isolation: `crates/reader-sources/src/package.rs`, `crates/reader-sources/src/network.rs`, `crates/reader-sources/tests/cookies.rs`, `src/sync_addons.rs`.
- Discovery, actual upstream IDs and signing trust API: [catalogues](catalogues.md).
- Implemented behavior versus outstanding plan gates: [implementation status](implementation-status.md).

Reader cache and chapter downloads offer one choice per codec (JPEG, WebP, AVIF,
JPEG XL); UI index 3 maps to stored lossy JXL index 4 in both groups. Each
persisted `lossless_inputs` flag defaults false, with a WebP/JXL-only switch
directly after quality. Known lossless PNG
and WebP inputs bypass quality when enabled; JPEG and unrecognized inputs keep
using quality. High-depth PNG originals are retained to avoid RGBA8 downcasting.
AVIF remains lossy: the shared ravif/rav1e encoder does not implement lossless
AV1, including at quality 100 (Kit verifies decoded pixels).
Legacy cache/download JXL lossless migrates on load to JXL plus this flag; the next
settings save persists it. Both groups retain separate `jxl_lossless_jpeg` flags
for reversible JPEG coefficient transcoding. Artwork classifies original bytes
before resizing to PNG. Source-scoped disk keys remain stable, so changing any
compression setting affects future writes and preserves existing variants.

Addon settings list all declared source/language catalogue entries separately from addon-defined configuration. Host switches default to enabled, persist independently by package/source ID, and affect Browse inventory rather than whole-addon authority. Hidden entries remain available in the settings list for re-enabling.

Android touch feedback binds `AppWindow.touch-menus` to Kit's `ControlInput`,
matching Nova. The real platform sets it independently of window width, so wide
Android layouts suppress card hover lift too, while narrow desktop windows keep
mouse hover. Rows, navigation, hero actions and context sheets use Kit's exact
`ControlTouchArea.feedback-active` policy; Flickable cancellation ends touch
feedback. Reader image/card gestures keep their existing drag/hold cancellation.

Library follows Nova’s control order: category rail (the default tab is “All”),
then sorting and the grid/list/search icons. The category/sort/search controls
reuse Nova's sibling overlay and native `LibraryScroll`; only the hero scrolls
away. The category lane pans horizontally, outside the vertical content scroller.
Per-category row positions survive navigation only during the current app
session. Only the last selected category survives restart in the device-local
`reader:library-navigation:v1` record, outside backups and sync.
Anchors use complete serialized SeriesIds and row fractions, independent of cover
URLs; restore resolves the current index and clamps after layout/resize. Scroll
callbacks update memory without storing row positions. Category changes queue
background writes; navigation/lifecycle flushes preserve the last selected
category. Search retains its existing narrow and wide reveal placement and
keyboard viewport handling. System Back on the unobstructed Library root captures
and flushes state, then yields to Slint Android's native Activity-finish fallback.
Search, menus and dialogs consume Back first; desktop Escape/Backspace retain
ordinary navigation. `tests/android_back.rs` verifies dispatch acceptance rather
than introducing a separate exit route or JNI bridge.

`reader-domain/src/library_status.rs` adapts Nova's actual `WatchStatus` and
progress bucket decision order to Reading, Plan to read, Completed, Dropped and On hold.
LibraryEntry.reading_status defaults to Auto for old metadata and participates in
backups. Manual pins override the derived bucket without changing chapter
progress/history; Back to automatic restores derivation. Built-in filters appear
ahead of user categories. Library/detail title menus share Change status sheets.

`crates/ui/status-menu.slint` is copied directly from Nova, with the host import,
reading labels and omission of its video-only New episodes choice adapted. The
action codes match Nova; both library and detail context menus use this picker.
Reading and Completed may also be pinned explicitly, without modifying chapter progress.


`src/metadata_refresh.rs` adapts Nova's saved-episode scheduler: check each minute,
sequential batches of four at five-minute intervals, recently read titles every
30 minutes and older titles every six hours. Half the budget protects overdue
recent titles; the rest rotates through older/unvisited entries. Only foreground
Library/Updates/History run work; reading, detail, settings, minimization, Android
pause and foreground requests cancel it. User-pinned Reading remains eligible;
Plan to read, Completed, On hold and Dropped pins opt out. Automatic caught-up
entries remain eligible. Deadlines persist as `reader:metadata-refresh:<SeriesId JSON>` in Nova KV, including bounded failure retry and manual-refresh cooldowns.
Fetch and publication are separate: source revocation, removal/status changes,
empty results and a newer foreground metadata baseline reject stale results.
Progress, history and disappeared chapters survive successful merges.

`Series.publication_status` now retains normalized addon publication state
(Unknown/Ongoing/Completed/OnHiatus/Cancelled), defaulting Unknown for old data.
Automatic Completed requires source Completed plus every known chapter read;
ongoing/unknown caught-up entries stay Reading. Explicit Completed overrides the
source/progress decision. It is independent of the user's `reading_status` pin.


Capture format indices append JPEG XL lossless/lossy at 5/6, preserving 0–4.
`CapturePreferences.jxl_quality` defaults to 85 for older `reader:captures` values,
persists independently of JPEG/WebP/AVIF and applies only to lossy JPEG XL. Shared
Kit encoding produces `.jxl` / `image/jxl` screencaps at either capture resolution;
Reader decode admission is checked before delivery. Original page exports keep
source bytes. Screenshot quality sits immediately after format. Cache settings
follow chapter cache (including thumbnail maintenance) → cache compression → artwork/memory → downloads; each
compression quality row directly follows its format, before optional transcode.

Artwork cache usage/clear/status are owned by `ImageStorageSettingsPage` beside
artwork disk/resolution/memory controls, before downloads. `MorePage` forwards
its existing artwork usage/status/busy properties and clear callback into that
page; chapter maintenance retains its independent callback and busy state.
`tests/capture_settings.rs::render_capture_and_cache_settings_previews` is an
opt-in software-rendered phone/desktop capture helper for reviewing these pages.

Legacy side-navigator values remain readable in saved preferences for compatibility; the UI uses only the bottom progress slider.

Reader device defaults use `reader:reader-settings`; series layout/orientation
overrides use `reader:reader-settings-series:<JSON SeriesId>`. Modes retain the
existing `reader:mode-choice:` namespace, with new Default inheritance and a
gapped long-strip choice. The native image worker freezes `DisplayOptions` for
every task and rejects results for an old recipe. Engine presentation maps crop,
RTL spread splitting, rotation/reverse and color filtering without modifying
source/cache bytes or expanding the existing output/decode memory limits.

Desktop Webtoon (including long strip with gaps) fits to a device-local
`desktop_webtoon_width` cap in `reader:reader-settings` (720 logical px by default,
320–1600 adjustable in the global and in-reader Webtoon layout form). Android
ignores this cap. Side padding reduces the capped column, not the uncapped
window, so every padding edit works on wide monitors; zoom scales the result.
Paged modes retain their independent 1100px fit cap. Geometry/persistence
regressions live in `tests/reader_settings.rs`; mode/UI comparison with Suwayomi
is in `docs/reader-modes.md`.

Continuous reading prepares the previous chapter near the active head and the
next chapter near the active tail, including when opening a middle chapter
from detail/History. Both directions follow ascending parsed chapter numbers, so a newest-first
`source_order` listing cannot invert navigation; `source_order` is only the
fallback for unnumbered chapters. Chapter-list display sorting stays independent.
Prepending shifts the live anchor/model indices and scroll distance without
resetting delegates or copying existing pixels; suffix pruning protects every
visible page, just as forward prefix pruning does. Native regressions beside
`src/reader_continuity.rs` open chapter 2 and exercise 2 → 1 and 2 → 3 in
Webtoon/LTR/RTL/vertical modes with both display sort settings.

Webtoon scroll admission in `src/app.rs::Reader::loaded_scroll_offset` stops at
an unloaded page boundary instead of anchoring into its estimated height. The
forward viewport may reveal a 48px loading edge; reverse scrolling waits at the
start of loaded artwork. Real geometry/pixels release the boundary on the next
scroll, without consuming a wheel/touch overshoot or moving past unread art.
Ordinary Webtoon gestures also stop at contiguous final decoded-row edges.
An intra-page edge demand requests an adjacent overlapping strip while keeping
the admitted viewport covered; completion never replays wheel/touch overshoot.
Float round trips stay just inside the covered physical row. Actual lack of
even one spare output row follows the established terminal admission error path.
Explicit seek/resume may still select an immediate loading anchor, where later
ordinary gestures wait for its replacement pixels. Chapter gaps and the 48px
unknown-page loading edge remain separate coordinate cases.
Terminal image errors remain visible and skippable. Explicit seek/resume anchors
remain authoritative; no scroll hot path reads KV.

Previous/Next chapter buttons use stable edge intent in the retained model.
Previous prepares the destination's last paged image/half or exact Webtoon tail
viewport; Next prepares its first image/start viewport. Short entry images are
measured and retained sequentially across the needed viewport, sharing at most
16 MiB of handoff headroom after old-visible floors within the hard 32 MiB model.
The reader keeps its admitted viewport until actual effective geometry and pixels
permit entry. Settling zoom reconciles that live source anchor for byte planning
without committing destination progress. Terminal errors clear pending intent;
transient retries preserve it. Mode remains stable; every chapter enters at 1×,
and explicit page input cancels pending chapter intent. These buttons ignore arbitrary saved destination
resumes; ordinary Detail/History opens keep their normal resume policy.
The target is captured relative to the chapter active at the button press.
Active zoom first settles to reconcile the actual old source viewport, then
returns to 1× before destination layout. The same preparation runs before an
asynchronous entry commits; programmatic settlement cannot cancel its intent.
Scroll frames from a running fling do not cancel it either; a new press on the
artwork (`reader-press`) does. Paged swipes commit their page step on release.
Opening Pages cancels pending chapter/boundary intent and snapshots settled zoom,
so Back restores the original reader route even if destination pixels arrive late.
Collection viewers route directional group actions through their existing group
overview controller instead of manga continuation.
Collection viewers are photo viewers: always black, chrome hidden on entry, any
tap toggles the chrome (no edge tap zones), and an `N / M` counter stays visible
while the chrome is hidden. A thumbnail filmstrip in the viewer is deferred: the
thumbnail service publishes only to the overview model while section 23 is open.

Continuous reading attaches prepared chapters to the same page model in Webtoon and paged modes.
Continuation uses the same metadata-only stable page inventory as direct entry,
including usable partial downloads and individually unavailable offline pages;
original verification and geometry probing occur only for selected image work.
Every prepend/prune explicitly restarts current-generation image scheduling in
all modes. Returning from Pages clears cancelled request markers and explicitly
resumes image and continuation scheduling while retaining existing pixels.
Paged frames use local image pan coordinates; Webtoon's chapter-stacked resume
offset never translates a paged frame. Both viewport dimensions bind directly
to the live reader frame, including its initial mount. Paged zoom scales the contained image
after fitting and requests matching physical-pixel detail. At normal zoom,
horizontal dragging follows LTR/RTL order and vertical paging follows vertical
drags; zoomed dragging pans artwork without turning pages.
Ctrl+wheel on desktop and native two-finger pinch on Android zoom from 0.5× to
8×. The fitted-width cap applies before magnification; source coordinates stay
anchored beneath the cursor or moving pinch midpoint, using the mounted frame's
real dimensions and contained-image margins. The display/decode budgets remain
unchanged. Above 1.01×, single taps only toggle controls, bypassing all tap zones
and inversion choices; double tap still toggles fit/2× and long press opens the
image menu. Pinch entry cancels pending taps/holds/gallery motion, and its
remaining finger is consumed through release.
Webtoon zoom follows a stable page and local image fraction rather than scaling
fixed chapter gaps as image content. Animation frames share one cached page
index; settled width changes reproject rows/boundaries in place before restoring
the source point. Duplicate logical-size reflow callbacks refresh detail without
restoring stale pan or replacing retained tile handles.
Per-page chapter/source ownership keeps caches, export filenames and progress
canonical; old segments are pruned in place toward previous/current/next,
retaining additional segments while the viewport genuinely spans tiny chapters.
Tail eligibility follows the visible bottom instead of requiring a short tail
to become the top anchor. Offscreen excess is pruned first while preserving the
nearest neighbors. Metadata work uses one actual worker and one latest pending
request: reverse demand cancels opposite speculation and replaces pending work;
an uncooperative cancelled provider retains the sole slot until it returns.
Per-chapter directional errors remain independent, and a failure immediately
advances the other eligible edge. Tokens/nonces discard superseded results,
with a fresh generation rejecting old indexed tasks. A 96px boundary row names
both chapters in Webtoon, with its arrow centered vertically; paged crossings
use the same 96px caption extent. Manga completion follows Mihon's selected
source-image policy: selecting the final image counts, including its first split
half and loading state; an explicitly failed image cannot newly complete. Webtoon
uses the active chapter's last image when fully visible or passed at the top of
the viewport. Gallery completion remains explicit. Reader selection provenance
keeps neutral Pages/zoom restoration from creating progress, history or completion
across checkpoints and exit. Periodic checkpoint storage remains off the UI thread.

Reader sync stores one record per source-scoped identity under
`profile_domain(<profile>, "reader_<kind>")` (`reader` for the first profile; see [profiles](profiles.md)), never a replicated whole-state
snapshot. Category IDs survive renames. Existing bookmark identities migrate
without changing backup schema: their immutable chapter/page/offset tuple is the
record key, so note edits retain identity and removal retains a tombstone.
`DomainStore::update_checkpoint` distinguishes delayed resume saves from
`update_manual_progress`; read/unread actions have their own register. The
persistence hook commits state, records and materialized baselines through one
Kit storage transaction even when networking is disabled. Remote projection
uses `UpdateKind::Projection` to avoid publishing received changes as local
edits. Drain `ProgressWriter` before projection or exchange.

Sync uses Kit’s generic engine with video features disabled. `NR1` invite tickets
and the `nova-reader/*` protocol namespace cannot pair with Nova’s `NV1`/`nova/*`.
`Services::open` serializes process initialization and retains one registry for
foreground and headless consumers of the same application directory. Sync is
opt-in; durable local changes are recorded without binding a network endpoint.

`reader:sync-settings-baseline` stores shared field baselines and
`reader:sync-settings-overrides` stores device-local override membership. The
shared scope badge/modal compares local and shared values; releasing an override
follows the current shared record immediately. The same `reader_settings` domain
carries `reader:mode-choice:<JSON SeriesId>` mode records and individual
`reader:reader-settings-series:<JSON SeriesId>/paged|webtoon/<field>` layout
records, including series outside the library. `null` is an explicit shared reset
to global inheritance, not record deletion. Serialized IDs are parsed before
JSON-pointer suffixes, so slashes in IDs do not change identity. Legacy persisted
nondefault choices seed once; untouched defaults/inheritance gain no authority.
Paths, credentials, cache quotas, image admission/cleanup policies, orientation,
brightness, fullscreen/cutout, keep-awake, page flash and desktop Webtoon width
stay local. In-reader General/Filters edit global defaults; mode/layout edits
are per-series. Their scope badges expose portable field overrides, while local
safety settings are inspector-only. Addon settings/compression labels describe
addon sync rather than claiming settings-domain device override controls.
Effective reload refreshes loaded series caches and the active mode, then
reapplies reader layout/platform preferences without echoing remote edits.
Addon source permissions remain device-local; received executable packages pass
Reader validation and wait for local approval when new permissions are needed.

Reader settings scrollers use Nova’s raw Flickable guards against
`SliderGesture.active` and `SliderGesture.hovered`, releasing on pointer
cancellation, panel close and settings navigation.
`tests/settings_slider_gestures.rs` covers actual horizontal touch/mouse drags,
outside release/cancellation and resumed vertical scrolling;
`tests/reader_layout_preferences.rs` checks mode/icon alignment and compact More.
Local preference saves diff the editor’s previous/next values against current
durable data, so an edit cannot republish unrelated stale fields after a remote
projection. Addon mutation/publication uses one reconciliation gate, keeps
permission denials local and preserves disjoint field edits.

The native, storage-free addon validator is
`crates/reader-sources/examples/validate-package.rs`; the shared addon release
repository uses it alongside Nova’s provider validator to enforce each app’s
contract. Canonical distribution sources and tag packaging live in
[`nova-addons`](https://github.com/misobarisic/nova-addons), with `reader-addons/`
separate from the existing video `addons/` paths. Local example fixtures and old
ZIP URLs remain for regression coverage and existing catalogue links.
