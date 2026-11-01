use super::*;
use jni::{InitArgsBuilder, JNIVersion};

const FIXTURE: &str = "dev/misob/nova/reader/DocumentBridgeFixture";

fn fixture_bool(env: &mut JNIEnv<'_>, method: &str) -> bool {
    env.call_static_method(FIXTURE, method, "()Z", &[])
        .unwrap()
        .z()
        .unwrap()
}

fn fixture_updates(env: &mut JNIEnv<'_>, method: &str) -> i32 {
    env.call_static_method(FIXTURE, method, "()I", &[])
        .unwrap()
        .i()
        .unwrap()
}
fn fixture_text(env: &mut JNIEnv<'_>, method: &str) -> String {
    let value = env
        .call_static_method(FIXTURE, method, "()Ljava/lang/String;", &[])
        .unwrap()
        .l()
        .unwrap();
    env.get_string(&JString::from(value)).unwrap().into()
}
fn fixture_data(env: &mut JNIEnv<'_>, method: &str) -> Vec<u8> {
    let value = env
        .call_static_method(FIXTURE, method, "()[B", &[])
        .unwrap()
        .l()
        .unwrap();
    env.convert_byte_array(jni::objects::JByteArray::from(value))
        .unwrap()
}
fn fixture_run(env: &mut JNIEnv<'_>, method: &str) {
    env.call_static_method(FIXTURE, method, "()V", &[]).unwrap();
}

fn browser_links_use_application_and_recover_from_launch_exceptions(
    env: &mut JNIEnv<'_>,
    application: &JObject<'_>,
) {
    fn assert_link(env: &mut JNIEnv<'_>, application: &JObject<'_>, expected: &str) {
        let context = env
            .get_static_field(
                "android/content/Context",
                "browserContext",
                "Landroid/content/Context;",
            )
            .unwrap()
            .l()
            .unwrap();
        assert!(
            env.is_same_object(&context, application).unwrap(),
            "browser intents must use the actual Application"
        );
        let intent = env
            .get_static_field(
                "android/content/Context",
                "browserIntent",
                "Landroid/content/Intent;",
            )
            .unwrap()
            .l()
            .unwrap();
        let action = env
            .get_field(&intent, "action", "Ljava/lang/String;")
            .unwrap()
            .l()
            .unwrap();
        let action: String = env.get_string(&JString::from(action)).unwrap().into();
        assert_eq!(action, "android.intent.action.VIEW");
        let uri = env
            .call_method(&intent, "getData", "()Landroid/net/Uri;", &[])
            .unwrap()
            .l()
            .unwrap();
        let uri = env
            .call_method(&uri, "toString", "()Ljava/lang/String;", &[])
            .unwrap()
            .l()
            .unwrap();
        let uri: String = env.get_string(&JString::from(uri)).unwrap().into();
        assert_eq!(uri, expected);
        assert_eq!(
            env.call_method(&intent, "getFlags", "()I", &[])
                .unwrap()
                .i()
                .unwrap(),
            0x1000_0000
        );
        let category = env.new_string("android.intent.category.BROWSABLE").unwrap();
        assert!(
            env.call_method(
                &intent,
                "hasCategory",
                "(Ljava/lang/String;)Z",
                &[JValue::Object(&category)]
            )
            .unwrap()
            .z()
            .unwrap()
        );
        assert!(
            env.get_field(&intent, "type", "Ljava/lang/String;")
                .unwrap()
                .l()
                .unwrap()
                .is_null(),
            "links must resolve without an image/video MIME filter"
        );
    }

    // No ReaderActivity has been created yet; browser links require no weak
    // Activity, picker reservation or Java-main-queue launch.
    let source = "https://github.com/misobarisic/nova-reader/blob/main/LICENSE?plain=1#L1";
    open_browser(source).unwrap();
    assert_link(env, application, source);
    fixture_run(env, "create");
    fixture_run(env, "recreate");
    fixture_run(env, "destroy");
    let license = "https://www.gnu.org/licenses/gpl-3.0.html";
    open_browser(license).unwrap();
    assert_link(env, application, license);
    let launches = env
        .get_static_field("android/content/Context", "browserLaunches", "I")
        .unwrap()
        .i()
        .unwrap();
    env.set_static_field(
        "android/content/Context",
        ("android/content/Context", "failBrowserLaunch", "Z"),
        JValue::Bool(1),
    )
    .unwrap();
    let error = open_browser(source).unwrap_err();
    assert!(error.starts_with("no app can open this link ("), "{error}");
    assert!(
        error.contains("java.lang.IllegalStateException: fixture no browser activity"),
        "{error}"
    );
    assert!(!env.exception_check().unwrap());
    assert_eq!(
        env.get_static_field("android/content/Context", "browserLaunches", "I")
            .unwrap()
            .i()
            .unwrap(),
        launches
    );
    env.set_static_field(
        "android/content/Context",
        ("android/content/Context", "failBrowserLaunch", "Z"),
        JValue::Bool(0),
    )
    .unwrap();
    let retry = "https://github.com/misobarisic/nova-reader";
    open_browser(retry).unwrap();
    assert_link(env, application, retry);
    assert!(!env.exception_check().unwrap());
}

