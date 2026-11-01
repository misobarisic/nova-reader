"""Ensure the shipped addon fixtures produce deterministic, licensed ZIPs."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from zipfile import ZipFile

ROOT = Path(__file__).resolve().parents[2]


class AddonPackagingTests(unittest.TestCase):
    def test_reference_packages_are_reproducible(self):
        for addon in ("neutral-fixture", "manhwa18cc", "mangafire"):
            with self.subTest(addon=addon), tempfile.TemporaryDirectory() as temporary:
                outputs = [Path(temporary) / name for name in ("one.zip", "two.zip")]
                for output in outputs:
                    subprocess.run([sys.executable, str(ROOT / "tools/package-addon.py"), str(ROOT / "examples/addons" / addon), str(output)], check=True, capture_output=True)
                self.assertEqual(outputs[0].read_bytes(), outputs[1].read_bytes())
                with ZipFile(outputs[0]) as archive:
                    self.assertIn("manifest.json", archive.namelist())
                    self.assertIn("NOTICE.md", archive.namelist())
                    if addon != "neutral-fixture":
                        self.assertIn("LICENSE", archive.namelist())


if __name__ == "__main__":
    unittest.main()
