use super::*;
use jni::{InitArgsBuilder, JNIVersion};

#[test]
fn application_context_launches_and_callbacks_resolve() {
    let classpath = format!(
        "-Djava.class.path={}",
        std::env::var("READER_JNI_CLASSPATH").unwrap()
    );
    let cache = format!(
        "-Djava.io.tmpdir={}",
        std::env::var("READER_JNI_CACHE").unwrap()
    );
    let args = InitArgsBuilder::new()
        .version(JNIVersion::V8)
        .option(&classpath)
        .option(&cache)
        .option("-Xcheck:jni")
        .build()
        .unwrap();
    let vm = JavaVM::new(args).unwrap();
    let mut env = vm.attach_current_thread().unwrap();
    let app = env
        .new_object("android/app/Application", "()V", &[])
        .unwrap();
    let app = env.new_global_ref(app).unwrap();
    unsafe {
        ndk_context::initialize_android_context(
            vm.get_java_vm_pointer().cast(),
            app.as_obj().as_raw().cast(),
        );
    }
    const FIXTURE: &str = "dev/misob/nova/reader/DocumentBridgeFixture";
    env.call_static_method(FIXTURE, "create", "()V", &[])
        .unwrap();
    assert_eq!(
        cache_dir().unwrap(),
        PathBuf::from(std::env::var("READER_JNI_CACHE").unwrap())
    );

    // The bridge receives a real Application, not an Activity. Each launch
    // queues to Java main, and the explicitly registered native callback runs.
    for (kind, expected) in [
        (ImportKind::AddonZip, "android.intent.action.OPEN_DOCUMENT"),
        (
            ImportKind::ComicArchive,
            "android.intent.action.OPEN_DOCUMENT",
        ),
        (
            ImportKind::ImageFolder,
            "android.intent.action.OPEN_DOCUMENT_TREE",
        ),
        (
            ImportKind::BackupJson,
            "android.intent.action.OPEN_DOCUMENT",
        ),
    ] {
        let (send, receive) = std::sync::mpsc::channel();
        pick_import(kind, move |result| {
            let _ = send.send(result);
        });
        env.call_static_method(FIXTURE, "drain", "()V", &[])
            .unwrap();
        let action = env
            .call_static_method(FIXTURE, "action", "()Ljava/lang/String;", &[])
            .unwrap()
            .l()
            .unwrap();
        let action: String = env.get_string(&JString::from(action)).unwrap().into();
        assert_eq!(action, expected);
        env.call_static_method(FIXTURE, "cancelResult", "()V", &[])
            .unwrap();
        assert_eq!(
            receive
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .unwrap_err(),
            "Document selection cancelled"
        );
        assert!(!env.exception_check().unwrap());
    }

    let staged = cache_dir().unwrap().join("metadata.json");
    std::fs::write(&staged, b"{}").unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    export_backup(staged, move |result| {
        let _ = send.send(result);
    });
    env.call_static_method(FIXTURE, "drain", "()V", &[])
        .unwrap();
    let action = env
        .call_static_method(FIXTURE, "action", "()Ljava/lang/String;", &[])
        .unwrap()
        .l()
        .unwrap();
    let action: String = env.get_string(&JString::from(action)).unwrap().into();
    assert_eq!(action, "android.intent.action.CREATE_DOCUMENT");
    env.call_static_method(FIXTURE, "cancelResult", "()V", &[])
        .unwrap();
    assert_eq!(
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap_err(),
        "Document selection cancelled"
    );

    // Recreation between native request and Java main execution must use the
    // replacement activity, even when the old activity is destroyed afterward.
    let (send, receive) = std::sync::mpsc::channel();
    pick_import(ImportKind::AddonZip, move |result| {
        let _ = send.send(result);
    });
    env.call_static_method(FIXTURE, "recreate", "()V", &[])
        .unwrap();
    env.call_static_method(FIXTURE, "drain", "()V", &[])
        .unwrap();
    assert!(
        env.call_static_method(FIXTURE, "currentLaunched", "()Z", &[])
            .unwrap()
            .z()
            .unwrap()
    );
    cancel_pending();
    assert_eq!(
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap_err(),
        "Document selection cancelled"
    );

    // Report a missing foreground activity through the bound callback.
    env.call_static_method(FIXTURE, "destroy", "()V", &[])
        .unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    pick_import(ImportKind::AddonZip, move |result| {
        let _ = send.send(result);
    });
    env.call_static_method(FIXTURE, "drain", "()V", &[])
        .unwrap();
    assert_eq!(
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap_err(),
        "Reader document activity is unavailable"
    );

    // JNI errors must retain the Java cause and clear it so a retry can work.
    env.set_static_field(
        "android/app/Application",
        ("android/app/Application", "failLoader", "Z"),
        JValue::Bool(1),
    )
    .unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    pick_import(ImportKind::AddonZip, move |result| {
        let _ = send.send(result);
    });
    let error = receive
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap()
        .unwrap_err();
    assert!(
        error.contains("java.lang.IllegalStateException: fixture class loader failure"),
        "{error}"
    );
    assert!(!env.exception_check().unwrap());
    env.set_static_field(
        "android/app/Application",
        ("android/app/Application", "failLoader", "Z"),
        JValue::Bool(0),
    )
    .unwrap();
    env.call_static_method(FIXTURE, "create", "()V", &[])
        .unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    pick_import(ImportKind::AddonZip, move |result| {
        let _ = send.send(result);
    });
    env.call_static_method(FIXTURE, "drain", "()V", &[])
        .unwrap();
    env.call_static_method(FIXTURE, "cancelResult", "()V", &[])
        .unwrap();
    assert!(
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .is_err()
    );
    assert!(!env.exception_check().unwrap());
    unsafe {
        ndk_context::release_android_context();
    }
}