fn image_export_and_clipboard_have_real_readable_bytes(env: &mut JNIEnv<'_>) {
    let staged = cache_dir().unwrap().join("page.png");
    std::fs::write(&staged, b"fixture encoded image").unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    export_image(
        staged.clone(),
        "chapter-1.png",
        "image/png",
        move |result| {
            let _ = send.send(result);
        },
    );
    fixture_run(env, "drain");
    assert_eq!(fixture_text(env, "mime"), "image/png");
    assert_eq!(fixture_text(env, "title"), "chapter-1.png");
    assert_eq!(
        fixture_text(env, "action"),
        "android.intent.action.CREATE_DOCUMENT"
    );
    let (busy_send, busy_receive) = std::sync::mpsc::channel();
    copy_image(staged.clone(), "image/png", move |result| {
        let _ = busy_send.send(result);
    });
    assert_eq!(
        busy_receive.recv().unwrap().unwrap_err(),
        "Another document request is active"
    );
    fixture_run(env, "exportResult");
    fixture_run(env, "awaitCopies");
    receive
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap()
        .unwrap();
    assert_eq!(fixture_text(env, "exported"), "fixture encoded image");

    // A destination failure is an error, and the next image operation can run.
    env.set_static_field(
        "android/content/ContentResolver",
        ("android/content/ContentResolver", "failOutput", "Z"),
        JValue::Bool(1),
    )
    .unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    export_image(staged.clone(), "page", "image/png", move |result| {
        let _ = send.send(result);
    });
    fixture_run(env, "drain");
    fixture_run(env, "exportResult");
    fixture_run(env, "awaitCopies");
    assert!(
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .contains("fixture destination failure")
    );
    env.set_static_field(
        "android/content/ContentResolver",
        ("android/content/ContentResolver", "failOutput", "Z"),
        JValue::Bool(0),
    )
    .unwrap();

    let (send, receive) = std::sync::mpsc::channel();
    copy_image(staged.clone(), "image/png", move |result| {
        let _ = send.send(result);
    });
    fixture_run(env, "drain");
    fixture_run(env, "awaitCopies");
    fixture_run(env, "drain");
    receive
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap()
        .unwrap();
    assert!(
        fixture_text(env, "clipboardUri").starts_with("content://dev.misob.nova.reader.images/")
    );
    assert_eq!(fixture_text(env, "clipboardMime"), "image/png");
    assert!(fixture_bool(env, "providerRejectsUnsafePaths"));
    // The staged file and original Activity can disappear before a recipient
    // reads; a new provider instance still resolves the independent cache copy.
    std::fs::remove_file(&staged).unwrap();
    fixture_run(env, "destroy");
    fixture_run(env, "restartProvider");
    assert_eq!(fixture_text(env, "clipboardBytes"), "fixture encoded image");
    fixture_run(env, "create");

    // Canonical cache containment and MIME validation precede publishing.
    for (path, mime) in [
        (PathBuf::from("/etc/passwd"), "image/png"),
        (staged.clone(), "text/plain"),
    ] {
        std::fs::write(&staged, b"fixture encoded image").unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        copy_image(path, mime, move |result| {
            let _ = send.send(result);
        });
        fixture_run(env, "drain");
        assert!(
            receive
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .is_err()
        );
    }
    env.set_static_field(
        "android/content/ClipboardManager",
        ("android/content/ClipboardManager", "failClipboard", "Z"),
        JValue::Bool(1),
    )
    .unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    copy_image(staged.clone(), "image/png", move |result| {
        let _ = send.send(result);
    });
    fixture_run(env, "drain");
    fixture_run(env, "awaitCopies");
    fixture_run(env, "drain");
    assert!(
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .contains("fixture clipboard failure")
    );
    env.set_static_field(
        "android/content/ClipboardManager",
        ("android/content/ClipboardManager", "failClipboard", "Z"),
        JValue::Bool(0),
    )
    .unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    copy_image(staged.clone(), "image/png", move |result| {
        let _ = send.send(result);
    });
    cancel_pending();
    fixture_run(env, "drain");
    assert!(
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .is_err()
    );
    assert!(fixture_bool(env, "retentionAndBounds"));
    std::fs::remove_file(staged).unwrap();
}

