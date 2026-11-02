"""Ensure the shipped addon fixtures produce deterministic, licensed ZIPs."""
from pathlib import Path
import json
import stat
import subprocess
import sys
import tempfile
import unittest
from zipfile import ZIP_DEFLATED, ZipFile

ROOT = Path(__file__).resolve().parents[2]


class AddonPackagingTests(unittest.TestCase):
    def test_reference_packages_are_reproducible(self):
        for addon in ("neutral-fixture", "manhwa18cc", "mangafire", "kagane", "toonily", "mitaku", "cosplaytele"):
            with self.subTest(addon=addon), tempfile.TemporaryDirectory() as temporary:
                outputs = [Path(temporary) / name for name in ("one.zip", "two.zip")]
                for output in outputs:
                    subprocess.run([sys.executable, str(ROOT / "tools/package-addon.py"), str(ROOT / "examples/addons" / addon), str(output)], check=True, capture_output=True)
                self.assertEqual(outputs[0].read_bytes(), outputs[1].read_bytes())
                with ZipFile(outputs[0]) as archive:
                    source = ROOT / "examples/addons" / addon
                    manifest = json.loads((source / "manifest.json").read_text())
                    expected = sorted(["manifest.json", manifest["entry"], *manifest.get("scripts", []), *manifest.get("assets", [])])
                    self.assertEqual(archive.namelist(), expected)
                    for entry in archive.infolist():
                        self.assertEqual(entry.date_time, (1980, 1, 1, 0, 0, 0))
                        self.assertEqual(entry.create_system, 3)
                        self.assertEqual(entry.external_attr >> 16, stat.S_IFREG | 0o644)
                        self.assertEqual(entry.compress_type, ZIP_DEFLATED)
                        self.assertEqual(archive.read(entry.filename), (source / entry.filename).read_bytes())
                        self.assertFalse(set(Path(entry.filename).parts) & {"tests", "fixtures", "node_modules"})
                    self.assertIn("NOTICE.md", archive.namelist())
                    if addon != "neutral-fixture":
                        self.assertIn("LICENSE", archive.namelist())

    def test_existing_released_packages_retain_identical_bytes(self):
        for addon in ("manhwa18cc", "mangafire", "kagane"):
            source = ROOT / "examples/addons" / addon
            manifest = json.loads((source / "manifest.json").read_text())
            published = ROOT / "addons" / f"{manifest['id']}-{manifest['version']}.zip"
            with self.subTest(addon=addon), tempfile.TemporaryDirectory() as temporary:
                output = Path(temporary) / "rebuilt.zip"
                subprocess.run([sys.executable, str(ROOT / "tools/package-addon.py"), str(source), str(output)], check=True, capture_output=True)
                self.assertEqual(output.read_bytes(), published.read_bytes(), "an already released addon version must remain byte-identical")


if __name__ == "__main__":
    unittest.main()
