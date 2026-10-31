"""Exercise actual Make commands against cargo-apk2's supported CLI contract."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class AndroidCommandTests(unittest.TestCase):
    def test_locked_fetch_precedes_apk2_without_unsupported_locked_argument(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            cargo = root / 'cargo'
            cargo.write_text('''#!/usr/bin/env python3
import os,sys
from pathlib import Path
args=sys.argv[1:]
if args[:2] == ['apk2','build'] and '--locked' in args:
    sys.exit('cargo-apk2 rejects --locked')
with Path(os.environ['CARGO_COMMAND_LOG']).open('a') as log:
    log.write(' '.join(args)+'\\n')
''')
            cargo.chmod(0o755)
            log = root / 'commands.log'
            env = dict(os.environ, PATH=f'{root}:{os.environ["PATH"]}', CARGO_COMMAND_LOG=str(log))
            for target in ('android-build-release', 'android-build-release-universal'):
                log.write_text('')
                subprocess.run(['make', '-f', str(ROOT / 'Makefile'), target,
                                'ANDROID_TARGET=armv7-linux-androideabi', 'RELEASE_PROFILE=cd-release'],
                               cwd=root, env=env, check=True, capture_output=True)
                commands = log.read_text().splitlines()
                self.assertTrue(commands[0].startswith('fetch --locked'))
                self.assertTrue(commands[1].startswith('apk2 build '))
                self.assertNotIn('--locked', commands[1])
                self.assertIn('--profile cd-release', commands[1])
                self.assertIn('--features android,update-check', commands[1])
                if target == 'android-build-release':
                    self.assertIn('--target armv7-linux-androideabi', commands[1])


if __name__ == '__main__':
    unittest.main()