fn avif_save_and_clipboard_preserve_encoded_bytes(env: &mut JNIEnv<'_>) {
    let encoded = include_bytes!("../../crates/reader-engine/tests/fixtures/tiny.avif");
    let staged = cache_dir().unwrap().join("avif-transfer.avif");
    for api in [26, 35] {
        env.call_static_method(FIXTURE, "setApi", "(I)V", &[JValue::Int(api)])
            .unwrap();
        fixture_run(env, "resetMedia");
        std::fs::write(&staged, encoded).unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        publish_image(
            staged.clone(),
            "capture",
            "image/avif",
            Some("Series"),
            move |result| {
                let _ = send.send(result);
            },
        );
        fixture_run(env, "drain");
        if api < 29 {
            assert_eq!(fixture_text(env, "mime"), "image/avif");
            assert_eq!(fixture_text(env, "title"), "capture.avif");
            fixture_run(env, "exportResult");
        }
        fixture_run(env, "awaitCopies");
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(fixture_data(env, "exportedData"), encoded);
        if api >= 29 {
            assert_eq!(fixture_text(env, "mediaMime"), "image/avif");
            assert_eq!(fixture_text(env, "mediaName"), "capture.avif");
            assert_eq!(
                fixture_text(env, "mediaPath"),
                "Pictures/Nova Reader/Series/"
            );
            assert!(fixture_bool(env, "mediaAllVisible"));
        }

        // Clipboard consumers receive the actual encoded AVIF, including on an
        // older OS whose image decoder may not support it. No silent transcoding.
        let (send, receive) = std::sync::mpsc::channel();
        copy_image(staged.clone(), "image/avif", move |result| {
            let _ = send.send(result);
        });
        fixture_run(env, "drain");
        fixture_run(env, "awaitCopies");
        fixture_run(env, "drain");
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(fixture_text(env, "clipboardMime"), "image/avif");
        assert_eq!(
            fixture_text(env, "clipboardDisplayName"),
            "Reader image.avif"
        );
        assert!(fixture_text(env, "clipboardUri").ends_with(".avif"));
        std::fs::remove_file(&staged).unwrap();
        fixture_run(env, "restartProvider");
        assert_eq!(fixture_data(env, "clipboardData"), encoded);
    }
}

