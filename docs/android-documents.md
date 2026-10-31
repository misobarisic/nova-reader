# Android documents

`src/android_documents.rs` and
`android/java/dev/misob/nova/reader/ReaderActivity.java` bridge Android's Storage
Access Framework (SAF) to reader services. The custom activity extends
`NativeActivity`; Slint/android-activity retain the native surface and event loop.
This bridge starts no background jobs or services.

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

Exports use `ACTION_CREATE_DOCUMENT`, accept only regular staged files under
Android's canonical cache directory, and enforce the 32 MiB stream limit. Export
success means the provider output stream closed successfully. A failing external
provider can leave a partial destination; an error is reported rather than
claiming success. The caller owns its staging file's cleanup.

Requests have monotonically increasing IDs and non-reused 16-bit Android activity
request codes; after 65,535 requests in one process the user must restart the app.
Results from cancelled/older requests cannot complete a later request. Cancellation
is checked during copying and before publication. A provider blocked in its own
I/O can delay cleanup; no guarantee is made about provider response timing.
Activity destruction cancels outstanding work, except configuration recreation,
which rebinds the request to the new activity. Process death does not promise to
resume an external picker or an interrupted provider copy.

## Verification

The Java activity compiles as Java 8 bytecode against Android API 35 classes. A
host harness executes the exact extracted transfer, filename, and ordering
helpers, covering strict byte admission, cancellation, a stalled provider, large
numeric names, and filename sanitizing. The
Rust JNI module is type-checked and linted against `jni 0.21` and `ndk-context 0.1`
in a host harness with its Android module gate removed. Its callback regression
checks single completion, busy rejection, and stale-result rejection. These
checks validate JNI argument and lifetime types; they are not Android executable
or device tests.

Before Android delivery, build the actual ARM64 target and APK, then verify on a
device: select an addon ZIP, CBZ and nested image folder; restore and export a
backup; cancel before and during a slow provider copy; rotate/recreate while the
picker is open; kill the process during copying; reject an over-limit document;
and exercise a provider that offers only a transient grant. Confirm the activity,
Java dex, native library name and native callback are packaged together.
