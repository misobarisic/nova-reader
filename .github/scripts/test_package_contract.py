"""Regressions for independent identity, release gates, and shipped PE imports."""
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / '.github/scripts' / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


release = module('release', 'validate-release.py')
windows = module('windows', 'windows-runtime.py')


class PackagingContractTests(unittest.TestCase):
    def test_tag_and_android_version_policy(self):
        release.validate('v0.1.0', '0.1.0')
        for tag, version in [('v0.2.0', '0.1.0'), ('0.1.0', '0.1.0'), ('v0.256.0', '0.256.0'), ('v0.1.0-beta', '0.1.0-beta')]:
            with self.subTest(tag=tag), self.assertRaises(RuntimeError):
                release.validate(tag, version)

    def test_windows_system_libraries_do_not_hide_missing_runtimes(self):
        for name in ['KERNEL32.dll', 'api-ms-win-core-synch-l1-2-0.dll', 'DWrite.dll']:
            self.assertTrue(windows.is_system(name))
        for name in ['libgcc_s_seh-1.dll', 'libstdc++-6.dll', 'libwinpthread-1.dll', 'unknown.dll']:
            self.assertFalse(windows.is_system(name))

    def test_desktop_identity(self):
        desktop = (ROOT / '.github/scripts/nova-reader.desktop').read_text()
        self.assertIn('Name=Nova Reader\n', desktop)
        self.assertIn('Exec=nova-reader\n', desktop)
        self.assertNotIn('video', desktop.lower())

    def test_release_jobs_depend_on_tag_checks(self):
        workflow = (ROOT / '.github/workflows/build-release.yml').read_text()
        for job in ('package-linux', 'package-windows', 'package-android'):
            body = workflow.split(f'  {job}:\n')[1][:200]
            self.assertIn('needs: [validate, check-windows, check-android]', body)
        self.assertIn('cargo test --workspace --locked', (ROOT / 'Makefile').read_text())


if __name__ == '__main__':
    unittest.main()
