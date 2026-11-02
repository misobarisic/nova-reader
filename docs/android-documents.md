# Android documents

`src/android_documents.rs` and
`android/java/dev/misob/nova/reader/ReaderActivity.java` bridge Android's Storage
Access Framework (SAF) to reader services. The custom activity extends
`NativeActivity`; Slint/android-activity retain the native surface and event loop.
This bridge starts no background jobs or services. `ndk-context` exposes the
Application context with android-activity 0.6. Rust uses its class loader and
cache directory without casting it to an Activity. Java posts the picker launch
to the main thread and resolves the current ReaderActivity there, retaining only
a weak reference. JNI explicitly registers the result callback on the app class;
Java launch exceptions preserve their class/message and are cleared before retry.

The same Application-context bridge controls the reader's Android system bars.
`set_reader_immersive(true)` hides both the status bar and the navigation bar
while the reader is open with **Fullscreen** enabled; `set_reader_immersive(false)`
restores both when Fullscreen is off or the reader closes. Java posts window
changes to main and resolves the current weak activity reference when the queue
runs. The latest desired state survives activity recreation and is reapplied on
creation and window focus. API 30 and newer hide
`WindowInsets.Type.systemBars()` through `WindowInsetsController` with transient
bars available by swipe; older versions use fullscreen plus navigation-hiding
immersive-sticky decor flags while retaining other flags. The bars stay hidden
while controls are shown too, so toggling the OSD cannot resize the reader.

The activity keeps its native surface behind the system bars in every page and
both reader control states. API 30+ uses `setDecorFitsSystemWindows(false)`;
API 26-29 retains `LAYOUT_STABLE|LAYOUT_HIDE_NAVIGATION|LAYOUT_FULLSCREEN` while toggling only bar
visibility flags. The status bar is transparent. Cutout layout uses `ALWAYS`
on API 30+ and `SHORT_EDGES` on API 28-29. Showing controls therefore keeps the
same client viewport instead of reserving or releasing a strip above the image.
These settings are reapplied on activity creation and window focus.
This matches Nova's normal transparent, edge-to-edge window policy in
`nova/crates/player/src/external.rs` (`EDGE_TO_EDGE` at line 523 and
`apply_system_bars` at lines 666-732). Reader hides both bars, matching Nova's
player; Slint's safe-area insets follow the visible bars, so hiding the
navigation bar releases its bottom inset instead of leaving a black strip.
Inherited window `FLAG_FULLSCREEN` and translucent status flags are cleared, so
leaving a chapter can restore the bars even if the native framework set
fullscreen. Decor fitting is configured once per creation/focus rather than
redispatched for each controls toggle. If the API 30+ insets controller is not
attached during creation, one decor-view post retries against the latest
activity/state; focus also reasserts it.

Slint 1.18's Android backend reports system-bar and cutout insets without setting
the window fitting policy. Its Java inset listeners remain installed: normal
pages apply the reported safe-area padding, chapter artwork fills the top of
the surface, and the reader controls independently respect the status/cutout
inset. Do not toggle Slint fullscreen or decor fitting with the controls; that
would change the viewport or suppress safe-area information used by the UI.

## Application integration

The Android Cargo target needs `jni = "0.21"` and `ndk-context = "0.1"`.
`package.metadata.android.java_sources` is `"android/java"`,
`application.has_code` is `true`, and the activity name is
`"dev.misob.nova.reader.ReaderActivity"`. Preserve the existing native library
metadata and private release signing configuration.

After Android startup, call:

- `pick_import(ImportKind::{AddonZip, ComicArchive, ImageFolder, BackupJson}, callback)`
  with a `FnOnce(Result<PathBuf, String>) + Send + 'static` callback.
- `cache_dir()` to obtain Android's app cache directory, stage validated metadata
  backup JSON there, then call `export_backup(staged_path, callback)` with a
  `FnOnce(Result<(), String>) + Send + 'static` callback.
- `cancel_pending()` to cancel the current request explicitly.
- `set_reader_immersive(bool)` to synchronize the Android status bar with reader
  controls. It returns JNI/queue errors and does not require a document request.
- `open_browser(url)` for license and source links. Adapted from Nova's
  `crates/player/src/external.rs` `open_browser` at revision `b69a1e5`, it launches
  `ACTION_VIEW` with `Uri.parse(url)`, `CATEGORY_BROWSABLE` and
  `FLAG_ACTIVITY_NEW_TASK`, with no MIME filter. The actual Application starts
  the intent, so browser links do not depend on a live ReaderActivity or picker
  state. JNI launch errors preserve the Java cause and clear exceptions before
  another link is opened.
- `export_image(staged_path, suggested_name, mime, callback)` to save a PNG,
  JPEG, WebP, GIF or AVIF through `ACTION_CREATE_DOCUMENT` with the matching image
  MIME type. The suggested filename is sanitized and receives its extension.
