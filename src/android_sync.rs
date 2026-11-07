//! Android sync jobs and framework Camera2 QR scanning. Headless jobs retain
//! only the Application context and never construct Slint or launch an Activity.
#![cfg(target_os = "android")]
use jni::{
    objects::{JByteArray, JObject, JValue},
    sys::{JNI_FALSE, JNI_TRUE, jboolean, jint, jstring},
};
use nova_android::{
    context::{call as call_class, load_class as app_class},
    slot::Slot,
};
use std::{
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

static CANCEL: LazyLock<Mutex<Arc<AtomicBool>>> =
    LazyLock::new(|| Mutex::new(Arc::new(AtomicBool::new(false))));
fn job_cancel() -> Arc<AtomicBool> {
    CANCEL.lock().unwrap_or_else(|e| e.into_inner()).clone()
}
type ScanCallback = Box<dyn FnOnce(Result<String, String>) + Send>;
static SCAN: Slot<ScanCallback> = Slot::new();

/// Idempotent schedule/cancel. Call with sync && background enabled.
pub(crate) fn schedule_background(enabled: bool) {
    if let Err(error) = call_class(|env, context| {
        let class = app_class(env, context, "dev.misob.nova.reader.ReaderSyncBridge")?;
        env.call_static_method(
            class,
            "schedule",
            "(Landroid/content/Context;Z)V",
            &[JValue::Object(context), JValue::Bool(enabled.into())],
        )?;
        Ok(())
    }) {
        eprintln!("Reader background sync schedule: {error}");
    }
}

/// Observe connectivity whenever sync is enabled, independently of job opt-in.
pub(crate) fn set_network_observer(enabled: bool) {
    if let Err(error) = call_class(|env, context| {
        let class = app_class(env, context, "dev.misob.nova.reader.ReaderSyncBridge")?;
        env.call_static_method(
            class,
            "observeNetwork",
            "(Landroid/content/Context;Z)V",
            &[JValue::Object(context), JValue::Bool(enabled.into())],
        )?;
        Ok(())
    }) {
        eprintln!("Reader sync network observer: {error}");
    }
}

pub(crate) fn copy_text(text: &str) -> Result<(), String> {
    call_class(|env, context| {
        let class = app_class(env, context, "dev.misob.nova.reader.ReaderSyncBridge")?;
        let text = env.new_string(text)?;
        env.call_static_method(
            class,
            "copyText",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[JValue::Object(context), JValue::Object(&text)],
        )?;
        Ok(())
    })
}

/// Callback runs on the camera thread; caller dispatches onto its UI event loop.
pub(crate) fn scan_invite(callback: impl FnOnce(Result<String, String>) + Send + 'static) {
    if let Err(callback) = SCAN.begin(Box::new(callback)) {
        callback(Err(nova_reader_ui::backend_text::tr(
            "A QR scan is already active",
        )));
        return;
    }
    if let Err(error) = call_class(|env, context| {
        let class = app_class(env, context, "dev.misob.nova.reader.QrScanActivity")?;
        env.call_static_method(
            class,
            "start",
            "(Landroid/content/Context;)V",
            &[JValue::Object(context)],
        )?;
        Ok(())
    }) {
        finish_scan(Err(error));
    }
}
fn finish_scan(result: Result<String, String>) {
    let callback = SCAN.take();
    if let Some(callback) = callback {
        callback(result);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_reader_ReaderSyncBridge_nativeNetworkChanged(
    _env: jni::JNIEnv,
    _this: JObject,
) {
    nova_sync::notify_network_change();
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_reader_ReaderSyncJobService_nativePrepareSync(
    _env: jni::JNIEnv,
    _this: JObject,
) {
    *CANCEL.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(AtomicBool::new(false));
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_reader_ReaderSyncJobService_nativeCancelSync(
    _env: jni::JNIEnv,
    _this: JObject,
) {
    job_cancel().store(true, Ordering::Release);
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_reader_ReaderSyncJobService_nativeRunSync(
    mut env: jni::JNIEnv,
    this: JObject,
) -> jstring {
    let result = run_headless(&mut env, &this).unwrap_or_else(|error| format!("failed: {error}"));
    env.new_string(result)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}
fn run_headless(env: &mut jni::JNIEnv, context: &JObject) -> Result<String, String> {
    let started = Instant::now();
    let cancel = job_cancel();
    nova_android::context::retain_app_context(env, context).map_err(|e| e.to_string())?;
    let path = nova_android::context::files_dir(env, context)?.join("nova-reader");
    let _ = crate::paths::ANDROID_DATA.set(path.clone());
    // Storage initialization must precede settings access; no Activity exists.
    let services = crate::services::Services::open(&path)?;
    let settings = nova_sync::read_settings();
    if !settings.enabled || !settings.background_enabled || cancel.load(Ordering::Acquire) {
        return Ok("skipped: sync disabled".into());
    }
    let Some(lease) = nova_sync::background_engine().map_err(|e| e.to_string())? else {
        return Ok("skipped: another background job owns engine".into());
    };
    // A fresh cancellation token belongs to this job generation, so an old
    // deadline cannot cancel a subsequent pass. One budget covers records AND
    // all package receipts, rather than granting each package another 90s.
    let (done, receiver) = std::sync::mpsc::channel();
    let deadline_cancel = cancel.clone();
    let remaining = Duration::from_secs(120).saturating_sub(started.elapsed());
    std::thread::Builder::new()
        .name("reader-sync-deadline".into())
        .spawn(move || {
            if receiver.recv_timeout(remaining) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                deadline_cancel.store(true, Ordering::Release);
            }
        })
        .map_err(|e| e.to_string())?;
    struct Deadline(std::sync::mpsc::Sender<()>);
    impl Drop for Deadline {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let _deadline = Deadline(done);
    let result = lease.engine.one_shot(
        &cancel,
        Duration::from_secs(120).saturating_sub(started.elapsed()),
    );
    crate::sync::apply_remote(&services.domain).map_err(|e| e.to_string())?;
    let packages = crate::sync_addons::reconcile(&services, cancel)?;
    Ok(format!(
        "{result:?}; packages pending={} failed={} confirmation={}",
        packages.pending, packages.failed, packages.confirmation_required
    ))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_reader_QrScanActivity_nativeHasRequest(
    _env: jni::JNIEnv,
    _this: JObject,
) -> jboolean {
    if SCAN.is_pending() {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_reader_QrScanActivity_nativeCancelled(
    _env: jni::JNIEnv,
    _this: JObject,
) {
    // Matched by `backend_text::is_user_cancel`; keep the wording in sync.
    finish_scan(Err("QR scan cancelled".into()));
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_reader_QrScanActivity_nativeOnFrame(
    env: jni::JNIEnv,
    _this: JObject,
    frame: JByteArray,
    width: jint,
    height: jint,
    stride: jint,
) -> jboolean {
    let Ok(bytes) = env.convert_byte_array(frame) else {
        return JNI_FALSE;
    };
    if width <= 0 || height <= 0 || stride < width {
        return JNI_FALSE;
    }
    let (width, height, stride) = (width as usize, height as usize, stride as usize);
    // Validate before allocation/indexing; avoid overflowing hostile JNI sizes.
    if width > 4096
        || height > 4096
        || stride > 16384
        || stride.checked_mul(height).is_none_or(|n| n > bytes.len())
    {
        return JNI_FALSE;
    }
    let mut image =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| bytes[y * stride + x]);
    for grid in image.detect_grids() {
        if let Ok((_, ticket)) = grid.decode() {
            if nova_sync::decode_ticket(&ticket).is_ok() {
                finish_scan(Ok(ticket));
                return JNI_TRUE;
            }
        }
    }
    JNI_FALSE
}
