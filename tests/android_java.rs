//! The download service's Java source is rendered from nova-android's template.
//! The committed copy is what cargo-apk2 compiles, so it must stay equal to the
//! rendering. Set `READER_UPDATE_JAVA=1` to rewrite it after changing the names.
use nova_android::java::Names;
use std::path::Path;

const NAMES: Names = Names {
    package: "dev.misob.nova.reader",
    prefix: "Reader",
    channel_id: "reader.background",
    wake_lock_tag: "reader:downloads",
};

#[test]
fn background_service_matches_the_shared_template() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("android/java/dev/misob/nova/reader")
        .join(NAMES.background_service_file());
    let rendered = NAMES.background_service();
    if std::env::var_os("READER_UPDATE_JAVA").is_some() {
        std::fs::write(&path, &rendered).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap();
    assert!(
        committed == rendered,
        "{} differs from the shared template; run with READER_UPDATE_JAVA=1",
        path.display()
    );
}