- `publish_image(staged_path, suggested_name, mime, series_folder, callback)` for
  reader Save actions: API 29+ saves directly to `Pictures/Nova Reader/`, or its
  optional per-series subfolder. API 26–28 requests a document-tree folder once
  and persists its read/write grant, then saves under its Nova Reader subfolder.
  `series_folder` is `Option<&str>`; `None` or empty uses the common folder.
- `publish_image_with_policy(staged_path, suggested_name, mime, series_folder,
  ImageSaveOptions { policy, resolver, current }, callback)` is Reader's current
  save path. Ask resolves asynchronously through the Slint conflict sheet;
  Overwrite and Do nothing are persisted automatic policies. Add number starts
  at `(2)` and retries unused exact names. Names are bounded to 160 UTF-8 bytes,
  series folders to 96. The current-generation callback is checked before saving
  and while streaming. MediaStore queries only exact app-owned paths/names and
  rejects provider renaming. Overwrites back up old bytes before modification;
  a failed provider rollback retains one bounded private backup and reports the
  failure instead of silently losing it.
- `copy_image(staged_path, mime, callback)` to place an image content URI on the
  Android clipboard. All image functions accept
  `FnOnce(Result<(), String>) + Send + 'static`; the caller removes the original
  staging file after either success or failure.

Callbacks can execute on a platform/copy thread. Callers must use
`slint::invoke_from_event_loop` before accessing the UI. Only one document request
is accepted at a time; busy, cancellation and provider failures arrive as errors.

Imports launch `ACTION_OPEN_DOCUMENT` (files) or `ACTION_OPEN_DOCUMENT_TREE`
(images). The bridge requests and takes persistable read grants when supported;
transient providers still work through immediate copying. Files stream through a
64 KiB buffer into private cache storage. No image, archive, or backup is passed
as a JNI byte array. The callback receives a regular file/directory that existing
source/domain/download services can inspect independently of the provider URI.

The parent staging directory is opaque. A selected file's basename preserves its
sanitized, length-bounded display name for useful local series titles; provider
names cannot select filesystem paths. Folder imports flatten supported images
into numbered filenames after natural numeric sorting of relative display names.
The receiver must remove staged imports and their empty opaque parent directory
after ingestion. Interrupted-process leftovers are ordinary disposable
app cache; they never count as completed downloaded chapters.

## Bounds and cancellation

| Input | Limit |
| --- | --- |
| Addon ZIP / metadata JSON | 32 MiB |
| CBZ/ZIP comic | 512 MiB |
| Image folder | 512 MiB combined, 32 MiB per image |
| Folder enumeration | 10,000 entries, depth 8, no repeated document IDs |
| Supported image names | PNG, JPG/JPEG, WebP |

Provider size declarations are checked early and independently enforced while
streaming. Actual addon/archive/schema/image validation remains in the receiving
Rust service. Provider metadata and MIME labels never bypass these validators.

Backup exports and explicit picker exports use `ACTION_CREATE_DOCUMENT`, accept
only regular staged files under Android's canonical cache directory, and enforce
the 32 MiB stream limit. Export
success means the provider output stream closed successfully. A failing external
provider can leave a partial destination; an error is reported rather than
claiming success. The caller owns its staging file's cleanup.

Image exports share the canonical cache containment, 32 MiB transfer limit,
background copy executor, cancellation and single pending request with document
operations. API 29+ reader saves insert a fresh MediaStore image row with its
display name, image MIME, `RELATIVE_PATH` and `IS_PENDING=1`. The image becomes
public only after the bounded stream closes successfully and `IS_PENDING` is
cleared. Transfer, close, publication and cancellation errors delete that pending
row. MediaStore allocates a separate row/path for filename collisions. Publishing
and completion are serialized with cancellation, so success describes a completed
image. An optional series folder must be one portable component, at most 80 UTF-8
bytes, with no path separators, dot/parent components or control characters.
No storage permission is needed for these app-created MediaStore images.

On API 26-28, Save uses the platform image picker and the user chooses the
destination; automatic per-series placement is available on API 29 and newer.
Clipboard images are copied into a separate private
`reader-clipboard` cache directory before the main thread publishes `ClipData`.
The callback reports success only after the clipboard accepts the content URI.
No file URI or filesystem path is placed on the clipboard.
AVIF saves and clipboard images keep their original encoded bytes and use
`image/avif` with an `.avif` filename. The bridge does not transcode for older
Android versions; a receiving app needs AVIF decoding support to display/paste
that format. The reader's own codec is independent of Android's image decoder.

`ReaderImageProvider` is a read-only, non-exported provider with
`grantUriPermissions=true`, so Android grants the receiving clipboard consumer
access to that URI. It serves only randomized image filenames in that directory,
rejects traversal and write requests, and exposes image MIME/name/size metadata.
Independent cached bytes remain readable after the original staging file is
removed or the activity/process exits; Android can start the provider without
the native reader. Cache eviction, clipboard expiry and force-stop are controlled
by Android. Files expire after 24 hours; each new copy retains at most eight
files (256 MiB maximum), removing older files first. Older clipboard entries can
therefore become unavailable. No background cleanup service is started.

