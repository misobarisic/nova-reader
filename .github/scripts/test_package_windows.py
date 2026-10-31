"""Exercise Nova's repeatable compiler-closure Windows staging with reader IDs."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from zipfile import ZipFile


class WindowsPackagingTests(unittest.TestCase):
    def test_repeated_packaging_preserves_readonly_runtime_inputs(self):
        script = Path(__file__).with_name('package-windows.sh').resolve()
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            executable = root / 'target/x86_64-pc-windows-gnu/cd-release/nova-reader.exe'
            executable.parent.mkdir(parents=True)
            executable.write_bytes(b'MZ reader fixture')
            (root / 'LICENSE').write_text('application license')
            (root / 'THIRD_PARTY_NOTICES.md').write_text('source attribution')
            inventory = root / 'target/licenses/x86_64-pc-windows-gnu.txt'
            inventory.parent.mkdir(parents=True)
            inventory.write_text('dependency licenses')
            inputs = root / 'runtime-inputs'
            inputs.mkdir()
            dll = inputs / 'libwinpthread-1.dll'
            dll.write_bytes(b'MZ readonly runtime fixture')
            dll.chmod(0o444)
            (inputs / 'not-a-dll.txt').write_text('must not be shipped')
            runtime = root / 'compiler-runtime'
            runtime.symlink_to(inputs, target_is_directory=True)
            tools = root / 'tools'
            tools.mkdir()
            query = tools / 'nix-store'
            query.write_text('#!/bin/sh\nprintf "%s\\n" "$MINGW_CC"\n')
            query.chmod(0o755)
            env = dict(os.environ, MINGW_CC=str(runtime), GITHUB_REF_NAME='v0.1.1',
                       NOVA_RELEASE_PROFILE='cd-release', PATH=f'{tools}:{os.environ["PATH"]}')
            for _ in range(2):
                subprocess.run(['bash', str(script)], cwd=root, env=env, check=True, capture_output=True)
                with ZipFile(root / 'dist/Nova-Reader-Windows-x86_64-0.1.1.zip') as archive:
                    self.assertEqual(set(archive.namelist()), {
                        'nova-reader.exe', 'LICENSE', 'THIRD_PARTY_NOTICES.md',
                        'DEPENDENCY_LICENSES.txt', 'libwinpthread-1.dll',
                    })
                    self.assertEqual(archive.read('libwinpthread-1.dll'), dll.read_bytes())
                # Repackaging must recover old read-only staging directories.
                (root / 'target/windows-package').chmod(0o555)
            self.assertEqual(dll.stat().st_mode & 0o222, 0)


if __name__ == '__main__':
    unittest.main()
