# Backup and import

Open **More → Data**. Choose profiles and components, then export one
`.nova-reader-backup` file with the system document picker. To import, choose a
file, decide where each profile goes, review every change and apply it. The last
three imports can be undone. The file format and its limits come from Kit's
`nova-backup`; Reader decides what the components are and how they merge.

## What an export holds

**Per profile** (each can be left out):

| Component | Contents |
| --- | --- |
| Library | Library entries with category assignments (by category name), reading status and per-title reader preferences, plus the cached title and chapter records they need |
| Categories | Category names. Names match case-insensitively; IDs are device-local and never travel |
| Reading progress, History, Bookmarks | The records of the profile's state, with the cached chapters they refer to |
| Reader and app settings | `show-adult`, reader settings, appearance, source shortcuts, download cleanup (category IDs carried as names), last update time and the state's reader preferences. Settings equal to their defaults are left out, and count as absent when compared |
| Per-title settings | Chapter order, per-series reader settings, mode choice and content presentation |
| Addon settings | Each installed addon's non-secret settings and hidden catalogues for that profile |
| Addon credentials | The addon's secret settings, **in plaintext**. Always included in an export; deselect the component on import to skip it |
| Per-device sync overrides | The profile's `reader:sync-settings-overrides` |

**Device** (stored once in the archive entry `device`):

| Component | Contents |
| --- | --- |
| Addon packages | The installed packages (`packages/<sha256>.zip`) with their enabled flag. An import installs them **disabled**: execution permissions are never granted from a file |
| This device's settings | Image storage and limits, screenshot options, per-addon compression and the "open last profile" choice. The selected profile never changes |
| Downloaded chapters, Local imports | Opt-in, **off by default** on export and on import. See below |

Never exported: sync identity, paired devices and every `sync:*` row, the
installed-source list (`reader:installed-sources:v1`), metadata-refresh and
cache bookkeeping, local sequences, retired profile IDs, caches, cookies, and the
last Library tab and scroll position (`reader:library-navigation:v1`). There is no
encryption and there are no scheduled backups. The export and the addon pages
warn that credentials are saved in plain text.

## The file

A ZIP: `manifest.json` (format `nova-reader-backup`, version 1, creation time,
app version, archive id, profiles with their components, packages),
`profiles/<id>/<component>.json` (`{"version":1,"records":{"kind\0identity":
value}}`, deflated), `packages/<sha256>.zip` and `blobs/…` (stored). Every file
has a SHA-256 and a record count in the manifest. Records are keyed by semantic
identity, never by a sync version. Opening an archive rejects duplicate or unsafe
entry names, symbolic links, bad checksums, wrong counts, records that would leave
a dangling reference and invalid packages, and expands at most 128 MiB (32 MiB per
component, 200,000 records, 4,096 entries). Components of a newer version are
listed as unsupported and skipped.

An export reads state, rows and records together under the domain lock, then
writes to a temporary file next to the destination, flushes it and renames it
into place. On Android the file is staged in the app cache and streamed to the
chosen document.

Legacy `.json` metadata backups still import: a file without the `PK` signature
opens as a single profile that **replaces** the profile on screen. Its
installed-addon list is ignored.

## Downloads and local imports

Finished chapters only (partial downloads are queue state). Each chapter record
holds the chapter identity and the id, size and SHA-256 of every page. The pages
are the files the download store already holds, copied **as they are** as stored
entries `blobs/downloads|local/<chapter key>/<file>`; strip containers and
compressed pages are never decoded or re-encoded. Page URLs and request headers
are not exported (they can carry credentials). Up to 262,144 files fit.

Importing adds a chapter only if it is missing, staged in its own directory,
verified page by page against the recorded checksum and the page validator, and
moved into place last, so a chapter appears whole or not at all. A damaged
chapter is reported and skipped. Existing chapters are never replaced, and
**Replace** never deletes a file. The only way chapters leave is undoing the
import that added them.

