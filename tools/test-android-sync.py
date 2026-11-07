#!/usr/bin/env python3
"""Compile sync Java and exercise the production Rust JNI frame/cancel adapter."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
ROOT = Path(__file__).resolve().parents[1]


def kit_crate(name):
    """Directory of a nova-kit crate in the dependency graph of this checkout."""
    metadata = subprocess.run(["cargo", "metadata", "--format-version", "1", "--locked"],
                              cwd=ROOT, check=True, capture_output=True, text=True).stdout
    for package in json.loads(metadata)["packages"]:
        if package["name"] == name:
            return Path(package["manifest_path"]).parent
    raise SystemExit(f"{name} is not in the dependency graph")

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--android-jar', type=Path, required=True)
    args = parser.parse_args()
    java = shutil.which('java')
    if not java: parser.error('a JDK is required')
    compiler = [shutil.which('javac')] if shutil.which('javac') else [java, '-m', 'jdk.compiler/com.sun.tools.javac.Main']
    with tempfile.TemporaryDirectory(prefix='reader-sync-jni-') as directory:
        work = Path(directory)
        (work / 'classes').mkdir()
        subprocess.run([*compiler, '-source', '8', '-target', '8', '-Xlint:-options', '-cp', str(args.android_jar), '-d', str(work / 'classes'), *map(str, (ROOT / 'android/java').rglob('*.java'))], check=True)
        (work / 'src').mkdir()
        (work / 'Cargo.toml').write_text('[package]\nname="reader-sync-jni-regression"\nversion="0.0.0"\nedition="2024"\n[workspace]\n[dependencies]\njni={version="=0.21.1",features=["invocation"]}\nrqrr={version="0.11",default-features=false}\n'
                                        f'nova-android={{path="{kit_crate("nova-android")}",features=["jni"]}}\n')
        production = (ROOT / 'src/android_sync.rs').read_text().replace('#![cfg(target_os = "android")]\n', '')
        fixture = (ROOT / 'tests/android-sync/jni_regression.rs').read_text()
        (work / 'src/lib.rs').write_text('#![allow(dead_code)]\n' + production + '\n' + fixture)
        env = dict(os.environ, CARGO_TARGET_DIR=str(ROOT / 'target/android-document-jni-tests'))
        subprocess.run(['cargo', 'test', '--manifest-path', str(work / 'Cargo.toml'), '--lib'], env=env, check=True)

if __name__ == '__main__': main()
