"""Stage only MinGW runtimes actually imported by the application and each DLL."""
import re
import os
import shutil
import subprocess
import sys
from pathlib import Path

SYSTEM = set('advapi32 bcrypt comctl32 comdlg32 crypt32 d2d1 d3d11 d3d12 dcomp dwrite dwmapi dxgi gdi32 imm32 kernel32 msvcrt ntdll ole32 oleaut32 opengl32 propsys psapi rpcrt4 secur32 setupapi shell32 shcore shlwapi user32 userenv uxtheme version winhttp winmm winspool ws2_32 wtsapi32'.split())


def imports(path, objdump):
    report = subprocess.check_output([str(objdump), '-p', str(path)], text=True)
    return re.findall(r'DLL Name:\s+(\S+)', report)


def is_system(name):
    stem = name.lower().removesuffix('.dll')
    return stem in SYSTEM or stem.startswith(('api-ms-win-', 'ext-ms-win-'))


def main():
    bundle, cc = map(Path, sys.argv[1:])
    objdump = Path(os.environ['MINGW_OBJDUMP'])
    closure = subprocess.check_output(['nix-store', '-qR', str(cc)], text=True).splitlines()
    candidates = {}
    for store in closure:
        for path in sorted(Path(store).rglob('*.dll')):
            candidates.setdefault(path.name.lower(), path)
    pending = list(bundle.glob('*.exe'))
    seen = set()
    while pending:
        path = pending.pop()
        for name in imports(path, objdump):
            if is_system(name) or name.lower() in seen:
                continue
            seen.add(name.lower())
            if re.search(r'(mpv|avcodec|avformat|avfilter|avutil|swscale|swresample)', name, re.I):
                raise RuntimeError(f'Unexpected video DLL: {name}')
            source = candidates.get(name.lower())
            if source is None:
                raise RuntimeError(f'Unresolved shipped PE import {name} in {path.name}')
            destination = bundle / name
            shutil.copyfile(source, destination)
            pending.append(destination)


if __name__ == '__main__':
    main()
