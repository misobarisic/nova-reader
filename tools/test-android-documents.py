#!/usr/bin/env python3
"""Run the production SAF/JNI bridge in a host JVM; no Android device is emulated."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--android-jar', type=Path)
    parser.add_argument('--clippy', action='store_true', help='also lint the production JNI module in the host harness')
    args = parser.parse_args()
    sdk = Path(os.environ.get('ANDROID_HOME', os.environ.get('ANDROID_SDK_ROOT', '.')))
    android_jar = args.android_jar or sdk / 'platforms/android-35/android.jar'
    if not android_jar.is_file():
        parser.error('provide --android-jar or an Android API 35 ANDROID_HOME')
    java = shutil.which('java')
    if not java:
        parser.error('a JDK is required')
    compiler = [shutil.which('javac')] if shutil.which('javac') else [java, '-m', 'jdk.compiler/com.sun.tools.javac.Main']
    with tempfile.TemporaryDirectory(prefix='reader-jni-regression-') as directory:
        work = Path(directory)
        classes, cache = work / 'classes', work / 'cache'
        classes.mkdir()
        cache.mkdir()
        sources = sorted((ROOT / 'tests/android-documents/java').rglob('*.java'))
        sources.append(ROOT / 'android/java/dev/misob/nova/reader/ReaderActivity.java')
        subprocess.run([*compiler, '-source', '8', '-target', '8', '-Xlint:-options',
                        '-cp', str(android_jar), '-d', str(classes), *map(str, sources)], check=True)
        (work / 'src').mkdir()
        (work / 'Cargo.toml').write_text(
            '[package]\nname="reader-document-jni-regression"\nversion="0.0.0"\nedition="2024"\n'
            '[workspace]\n[dependencies]\njni={version="=0.21.1",features=["invocation"]}\nndk-context="=0.1.1"\n'
        )
        production = (ROOT / 'src/android_documents.rs').read_text().replace('#![cfg(target_os = "android")]\n', '')
        regression = json.dumps(str(ROOT / 'tests/android-documents/jni_regression.rs'))
        (work / 'src/lib.rs').write_text(production + '\n#[cfg(test)] mod host_regression { include!(' + regression + '); }\n')
        env = dict(os.environ, READER_JNI_CLASSPATH=os.pathsep.join([str(classes), str(android_jar.resolve())]),
                   READER_JNI_CACHE=str(cache), CARGO_TARGET_DIR=str(ROOT / 'target/android-document-jni-tests'))
        subprocess.run(['cargo', 'test', '--manifest-path', str(work / 'Cargo.toml'), '--lib', '--', '--test-threads=1'],
                       env=env, check=True)
        if args.clippy:
            subprocess.run(['cargo', 'clippy', '--manifest-path', str(work / 'Cargo.toml'), '--lib', '--tests', '--', '-D', 'warnings'],
                           env=env, check=True)


if __name__ == '__main__':
    main()
