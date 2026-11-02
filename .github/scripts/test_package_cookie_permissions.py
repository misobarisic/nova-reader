"""Cookie permission validation must agree with the native package boundary."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("package_addon", ROOT / "tools/package-addon.py")
PACKAGER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGER)


class CookiePermissionPackagingTests(unittest.TestCase):
    def files(self, cookies):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = {
                "formatVersion": 2, "contract": "nova-reader1", "id": "cookie-fixture",
                "version": "1.0.0", "entry": "main.js",
                "permissions": {"domains": ["example.com"], "cookies": cookies},
            }
            (root / "manifest.json").write_text(json.dumps(manifest))
            (root / "main.js").write_text("void 0;")
            return PACKAGER.files_for(root)

    def test_exact_scoped_permission_and_legacy_empty(self):
        for cookies in ([], [{"origin": "https://example.com", "names": ["mature"]}],
                        [{"origin": "https://cdn.example.com", "names": ["mature", "other"]}]):
            with self.subTest(cookies=cookies):
                self.assertEqual(self.files(cookies), ["main.js", "manifest.json"])

    def test_rejects_ambiguous_origins_names_and_excessive_grants(self):
        invalid = [None, {}, [{"origin": "https://example.com", "names": []}],
                   [{"origin": "https://example.com", "names": ["mature"], "path": "/"}]]
        for origin in ("http://example.com", "https://example.com/", "https://EXAMPLE.com",
                       "https://example.com:443", "https://example.com:444", "https://other.com",
                       "https://example.com/path", "https://example.com?x", "https://example.com#x",
                       "https://user@example.com", "https://example.com.evil.test"):
            invalid.append([{"origin": origin, "names": ["mature"]}])
        for names in ([""], ["x" * 129], ["a;b"], ["a=b"], ["a\nb"], ["é"], ["x", "x"],
                      [str(i) for i in range(33)]):
            invalid.append([{"origin": "https://example.com", "names": names}])
        invalid.append([{"origin": "https://example.com", "names": ["x"]}] * 2)
        invalid.append([{"origin": f"https://s{i}.example.com", "names": ["x"]} for i in range(17)])
        for cookies in invalid:
            with self.subTest(cookies=cookies), self.assertRaises(ValueError):
                self.files(cookies)


if __name__ == "__main__":
    unittest.main()
