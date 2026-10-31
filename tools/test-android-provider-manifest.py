#!/usr/bin/env python3
"""Check the patched cargo-apk2 manifest serializer without an Android SDK.

Supply a checkout of the cargo-apk2 revision pinned by flake.nix. The source is
copied to temporary storage; the checkout and workspace lockfile are untouched.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
ANDROID = '{http://schemas.android.com/apk/res/android}'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apk2-source', type=Path, required=True)
    args = parser.parse_args()
    source = args.apk2_source / 'ndk-build2/src/manifest.rs'
    if not source.is_file():
        parser.error('--apk2-source must contain ndk-build2/src/manifest.rs')
    with tempfile.TemporaryDirectory(prefix='reader-provider-manifest-') as directory:
        work = Path(directory)
        manifest = work / 'ndk-build2/src/manifest.rs'
        manifest.parent.mkdir(parents=True)
        shutil.copyfile(source, manifest)
        subprocess.run(['patch', '--batch', '--fuzz=0', '-p1', '-i',
                        str(ROOT / 'android/cargo-apk2-image-provider.patch')], cwd=work, check=True)
        (work / 'src').mkdir()
        (work / 'Cargo.toml').write_text(
            '[package]\nname="reader-provider-manifest-regression"\nversion="0.0.0"\nedition="2024"\n'
            '[workspace]\n[dependencies]\nserde={version="1",features=["derive"]}\n'
            'quick-xml={version="=0.41.0",features=["serialize"]}\ntoml="1"\n'
        )
        manifest_path = json.dumps(str(manifest))
        (work / 'src/main.rs').write_text('''
#[allow(dead_code)]
mod error {
    #[derive(Debug)]
    pub enum NdkError { Io(std::io::Error), Serialize(quick_xml::SeError) }
    impl From<std::io::Error> for NdkError {
        fn from(error: std::io::Error) -> Self { Self::Io(error) }
    }
}
#[allow(dead_code)]
#[path = ''' + manifest_path + '''] mod manifest;
fn main() {
    let metadata: toml::Value = toml::from_str(&std::fs::read_to_string(std::env::var("READER_CARGO_MANIFEST").unwrap()).unwrap()).unwrap();
    let mut manifest = manifest::AndroidManifest::default();
    manifest.application = metadata["package"]["metadata"]["android"]["application"].clone().try_into().unwrap();
    manifest.write_to(std::path::Path::new(&std::env::var("READER_MANIFEST_OUTPUT").unwrap())).unwrap();
    // The additional provider field must remain optional for older consumers.
    let defaults: manifest::Application = toml::from_str("label='reader'").unwrap();
    assert!(defaults.providers.is_empty());
}
''')
        env = dict(os.environ, READER_CARGO_MANIFEST=str(ROOT / 'Cargo.toml'),
                   READER_MANIFEST_OUTPUT=str(work),
                   CARGO_TARGET_DIR=str(ROOT / 'target/android-provider-manifest-tests'))
        subprocess.run(['cargo', 'run', '--manifest-path', str(work / 'Cargo.toml'), '--quiet'],
                       env=env, check=True)
        application = ET.parse(work / 'AndroidManifest.xml').getroot().find('application')
        providers = application.findall('provider')
        assert len(providers) == 1, 'Reader must package exactly one image provider'
        assert providers[0].attrib == {
            ANDROID + 'name': 'dev.misob.nova.reader.ReaderImageProvider',
            ANDROID + 'authorities': 'dev.misob.nova.reader.images',
            ANDROID + 'exported': 'false',
            ANDROID + 'grantUriPermissions': 'true',
        }, providers[0].attrib
        activity = application.find('activity')
        assert activity.attrib[ANDROID + 'name'] == 'dev.misob.nova.reader.ReaderActivity'
        assert application.attrib[ANDROID + 'hasCode'] == 'true'
        assert application.attrib[ANDROID + 'extractNativeLibs'] == 'false'
        assert './android/cargo-apk2-image-provider.patch' in (ROOT / 'flake.nix').read_text()
        print('Provider metadata serializes with private per-URI grants; native activity flags are preserved.')


if __name__ == '__main__':
    main()
