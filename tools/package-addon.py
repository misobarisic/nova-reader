#!/usr/bin/env python3
"""Build a deterministic format-2 reader addon. Native install remains authoritative."""
import argparse
import json
import pathlib
import re
import stat
import zipfile

LIMIT = 8 * 1024 * 1024


def files_for(root):
    manifest_path = root / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest.get("formatVersion") != 2 or manifest.get("contract") != "nova-reader1":
        raise ValueError("expected format 2 nova-reader1 package")
    allowed = {"formatVersion", "contract", "api", "id", "name", "version", "language", "nsfw", "entry", "scripts", "assets", "icon", "permissions", "capabilities", "configuration"}
    if set(manifest) - allowed:
        raise ValueError("unknown manifest fields")
    if not re.fullmatch(r"[A-Za-z0-9._-]{1,64}", manifest["id"]):
        raise ValueError("invalid source ID")
    if not re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", manifest["version"]):
        raise ValueError("invalid semantic version")
    fields = manifest.get("configuration", [])
    if not isinstance(fields, list) or len(fields) > 64 or len(json.dumps(fields).encode()) > 65536:
        raise ValueError("source settings schema limit")
    keys = set()
    for field in fields:
        if not isinstance(field, dict) or set(field) - {"key", "name", "kind", "default", "options"}:
            raise ValueError("invalid settings field")
        key, name, kind = field.get("key"), field.get("name"), field.get("kind")
        if not isinstance(key, str) or not re.fullmatch(r"[A-Za-z0-9._-]{1,64}", key) or key in keys or not isinstance(name, str) or not name.strip() or len(name.encode()) > 128 or any(ord(c) < 32 for c in name):
            raise ValueError("invalid settings identity")
        keys.add(key)
        options, value = field.get("options", []), field.get("default")
        if not isinstance(options, list) or len(options) > 128 or any(not isinstance(o, str) or len(o.encode()) > 256 or any(ord(c) < 32 for c in o) for o in options) or len(options) != len(set(options)):
            raise ValueError("invalid settings options")
        if kind == "bool":
            valid = isinstance(value, bool) and not options
        elif kind == "select":
            valid = isinstance(value, str) and value in options
        elif kind in ("text", "secret"):
            valid = isinstance(value, str) and len(value.encode()) <= 4096 and "\0" not in value and not options and (kind != "secret" or value == "")
        else:
            valid = False
        if not valid:
            raise ValueError("invalid settings default or kind")
    scripts = [manifest["entry"], *manifest.get("scripts", [])]
    names = ["manifest.json", *scripts, *manifest.get("assets", [])]
    if len(names) != len(set(names)) or len(names) > 162:
        raise ValueError("duplicate files or file count limit")
    for name in names:
        if not isinstance(name, str) or len(name) > 240 or "\\" in name or ":" in name or any(p in ("", ".", "..") for p in name.split("/")) or any(ord(c) < 32 for c in name):
            raise ValueError("unsafe file path")
        path = root / name
        if any(p.is_symlink() for p in [path, *path.parents]) or not path.is_file():
            raise ValueError(f"missing/non-file/symlink input: {name}")
    if any(not p.endswith(".js") for p in scripts):
        raise ValueError("scripts must use .js")
    if sum((root / p).stat().st_size + 3 for p in scripts) > 2 * 1024 * 1024:
        raise ValueError("script byte limit")
    if sum((root / p).stat().st_size for p in names) > LIMIT:
        raise ValueError("expanded archive byte limit")
    icon = manifest.get("icon")
    if icon is not None:
        if icon not in manifest.get("assets", []) or not icon.endswith(".png"):
            raise ValueError("icon must be a declared PNG asset")
        data = (root / icon).read_bytes()
        if len(data) > 256 * 1024 or len(data) < 24 or data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR" or any(not 0 < int.from_bytes(data[i:i+4], "big") <= 512 for i in (16, 20)):
            raise ValueError("icon exceeds PNG size/dimension limits")
    return sorted(names)


def package(root, output):
    names = files_for(root)
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(output.suffix + ".tmp")
    try:
        with zipfile.ZipFile(temporary, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
            for name in names:
                info = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
                info.create_system = 3
                info.external_attr = (stat.S_IFREG | 0o644) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                archive.writestr(info, (root / name).read_bytes(), compresslevel=9)
        if temporary.stat().st_size > LIMIT:
            raise ValueError("archive byte limit")
        temporary.replace(output)
    finally:
        temporary.unlink(missing_ok=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path)
    parser.add_argument("output", type=pathlib.Path)
    args = parser.parse_args()
    package(args.source.resolve(), args.output.resolve())