## Importing

Each archived profile maps to a target: the profile on this device with the same
id (the Default profile of a fresh device, or the same profile of the same
account), otherwise a **New** profile. **Change** cycles through the existing
profiles and New. Two sources cannot share a target, a retired profile id is
refused, and the selected profile stays selected. The Data page's component
choices apply to every included profile.

* **Merge** adds missing records and keeps conflicting local values. For reading
  progress and history the record with the newer `last_read_at` wins, as in sync.
* **Replace** makes the selected components equal to the archive and deletes the
  records it lacks. Cached titles and chapters are cache, and stay. Empty and
  omitted components are different.
* **Use backup for conflicts** takes the archive's value for every conflict in a
  component (and beats the clock for progress).

Categories match by name; an assignment to a category that does not exist is
omitted and reported. Library records of titles or chapters not cached here carry
their cached copies. Addon settings and credentials apply only to installed
addons (including packages imported at the same time); the preview says
"Reinstall needed" for the rest.

The preview is required and bound to the revisions it was built against (profile
rows, its sync records, the profile directory, device rows and installed
packages); changing anything while it is open requires a new review. It reports
archive, new, identical, conflicting, updated, preserved and deleted records and
examples. The confirmation states that changes, **including removals, sync to
paired devices**.

## Applying and undoing

Under the domain lock and the record owner, the restore stages everything on a
copy of the owner and saves it once, then replaces the live owner:

1. new profile rows in the `profiles` domain;
2. each profile's state through the same preparation as an ordinary edit
   (`sync::stage`, with manual-progress semantics), so changed records get fresh
   versions and removals become tombstones;
3. settings rows, per-title rows and presentation (`sync_settings::stage_import`
   publishes only the fields the restore changed and not those under a
   per-device override; `content_presentation::stage_import` writes the row and
   its record);
4. device rows and the recovery receipt.

A failure at any point drops the copy: the live store, rows and receipts are
unchanged. After the commit, packages are installed (disabled), addon settings
and credentials are written through the registry and each target's addon
descriptors are published; failures there are reported and do not undo the
commit. Chapter files are copied before the commit and are individually whole.

**Undo last import** builds a fresh review from the newest recovery receipt:
records that still equal what the import wrote revert, later edits and unrelated
new records stay (enable **Use backup for conflicts** to force them). It creates
compensating edits at new versions, leaves installed packages and created
profiles in place and removes only chapters that import copied in. Three receipts
are kept; **Delete recovery copies** lists them and asks first. Receipts are
local plaintext, like the exports.

## Platforms

Desktop uses the native dialogs (`.nova-reader-backup`, and `.json` to open).
Android uses `ACTION_CREATE_DOCUMENT` (`application/zip`) and
`ACTION_OPEN_DOCUMENT` (archives and legacy JSON), streaming through the app
cache without a size cap beyond 64 GiB; an import copy lives until the import
ends. See [Android documents](android-documents.md); real provider behaviour
needs device validation.

## Command line

```sh
nova-reader backup FILE.nova-reader-backup [--with-downloads] [--with-local-imports]
nova-reader verify-backup FILE.nova-reader-backup
nova-reader restore FILE [--replace] [--use-backup] [--with-downloads] [--with-local-imports]
nova-reader undo-restore
```

`restore` accepts both formats and applies without a review step.

## Where to look

`src/backup/{mod,archive,components,merge,plan,apply,files,legacy}.rs` (tests
in `src/backup/tests.rs` and beside `merge.rs`), `src/backup_actions.rs`,
`crates/ui/backup.slint`, `tests/backup_page.rs`, `src/sync.rs::stage`,
`src/sync_settings.rs::{stage_import,snapshot_fields}`,
`src/content_presentation.rs::stage_import`,
`crates/reader-downloads/src/lib.rs::import_complete` and
`crates/reader-sources/src/package.rs::{export_settings,import_settings}`.
