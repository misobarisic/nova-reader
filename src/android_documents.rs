//! Android window, browser and SAF bridge. Payloads cross JNI as app-private
//! file paths, never as large Java byte arrays. Callbacks run on the result/copy
//! thread; UI callers must hop back through Slint's event loop. Only use after
//! Android startup.
#![cfg(target_os = "android")]

use jni::{
    JNIEnv, JavaVM, NativeMethod,
    objects::{JClass, JObject, JString, JValue},
    sys::jlong,
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
};

const ACTIVITY_CLASS: &str = "dev.misob.nova.reader.ReaderActivity";
static NEXT_ID: AtomicI64 = AtomicI64::new(1);
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

type ImportCallback = Box<dyn FnOnce(Result<PathBuf, String>) + Send>;
type ExportCallback = Box<dyn FnOnce(Result<(), String>) + Send>;
enum Callback {
    Import(ImportCallback),
    Export(ExportCallback),
}
struct Pending {
    id: i64,
    callback: Callback,
    image_conflict: Option<ImageConflictResolver>,
    image_current: Option<ImageExportCurrent>,
}

#[derive(Clone, Copy, Debug)]
#[repr(i32)]
pub enum ImageConflictPolicy {
    Ask = 0,
    Overwrite = 1,
    DoNothing = 2,
}
#[derive(Clone, Copy, Debug)]
#[repr(i32)]
pub enum ImageConflictChoice {
    Overwrite = 0,
    AddNumber = 1,
    Cancel = 2,
}
pub type ImageConflictReply = Box<dyn FnOnce(ImageConflictChoice) + Send>;
pub type ImageConflictResolver = Arc<dyn Fn(String, ImageConflictReply) + Send + Sync>;
pub type ImageExportCurrent = Arc<dyn Fn() -> bool + Send + Sync>;
pub struct ImageSaveOptions {
    pub policy: ImageConflictPolicy,
    pub resolver: ImageConflictResolver,
    pub current: ImageExportCurrent,
}

#[derive(Clone, Copy, Debug)]
#[repr(i32)]
pub enum ImportKind {
    AddonZip = 0,
    ComicArchive = 1,
    ImageFolder = 2,
    BackupFile = 3,
}

fn take_java_exception(env: &mut JNIEnv<'_>) -> Option<String> {
    if !env.exception_check().unwrap_or(false) {
        return None;
    }
    let exception = env.exception_occurred().ok();
    let _ = env.exception_clear();
    let message = exception.and_then(|exception| {
        let exception = env.auto_local(exception);
        let text = env
            .call_method(&*exception, "toString", "()Ljava/lang/String;", &[])
            .ok()?
            .l()
            .ok()?;
        let text = env.auto_local(JString::from(text));
        env.get_string(&text).ok().map(Into::into)
    });
    // A broken Throwable.toString must not leave another pending exception.
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    message
}

