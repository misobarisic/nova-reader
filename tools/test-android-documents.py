#!/usr/bin/env python3
"""Run the production SAF/JNI bridge in a host JVM; no Android device is emulated."""
import argparse
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def braced_item(source, marker):
    start = source.index(marker)
    opening = source.index('{', start)
    depth = 0
    for index in range(opening, len(source)):
        # These extracted data declarations/request_conflict contain no string
        # literals or comments with braces; reject source drift explicitly.
        if source[index] == '{':
            depth += 1
        elif source[index] == '}':
            depth -= 1
            if depth == 0:
                return source[start:index + 1]
    raise ValueError(f'unterminated Rust item: {marker}')


def android_export_adapter():
    """Type-check the actual cfg(Android) adapter against actual shared shapes.

    No desktop UI/build script/codecs are needed. Data declarations and the
    conflict dispatcher come from reader_export.rs; policy variants come from
    capture_settings.rs. The production Android platform body is unchanged.
    """
    source = (ROOT / 'src/reader_export.rs').read_text()
    policy_source = (ROOT / 'src/capture_settings.rs').read_text()
    marker = '#[cfg(target_os = "android")]\nmod platform {'
    start = source.index(marker)
    end = source.index('\n#[cfg(test)]', start)
    platform = source[start:end].replace('#[cfg(target_os = "android")]\n', '', 1)
    symbols = sorted(set(re.findall(r'crate::android_documents::([A-Za-z_][A-Za-z0-9_]*)', platform)))
    aliases = '\n'.join(re.search(rf'(?:pub )?type {name}\b.*?;', source, re.S).group(0)
                        for name in ['Callback', 'ConflictReply', 'ConflictResolver', 'ExportCurrent'])
    declarations = '\n'.join(braced_item(source, marker) for marker in [
        'pub struct ExportImage', 'pub enum ConflictChoice', 'pub struct SaveContext',
        'pub fn request_conflict(',
    ])
    policy = braced_item(policy_source, 'pub enum ConflictPolicy')
    return ('mod android_documents { pub use crate::{' + ','.join(symbols) + '}; }\n'
            '#[allow(dead_code)] mod android_export_adapter { use std::sync::Arc;\n'
            '#[derive(Clone, Copy, Default)]\n' + policy + '\n' + aliases + '\n' + declarations + '\n' + platform + '\n}\n')


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
        production_classes = work / 'production-classes'
        production_classes.mkdir()
        production_sources = sorted((ROOT / 'android/java').rglob('*.java'))
        # Compile against real API classes as well as the runtime fixtures, so
        # fixture signatures cannot accidentally conceal an Android API mismatch.
        subprocess.run([*compiler, '-source', '8', '-target', '8', '-Xlint:-options',
                        '-cp', str(android_jar), '-d', str(production_classes),
                        *map(str, production_sources)], check=True)
        sources = sorted((ROOT / 'tests/android-documents/java').rglob('*.java'))
        sources.extend(production_sources)
        subprocess.run([*compiler, '-source', '8', '-target', '8', '-Xlint:-options',
                        '-cp', str(android_jar), '-d', str(classes), *map(str, sources)], check=True)
        (work / 'src').mkdir()
        (work / 'Cargo.toml').write_text(
            '[package]\nname="reader-document-jni-regression"\nversion="0.0.0"\nedition="2024"\n'
            '[workspace]\n[dependencies]\njni={version="=0.21.1",features=["invocation"]}\nndk-context="=0.1.1"\n'
        )
        production = (ROOT / 'src/android_documents.rs').read_text().replace('#![cfg(target_os = "android")]\n', '')
        regression = json.dumps(str(ROOT / 'tests/android-documents/jni_regression.rs'))
        (work / 'src/lib.rs').write_text(production + '\n' + android_export_adapter()
                                       + '\n#[cfg(test)] mod host_regression { include!(' + regression + '); }\n')
        env = dict(os.environ, READER_JNI_CLASSPATH=os.pathsep.join([str(classes), str(android_jar.resolve())]),
                   READER_JNI_CACHE=str(cache), CARGO_TARGET_DIR=str(ROOT / 'target/android-document-jni-tests'))
        subprocess.run(['cargo', 'test', '--manifest-path', str(work / 'Cargo.toml'), '--lib', '--', '--test-threads=1'],
                       env=env, check=True)
        if args.clippy:
            subprocess.run(['cargo', 'clippy', '--manifest-path', str(work / 'Cargo.toml'), '--lib', '--tests', '--', '-D', 'warnings'],
                           env=env, check=True)


if __name__ == '__main__':
    main()
