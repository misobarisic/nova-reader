"""The Android download service must retain Nova's pinned behavior and manifest."""
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]


class BackgroundServicePackagingTests(unittest.TestCase):
    def test_nova_copy_and_reader_service_manifest(self):
        path = ROOT / "tools/test-android-background.py"
        spec = importlib.util.spec_from_file_location("reader_background_contract", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        module.copied_source_and_manifest()


if __name__ == "__main__":
    unittest.main()
