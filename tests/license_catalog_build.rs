//! Execute Nova's copied generator in child processes with a bounded mock tool.
//! This keeps feature/env selection isolated from the test runner and real tool.
#![cfg(unix)]
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

#[allow(dead_code)]
mod actual_build_script {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/build.rs"));
    pub fn generate() {
        generate_license_catalog();
    }
}

#[test]
#[ignore = "child-process entry point; exercised by parent regressions"]
fn license_generator_child() {
    actual_build_script::generate();
}

struct Fixture {
    _directory: tempfile::TempDir,
    bin: PathBuf,
    out: PathBuf,
    log: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join("bin");
        let out = directory.path().join("out");
        let log = directory.path().join("calls.jsonl");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&out).unwrap();
        let python = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let script = format!("#!{}\n", String::from_utf8(python.stdout).unwrap().trim())
            + r#"import json, os, sys
with open(os.environ['MOCK_ABOUT_LOG'], 'a') as log:
    log.write(json.dumps({'args': sys.argv[1:], 'cwd': os.getcwd()}) + '\n')
if os.environ.get('MOCK_ABOUT_FAIL'):
    sys.stderr.write('mock unresolved license\n')
    sys.exit(7)
if os.environ.get('MOCK_ABOUT_INVALID_UTF8'):
    sys.stdout.buffer.write(b'\xff')
    sys.exit(0)
if sys.argv[-1].endswith('open_source_crate_sources.hbs'):
    print('fixture-crate 1.2.3\tMIT\thttps://example.test/crate')
else:
    print('MOCK FULL LICENSE ' + os.environ.get('MOCK_ABOUT_LABEL', 'original'))