fn media_store_publishes_complete_images_and_rolls_back_failures(env: &mut JNIEnv<'_>) {
    let staged = cache_dir().unwrap().join("published.png");
    std::fs::write(&staged, b"published encoded image").unwrap();
    for api in [29, 35] {
        env.call_static_method(FIXTURE, "setApi", "(I)V", &[JValue::Int(api)])
            .unwrap();
        fixture_run(env, "resetMedia");
        for (series, expected) in [
            (None, "Pictures/Nova Reader/"),
            (Some("Series 名"), "Pictures/Nova Reader/Series 名/"),
        ] {
            let (send, receive) = std::sync::mpsc::channel();
            publish_image(
                staged.clone(),
                "same-name.png",
                "image/png",
                series,
                move |result| {
                    let _ = send.send(result);
                },
            );
            fixture_run(env, "drain");
            fixture_run(env, "awaitCopies");
            receive
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .unwrap();
            assert_eq!(fixture_text(env, "mediaPath"), expected);
            assert_eq!(fixture_text(env, "mediaName"), "same-name.png");
            assert_eq!(fixture_text(env, "mediaMime"), "image/png");
            assert_eq!(fixture_text(env, "exported"), "published encoded image");
            assert!(fixture_bool(env, "mediaAllVisible"));
        }
        assert_eq!(
            fixture_updates(env, "mediaRows"),
            2,
            "fresh rows preserve filename collisions"
        );
        assert_eq!(fixture_updates(env, "mediaCommits"), 2);
    }

    // Insert/open/write/close/publish failures cannot leave incomplete rows.
    for phase in 1..=5 {
        fixture_run(env, "resetMedia");
        env.call_static_method(FIXTURE, "failMedia", "(I)V", &[JValue::Int(phase)])
            .unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        publish_image(
            staged.clone(),
            "page.png",
            "image/png",
            None,
            move |result| {
                let _ = send.send(result);
            },
        );
        fixture_run(env, "drain");
        fixture_run(env, "awaitCopies");
        assert!(
            receive
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .is_err()
        );
        assert_eq!(fixture_updates(env, "mediaRows"), 0);
        assert_eq!(fixture_updates(env, "mediaCommits"), 0);
        assert_eq!(fixture_updates(env, "mediaDeletes"), i32::from(phase != 1));
    }
    fixture_run(env, "resetMedia");
    fixture_run(env, "cancelMediaDuringWrite");
    let (send, receive) = std::sync::mpsc::channel();
    publish_image(
        staged.clone(),
        "page.png",
        "image/png",
        None,
        move |result| {
            let _ = send.send(result);
        },
    );
    fixture_run(env, "drain");
    fixture_run(env, "awaitCopies");
    assert!(
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .is_err()
    );
    assert_eq!(fixture_updates(env, "mediaRows"), 0);
    assert_eq!(fixture_updates(env, "mediaDeletes"), 1);

    fixture_run(env, "resetMedia");
    let long_folder = "界".repeat(27); // 81 UTF-8 bytes.
    for folder in [
        "..",
        "../escape",
        "a/b",
        "a\\b",
        "bad\n\nname",
        "trailing.",
        &long_folder,
    ] {
        let (send, receive) = std::sync::mpsc::channel();
        publish_image(
            staged.clone(),
            "page.png",
            "image/png",
            Some(folder),
            move |result| {
                let _ = send.send(result);
            },
        );
        fixture_run(env, "drain");
        assert!(
            receive
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .is_err()
        );
        assert_eq!(fixture_updates(env, "mediaRows"), 0);
    }

    // API 26-28 keeps the platform picker, with no legacy storage permission.
    env.call_static_method(FIXTURE, "setApi", "(I)V", &[JValue::Int(28)])
        .unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    publish_image(
        staged.clone(),
        "legacy.png",
        "image/png",
        Some("Series"),
        move |result| {
            let _ = send.send(result);
        },
    );
    fixture_run(env, "drain");
    assert_eq!(
        fixture_text(env, "action"),
        "android.intent.action.CREATE_DOCUMENT"
    );
    assert_eq!(fixture_text(env, "title"), "legacy.png");
    assert_eq!(fixture_text(env, "mime"), "image/png");
    assert_eq!(fixture_updates(env, "mediaRows"), 0);
    fixture_run(env, "exportResult");
    fixture_run(env, "awaitCopies");
    receive
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap()
        .unwrap();
    env.call_static_method(FIXTURE, "setApi", "(I)V", &[JValue::Int(35)])
        .unwrap();
    std::fs::remove_file(staged).unwrap();
}

