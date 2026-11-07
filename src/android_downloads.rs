//! Download foreground-service adapter. The Application context, class lookup
//! and the start/update/stop state machine live in nova-android; the Java
//! service is rendered from the same template as Nova's (see `tests/android_java.rs`).
#![cfg(target_os = "android")]
use nova_android::{context::JniDriver, service::DownloadService};

const SERVICE_CLASS: &str = "dev.misob.nova.reader.ReaderBackgroundService";

static DOWNLOAD_SERVICE: DownloadService<JniDriver> =
    DownloadService::new(JniDriver::new(SERVICE_CLASS));

/// Start, update, or stop the download foreground service. `busy` is whether
/// any download is queued or transferring; `title`/`text` label the ongoing
/// notification. Cheap to call every tick: the JNI/notification work is
/// throttled to state changes and one refresh per second. A start the platform
/// rejects is retried on the next busy tick.
pub(crate) fn set_download_service(busy: bool, title: &str, text: &str) {
    DOWNLOAD_SERVICE.set(busy, title, text);
}
