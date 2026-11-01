"""Literal Nova license-builder parity; behavior lives in license_catalog_build.rs."""
import hashlib
from pathlib import Path
import unittest
import re

def rust_function(source, name):
    start = source.index(f'fn {name}(')
    opening = source.index('{', start)
    depth, quoted, escaped, comment = 0, False, False, False
    for index in range(opening, len(source)):
        ch = source[index]
        if comment:
            if ch == '\n': comment = False
            continue
        if quoted:
            if escaped: escaped = False
            elif ch == '\\': escaped = True
            elif ch == '"': quoted = False
            continue
        if ch == '/' and source[index:index+2] == '//':
            comment = True
            continue
        if ch == '"': quoted = True
        elif ch == '{': depth += 1
        elif ch == '}':
            depth -= 1
            if depth == 0: return source[start:index+1]
    raise ValueError(f'unclosed Rust function {name}')

def canonical_function(source, name):
    source = rust_function(source, name)
    if name == 'generate_license_catalog':
        start = source.index('    for path in [')
        end = source.index('    let target = env::var("TARGET")', start)
        source = source[:start] + 'READER_INPUT_WATCHES\n' + source[end:]
        source = source.replace('NOVA READER — OPEN-SOURCE LICENSES', 'NOVA — OPEN-SOURCE LICENSES')
        start = source.index('    catalog.push_str(include_str!("assets/open_source_vendors.txt"));') + len('    catalog.push_str(include_str!("assets/open_source_vendors.txt"));')
        end = source.index('    let out_dir =', start)
        source = source[:start] + '\nREADER_LOCAL_NOTICES\n' + source[end:]
    elif name == 'render_cargo_about':
        start = source.index('    let mut features = Vec::new();')
        end = source.index('    let output = command', start)
        source = source[:start] + 'READER_FEATURE_SELECTION\n' + source[end:]
    return re.sub(r'\s+', ' ', source).strip()

ROOT = Path(__file__).resolve().parents[2]
# Derived from clean Nova b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda, not Reader.
NOVA_FUNCTION_DIGESTS = {
    "generate_license_catalog": "b5bc0badea3beaa0fb8c44b31c01de30560079ac28b9c96e4b42b0d6ddad563f",
    "render_cargo_about": "c24d6a2a675f4bd9ebea30809739454f21b09ae51616df4745dd20cf39691de6",
    "write_if_changed": "61c2b3d89c483f12a9052017495cfadad35764e6abd48093bd9be4fa6aa26ffd",
}
NOVA_RESOURCE_DIGESTS = {
    "about.toml": "e358c1157135eee5ca14f69d68938514af4f813a78a13af05650b7713b2d53fd",
    "assets/open_source_licenses.hbs": "6f8896fff10a749e193f22b908aad201a89ed30ebbe06683eac7cba20dbd834f",
    "assets/open_source_crate_sources.hbs": "b317eb5666e24eee6c40fe46fb898855509988fc60fc7d5cffbe53f94d8be23b",
}


class LicenseBuildParityTests(unittest.TestCase):
    def test_actual_nova_functions_are_copied_with_only_reader_adaptations(self):
        source = (ROOT / "build.rs").read_text()
        for name, expected in NOVA_FUNCTION_DIGESTS.items():
            with self.subTest(function=name):
                canonical = canonical_function(source, name)
                self.assertEqual(hashlib.sha256(canonical.encode()).hexdigest(), expected)

    def test_actual_nova_policy_and_templates_are_retained(self):
        for name, expected in NOVA_RESOURCE_DIGESTS.items():
            with self.subTest(resource=name):
                raw = (ROOT / name).read_bytes()
                if name.endswith("open_source_licenses.hbs"):
                    raw = raw.replace("NOVA READER —".encode(), "NOVA —".encode(), 1)
                self.assertEqual(hashlib.sha256(raw).hexdigest(), expected)

    def test_duplicate_inventory_jobs_and_scripts_are_removed(self):
        workflow = (ROOT / ".github/workflows/build-release.yml").read_text()
        self.assertNotIn("Generate Android dependency license inventory", workflow)
        self.assertNotIn("Generate Linux dependency license inventory", workflow)
        self.assertNotIn("Generate Windows dependency license inventory", workflow)
        self.assertIn("cargo install cargo-about --locked --version 0.9.1", workflow)
        for script in ("package-linux.sh", "package-windows.sh"):
            self.assertNotIn("target/licenses/", (ROOT / ".github/scripts" / script).read_text())
        for obsolete in (".github/scripts/dependency-licenses.sh", ".github/scripts/about.toml", "assets/dependency-licenses.hbs"):
            self.assertFalse((ROOT / obsolete).exists(), obsolete)


if __name__ == "__main__":
    unittest.main()
