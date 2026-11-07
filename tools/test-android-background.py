#!/usr/bin/env python3
"""Verify Nova's exact copied service and run its Android lifecycle in a host JVM."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def kit_crate(name):
    """Directory of a nova-kit crate in the dependency graph of this checkout."""
    metadata = subprocess.run(["cargo", "metadata", "--format-version", "1", "--locked"],
                              cwd=ROOT, check=True, capture_output=True, text=True).stdout
    for package in json.loads(metadata)["packages"]:
        if package["name"] == name:
            return Path(package["manifest_path"]).parent
    raise SystemExit(f"{name} is not in the dependency graph")
# Source: https://github.com/misobarisic/nova/tree/ee3877808764d70362a6e0b442e79e5128317abf
# android/java/dev/misob/nova/{NovaBackgroundService,NovaActivity}.java
NOVA_SERVICE_SHA256 = "bd4fdd9fde4fc45fed915ffd8b7b3b8bf89c7c8d4c931e9f1cdd7f66d7efe29c"
NOVA_PERMISSION_SHA256 = "a4c737c1af047198e29aeded61ae12d84d5e7f573b608898546af398d3d98405"


def method(source, marker):
    start = source.index(marker)
    opening = source.index("{", start)
    depth = 0
    for index in range(opening, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[start:index + 1]
    raise AssertionError(f"unterminated method: {marker}")


def copied_source_and_manifest():
    service = (ROOT / "android/java/dev/misob/nova/reader/ReaderBackgroundService.java").read_text()
    original = (service.replace("package dev.misob.nova.reader;", "package dev.misob.nova;")
                .replace("ReaderBackgroundService", "NovaBackgroundService")
                .replace("ReaderActivity", "NovaActivity")
                .replace('"reader.background"', '"nova.background"')
                .replace('"reader:downloads"', '"nova:downloads"'))
    assert hashlib.sha256(original.encode()).hexdigest() == NOVA_SERVICE_SHA256, "service must remain Nova byte-equivalent after mechanical Reader identity changes"
    activity = (ROOT / "android/java/dev/misob/nova/reader/ReaderActivity.java").read_text()
    permission = method(activity, "    public static void requestNotificationPermission(int requestCode)")
    permission = permission.replace("ReaderActivity", "NovaActivity").replace("notificationForeground", "foreground")
    assert hashlib.sha256(permission.encode()).hexdigest() == NOVA_PERMISSION_SHA256, "permission helper must preserve Nova's main-thread resumed-Activity logic"
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["metadata"]["android"]
    permissions = {entry["name"] for entry in manifest["uses_permission"]}
    assert {"android.permission.FOREGROUND_SERVICE", "android.permission.FOREGROUND_SERVICE_DATA_SYNC", "android.permission.WAKE_LOCK", "android.permission.POST_NOTIFICATIONS"} <= permissions
    services = manifest["application"]["service"]
    service = next(item for item in services if item["name"] == "dev.misob.nova.reader.ReaderBackgroundService")
    assert service["foreground_service_type"] == "dataSync"
    assert service.get("exported", False) is False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--android-jar", type=Path, required=True)
    parser.add_argument("--jni", action="store_true", help="also execute the production Rust JNI download adapter")
    parser.add_argument("--clippy", action="store_true", help="lint the production adapter inside the JNI harness")
    args = parser.parse_args()
    if not args.android_jar.is_file():
        parser.error("provide an Android API35 android.jar")
    java = shutil.which("java")
    if not java:
        parser.error("a JDK is required")
    compiler = [shutil.which("javac")] if shutil.which("javac") else [java, "-m", "jdk.compiler/com.sun.tools.javac.Main"]
    copied_source_and_manifest()
    with tempfile.TemporaryDirectory(prefix="reader-background-java-") as directory:
        work = Path(directory)
        actual = work / "production"
        actual.mkdir()
        production = sorted((ROOT / "android/java").rglob("*.java"))
        subprocess.run([*compiler, "-source", "8", "-target", "8", "-Xlint:-options", "-cp", str(args.android_jar), "-d", str(actual), *map(str, production)], check=True)
        classes = work / "classes"
        classes.mkdir()
        stubs = sorted((ROOT / "tests/android-documents/java").rglob("*.java"))
        fixture = sorted((ROOT / "tests/android-background/java").rglob("*.java"))
        runtime = [source for source in production if source.name in {"ReaderActivity.java", "ReaderImageProvider.java", "ReaderBackgroundService.java"}]
        subprocess.run([*compiler, "-source", "8", "-target", "8", "-Xlint:-options", "-cp", str(args.android_jar), "-d", str(classes), *map(str, stubs + fixture + runtime)], check=True)
        subprocess.run([java, "-cp", str(classes), "dev.misob.nova.reader.BackgroundServiceFixture"], check=True)
        if args.jni or args.clippy:
            (work / "src").mkdir()
            (work / "Cargo.toml").write_text(
                '[package]\nname="reader-background-jni-regression"\nversion="0.0.0"\nedition="2024"\n'
                '[workspace]\n[dependencies]\njni={version="=0.21.1",features=["invocation"]}\n'
                f'nova-android={{path="{kit_crate("nova-android")}",features=["jni"]}}\n')
            production = (ROOT / "src/android_downloads.rs").read_text().replace('#![cfg(target_os = "android")]\n', '')
            fixture = (ROOT / "tests/android-background/jni_regression.rs").read_text()
            (work / "src/lib.rs").write_text('#![allow(dead_code)]\n' + production + '\n#[cfg(test)] mod regression {\n' + fixture + '\n}\n')
            env = dict(os.environ,
                       READER_BACKGROUND_JNI_CLASSPATH=os.pathsep.join([str(classes), str(args.android_jar.resolve())]),
                       CARGO_TARGET_DIR=str(ROOT / "target/android-document-jni-tests"))
            subprocess.run(['cargo', 'test', '--manifest-path', str(work / 'Cargo.toml'), '--lib', '--', '--test-threads=1', '--nocapture'], env=env, check=True)
            if args.clippy:
                subprocess.run(['cargo', 'clippy', '--manifest-path', str(work / 'Cargo.toml'), '--lib', '--tests', '--', '-D', 'warnings'], env=env, check=True)
    print("Nova service/permission byte-equivalence and Reader manifest contract passed")


if __name__ == "__main__":
    main()
