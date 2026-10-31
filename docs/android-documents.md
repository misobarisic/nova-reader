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

The same Application-context bridge controls the reader's Android status bar.
`set_reader_immersive(true)` hides the status bar while chapter controls are
hidden; `set_reader_immersive(false)` restores it when controls are shown or the
reader closes. Java posts window changes to main and resolves the current weak
activity reference when the queue runs. The latest desired state survives
activity recreation and is reapplied on creation and window focus. API 30 and
newer use `WindowInsetsController` with transient bars available by swipe; older
versions use fullscreen/immersive-sticky decor flags while retaining other flags.
Only the status bar is hidden; navigation bar visibility is left to Android.

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
- `export_image(staged_path, suggested_name, mime, callback)` to save a PNG,
  JPEG, WebP or GIF through `ACTION_CREATE_DOCUMENT` with the matching image
  MIME type. The suggested filename is sanitized and receives its extension.
- `publish_image(staged_path, suggested_name, mime, series_folder, callback)` for
  reader Save actions: API 29+ saves directly to `Pictures/Nova Reader/`, or its
  optional per-series subfolder. API 26-28 opens the image destination picker.
  `series_folder` is `Option<&str>`; `None` or empty uses the common folder.
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
checks single completion, busy rejection and stale-result rejection. The window
fixtures exercise both API 29 decor flags and API 35 insets: queued hide/show,
transient swipe behavior, retained state across recreation and missing activity,
focus reapplication, rapid toggles, and rejection of updates to stale windows.
They also check that unrelated decor flags and navigation visibility survive.
Image fixtures verify MIME/filename picker metadata, real export bytes, provider
stream failures, clipboard bytes after staging deletion/activity destruction and
provider restart, cache/MIME bounds, read-only traversal rejection, retention,
cancellation, busy rejection and clipboard failure reporting.
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
In a chapter, verify controls start hidden, tapping restores both the controls
and status bar, tapping again hides both, a system swipe temporarily reveals the
status bar, and rotation or returning from another app preserves the requested
state. Leaving the reader must restore the status bar.
For all four image actions, verify saved/pasted pixels, image MIME and filename,
common/per-series Pictures placement on API 29+, destination cancellation/failure
on API 26-28, cleanup after interrupted saves, and pasting into another app after
Reader is closed. Confirm the APK manifest includes the private image provider and DEX
includes both Java classes. Screenshot composition is tested by the reader
export backend; the Android bridge transfers the staged encoded bytes.
