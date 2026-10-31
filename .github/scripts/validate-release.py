"""Release gates run on the checked-out tagged commit, never a prior main build."""
import os
import subprocess
import tomllib
from pathlib import Path


def validate(tag, version):
    if tag != f'v{version}':
        raise RuntimeError(f'Tag {tag!r} must equal v{version}')
    if not all(p.isdecimal() and int(p) <= 255 for p in version.split('.')) or len(version.split('.')) != 3:
        raise RuntimeError('Android releases require major.minor.patch components <=255')


def main():
    if os.environ.get('GITHUB_REF_TYPE') != 'tag':
        return
    version = tomllib.loads(Path('Cargo.toml').read_text())['package']['version']
    tag = os.environ['GITHUB_REF_NAME']
    validate(tag, version)
    head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    tagged = subprocess.check_output(['git', 'rev-parse', f'refs/tags/{tag}^{{commit}}'], text=True).strip()
    if head != tagged or head != os.environ['GITHUB_SHA']:
        raise RuntimeError('Checkout must be the exact tagged commit')


if __name__ == '__main__':
    main()