fn assert_reader_window_layout(env: &mut JNIEnv<'_>) {
    assert!(fixture_bool(env, "edgeToEdge"));
    assert!(fixture_bool(env, "transparentStatusBar"));
    assert!(fixture_bool(env, "cutoutAllowed"));
    assert!(fixture_bool(env, "unrelatedWindowFlagsPreserved"));
    assert!(!fixture_bool(env, "navigationHidden"));
}

fn reader_immersive_follows_controls_and_activity_lifecycle(env: &mut JNIEnv<'_>) {
    for api in [26, 28, 29, 30, 35] {
        env.call_static_method(FIXTURE, "setApi", "(I)V", &[JValue::Int(api)])
            .unwrap();
        fixture_run(env, "destroy");
        fixture_run(env, "create");
        assert_reader_window_layout(env);
        let layout_changes = fixture_updates(env, "layoutChanges");
        let fitting_calls = fixture_updates(env, "decorFitCalls");
        assert_eq!(layout_changes, 1);
        set_reader_immersive(false).unwrap();
        env.call_static_method(FIXTURE, "drain", "()V", &[])
            .unwrap();
        assert!(!fixture_bool(env, "statusHidden"));
        assert_reader_window_layout(env);
        assert_eq!(fixture_updates(env, "layoutChanges"), layout_changes);
        assert_eq!(fixture_updates(env, "decorFitCalls"), fitting_calls);

        set_reader_immersive(true).unwrap();
        assert!(
            !fixture_bool(env, "statusHidden"),
            "must queue to Java main"
        );
        env.call_static_method(FIXTURE, "drain", "()V", &[])
            .unwrap();
        assert!(fixture_bool(env, "statusHidden"));
        assert!(fixture_bool(env, "transientSwipe"));
        assert!(!fixture_bool(env, "navigationHidden"));
        assert!(fixture_bool(env, "stableLayout"));
        assert_reader_window_layout(env);
        assert_eq!(
            fixture_updates(env, "layoutChanges"),
            layout_changes,
            "show/hide must preserve the client layout"
        );
        assert_eq!(
            fixture_updates(env, "decorFitCalls"),
            fitting_calls,
            "OSD toggles must not redispatch decor fitting"
        );

        // Recreation and focus regain restore the requested status-bar state.
        env.call_static_method(FIXTURE, "recreate", "()V", &[])
            .unwrap();
        assert!(fixture_bool(env, "statusHidden"));
        assert_reader_window_layout(env);
        env.call_static_method(FIXTURE, "revealStatusBar", "()V", &[])
            .unwrap();
        assert!(!fixture_bool(env, "statusHidden"));
        env.call_static_method(FIXTURE, "focus", "()V", &[])
            .unwrap();
        assert!(fixture_bool(env, "statusHidden"));
        assert_reader_window_layout(env);
        fixture_run(env, "disturbWindowLayout");
        assert!(!fixture_bool(env, "edgeToEdge"));
        fixture_run(env, "focus");
        assert_reader_window_layout(env);

        // A queued show uses the replacement window, never the destroyed one.
        let previous_updates = fixture_updates(env, "windowUpdates");
        set_reader_immersive(false).unwrap();
        env.call_static_method(FIXTURE, "recreate", "()V", &[])
            .unwrap();
        assert!(!fixture_bool(env, "statusHidden"));
        assert_reader_window_layout(env);
        env.call_static_method(FIXTURE, "drain", "()V", &[])
            .unwrap();
        assert!(!fixture_bool(env, "statusHidden"));
        assert!(fixture_bool(env, "previousStatusHidden"));
        assert_eq!(
            fixture_updates(env, "previousWindowUpdates"),
            previous_updates
        );
        assert!(!fixture_bool(env, "navigationHidden"));
        assert!(fixture_bool(env, "stableLayout"));
        assert_reader_window_layout(env);
        let layout_changes = fixture_updates(env, "layoutChanges");

        // The latest state wins when controls toggle faster than main drains.
        set_reader_immersive(true).unwrap();
        set_reader_immersive(false).unwrap();
        env.call_static_method(FIXTURE, "drain", "()V", &[])
            .unwrap();
        assert!(!fixture_bool(env, "statusHidden"));
        assert_reader_window_layout(env);
        assert_eq!(fixture_updates(env, "layoutChanges"), layout_changes);

        // An update between activities is retained for the next onCreate.
        env.call_static_method(FIXTURE, "destroy", "()V", &[])
            .unwrap();
        set_reader_immersive(true).unwrap();
        env.call_static_method(FIXTURE, "drain", "()V", &[])
            .unwrap();
        env.call_static_method(FIXTURE, "create", "()V", &[])
            .unwrap();
        assert!(fixture_bool(env, "statusHidden"));
        assert_reader_window_layout(env);
        set_reader_immersive(false).unwrap();
        env.call_static_method(FIXTURE, "drain", "()V", &[])
            .unwrap();
        assert!(!fixture_bool(env, "statusHidden"));
        assert_reader_window_layout(env);
        assert_eq!(fixture_updates(env, "layoutChanges"), 1);
    }
    // onCreate may precede controller attachment. Its queued retry converges
    // without another Rust request or a fullscreen/fitting toggle.
    set_reader_immersive(true).unwrap();
    fixture_run(env, "drain");
    fixture_run(env, "deferInsets");
    fixture_run(env, "recreate");
    fixture_run(env, "attachInsets");
    fixture_run(env, "drain");
    assert!(fixture_bool(env, "statusHidden"));
    assert_reader_window_layout(env);
    set_reader_immersive(false).unwrap();
    fixture_run(env, "deferInsets");
    fixture_run(env, "recreate");
    fixture_run(env, "attachInsets");
    let previous_updates = fixture_updates(env, "windowUpdates");
    fixture_run(env, "recreate");
    fixture_run(env, "drain");
    assert!(!fixture_bool(env, "statusHidden"));
    assert_reader_window_layout(env);
    assert_eq!(
        fixture_updates(env, "previousWindowUpdates"),
        previous_updates,
        "deferred attachment must resolve the current activity"
    );
}

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
    browser_links_use_application_and_recover_from_launch_exceptions(&mut env, app.as_obj());
    env.call_static_method(FIXTURE, "create", "()V", &[])
        .unwrap();
    assert_eq!(
        cache_dir().unwrap(),
        PathBuf::from(std::env::var("READER_JNI_CACHE").unwrap())
    );
    reader_immersive_follows_controls_and_activity_lifecycle(&mut env);
    image_export_and_clipboard_have_real_readable_bytes(&mut env);
    media_store_publishes_complete_images_and_rolls_back_failures(&mut env);
    avif_save_and_clipboard_preserve_encoded_bytes(&mut env);

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
    let error = set_reader_immersive(true).unwrap_err();
    assert!(
        error.contains("java.lang.IllegalStateException: fixture class loader failure"),
        "{error}"
    );
    assert!(!env.exception_check().unwrap());
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
