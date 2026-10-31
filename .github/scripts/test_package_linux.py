"""Keep Debian artifact verification from closing dpkg-deb's output early."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class LinuxPackagingTests(unittest.TestCase):
    def test_debian_contents_check_consumes_large_archive_listing(self):
        script = Path(__file__).with_name('package-linux.sh').read_text()
        check = next(line for line in script.splitlines() if line.startswith('dpkg-deb --contents '))
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package = root / 'package'
            (package / 'DEBIAN').mkdir(parents=True)
            (package / 'DEBIAN').chmod(0o755)
            (package / 'DEBIAN/control').write_text(
                'Package: nova-reader\nVersion: 0.1.1\nArchitecture: amd64\n'
                'Maintainer: Nova Reader\nDescription: packaging fixture\n')
            binary = package / 'usr/bin/nova-reader'
            binary.parent.mkdir(parents=True)
            binary.write_text('fixture')
            # The binary appears early; enough later output reproduces the old
            # grep -q/pipefail SIGPIPE instead of depending on pipe buffer size.
            docs = package / 'usr/share/doc/nova-reader'
            docs.mkdir(parents=True)
            for i in range(1000):
                (docs / f'{i:04}-{"x" * 100}').touch()
            deb = root / 'reader fixture.deb'
            subprocess.run(['dpkg-deb', '--root-owner-group', '--build', str(package), str(deb)],
                           check=True, capture_output=True)
            subprocess.run(['bash', '-e', '-o', 'pipefail', '-c', check],
                           env=dict(os.environ, deb_file=str(deb)), check=True, capture_output=True)


if __name__ == '__main__':
    unittest.main()
