#!/usr/bin/env bash
set -euo pipefail

: "${MINGW_CC:?run through nix develop .#windows}"

tag="${GITHUB_REF_NAME#v}"
tag="${tag//\//-}"
bundle="target/windows-package"
dist="dist"
# Match Nova: old runs may have inherited read-only Nix directory modes.
if [[ -d "$bundle" ]]; then
  find "$bundle" -type d -exec chmod u+w {} +
fi
rm -rf "$bundle"
mkdir -p "$bundle" "$dist"

profile="${NOVA_RELEASE_PROFILE:-release}"
cp "target/x86_64-pc-windows-gnu/$profile/nova-reader.exe" "$bundle/nova-reader.exe"
cp LICENSE THIRD_PARTY_NOTICES.md "$bundle/"
cp target/licenses/x86_64-pc-windows-gnu.txt "$bundle/DEPENDENCY_LICENSES.txt"

# Nova's compiler-closure staging supplies the MinGW runtime DLLs without
# trying to bundle DLLs supplied by Windows itself (for example UIAutomationCore).
while IFS= read -r store_path; do
  while IFS= read -r -d '' dll; do
    name="${dll##*/}"
    if [[ ! -e "$bundle/$name" ]]; then
      cp "$dll" "$bundle/$name"
    fi
  done < <(find -L "$store_path" -type f -iname "*.dll" -print0 2>/dev/null || true)
done < <(nix-store -qR "$MINGW_CC")

python3 - "$tag" "$bundle" "$dist" <<'PY'
from pathlib import Path
from sys import argv
from zipfile import ZIP_DEFLATED, ZipFile

tag, bundle_name, dist_name = argv[1:]
bundle = Path(bundle_name)
dist = Path(dist_name)
output = dist / f"Nova-Reader-Windows-x86_64-{tag}.zip"
files = sorted(path for path in bundle.rglob("*") if path.is_file())
if not any(path.name == "nova-reader.exe" for path in files):
    raise SystemExit("Windows bundle is missing nova-reader.exe")
with ZipFile(output, "w", compression=ZIP_DEFLATED, compresslevel=9) as archive:
    for path in files:
        archive.write(path, arcname=path.relative_to(bundle))
print(f"Created {output} with {len(files)} files")
PY
