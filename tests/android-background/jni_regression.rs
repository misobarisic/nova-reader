use super::*;
use jni::{
    InitArgsBuilder, JNIVersion, JavaVM,
    objects::{JObject, JString},
};
use nova_android::context::{initialize_foreground_context, with_app_context};
use nova_android::service::NOTIFICATION_REFRESH;
use std::time::{Duration, Instant};

const FIXTURE: &str = "dev/misob/nova/reader/BackgroundServiceFixture";
fn fixture_run(env: &mut jni::JNIEnv<'_>, method: &str) {
    env.call_static_method(FIXTURE, method, "()V", &[]).unwrap();
}
fn fixture_int(env: &mut jni::JNIEnv<'_>, method: &str) -> i32 {
    env.call_static_method(FIXTURE, method, "()I", &[])
        .unwrap()
        .i()
        .unwrap()
}
fn fixture_bool(env: &mut jni::JNIEnv<'_>, method: &str) -> bool {
    env.call_static_method(FIXTURE, method, "()Z", &[])
        .unwrap()
        .z()
        .unwrap()
}
fn fixture_text(env: &mut jni::JNIEnv<'_>, method: &str) -> String {
    let value = env
        .call_static_method(FIXTURE, method, "()Ljava/lang/String;", &[])
        .unwrap()
        .l()
        .unwrap();
    env.get_string(&JString::from(value)).unwrap().into()
}
fn native_tick(busy: bool, title: &str, text: &str) {
    let title = title.to_owned();
    let text = text.to_owned();
    std::thread::spawn(move || set_download_service(busy, &title, &text))
        .join()
        .unwrap();
}

#[test]
fn production_download_service_jni_survives_activity_drop_and_throttles_notifications() {
    let classpath = format!(
        "-Djava.class.path={}",
        std::env::var("READER_BACKGROUND_JNI_CLASSPATH").unwrap()
    );
    let args = InitArgsBuilder::new()
        .version(JNIVersion::V8)
        .option(&classpath)
        .option("-Xcheck:jni")
        .build()
        .unwrap();
    let vm = JavaVM::new(args).unwrap();
    let mut env = vm.attach_current_thread().unwrap();
    let activity = env
        .call_static_method(
            FIXTURE,
            "bootstrapActivity",
            "()Ldev/misob/nova/reader/ReaderActivity;",
            &[],
        )
        .unwrap()
        .l()
        .unwrap();
    initialize_foreground_context(vm.get_java_vm_pointer().cast(), activity.as_raw().cast());
    fixture_run(&mut env, "dropActivity");
    env.delete_local_ref(activity).unwrap();
    assert!(
        with_app_context(|env, raw| {
            let context = unsafe { JObject::from_raw(raw) };
            env.is_instance_of(&context, "android/app/Application")
        })
        .unwrap(),
        "retain Application rather than a resumed/destroyed Activity"
    );

    native_tick(true, "Reader downloads", "1 chapter queued");
    assert_eq!(fixture_int(&mut env, "foregroundStarts"), 1);
    assert!(fixture_bool(&mut env, "applicationOwnsService"));
    fixture_run(&mut env, "createService");
    fixture_run(&mut env, "drain");
    let initial = fixture_int(&mut env, "notificationUpdates");
    native_tick(true, "Reader downloads", "1 chapter queued");
    assert_eq!(
        fixture_int(&mut env, "notificationUpdates"),
        initial,
        "same body is throttled"
    );
    native_tick(true, "Reader downloads", "2 / 4 pages");
    assert_eq!(
        fixture_int(&mut env, "notificationUpdates"),
        initial + 1,
        "changed body updates immediately"
    );
    assert_eq!(
        fixture_text(&mut env, "notificationTitle"),
        "Reader downloads"
    );
    assert_eq!(fixture_text(&mut env, "notificationText"), "2 / 4 pages");
    assert_eq!(
        fixture_int(&mut env, "foregroundStarts"),
        1,
        "progress never restarts active service"
    );
    assert_eq!(
        fixture_int(&mut env, "classLoads"),
        1,
        "native threads reuse the global service class"
    );
    // The refresh window has elapsed (the shared service takes the clock).
    std::thread::spawn(|| {
        DOWNLOAD_SERVICE.set_at(
            true,
            "Reader downloads",
            "2 / 4 pages",
            Instant::now() + NOTIFICATION_REFRESH + Duration::from_millis(1),
        )
    })
    .join()
    .unwrap();
    assert_eq!(
        fixture_int(&mut env, "notificationUpdates"),
        initial + 2,
        "elapsed notification refresh is admitted"
    );
    native_tick(false, "", "");
    assert_eq!(fixture_int(&mut env, "serviceStops"), 1);
    fixture_run(&mut env, "destroyService");
    native_tick(false, "", "");
    assert_eq!(
        fixture_int(&mut env, "serviceStops"),
        1,
        "idle ticks are no-ops"
    );
    native_tick(true, "Reader downloads", "2 / 4 pages");
    assert_eq!(
        fixture_int(&mut env, "foregroundStarts"),
        2,
        "rising edge bypasses old-body throttle"
    );
    fixture_run(&mut env, "createService");
    native_tick(false, "", "");
    fixture_run(&mut env, "destroyService");

    // The queue also calls this adapter from a thread already attached to the
    // VM. Detaching a temporary worker cannot hide its pending Java exception.
    fixture_run(&mut env, "failStartOnce");
    set_download_service(true, "Reader downloads", "restart fixture");
    assert!(
        !env.exception_check().unwrap(),
        "a rejected start must clear the existing caller thread's exception"
    );
    assert!(
        !DOWNLOAD_SERVICE.is_active(),
        "failed promotion must leave a rising edge available for retry"
    );
    assert_eq!(fixture_int(&mut env, "foregroundStarts"), 3);
    // Same busy state and same body: no artificial false/idle tick is needed
    // for Android's next allowed foreground start to recover.
    set_download_service(true, "Reader downloads", "restart fixture");
    assert!(!env.exception_check().unwrap());
    assert_eq!(fixture_int(&mut env, "foregroundStarts"), 4);
    assert!(fixture_bool(&mut env, "applicationOwnsService"));
    assert_eq!(fixture_int(&mut env, "classLoads"), 1);
    native_tick(false, "", "");
    fixture_run(&mut env, "drain");
}