fn with_context<T>(
    call: impl FnOnce(&mut JNIEnv<'_>, JObject<'_>, JClass<'_>) -> jni::errors::Result<T>,
) -> Result<T, String> {
    // android-activity 0.6 publishes the Application, not an Activity, here.
    // As in Nova, resolve app classes through this Context's class loader and
    // let Java find the current foreground Activity on its main thread.
    let context = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(context.vm().cast()) }
        .map_err(|error| format!("Android document picker: {error}"))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|error| format!("Android document picker: {error}"))?;
    let result = env.with_local_frame(24, |env| {
        let application = unsafe { JObject::from_raw(context.context().cast()) };
        let loader = env
            .call_method(
                &application,
                "getClassLoader",
                "()Ljava/lang/ClassLoader;",
                &[],
            )?
            .l()?;
        let name = env.new_string(ACTIVITY_CLASS)?;
        let class = env
            .call_method(
                &loader,
                "loadClass",
                "(Ljava/lang/String;)Ljava/lang/Class;",
                &[JValue::Object(&name)],
            )?
            .l()?;
        let class = JClass::from(class);
        // NativeActivity loads the library through the framework. Bind the
        // app-Dex callback explicitly instead of depending on JVM native
        // library discovery on a later Java UI/copy thread.
        env.register_native_methods(
            &class,
            &[
                NativeMethod {
                    name: "nativeDocumentResult".into(),
                    sig: "(JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;)V".into(),
                    fn_ptr: Java_dev_misob_nova_reader_ReaderActivity_nativeDocumentResult
                        as *mut std::ffi::c_void,
                },
                NativeMethod {
                    name: "nativeImageConflict".into(),
                    sig: "(JLjava/lang/String;)V".into(),
                    fn_ptr: Java_dev_misob_nova_reader_ReaderActivity_nativeImageConflict
                        as *mut std::ffi::c_void,
                },
                NativeMethod {
                    name: "nativeImageSaveCurrent".into(),
                    sig: "(J)Z".into(),
                    fn_ptr: Java_dev_misob_nova_reader_ReaderActivity_nativeImageSaveCurrent
                        as *mut std::ffi::c_void,
                },
            ],
        )?;
        call(env, application, class)
    });
    let exception = take_java_exception(&mut env);
    result.map_err(|error| {
        format!(
            "Android document picker: {}",
            exception.unwrap_or_else(|| error.to_string())
        )
    })
}
fn reserve(callback: Callback) -> Result<i64, (Callback, &'static str)> {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if pending.is_some() {
        return Err((callback, "Another document request is active"));
    }
    // The pending lock serializes allocators. Never reuse Android's 16-bit
    // activity request codes, so a delayed result cannot complete a newer job.
    let id = NEXT_ID.load(Ordering::Relaxed);
    if id > 65535 {
        return Err((callback, "Restart the app to open another document"));
    }
    NEXT_ID.store(id + 1, Ordering::Relaxed);
    *pending = Some(Pending {
        id,
        callback,
        image_conflict: None,
        image_current: None,
    });
    Ok(id)
}
fn invoke(callback: Callback, result: Result<PathBuf, String>) {
    // A consumer panic must never unwind through an Android JNI entry point.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match callback {
        Callback::Import(callback) => callback(result),
        Callback::Export(callback) => callback(result.map(|_| ())),
    }));
}
fn finish(id: i64, result: Result<PathBuf, String>) {
    let callback = {
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        if pending.as_ref().is_none_or(|request| request.id != id) {
            return;
        }
        pending.take().map(|request| request.callback)
    };
    if let Some(callback) = callback {
        invoke(callback, result);
    }
}
fn launch(kind: i32, path: Option<&std::path::Path>, callback: Callback) {
    let id = match reserve(callback) {
        Ok(id) => id,
        Err((callback, message)) => {
            invoke(callback, Err(message.into()));
            return;
        }
    };
    let result = with_context(|env, _, class| {
        let path = env.new_string(
            path.map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )?;
        env.call_static_method(
            class,
            "requestDocument",
            "(IJLjava/lang/String;)V",
            &[JValue::Int(kind), JValue::Long(id), JValue::Object(&path)],
        )?;
        Ok(())
    });
    if let Err(error) = result {
        finish(id, Err(error));
    }
}
pub fn pick_import(
    kind: ImportKind,
    callback: impl FnOnce(Result<PathBuf, String>) + Send + 'static,
) {
    launch(kind as i32, None, Callback::Import(Box::new(callback)));
}
pub fn export_backup(staged: PathBuf, callback: impl FnOnce(Result<(), String>) + Send + 'static) {
    launch(4, Some(&staged), Callback::Export(Box::new(callback)));
}
fn image_action(
    kind: i32,
    staged: PathBuf,
    suggested_name: &str,
    mime: &str,
    series_folder: Option<&str>,
    callback: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    image_action_current(
        kind,
        staged,
        suggested_name,
        mime,
        series_folder,
        None,
        callback,
    );
}
fn image_action_current(
    kind: i32,
    staged: PathBuf,
    suggested_name: &str,
    mime: &str,
    series_folder: Option<&str>,
    current: Option<ImageExportCurrent>,
    callback: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    let id = match reserve(Callback::Export(Box::new(callback))) {
        Ok(id) => id,
        Err((callback, message)) => {
            invoke(callback, Err(message.into()));
            return;
        }
    };
    if let Some(pending) = PENDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .filter(|pending| pending.id == id)
    {
        pending.image_current = current;
    }
    let result = with_context(|env, _, class| {
        let path = env.new_string(staged.to_string_lossy())?;
        let name = env.new_string(suggested_name)?;
        let mime = env.new_string(mime)?;
        if kind == 7 {
            let folder = env.new_string(series_folder.unwrap_or_default())?;
            env.call_static_method(
                class,
                "publishImage",
                "(JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
                &[
                    JValue::Long(id),
                    JValue::Object(&path),
                    JValue::Object(&name),
                    JValue::Object(&mime),
                    JValue::Object(&folder),
                ],
            )?;
            return Ok(());
        }
        env.call_static_method(
            class,
            "requestImage",
            "(IJLjava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            &[
                JValue::Int(kind),
                JValue::Long(id),
                JValue::Object(&path),
                JValue::Object(&name),
                JValue::Object(&mime),
            ],
        )?;
        Ok(())
    });
    if let Err(error) = result {
        finish(id, Err(error));
    }
}
/// Save an encoded image staged inside the app cache using Android's picker.
/// The caller owns staging cleanup after the callback, including failures.
pub fn export_image(
    staged: PathBuf,
    suggested_name: &str,
    mime: &str,
    callback: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    image_action(5, staged, suggested_name, mime, None, callback);
}
/// Save an image under Pictures/Nova Reader[/series] using MediaStore on API 29+.
/// Earlier Android versions request a persistent SAF folder grant. This
/// compatibility entry point cancels collisions; interactive callers use
/// publish_image_with_policy. The caller owns staging cleanup after the callback.
pub fn publish_image(
    staged: PathBuf,
    suggested_name: &str,
    mime: &str,
    series_folder: Option<&str>,
    callback: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    image_action(7, staged, suggested_name, mime, series_folder, callback);
}
/// Save with an explicit collision policy. The request and staging remain
/// reserved while the resolver asynchronously chooses overwrite/number/cancel.
pub fn publish_image_with_policy(
    staged: PathBuf,
    suggested_name: &str,
    mime: &str,
    series_folder: Option<&str>,
    options: ImageSaveOptions,
    callback: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    let id = match reserve(Callback::Export(Box::new(callback))) {
        Ok(id) => id,
        Err((callback, message)) => {
            invoke(callback, Err(message.into()));
            return;
        }
    };
    if let Some(pending) = PENDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .filter(|pending| pending.id == id)
    {
        pending.image_conflict = Some(options.resolver);
        pending.image_current = Some(options.current);
    }
    let result = with_context(|env, _, class| {
        let path = env.new_string(staged.to_string_lossy())?;
        let name = env.new_string(suggested_name)?;
        let mime = env.new_string(mime)?;
        let folder = env.new_string(series_folder.unwrap_or_default())?;
        env.call_static_method(
            class,
            "publishImageWithPolicy",
            "(JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;I)V",
            &[
                JValue::Long(id),
                JValue::Object(&path),
                JValue::Object(&name),
                JValue::Object(&mime),
                JValue::Object(&folder),
                JValue::Int(options.policy as i32),
            ],
        )?;
        Ok(())
    });
    if let Err(error) = result {
        finish(id, Err(error));
    }
}
fn resolve_image_conflict(id: i64, choice: ImageConflictChoice) {
    if PENDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_none_or(|pending| pending.id != id)
    {
        return;
    }
    let result = with_context(|env, _, class| {
        env.call_static_method(
            class,
            "resolveImage",
            "(JI)V",
            &[JValue::Long(id), JValue::Int(choice as i32)],
        )?;
        Ok(())
    });
    if let Err(error) = result {
        finish(id, Err(error));
    }
}
/// Copy an encoded image to Android's clipboard as a grant-readable content URI.
/// Java retains an independent bounded cache copy; the caller owns staging cleanup.
pub fn copy_image(
    staged: PathBuf,
    mime: &str,
    callback: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    image_action(6, staged, "Reader image", mime, None, callback);
}
/// Recheck reader/selection ownership on Java's actual clipboard dispatch,
/// after its asynchronous staging and main-thread handoff.
pub fn copy_image_current(
    staged: PathBuf,
    mime: &str,
    current: ImageExportCurrent,
    callback: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    image_action_current(
        6,
        staged,
        "Reader image",
        mime,
        None,
        Some(current),
        callback,
    );
}
/// Open a source/license link in the system browser, following Nova's link
/// intent policy. The Application context needs NEW_TASK and never retains an Activity.
pub fn open_browser(url: &str) -> Result<(), String> {
    with_context(|env, application, _| {
        let raw = env.new_string(url)?;
        let uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&raw)],
            )?
            .l()?;
        let action = env.new_string("android.intent.action.VIEW")?;
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;Landroid/net/Uri;)V",
            &[JValue::Object(&action), JValue::Object(&uri)],
        )?;
        let browsable = env.new_string("android.intent.category.BROWSABLE")?;
        env.call_method(
            &intent,
            "addCategory",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&browsable)],
        )?;
        env.call_method(
            &intent,
            "addFlags",
            "(I)Landroid/content/Intent;",
            &[JValue::Int(0x1000_0000)],
        )?;
        env.call_method(
            application,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )?;
        Ok(())
    })
    .map_err(|error| {
        format!(
            "no app can open this link ({})",
            error
                .strip_prefix("Android document picker: ")
                .unwrap_or(&error)
        )
    })
}
/// Hide both Android system bars while reading, or restore them afterwards.
/// Java applies this on main and retains it across recreation.
pub fn set_reader_immersive(immersive: bool) -> Result<(), String> {
    with_context(|env, _, class| {
        env.call_static_method(
            class,
            "setReaderImmersive",
            "(Z)V",
            &[JValue::Bool(immersive.into())],
        )?;
        Ok(())
    })
}
/// Apply reader-only Android orientation, wake and cutout preferences on the
/// Java main thread. Rotation: 0 system, 1 free, 2 sensor portrait, 3 sensor
/// landscape, 4 locked portrait, 5 locked landscape, 6 reverse portrait.
/// Leaving the reader restores the Activity's prior orientation/window state.
pub fn set_reader_presentation(
    reading: bool,
    immersive: bool,
    rotation: i32,
    keep_awake: bool,
    cutout: bool,
) -> Result<(), String> {
    with_context(|env, _, class| {
        env.call_static_method(
            class,
            "setReaderPresentation",
            "(ZZIZZ)V",
            &[
                JValue::Bool(reading.into()),
                JValue::Bool(immersive.into()),
                JValue::Int(rotation),
                JValue::Bool(keep_awake.into()),
                JValue::Bool(cutout.into()),
            ],
        )?;
        Ok(())
    })
}
/// Android's application cache root. Document staging is removed after use;
/// disposable artwork uses its own `posters` subdirectory under this root.
pub fn cache_dir() -> Result<PathBuf, String> {
    with_context(|env, application, _| {
        let file = env
            .call_method(application, "getCacheDir", "()Ljava/io/File;", &[])?
            .l()?;
        let path = env
            .call_method(file, "getAbsolutePath", "()Ljava/lang/String;", &[])?
            .l()?;
        let path: String = env.get_string(&JString::from(path))?.into();
        Ok(PathBuf::from(path))
    })
}
/// Cancel pending picker/copy work. A provider may take time to close; stale
/// results are ignored by request ID and never complete a later request.
pub fn cancel_pending() {
    let id = PENDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|p| p.id);
    if let Some(id) = id {
        let _ = with_context(|env, _, class| {
            env.call_static_method(class, "cancelDocument", "(J)V", &[JValue::Long(id)])?;
            Ok(())
        });
        finish(id, Err("Document selection cancelled".into()));
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_reader_ReaderActivity_nativeImageSaveCurrent(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    id: jlong,
) -> jni::sys::jboolean {
    let current = {
        let pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let Some(pending) = pending.as_ref().filter(|pending| pending.id == id) else {
            return 0;
        };
        pending.image_current.clone()
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        current.is_none_or(|current| current())
    }))
    .unwrap_or(false)
    .into()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_reader_ReaderActivity_nativeImageConflict(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    id: jlong,
    name: JString<'_>,
) {
    let resolver = PENDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|pending| pending.id == id)
        .and_then(|pending| pending.image_conflict.clone());
    let decoded = env.get_string(&name).map(String::from);
    let name = match decoded {
        Ok(name) => name,
        Err(_) => {
            let _ = take_java_exception(&mut env);
            resolve_image_conflict(id, ImageConflictChoice::Cancel);
            return;
        }
    };
    if let Some(resolver) = resolver {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            resolver(
                name,
                Box::new(move |choice| resolve_image_conflict(id, choice)),
            )
        }))
        .is_err()
        {
            resolve_image_conflict(id, ImageConflictChoice::Cancel);
        }
    } else {
        resolve_image_conflict(id, ImageConflictChoice::Cancel);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_misob_nova_reader_ReaderActivity_nativeDocumentResult(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    id: jlong,
    path: JString<'_>,
    _name: JString<'_>,
    error: JString<'_>,
) {
    let decoded = (|| -> jni::errors::Result<Result<PathBuf, String>> {
        if !error.is_null() {
            let message: String = env.get_string(&error)?.into();
            return Ok(Err(message));
        }
        if path.is_null() {
            return Ok(Err("Android returned no document path".into()));
        }
        let path: String = env.get_string(&path)?.into();
        Ok(Ok(PathBuf::from(path)))
    })();
    if decoded.is_err() && env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    let result = decoded.unwrap_or_else(|error| Err(format!("Android document result: {error}")));
    finish(id, result);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callbacks_are_single_use_busy_and_stale_results_do_not_replace_request() {
        let (send, receive) = std::sync::mpsc::channel();
        let first = reserve(Callback::Import(Box::new(move |result| {
            let _ = send.send(result);
        })))
        .ok()
        .unwrap();
        let (busy_send, busy_receive) = std::sync::mpsc::channel();
        let busy = reserve(Callback::Export(Box::new(move |result| {
            let _ = busy_send.send(result);
        })));
        let Err((callback, message)) = busy else {
            panic!("second request was admitted")
        };
        invoke(callback, Err(message.into()));
        assert!(busy_receive.recv().unwrap().is_err());
        finish(first + 1, Ok(PathBuf::from("stale")));
        assert!(receive.try_recv().is_err());
        finish(first, Ok(PathBuf::from("selected")));
        assert_eq!(receive.recv().unwrap().unwrap(), PathBuf::from("selected"));
        finish(first, Err("duplicate".into()));
        assert!(receive.try_recv().is_err());
        assert!(PENDING.lock().unwrap().is_none());
    }
}
