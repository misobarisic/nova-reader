# Profiles

More → Profiles creates, renames, selects and deletes personal profiles. A profile
owns the person-specific parts of Reader: library, categories, reading progress,
history, bookmarks, reader/appearance/display settings, per-title choices
(chapter order, reading mode, Reader/Gallery presentation), source visibility and
the per-addon settings and catalogue switches. Existing installations migrate
into the first profile, **Default**, without losing anything. A new profile
starts empty with normal defaults.

Phase 1 covers create, rename, delete, switch, the optional startup picker and
sync of profiles and their data. PINs, restricted/kids mode and per-profile CLI
flags are phase 2; `profiles::DevicePreferences` and the `profiles` directory
rows are the places to add them (unknown JSON fields in `profiles` rows are kept).

## Identity

* The first profile has the id `reader`. Reader's record domains were already
  `profile:reader:reader_*`, so existing data and 0.1.35 peers need no sync
  migration. Its row is seeded as `{"name":"Default"}` with the neutral version
  `0.0.<device>`, so a device that joins later can never overwrite a name another
  device chose. The word "Default" is translated when shown.
* New ids are `p{device_hex}_{sequence_hex}` from the account device id and a
  durable local sequence (`try_allocate_local_sequence("profiles", 1)`). Ids match
  `[A-Za-z0-9_-]{1,64}`; names are trimmed, at most 64 characters, no control
  characters.
* Account sync domains: `profiles` (id → `{"name":…}`) and `profile_deletions`
  (id → `"1"`). Kit refuses every later live write to `profile:{id}:*` once a
  deletion marker exists, so a deleted id is never reused or revived. If peers
  delete every profile while this device is offline, `profiles::initialize`
  creates a fresh profile id.
* The selection and **Open last profile on startup** (default on) are device-local
  at `profiles:device:v1` and never synced.

## Storage

Storage cannot be reopened (one directory per process), so switching re-keys.
`crates/reader-domain/src/profile.rs` owns the whitelist and the active pointer;
personal keys live at `profile:{id}:{key}`, everything else passes through.

| Personal (per profile) | Device-global |
| --- | --- |
| `reader:state`, `reader:show-adult`, `reader:chapter-order:`, `reader:reader-settings`, `reader:reader-settings-series:`, `reader:mode-choice:`, `reader:content-presentation:`, `reader:source-browser`, `reader:library-navigation:v1`, `reader:last-update`, `reader:disabled-catalogs:`, `reader:appearance`, `reader:sync-settings-baseline`, `reader:sync-settings-overrides`, `reader:download-cleanup` (category ids) | image storage/limits, captures, source compression, metadata-refresh deadlines, category sequence, cache GC, `sync:*`, `profiles:device:v1`, installed addon packages, downloaded chapter files, `reader:installed-sources:v1` |

`personal_and_device_global_keys_are_pinned` fixes the split. Scoping is off until
`DomainStore::open` enables it, so bare-`nova_storage` code keeps the old layout.
`DomainStore::open` first moves every legacy unprefixed personal key to
`profile:reader:<key>` in one transaction (`migrate_legacy`): idempotent, crash
safe, and an existing destination always wins.

`ReaderState.sources` (installed-addon descriptors) describes device-global
packages, and its only reader is the "first install enables the addon" rule. If it
were per profile, an update installed from a second profile could re-enable an
addon whose permissions had expanded. It is therefore split out of each profile's
state into `reader:installed-sources:v1` and overlaid on every read.

## Sync

Each profile has its own record domains, `profile:{id}:reader_{kind}`, including
`reader_settings`, `reader_addons` and `reader_presentation`. Every paired device
synchronizes every profile. Library kinds of an **unselected** profile are
materialized into its durable state in the background without touching the
screen; its settings, presentation and addon records wait until it is selected
(selection runs a seed, a projection and `sync_addons::publish`). Unknown
domains are accepted and relayed by 0.1.35 peers, so no ALPN bump was needed.
Addon packages and the enabled flag stay device-global in this phase: switching
profiles republishes the device's actual state as the new profile's intent, and a
peer's removal of an addon is applied only if no other live profile still lists it.

## Switching

`Controller::select_profile` (UI thread): flush reading progress and the Library
location, invalidate all generation-scoped work, persist the selection, swap
`DomainStore` (under its serial lock), point the registry at the new profile,
reset entry-presentation and reader-settings caches, rebuild `View`, reload
appearance/source-visibility/Library location/download-cleanup, clear the Library
search, navigate to the Library and render. A failed selection write leaves the
old profile untouched. Workers started with `profiles::spawn` stay pinned to the
profile that launched them, so a slow browse/details/hint worker finishing after a
switch writes into its own profile, never the new one.

Downloaded chapter files are shared; each profile lists only downloads of series in
its own state, and no file is deleted because another profile still references it.

## Deleting

Delete asks through the shared confirmation sheet, then retires the id, tombstones
its records, removes `profile:{id}:*`, its sync baselines and its addon-settings
directory. At least one profile always remains. Deleting the selected profile
switches to a survivor first. If a peer deletes the selected profile, this device
falls back to a survivor and shows the picker.

## Source registry

The registry keeps its original `default` identity for profile `reader`
(`profiles::registry_profile`), so `addons/settings/<sha256("default")>/` and the
session partitions need no move; other profiles use their id.

## UI

Section 24, a MenuLink above Settings. With **Open last profile on startup** off,
the picker is required on launch: Back and every other destination are blocked
until a choice (Slint `go-back` and `Controller::navigate` both enforce it).
