//! Nova's download foreground-service JNI adapter, copied from
//! misobarisic/nova@ee3877808764d70362a6e0b442e79e5128317abf
//! src/app/android_bg.rs. Reader adapts app/class names and download-only imports,
//! and clears JNI failures/retries rejected starts instead of latching a service
//! that never started. The Java service remains the mechanically renamed copy.
#![cfg(target_os = "android")]
use jni::JavaVM;
use jni::objects::{GlobalRef, JClass, JObject, JValue};
use jni::sys::jobject;
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const SERVICE_CLASS: &str = "dev.misob.nova.reader.ReaderBackgroundService";
const NOTIFICATION_REFRESH: Duration = Duration::from_secs(1);
static DOWNLOAD_SERVICE_ACTIVE: AtomicBool = AtomicBool::new(false);
static LAST_NOTIFICATION: Mutex<Option<(String, Instant)>> = Mutex::new(None);
static SERVICE_CLASS_REF: OnceLock<GlobalRef> = OnceLock::new();
static APP_CONTEXT: OnceLock<(JavaVM, GlobalRef)> = OnceLock::new();

/// Retain only the Application, so background work never depends on an
/// Activity surviving rotation or being initialized by `ndk-context`.
fn retain_app_context(env: &mut jni::JNIEnv, context: &JObject) -> jni::errors::Result<()> {
    if APP_CONTEXT.get().is_none() {
        let app = env
            .call_method(
                context,
                "getApplicationContext",
                "()Landroid/content/Context;",
                &[],
            )?
            .l()?;
        let global = env.new_global_ref(app)?;
        let vm = env.get_java_vm()?;
        let _ = APP_CONTEXT.set((vm, global));
    }
    Ok(())
}

/// Called before foreground app startup; jobs initialize the same adapter from
/// their own Context. Neither path requires a previously launched Activity.
pub(crate) fn initialize_foreground_context(
    vm: *mut std::ffi::c_void,
    activity: *mut std::ffi::c_void,
) {
    // SAFETY: AndroidApp owns these JNI references throughout android_main.
    let result = (|| -> jni::errors::Result<()> {
        let vm = unsafe { JavaVM::from_raw(vm.cast()) }?;
        let mut env = vm.attach_current_thread()?;
        let activity = unsafe { JObject::from_raw(activity.cast()) };
        retain_app_context(&mut env, &activity)
    })();
    if let Err(error) = result {
        eprintln!("Reader: unable to initialize Android download context: {error}");
    }
}

/// Run `f` with the retained Application Context. Returns `None` before either
/// foreground or headless bootstrap, without calling ndk-context's panicking
/// accessor. Foreground UI actions resolve a live Activity separately in Java.
pub(crate) fn with_app_context<R>(
    f: impl FnOnce(&mut jni::JNIEnv, jobject) -> jni::errors::Result<R>,
) -> Option<R> {
    let (vm, context) = APP_CONTEXT.get()?;
    let mut guard = vm.attach_current_thread().ok()?;
    let result = f(&mut guard, context.as_obj().as_raw());
    // UI callers may already be attached: dropping their nested AttachGuard
    // does not clear a Java exception as detaching a fresh worker would.
    let pending_exception = guard.exception_check().unwrap_or(false);
    if pending_exception {
        let _ = guard.exception_clear();
    }
    match result {
        Ok(value) if !pending_exception => Some(value),
        Ok(_) => {
            eprintln!("Reader: Android download call left a Java exception");
            None
        }
        Err(error) => {
            eprintln!("Reader: Android download call failed: {error}");
            None
        }
    }
}

/// Resolve an app class through the Context's class loader (the reliable way to
/// find app-dex classes from a native thread), caching the global ref.
/// Shared with the camera scanner (`android_qr.rs`).
pub(crate) fn app_class<'local>(
    env: &mut jni::JNIEnv<'local>,
    context: &JObject<'local>,
    dotted: &str,
    cache: &'static OnceLock<GlobalRef>,
) -> jni::errors::Result<JClass<'local>> {
    if let Some(global) = cache.get() {
        // Alias the cached global ref; `JClass`'s Drop is a no-op for local
        // views, so the cache keeps ownership.
        return Ok(unsafe { JClass::from_raw(global.as_obj().as_raw()) });
    }
    let loader = env
        .call_method(context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
        .l()?;
    let name = env.new_string(dotted)?;
    let class = env
        .call_method(
            &loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&name)],
        )?
        .l()?;
    let global = env.new_global_ref(&class)?;
    let _ = cache.set(global);
    Ok(class.into())
}

/// Start, update, or stop the download foreground service. `busy` is whether
/// any download is queued or transferring; `title`/`text` label the ongoing
/// notification. Cheap to call every tick: the JNI/notification work is
/// throttled to state changes and one refresh per second.
pub(crate) fn set_download_service(busy: bool, title: &str, text: &str) {
    let was_active = DOWNLOAD_SERVICE_ACTIVE.swap(busy, Ordering::AcqRel);
    let changed = was_active != busy;
    if !busy {
        if !changed {
            return;
        }
        with_app_context(|env, raw| {
            let context = unsafe { JObject::from_raw(raw) };
            let class = app_class(env, &context, SERVICE_CLASS, &SERVICE_CLASS_REF)?;
            env.call_static_method(
                &class,
                "stop",
                "(Landroid/content/Context;)V",
                &[JValue::Object(&context)],
            )?;
            Ok(())
        });
        return;
    }

    // Active: throttle text updates, but always act on the rising edge.
    let body = format!("{title}\n{text}");
    {
        let mut last = LAST_NOTIFICATION.lock().unwrap();
        if !changed
            && last.as_ref().is_some_and(|(previous, at)| {
                previous == &body && at.elapsed() < NOTIFICATION_REFRESH
            })
        {
            return;
        }
        *last = Some((body.clone(), Instant::now()));
    }
    let mut started = was_active;
    with_app_context(|env, raw| {
        let context = unsafe { JObject::from_raw(raw) };
        let class = app_class(env, &context, SERVICE_CLASS, &SERVICE_CLASS_REF)?;
        if !was_active {
            env.call_static_method(
                &class,
                "start",
                "(Landroid/content/Context;)V",
                &[JValue::Object(&context)],
            )?;
            started = true;
        }
        let title = env.new_string(title)?;
        let text = env.new_string(text)?;
        env.call_static_method(
            &class,
            "update",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)V",
            &[
                JValue::Object(&context),
                JValue::Object(&title),
                JValue::Object(&text),
            ],
        )?;
        Ok(())
    });
    if !started {
        // The monitor serializes start/update/stop calls. A platform rejection
        // must leave the next busy tick able to retry without an idle cycle.
        DOWNLOAD_SERVICE_ACTIVE.store(false, Ordering::Release);
        *LAST_NOTIFICATION.lock().unwrap() = None;
    }
}
