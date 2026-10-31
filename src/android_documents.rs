//! Android SAF bridge. Payloads cross JNI as app-private file paths, never as
//! large Java byte arrays. Callbacks run on the result/copy thread; UI callers
//! must hop back through Slint's event loop. Only use after Android startup.
#![cfg(target_os = "android")]

use jni::{
    JNIEnv, JavaVM,
    objects::{JClass, JObject, JString, JValue},
    sys::jlong,
};
use std::{
    path::PathBuf,
    sync::{
        Mutex,
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
}

#[derive(Clone, Copy, Debug)]
#[repr(i32)]
pub enum ImportKind {
    AddonZip = 0,
    ComicArchive = 1,
    ImageFolder = 2,
    BackupJson = 3,
}

fn with_activity<T>(
    call: impl FnOnce(&mut JNIEnv<'_>, JObject<'_>, JClass<'_>) -> jni::errors::Result<T>,
) -> Result<T, String> {
    // android-activity owns these references until NativeActivity is destroyed.
    let context = ndk_context::android_context();
    let result = (|| -> jni::errors::Result<T> {
        let vm = unsafe { JavaVM::from_raw(context.vm().cast()) }?;
        let mut env = vm.attach_current_thread()?;
        let result = env.with_local_frame(24, |env| {
            let activity = unsafe { JObject::from_raw(context.context().cast()) };
            let loader = env
                .call_method(
                    &activity,
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
            call(env, activity, class.into())
        });
        if result.is_err() && env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
        result
    })();
    result.map_err(|error| format!("Android document picker: {error}"))
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
    *pending = Some(Pending { id, callback });
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
    let result = with_activity(|env, activity, class| {
        let path = env.new_string(
            path.map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )?;
        env.call_static_method(
            class,
            "requestDocument",
            "(Landroid/app/Activity;IJLjava/lang/String;)V",
            &[
                JValue::Object(&activity),
                JValue::Int(kind),
                JValue::Long(id),
                JValue::Object(&path),
            ],
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
/// Cache directory accepted by export_backup. Stage only validated backup JSON
/// here; delete staging/import files after the receiving service consumes them.
pub fn cache_dir() -> Result<PathBuf, String> {
    with_activity(|env, activity, _| {
        let file = env
            .call_method(activity, "getCacheDir", "()Ljava/io/File;", &[])?
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
        let _ = with_activity(|env, _, class| {
            env.call_static_method(class, "cancelDocument", "(J)V", &[JValue::Long(id)])?;
            Ok(())
        });
        finish(id, Err("Document selection cancelled".into()));
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
