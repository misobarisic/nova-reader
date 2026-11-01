"""Keep release packaging aligned with Nova b69a1e5, beyond the YAML graph."""
import ast
import hashlib
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]


def canonical_android(source):
    source = source.replace("nova_reader.apk", "nova.apk").replace("nova-reader-apk-", "nova-apk-").replace("Nova-Reader-Android", "Nova-Android")
    tree = ast.parse(source)
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(isinstance(target, ast.Name) and target.id == "NATIVE_LIBRARIES" for target in node.targets):
            node.value = ast.Tuple(elts=[], ctx=ast.Load())
        if isinstance(node, ast.FunctionDef) and node.name == "verify_version":
            node.body = [ast.Pass()]
    def stable_node(value):
        if isinstance(value, ast.AST):
            return [type(value).__name__, {
                field: stable_node(item) for field, item in ast.iter_fields(value)
                if item is not None and item != []
            }]
        if isinstance(value, list):
            return [stable_node(item) for item in value]
        if isinstance(value, bytes):
            return {"bytes": value.hex()}
        return value
    return json.dumps(stable_node(tree), sort_keys=True, separators=(",", ":"))


def canonical_shell(source, platform):
    source = source.replace("nova-reader", "nova").replace("Nova-Reader", "Nova").replace("Nova Reader", "Nova")
    source = source.replace("Section: graphics", "Section: video").replace("Cross-platform comic reader", "Cross-platform media catalog and player").replace("Nova is a comic reader", "Nova is a media catalog and player")
    source = "\n".join(line for line in source.splitlines() if "DEPENDENCY_LICENSES.txt" not in line)
    if platform == "linux":
        # Only the reviewed resource pins differ from Nova's mutable continuous
        # downloads. test_package_linux.py asserts these exact new values.
        for reader, nova in {
            "# Dated releases isolate packaging from continuous updates; verified SHA-256\n# pins still fail closed if upstream replaces a dated asset. See docs/packaging.md.": "# AppImage's continuous release assets are mutable, so pin their published\n# SHA-256 digests and fail closed if the upstream binaries change.",
            "https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage": "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage",
            "c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d": "8aea8da0f7f7039d2a2cecb14657d752a222a5e1d3825caeef186c82f751cdd1",
            "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/1-alpha-20250213-1/linuxdeploy-plugin-appimage-x86_64.AppImage": "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/continuous/linuxdeploy-plugin-appimage-x86_64.AppImage",
            "992d502a248e14ab185448ddf6f6e7d25558cb84d4623c354c3af350c25fccb3": "49d6a17160675a6bd1781699aae6bdf7692d98552e02a3671d2183d10547842e",
        }.items():
            source = source.replace(reader, nova)
    if platform == "windows":
        source = source.replace('cp LICENSE THIRD_PARTY_NOTICES.md "$bundle/"', 'cp LICENSE "$bundle/LICENSE"')
        start = source.find('cp "$MPV_SOURCE/SOURCES"')
        if start >= 0:
            end = source.index('# Include only DLL', start)
            source = source[:start] + source[end:]
        source = source.replace(': "${MPV_SOURCE:?run through nix develop .#windows}"', '')
        source = source.replace('if not any(path.name.lower() == "libmpv-2.dll" for path in files):\n    raise SystemExit("Windows bundle is missing libmpv-2.dll")\n', '')
    return "\n".join(line for line in source.splitlines() if line.strip())


class NovaPackagingParityTests(unittest.TestCase):
    # Digests of the canonical upstream scripts at the immutable reference
    # b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda, not Reader-generated snapshots.
    def test_android_packaging_matches_nova(self):
        canonical = canonical_android((ROOT / '.github/scripts/package-android.py').read_text())
        self.assertEqual(hashlib.sha256(canonical.encode()).hexdigest(), 'afc49e4e4a1d9c13ab900bfbcd5e846917ce9680a3675d1a2f87d292d86a3e05')

    def test_linux_packaging_matches_nova(self):
        canonical = canonical_shell((ROOT / '.github/scripts/package-linux.sh').read_text(), 'linux')
        self.assertEqual(hashlib.sha256(canonical.encode()).hexdigest(), '6d305b787eb154f0d0f0e78c81f15d0ac3884e2503a731ff438bdb7416194c67')

    def test_windows_packaging_matches_nova(self):
        canonical = canonical_shell((ROOT / '.github/scripts/package-windows.sh').read_text(), 'windows')
        self.assertEqual(hashlib.sha256(canonical.encode()).hexdigest(), 'a53945878b894b92622b6acd0b82d7b7c65729d7260c113806b326bd44ad3763')
