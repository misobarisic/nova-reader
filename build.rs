// License catalog functions copied from Nova b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda.
// Reader adaptations: watched inputs/features, project identity and local notices.
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    generate_license_catalog();
    // cargo-apk2 supplies CARGO_ENCODED_RUSTFLAGS, overriding target rustflags
    // from .cargo/config.toml. Emit this on the final Android cdylib link so
    // its ELF load segments remain compatible with 16 KiB Android pages.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        println!("cargo:rustc-link-arg-cdylib=-Wl,-z,max-page-size=16384");
    }
}

/// Generate the license and source catalog compiled into Settings → About.
fn generate_license_catalog() {
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "about.toml",
        "assets/open_source_licenses.hbs",
        "assets/open_source_crate_sources.hbs",
        "assets/open_source_project_sources.tsv",
        "assets/open_source_vendors.txt",
        "LICENSE",
        "THIRD_PARTY_NOTICES.md",
        "assets/fonts/LICENSE.txt",
        "crates/ui/Cargo.toml",
        "crates/reader-domain/Cargo.toml",
        "crates/reader-sources/Cargo.toml",
        "crates/reader-engine/Cargo.toml",
        "crates/reader-engine/AVIF-NOTICE.txt",
        "crates/reader-downloads/Cargo.toml",
        "crates/reader-capture/Cargo.toml",
        "crates/reader-catalogue/Cargo.toml",
        "assets/licenses/avif-decoder-BSD-2-Clause.txt",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    for name in [
        "TARGET",
        "CARGO_FEATURE_DESKTOP",
        "CARGO_FEATURE_ANDROID",
        "CARGO_FEATURE_UPDATE_CHECK",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    let target = env::var("TARGET").unwrap_or_else(|_| "unknown-target".into());
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let mut sources = render_cargo_about(
        &manifest_dir,
        "assets/open_source_crate_sources.hbs",
        &target,
    );
    sources.push_str(include_str!("assets/open_source_project_sources.tsv"));

    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".into());
    let mut catalog =
        format!("NOVA READER — OPEN-SOURCE LICENSES\nTarget: {target}\nVersion: {version}\n\n",);
    catalog.push_str(&render_cargo_about(
        &manifest_dir,
        "assets/open_source_licenses.hbs",
        &target,
    ));
    catalog.push_str("\n\n");
    catalog.push_str(include_str!("assets/open_source_vendors.txt"));
    catalog.push_str("\n\n===== Nova Reader third-party notices =====\n\n");
    catalog.push_str(include_str!("THIRD_PARTY_NOTICES.md"));
    catalog.push_str("\n\n===== AVIF decoder: BSD-2-Clause =====\n\n");
    catalog.push_str(include_str!(
        "assets/licenses/avif-decoder-BSD-2-Clause.txt"
    ));
    catalog.push_str("\n\n===== AVIF adapter: BSD-3-Clause =====\n\n");
    catalog.push_str(include_str!("crates/reader-engine/AVIF-NOTICE.txt"));
    catalog.push_str("\n\n===== Nova Reader project license: GPL-3.0-or-later =====\n\n");
    catalog.push_str(include_str!("LICENSE"));
    catalog.push_str("\n\n===== Roboto font license: Apache-2.0 =====\n\n");
    catalog.push_str(include_str!("assets/fonts/LICENSE.txt"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let catalog_path = out_dir.join("nova_license_catalog.txt");
    let compressed_path = out_dir.join("nova_license_catalog.zst");
    let recipe_path = out_dir.join("nova_license_catalog.recipe");
    const COMPRESSION_LEVEL: i32 = 22;
    let recipe = format!("zstd:{COMPRESSION_LEVEL};checksum:true;size:true;v1\n");
    let cached = compressed_path.is_file()
        && fs::read(&catalog_path).is_ok_and(|old| old.as_slice() == catalog.as_bytes())
        && fs::read(&recipe_path).is_ok_and(|old| old.as_slice() == recipe.as_bytes());
    // The catalog rarely changes: spend a fraction of a second at build time
    // to minimize the embedded payload. Pledging its size also records it in
    // the frame, and the runtime uses the same exact size for its decode buffer.
    // Reuse the frame on unrelated build-script reruns. Comparing the original
    // bytes avoids cache hashes, and the recipe invalidates intentional codec
    // setting changes even when the legal text itself stays identical.
    if !cached {
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), COMPRESSION_LEVEL)
            .expect("create license catalog compressor");
        encoder
            .include_checksum(true)
            .expect("checksum license catalog");
        encoder
            .set_pledged_src_size(Some(catalog.len() as u64))
            .expect("record license catalog size");
        encoder
            .write_all(catalog.as_bytes())
            .expect("compress license catalog");
        let compressed = encoder
            .finish()
            .expect("finish license catalog compression");
        write_if_changed(&compressed_path, &compressed).expect("write compressed license catalog");
        write_if_changed(&recipe_path, recipe.as_bytes())
            .expect("write license compression recipe");
    }
    write_if_changed(
        &out_dir.join("nova_license_catalog_size.rs"),
        format!(
            "pub(crate) const OPEN_SOURCE_LICENSE_CATALOG_SIZE: usize = {};\n",
            catalog.len()
        )
        .as_bytes(),
    )
    .expect("write license catalog size");
    // Retain the readable build artifact for inspection and exact round-trip
    // tests; the production binary embeds only the compressed file.
    write_if_changed(&catalog_path, catalog.as_bytes()).expect("write generated license catalog");
    write_if_changed(
        &out_dir.join("nova_license_sources.tsv"),
        sources.as_bytes(),
    )
    .expect("write generated license source links");
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if fs::read(path).is_ok_and(|old| old.as_slice() == bytes) {
        return Ok(());
    }
    fs::write(path, bytes)
}

fn render_cargo_about(manifest_dir: &PathBuf, template: &str, target: &str) -> String {
    let mut command = Command::new("cargo-about");
    command
        .current_dir(manifest_dir)
        .arg("generate")
        .arg("--locked")
        .arg("--fail")
        .arg("-c")
        .arg("about.toml")
        .arg("--target")
        .arg(target);

    let mut features = Vec::new();
    if env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        features.push("desktop");
    }
    if env::var_os("CARGO_FEATURE_ANDROID").is_some() {
        features.push("android");
    }
    if env::var_os("CARGO_FEATURE_UPDATE_CHECK").is_some() {
        features.push("update-check");
    }
    // Reader defaults include update-check; select only the actual build's
    // features so Android and distro builds do not inherit desktop/updater code.
    command.arg("--no-default-features");
    if !features.is_empty() {
        command.arg("--features").arg(features.join(","));
    }

    let output = command
        .arg(template)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "license catalog generation needs cargo-about; run nix develop (or nix develop .#android): {error}"
            )
        });
    if !output.status.success() {
        panic!(
            "cargo-about failed to generate {template}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).unwrap_or_else(|error| {
        panic!("cargo-about returned non-UTF-8 output for {template}: {error}")
    })
}
