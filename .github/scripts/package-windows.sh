#!/usr/bin/env bash
set -euo pipefail
: "${MINGW_CC:?run through nix develop .#windows}"
profile="${NOVA_READER_RELEASE_PROFILE:-release}"
tag="${GITHUB_REF_NAME:-local}"
tag="${tag#v}"
tag="${tag//\//-}"
bundle="target/windows-package"
rm -rf "$bundle"
mkdir -p "$bundle" dist
cp "target/x86_64-pc-windows-gnu/$profile/nova-reader.exe" "$bundle/"
cp LICENSE THIRD_PARTY_NOTICES.md "$bundle/"
cp target/licenses/x86_64-pc-windows-gnu.txt "$bundle/DEPENDENCY_LICENSES.txt"
# Resolve every non-system PE import recursively; copying only used runtime DLLs
# prevents accidentally shipping unrelated toolchain or media libraries.
python3 .github/scripts/windows-runtime.py "$bundle" "$MINGW_CC"
python3 - "$tag" "$bundle" <<'PY'
from pathlib import Path
from sys import argv
from zipfile import ZipFile, ZIP_DEFLATED
bundle = Path(argv[2])
with ZipFile(f"dist/Nova-Reader-Windows-x86_64-{argv[1]}.zip", "w", compression=ZIP_DEFLATED) as archive:
    for path in sorted(bundle.rglob("*")):
        if path.is_file():
            archive.write(path, path.relative_to(bundle))
PY