"#;
        let tool = bin.join("cargo-about");
        fs::write(&tool, script).unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _directory: directory,
            bin,
            out,
            log,
        }
    }
    fn command(&self, target: &str, features: &[&str]) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "license_generator_child",
                "--ignored",
                "--nocapture",
            ])
            .env("PATH", &self.bin)
            .env("OUT_DIR", &self.out)
            .env("TARGET", target)
            .env("CARGO_MANIFEST_DIR", env!("CARGO_MANIFEST_DIR"))
            .env("CARGO_PKG_VERSION", "1.2.3")
            .env("MOCK_ABOUT_LOG", &self.log);
        for feature in ["DESKTOP", "ANDROID", "UPDATE_CHECK"] {
            command.env_remove(format!("CARGO_FEATURE_{feature}"));
        }
        for feature in features {
            command.env(format!("CARGO_FEATURE_{feature}"), "1");
        }
        command
            .env_remove("MOCK_ABOUT_FAIL")
            .env_remove("MOCK_ABOUT_INVALID_UTF8");
        command
    }
    fn generate(&self, target: &str, features: &[&str]) -> Output {
        let result = self.command(target, features).output().unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        result
    }
    fn calls(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

#[test]
fn generator_renders_only_the_current_target_and_actual_features() {
    for (target, features, expected) in [
        (
            "aarch64-linux-android",
            vec!["ANDROID", "UPDATE_CHECK"],
            Some("android,update-check"),
        ),
        ("armv7-linux-androideabi", vec!["ANDROID"], Some("android")),
        (
            "x86_64-pc-windows-gnu",
            vec!["DESKTOP", "UPDATE_CHECK"],
            Some("desktop,update-check"),
        ),
        ("x86_64-unknown-linux-gnu", vec!["DESKTOP"], Some("desktop")),
        ("x86_64-unknown-linux-gnu", vec![], None),
    ] {
        let fixture = Fixture::new();
        let output = fixture.generate(target, &features);
        let watches = String::from_utf8(output.stdout).unwrap();
        for key in [
            "TARGET",
            "CARGO_FEATURE_DESKTOP",
            "CARGO_FEATURE_ANDROID",
            "CARGO_FEATURE_UPDATE_CHECK",
        ] {
            assert!(watches.contains(&format!("cargo:rerun-if-env-changed={key}")));
        }
        for path in [
            "THIRD_PARTY_NOTICES.md",
            "assets/fonts/LICENSE.txt",
            "vendor/ferri-pdf-rav1d/COPYING",
        ] {
            assert!(watches.contains(&format!("cargo:rerun-if-changed={path}")));
        }
        let calls = fixture.calls();
        assert_eq!(
            calls.len(),
            2,
            "one source render and one license render per current target"
        );
        for (call, template) in calls.iter().zip([
            "assets/open_source_crate_sources.hbs",
            "assets/open_source_licenses.hbs",
        ]) {
            let mut args = vec![
                "generate",
                "--locked",
                "--fail",
                "-c",
                "about.toml",
                "--target",
                target,
                "--no-default-features",
            ];
            if let Some(features) = expected {
                args.extend(["--features", features]);
            }
            args.push(template);
            assert_eq!(call["args"], serde_json::json!(args));
            assert_eq!(
                Path::new(call["cwd"].as_str().unwrap()),
                Path::new(env!("CARGO_MANIFEST_DIR"))
            );
        }
    }
}

#[test]
fn generator_reuses_the_exact_nova_frame_recipe_and_retains_all_local_notices() {
    let fixture = Fixture::new();
    fixture.generate("x86_64-unknown-linux-gnu", &["DESKTOP"]);
    let catalog = fs::read(fixture.out.join("nova_license_catalog.txt")).unwrap();
    let compressed_path = fixture.out.join("nova_license_catalog.zst");
    let compressed = fs::read(&compressed_path).unwrap();
    assert_eq!(compressed[4] & 4, 4, "Zstd frame checksum enabled");
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(&compressed).unwrap(),
        Some(catalog.len() as u64)
    );
    assert_eq!(
        zstd::bulk::decompress(&compressed, catalog.len()).unwrap(),
        catalog
    );
    let text = std::str::from_utf8(&catalog).unwrap();
    for path in [
        "LICENSE",
        "THIRD_PARTY_NOTICES.md",
        "assets/fonts/LICENSE.txt",
        "vendor/ferri-pdf-rav1d/COPYING",
        "crates/reader-engine/AVIF-NOTICE.txt",
    ] {
        let notice = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap();
        assert!(text.contains(&notice), "full notice omitted: {path}");
    }
    assert_eq!(
        fs::read_to_string(fixture.out.join("nova_license_catalog.recipe")).unwrap(),
        "zstd:22;checksum:true;size:true;v1\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.out.join("nova_license_catalog_size.rs")).unwrap(),
        format!(
            "pub(crate) const OPEN_SOURCE_LICENSE_CATALOG_SIZE: usize = {};\n",
            catalog.len()
        )
    );
    let links = fs::read_to_string(fixture.out.join("nova_license_sources.tsv")).unwrap();
    assert!(links.contains("fixture-crate 1.2.3\tMIT\thttps://example.test/crate"));
    assert!(links.contains(include_str!("../assets/open_source_project_sources.tsv")));
    let before = fs::metadata(&compressed_path).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    fixture.generate("x86_64-unknown-linux-gnu", &["DESKTOP"]);
    assert_eq!(
        fixture.calls().len(),
        4,
        "Nova rerenders text but reuses identical compressed output"
    );
    assert_eq!(fs::read(&compressed_path).unwrap(), compressed);
    assert_eq!(
        fs::metadata(&compressed_path).unwrap().modified().unwrap(),
        before
    );
    fs::write(
        fixture.out.join("nova_license_catalog.recipe"),
        "stale recipe",
    )
    .unwrap();
    fs::write(&compressed_path, "invalid old frame").unwrap();
    fixture.generate("x86_64-unknown-linux-gnu", &["DESKTOP"]);
    assert_eq!(
        fs::read(&compressed_path).unwrap(),
        compressed,
        "recipe mismatch regenerates the frame"
    );
}

#[test]
fn generator_fails_closed_for_missing_tool_unresolved_license_and_invalid_text() {
    let fixture = Fixture::new();
    let missing = fixture
        .command("x86_64-unknown-linux-gnu", &["DESKTOP"])
        .env("PATH", fixture.out.join("missing"))
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr)
            .contains("license catalog generation needs cargo-about")
    );
    fixture.generate("x86_64-unknown-linux-gnu", &["DESKTOP"]);
    let original = fs::read(fixture.out.join("nova_license_catalog.zst")).unwrap();
    for (key, message) in [
        ("MOCK_ABOUT_FAIL", "mock unresolved license"),
        (
            "MOCK_ABOUT_INVALID_UTF8",
            "cargo-about returned non-UTF-8 output",
        ),
    ] {
        let failed = fixture
            .command("x86_64-unknown-linux-gnu", &["DESKTOP"])
            .env(key, "1")
            .output()
            .unwrap();
        assert!(!failed.status.success());
        assert!(String::from_utf8_lossy(&failed.stderr).contains(message));
        assert_eq!(
            fs::read(fixture.out.join("nova_license_catalog.zst")).unwrap(),
            original
        );
    }
}