The pinned cargo-apk2 schema requires
`android/cargo-apk2-image-provider.patch` to serialize the provider in application
metadata. `flake.nix` applies that patch alongside the existing native-loading
patches. Cargo metadata declares its fixed authority and private grant settings;
the Java source is compiled into DEX with the activity.

Requests have monotonically increasing IDs and non-reused 16-bit Android activity
request codes; after 65,535 requests in one process the user must restart the app.
Results from cancelled/older requests cannot complete a later request. Cancellation
is checked during copying and before publication. A provider blocked in its own
I/O can delay cleanup; no guarantee is made about provider response timing.
Activity destruction cancels outstanding work, except configuration recreation,
which rebinds the request to the new activity. Process death does not promise to
resume an external picker or an interrupted provider copy.

## Verification

The Java activity compiles as Java 8 bytecode against Android API 35 classes.
The committed host regression runs the production activity and Rust JNI module
in a real JVM with `-Xcheck:jni`. Small Android fixtures provide a deterministic
main queue, Application context and activity lifecycle; they do not emulate an
Android device or a document provider.

```sh
nix develop .#android --command python3 tools/test-android-documents.py --clippy
# Or supply API classes directly when a full SDK is not installed:
python3 tools/test-android-documents.py --android-jar /path/to/android.jar --clippy
# Check actual patched upstream manifest serialization without an SDK:
python3 tools/test-android-provider-manifest.py --apk2-source /path/to/pinned/cargo-apk2
```

It checks cache access through Application, all four import launch types,
backup export launch, cancellation/native callback registration, recreation
between request and launch, missing activity errors, Java exception reporting
and successful retry after clearing the exception. The Rust callback test also
checks single completion, busy rejection and stale-result rejection.
Browser fixtures verify the original URI, browsable category, new-task flag,
missing MIME filter and Application identity before activity creation and after
recreation/destruction, plus launch exception reporting and successful recovery.
The window fixtures exercise API 26/28/29 decor flags and API 30/35 insets: queued hide/show,
transient swipe behavior, retained state across recreation and missing activity,
focus reapplication, rapid toggles, and rejection of updates to stale windows.
They hide and restore both the status and navigation bars, check that a focus
regain re-hides both, and check transparent status bars, the matching cutout
policies and edge-to-edge fitting with both hidden/shown controls, unchanged
layout flags across toggles, and restoration after recreation/focus. Unrelated
window/decor flags and navigation visibility must survive.
Fixtures begin with fullscreen/navigation hiding already set and verify normal
bar restoration, deferred controller attachment and a stale attachment retry.
Image fixtures verify MIME/filename picker metadata, real export bytes, provider
stream failures, clipboard bytes after staging deletion/activity destruction and
provider restart, cache/MIME bounds, read-only traversal rejection, retention,
cancellation, busy rejection and clipboard failure reporting.
The shared real AVIF fixture also crosses API 26 SAF and API 35 MediaStore paths
and the clipboard provider with exact byte comparison, correct MIME/extension,
and successful reads after staging deletion and provider restart.
MediaStore fixtures exercise common/per-series paths and filename/MIME metadata
on API 29 and 35, separate rows for collisions, rollback after
insert/open/write/close/publication failures and cancellation during copying,
folder rejection and API 28 picker fallback. Production Java
sources are also compiled separately against real API classes. The manifest
regression applies the provider patch to the supplied upstream source, reads the
actual Cargo metadata and verifies the serialized provider/native activity XML.
Host JNI execution and Java compilation do not establish Android provider/device
behavior.

Before Android delivery, build the actual ARM64 target and APK, then verify on a
device: select an addon ZIP, CBZ and nested image folder; restore and export a
backup; cancel before and during a slow provider copy; rotate/recreate while the
picker is open; kill the process during copying; reject an over-limit document;
and exercise a provider that offers only a transient grant. Confirm the activity,
Java dex, native library name and native callback are packaged together.
In a chapter, verify controls start hidden and both the status and navigation
bars are hidden, tapping shows the controls without revealing the bars, a system
swipe temporarily reveals the bars, and rotation or returning from another app
preserves the hidden state. Leaving the reader must restore both bars. With
**Fullscreen** turned off, both bars stay visible inside the reader.
Check portrait and landscape on a cutout device: chapter pixels must reach the
top edge with either control state, the image must not move when toggling
controls, the visible toolbar must remain clear of status icons/cutouts, and
normal pages must keep their safe-area padding.
For all four image actions, verify saved/pasted pixels, image MIME and filename,
common/per-series Pictures placement on API 29+, destination cancellation/failure
on API 26-28, cleanup after interrupted saves, and pasting into another app after
Reader is closed. Confirm the APK manifest includes the private image provider and DEX
includes both Java classes. Screenshot composition is tested by the reader
export backend; the Android bridge transfers the staged encoded bytes.
Open About's license and source links and confirm Android dispatches them to the
system browser while the reader's normal navigation remains usable afterward.
