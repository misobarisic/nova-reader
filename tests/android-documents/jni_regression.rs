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
fn saved_image_text(env: &mut JNIEnv<'_>, name: &str) -> String {
    let name = env.new_string(name).unwrap();
    let text = env
        .call_static_method(
            FIXTURE,
            "savedText",
            "(Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(&name)],
        )
        .unwrap()
        .l()
        .unwrap();
    env.get_string(&JString::from(text)).unwrap().into()
}
struct ImageSave {
    prompts: std::sync::mpsc::Receiver<(String, ImageConflictReply)>,
    done: std::sync::mpsc::Receiver<Result<(), String>>,
}
fn collision_save(
    path: &std::path::Path,
    name: &str,
    policy: ImageConflictPolicy,
    current: Arc<std::sync::atomic::AtomicBool>,
) -> ImageSave {
    let (prompt_send, prompts) = std::sync::mpsc::channel();
    let (done_send, done) = std::sync::mpsc::channel();
    publish_image_with_policy(
        path.to_owned(),
        name,
        "image/png",
        None,
        ImageSaveOptions {
            policy,
            resolver: Arc::new(move |name, reply| {
                let _ = prompt_send.send((name, reply));
            }),
            current: Arc::new(move || current.load(Ordering::SeqCst)),
        },
        move |result| {
            let _ = done_send.send(result);
        },
    );
    ImageSave { prompts, done }
}
fn current_image() -> Arc<std::sync::atomic::AtomicBool> {
    Arc::new(std::sync::atomic::AtomicBool::new(true))
}
fn collision_pump(env: &mut JNIEnv<'_>, choose_tree: bool) {
    fixture_run(env, "drain");
    if choose_tree {
        fixture_run(env, "imageTreeResult");
    }
    fixture_run(env, "awaitCopies");
}
fn image_collisions_require_a_choice_and_preserve_existing_bytes(env: &mut JNIEnv<'_>) {
    let staged = cache_dir().unwrap().join("collision.image");
    for api in [26, 35] {
        env.call_static_method(FIXTURE, "setApi", "(I)V", &[JValue::Int(api)])
            .unwrap();
        fixture_run(env, "resetMedia");
        fixture_run(env, "resetTree");
        std::fs::write(&staged, b"old encoded image").unwrap();
        let first = collision_save(
            &staged,
            "photo.png",
            ImageConflictPolicy::Ask,
            current_image(),
        );
        collision_pump(env, api < 29);
        first
            .done
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(first.prompts.try_recv().is_err());
        assert_eq!(saved_image_text(env, "photo.png"), "old encoded image");
        std::fs::write(&staged, b"new encoded image").unwrap();

        let ask = collision_save(
            &staged,
            "photo.png",
            ImageConflictPolicy::Ask,
            current_image(),
        );
        collision_pump(env, false);
        let (name, reply) = ask
            .prompts
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(name, "photo.png");
        assert!(ask.done.try_recv().is_err());
        assert_eq!(saved_image_text(env, "photo.png"), "old encoded image");
        let (send, receive) = std::sync::mpsc::channel();
        copy_image(staged.clone(), "image/png", move |result| {
            let _ = send.send(result);
        });
        assert!(
            receive.recv().unwrap().is_err(),
            "a collision keeps the sole staging request reserved"
        );
        reply(ImageConflictChoice::Cancel);
        assert_eq!(
            ask.done.recv().unwrap().unwrap_err(),
            "Image action cancelled"
        );
        assert_eq!(saved_image_text(env, "photo.png"), "old encoded image");

        let skip = collision_save(
            &staged,
            "photo.png",
            ImageConflictPolicy::DoNothing,
            current_image(),
        );
        collision_pump(env, false);
        assert_eq!(
            skip.done.recv().unwrap().unwrap_err(),
            "Image already exists; nothing saved."
        );
        assert!(skip.prompts.try_recv().is_err());
        assert_eq!(saved_image_text(env, "photo.png"), "old encoded image");
        let overwrite = collision_save(
            &staged,
            "photo.png",
            ImageConflictPolicy::Overwrite,
            current_image(),
        );
        collision_pump(env, false);
        overwrite.done.recv().unwrap().unwrap();
        assert!(overwrite.prompts.try_recv().is_err());
        assert_eq!(saved_image_text(env, "photo.png"), "new encoded image");
        assert_eq!(
            fixture_updates(env, if api < 29 { "treeFiles" } else { "mediaRows" }),
            1
        );
        if api >= 29 {
            assert!(fixture_bool(env, "ownershipQueried"));
        }

        for number in 2..=3 {
            std::fs::write(&staged, format!("numbered image {number}")).unwrap();
            let numbered = collision_save(
                &staged,
                "photo.png",
                ImageConflictPolicy::Ask,
                current_image(),
            );
            collision_pump(env, false);
            numbered.prompts.recv().unwrap().1(ImageConflictChoice::AddNumber);
            fixture_run(env, "awaitCopies");
            numbered.done.recv().unwrap().unwrap();
            assert_eq!(
                saved_image_text(env, &format!("photo ({number}).png")),
                format!("numbered image {number}")
            );
            assert_eq!(saved_image_text(env, "photo.png"), "new encoded image");
        }
        for phase in 1..=if api < 29 { 2 } else { 3 } {
            std::fs::write(&staged, b"replacement that fails").unwrap();
            env.call_static_method(FIXTURE, "failOverwrite", "(I)V", &[JValue::Int(phase)])
                .unwrap();
            let failed = collision_save(
                &staged,
                "photo.png",
                ImageConflictPolicy::Overwrite,
                current_image(),
            );
            collision_pump(env, false);
            assert!(failed.done.recv().unwrap().is_err());
            assert_eq!(saved_image_text(env, "photo.png"), "new encoded image");
            assert_eq!(fixture_updates(env, "imageBackups"), 0);
        }
        fixture_run(env, "cancelMediaDuringWrite");
        let cancelled = collision_save(
            &staged,
            "photo.png",
            ImageConflictPolicy::Overwrite,
            current_image(),
        );
        collision_pump(env, false);
        assert!(cancelled.done.recv().unwrap().is_err());
        assert_eq!(saved_image_text(env, "photo.png"), "new encoded image");
        assert_eq!(fixture_updates(env, "imageBackups"), 0);

        let current = current_image();
        let stale = collision_save(
            &staged,
            "photo.png",
            ImageConflictPolicy::Ask,
            current.clone(),
        );
        collision_pump(env, false);
        let reply = stale.prompts.recv().unwrap().1;
        current.store(false, Ordering::SeqCst);
        reply(ImageConflictChoice::Overwrite);
        fixture_run(env, "awaitCopies");
        assert!(stale.done.recv().unwrap().is_err());
        assert_eq!(saved_image_text(env, "photo.png"), "new encoded image");
        std::fs::write(&staged, b"rotation replacement").unwrap();
        let recreated = collision_save(
            &staged,
            "photo.png",
            ImageConflictPolicy::Ask,
            current_image(),
        );
        collision_pump(env, false);
        let reply = recreated.prompts.recv().unwrap().1;
        fixture_run(env, "recreate");
        reply(ImageConflictChoice::Overwrite);
        fixture_run(env, "awaitCopies");
        recreated.done.recv().unwrap().unwrap();
        assert_eq!(saved_image_text(env, "photo.png"), "rotation replacement");

        let late = collision_save(
            &staged,
            "photo.png",
            ImageConflictPolicy::Ask,
            current_image(),
        );
        collision_pump(env, false);
        let late_reply = late.prompts.recv().unwrap().1;
        cancel_pending();
        assert!(late.done.recv().unwrap().is_err());
        late_reply(ImageConflictChoice::Overwrite);
        fixture_run(env, "awaitCopies");
        assert_eq!(saved_image_text(env, "photo.png"), "rotation replacement");
        if api >= 29 {
            let replaced = collision_save(
                &staged,
                "photo.png",
                ImageConflictPolicy::Ask,
                current_image(),
            );
            collision_pump(env, false);
            let reply = replaced.prompts.recv().unwrap().1;
            fixture_run(env, "replaceSelectedMedia");
            reply(ImageConflictChoice::Overwrite);
            fixture_run(env, "awaitCopies");
            assert!(replaced.done.recv().unwrap().is_err());
            assert_eq!(saved_image_text(env, "photo.png"), "unselected replacement");
            assert_eq!(saved_image_text(env, "moved.png"), "rotation replacement");
        }
    }
    // A foreign row must never be overwritten, even if its name blocks a fresh
    // insertion. Detect and remove a provider's silent auto-numbering instead.
    fixture_run(env, "resetMedia");
    let arguments = [
        "photo.png",
        "Pictures/Nova Reader/",
        "other.application",
        "foreign bytes",
    ]
    .map(|value| env.new_string(value).unwrap());
    env.call_static_method(
        FIXTURE,
        "seedMedia",
        "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
        &arguments
            .iter()
            .map(|value| JValue::Object(value))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let foreign = collision_save(
        &staged,
        "photo.png",
        ImageConflictPolicy::Overwrite,
        current_image(),
    );
    collision_pump(env, false);
    assert!(
        foreign
            .done
            .recv()
            .unwrap()
            .unwrap_err()
            .contains("changed the requested filename")
    );
    assert_eq!(saved_image_text(env, "photo.png"), "foreign bytes");
    assert_eq!(fixture_updates(env, "mediaRows"), 1);
    fixture_run(env, "resetMedia");
    let current = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let expired = collision_save(&staged, "photo.png", ImageConflictPolicy::Ask, current);
    collision_pump(env, false);
    assert!(expired.done.recv().unwrap().is_err());
    assert!(expired.prompts.try_recv().is_err());
    assert_eq!(fixture_updates(env, "mediaRows"), 0);
    for filename in [
        format!("{}.png", "t".repeat(154)),
        format!("{}.png", "é".repeat(77)),
    ] {
        fixture_run(env, "resetMedia");
        let first = collision_save(
            &staged,
            &filename,
            ImageConflictPolicy::Ask,
            current_image(),
        );
        collision_pump(env, false);
        first.done.recv().unwrap().unwrap();
        assert_eq!(
            fixture_text(env, "mediaName"),
            filename,
            "the deterministic name must not be silently shortened"
        );
        let numbered = collision_save(
            &staged,
            &filename,
            ImageConflictPolicy::Ask,
            current_image(),
        );
        collision_pump(env, false);
        let (shown, reply) = numbered.prompts.recv().unwrap();
        assert_eq!(shown, filename);
        reply(ImageConflictChoice::AddNumber);
        fixture_run(env, "awaitCopies");
        numbered.done.recv().unwrap().unwrap();
        let actual = fixture_text(env, "mediaName");
        assert!(actual.len() <= 160);
        assert!(actual.ends_with(" (2).png"));
    }
    fixture_run(env, "resetMedia");
    env.set_static_field(
        "android/content/ContentResolver",
        ("android/content/ContentResolver", "failQuery", "Z"),
        JValue::Bool(1),
    )
    .unwrap();
    let failed = collision_save(
        &staged,
        "photo.png",
        ImageConflictPolicy::Ask,
        current_image(),
    );
    collision_pump(env, false);
    assert!(
        failed
            .done
            .recv()
            .unwrap()
            .unwrap_err()
            .contains("fixture query failure")
    );
    assert!(!env.exception_check().unwrap());
    env.set_static_field(
        "android/content/ContentResolver",
        ("android/content/ContentResolver", "failQuery", "Z"),
        JValue::Bool(0),
    )
    .unwrap();
    let retried = collision_save(
        &staged,
        "photo.png",
        ImageConflictPolicy::Ask,
        current_image(),
    );
    collision_pump(env, false);
    retried.done.recv().unwrap().unwrap();
    std::fs::remove_file(staged).unwrap();
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
    // The editor can be cancelled after Java has staged its encoded image but
    // before the queued main-thread clipboard write. Preserve the old clip.
    let previous_clip = fixture_text(env, "clipboardUri");
    let current = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let check = current.clone();
    let (send, receive) = std::sync::mpsc::channel();
    copy_image_current(
        staged.clone(),
        "image/png",
        Arc::new(move || check.load(Ordering::Acquire)),
        move |result| {
            let _ = send.send(result);
        },
    );
    fixture_run(env, "drain");
    fixture_run(env, "awaitCopies");
    current.store(false, Ordering::Release);
    fixture_run(env, "drain");
    assert!(
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .is_err()
    );
    assert_eq!(fixture_text(env, "clipboardUri"), previous_clip);
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
    for (encoded, mime, ext) in [
        (include_bytes!("../../crates/reader-engine/tests/fixtures/tiny.avif").as_slice(), "image/avif", "avif"),
        (include_bytes!("../../crates/reader-engine/tests/fixtures/jxl/rgba.jxl").as_slice(), "image/jxl", "jxl"),
    ] {
    let staged = cache_dir().unwrap().join(format!("codec-transfer.{ext}"));
    for api in [26, 35] {
        env.call_static_method(FIXTURE, "setApi", "(I)V", &[JValue::Int(api)])
            .unwrap();
        fixture_run(env, "resetMedia");
        if api < 29 {
            fixture_run(env, "resetTree");
        }
        std::fs::write(&staged, encoded).unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        publish_image(
            staged.clone(),
            "capture",
            mime,
            Some("Series"),
            move |result| {
                let _ = send.send(result);
            },
        );
        fixture_run(env, "drain");
        if api < 29 {
            assert_eq!(
                fixture_text(env, "action"),
                "android.intent.action.OPEN_DOCUMENT_TREE"
            );
            fixture_run(env, "imageTreeResult");
        }
        fixture_run(env, "awaitCopies");
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(fixture_data(env, "exportedData"), encoded);
        if api >= 29 {
            assert_eq!(fixture_text(env, "mediaMime"), mime);
            assert_eq!(fixture_text(env, "mediaName"), format!("capture.{ext}"));
            assert_eq!(
                fixture_text(env, "mediaPath"),
                "Pictures/Nova Reader/Series/"
            );
            assert!(fixture_bool(env, "mediaAllVisible"));
        }

        // Clipboard consumers receive the actual encoded image, including on an
        // older OS whose image decoder may not support it. No silent transcoding.
        let (send, receive) = std::sync::mpsc::channel();
        copy_image(staged.clone(), mime, move |result| {
            let _ = send.send(result);
        });
        fixture_run(env, "drain");
        fixture_run(env, "awaitCopies");
        fixture_run(env, "drain");
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(fixture_text(env, "clipboardMime"), mime);
        assert_eq!(
            fixture_text(env, "clipboardDisplayName"),
            format!("Reader image.{ext}")
        );
        assert!(fixture_text(env, "clipboardUri").ends_with(&format!(".{ext}")));
        std::fs::remove_file(&staged).unwrap();
        fixture_run(env, "restartProvider");
        assert_eq!(fixture_data(env, "clipboardData"), encoded);
    }
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
    let long_folder = "界".repeat(33); // 99 UTF-8 bytes, above the shared 96-byte folder limit.
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

    fixture_run(env, "resetMedia");
    let boundary_folder = "界".repeat(32); // 96 UTF-8 bytes, accepted on both platforms.
    let (send, receive) = std::sync::mpsc::channel();
    publish_image(
        staged.clone(),
        "page.png",
        "image/png",
        Some(&boundary_folder),
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
    assert_eq!(
        fixture_text(env, "mediaPath"),
        format!("Pictures/Nova Reader/{boundary_folder}/")
    );

    fixture_run(env, "resetMedia");
    // API 26-28 grants a scoped folder once, without broad storage permission.
    env.call_static_method(FIXTURE, "setApi", "(I)V", &[JValue::Int(28)])
        .unwrap();
    fixture_run(env, "resetTree");
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
        "android.intent.action.OPEN_DOCUMENT_TREE"
    );
    assert_eq!(fixture_updates(env, "mediaRows"), 0);
    fixture_run(env, "imageTreeResult");
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
}

fn reader_immersive_follows_controls_and_activity_lifecycle(env: &mut JNIEnv<'_>) {
    for api in [26, 28, 29, 30, 35] {
        env.call_static_method(FIXTURE, "setApi", "(I)V", &[JValue::Int(api)])
            .unwrap();
        fixture_run(env, "destroy");
        fixture_run(env, "create");
        assert_reader_window_layout(env);
        assert!(
            !fixture_bool(env, "navigationHidden"),
            "bars stay visible outside immersive reading"
        );
        let layout_changes = fixture_updates(env, "layoutChanges");
        let fitting_calls = fixture_updates(env, "decorFitCalls");
        assert_eq!(layout_changes, 1);
        set_reader_immersive(false).unwrap();
        env.call_static_method(FIXTURE, "drain", "()V", &[])
            .unwrap();
        assert!(!fixture_bool(env, "statusHidden"));
        assert!(!fixture_bool(env, "navigationHidden"));
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
        assert!(
            fixture_bool(env, "navigationHidden"),
            "immersive reading hides the navigation bar too"
        );
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

        // Recreation and focus regain restore the requested bars state.
        env.call_static_method(FIXTURE, "recreate", "()V", &[])
            .unwrap();
        assert!(fixture_bool(env, "statusHidden"));
        assert!(fixture_bool(env, "navigationHidden"));
        assert_reader_window_layout(env);
        env.call_static_method(FIXTURE, "revealSystemBars", "()V", &[])
            .unwrap();
        assert!(!fixture_bool(env, "statusHidden"));
        assert!(!fixture_bool(env, "navigationHidden"));
        env.call_static_method(FIXTURE, "focus", "()V", &[])
            .unwrap();
        assert!(fixture_bool(env, "statusHidden"));
        assert!(
            fixture_bool(env, "navigationHidden"),
            "focus regain re-hides both bars"
        );
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
    assert!(
        fixture_bool(env, "navigationHidden"),
        "deferred attachment hides both bars"
    );
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

fn reader_presentation_restores_orientation_wake_and_cutout(env: &mut JNIEnv<'_>) {
    for api in [26, 28, 29, 30, 35] {
        set_reader_presentation(false, false, 0, false, true).unwrap();
        fixture_run(env, "drain");
        fixture_run(env, "destroy");
        env.call_static_method(FIXTURE, "setApi", "(I)V", &[JValue::Int(api)])
            .unwrap();
        fixture_run(env, "create");
        fixture_run(env, "resume");
        fixture_run(env, "seedReaderBaseline");
        set_reader_presentation(true, true, 1, true, false).unwrap();
        assert_eq!(
            fixture_updates(env, "orientation"),
            8,
            "presentation must queue to main"
        );
        assert!(!fixture_bool(env, "keepAwake"));
        fixture_run(env, "drain");
        assert_eq!(fixture_updates(env, "orientation"), 10);
        assert!(fixture_bool(env, "keepAwake"));
        assert!(fixture_bool(env, "statusHidden"));
        if api >= 28 {
            assert_eq!(fixture_updates(env, "cutoutMode"), 2);
        }
        let layout_changes = fixture_updates(env, "layoutChanges");
        for (rotation, expected) in [(0, -1), (1, 10), (2, 7), (3, 6), (4, 1), (5, 0), (6, 9)] {
            set_reader_presentation(true, false, rotation, false, true).unwrap();
            fixture_run(env, "drain");
            assert_eq!(fixture_updates(env, "orientation"), expected);
            assert!(!fixture_bool(env, "keepAwake"));
            assert!(!fixture_bool(env, "statusHidden"));
            if api >= 28 {
                assert_eq!(fixture_updates(env, "cutoutMode"), 1);
            }
            assert_eq!(
                fixture_updates(env, "layoutChanges"),
                layout_changes,
                "OSD must preserve edge-to-edge layout"
            );
        }
        set_reader_presentation(true, true, 4, true, false).unwrap();
        fixture_run(env, "drain");
        fixture_run(env, "pause");
        assert!(!fixture_bool(env, "keepAwake"));
        set_reader_presentation(true, true, 4, true, false).unwrap();
        fixture_run(env, "drain");
        assert!(
            !fixture_bool(env, "keepAwake"),
            "paused requests must not reactivate the wake flag"
        );
        fixture_run(env, "resume");
        assert!(fixture_bool(env, "keepAwake"));
        set_reader_presentation(false, true, 6, true, false).unwrap();
        fixture_run(env, "drain");
        assert_eq!(fixture_updates(env, "orientation"), 8);
        assert!(!fixture_bool(env, "keepAwake"));
        assert!(!fixture_bool(env, "statusHidden"));
        assert!(fixture_bool(env, "cutoutAllowed"));
        set_reader_presentation(true, true, 5, true, false).unwrap();
        fixture_run(env, "drain");
        fixture_run(env, "recreate");
        assert!(!fixture_bool(env, "previousKeepAwake"));
        assert!(
            !fixture_bool(env, "keepAwake"),
            "creation must wait for resume before keeping awake"
        );
        fixture_run(env, "resume");
        assert!(fixture_bool(env, "keepAwake"));
        assert_eq!(fixture_updates(env, "orientation"), 0);
        set_reader_presentation(true, true, 2, true, false).unwrap();
        set_reader_presentation(false, false, 0, false, true).unwrap();
        fixture_run(env, "drain");
        assert_eq!(
            fixture_updates(env, "orientation"),
            -1,
            "replacement Activity restores its own baseline"
        );
        assert!(!fixture_bool(env, "statusHidden"));
        assert!(fixture_bool(env, "cutoutAllowed"));
    }
    // A pre-existing normal wake flag survives a reader exit while paused,
    // while the paused window itself always releases it.
    fixture_run(env, "seedKeepAwake");
    set_reader_presentation(true, false, 2, false, true).unwrap();
    fixture_run(env, "drain");
    assert!(!fixture_bool(env, "keepAwake"));
    fixture_run(env, "pause");
    set_reader_presentation(false, false, 0, false, true).unwrap();
    fixture_run(env, "drain");
    assert!(!fixture_bool(env, "keepAwake"));
    fixture_run(env, "resume");
    assert!(
        fixture_bool(env, "keepAwake"),
        "reader exit restores the prior normal wake flag after resume"
    );
    let error = set_reader_presentation(true, true, 7, true, true).unwrap_err();
    assert!(error.contains("Unknown reader rotation"), "{error}");
    assert!(!env.exception_check().unwrap());
    set_reader_presentation(false, false, 0, false, true).unwrap();
    fixture_run(env, "drain");
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
    reader_presentation_restores_orientation_wake_and_cutout(&mut env);
    image_export_and_clipboard_have_real_readable_bytes(&mut env);
    media_store_publishes_complete_images_and_rolls_back_failures(&mut env);
    avif_save_and_clipboard_preserve_encoded_bytes(&mut env);
    image_collisions_require_a_choice_and_preserve_existing_bytes(&mut env);

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
