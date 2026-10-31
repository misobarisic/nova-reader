"""Regressions for independent identity, release gates, and shipped PE imports."""
import importlib.util
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / '.github/scripts' / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


release = module('release', 'validate-release.py')
class PackagingContractTests(unittest.TestCase):
    def test_tag_and_android_version_policy(self):
        release.validate('v0.1.0', '0.1.0')
        for tag, version in [('v0.2.0', '0.1.0'), ('0.1.0', '0.1.0'), ('v0.256.0', '0.256.0'), ('v0.1.0-beta', '0.1.0-beta')]:
            with self.subTest(tag=tag), self.assertRaises(RuntimeError):
                release.validate(tag, version)

    def test_desktop_identity(self):
        desktop = (ROOT / '.github/scripts/nova-reader.desktop').read_text()
        self.assertIn('Name=Nova Reader\n', desktop)
        self.assertIn('Exec=nova-reader\n', desktop)
        self.assertNotIn('video', desktop.lower())

    def test_nova_tag_push_builds_without_check_job_dependencies(self):
        workflow = (ROOT / '.github/workflows/build-release.yml').read_text()
        check_condition = "if: github.event_name == 'pull_request' || (github.event_name == 'push' && github.ref == 'refs/heads/main')"
        for job in ('check-linux', 'check-windows', 'check-android'):
            body = re.split(r'\n  [\w-]+:\n', workflow.split(f'  {job}:\n')[1], maxsplit=1)[0]
            self.assertIn(check_condition, body)
        for job in ('linux-build', 'windows-build', 'android-build'):
            body = re.split(r'\n  [\w-]+:\n', workflow.split(f'  {job}:\n')[1], maxsplit=1)[0]
            self.assertNotIn('needs:', body)
            self.assertIn("startsWith(github.ref, 'refs/tags/')", body)
        self.assertNotIn('validate-release.py', workflow)
        self.assertIn('needs: [android-build]', workflow.split('  android-package:\n')[1][:200])
        self.assertIn('needs: [android-package, linux-build, windows-build]', workflow)



if __name__ == '__main__':
    unittest.main()
