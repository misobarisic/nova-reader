#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

profile="${NOVA_RELEASE_PROFILE:-release}"
binary="target/$profile/nova-reader"
if [[ ! -x "$binary" ]]; then
  echo "Missing release binary: $binary (run cargo build --profile $profile --bin nova-reader first)" >&2
  exit 1
fi

if ldd "$binary" | grep -Ei 'lib(mpv|avcodec|avfilter|avformat|avutil|swscale|swresample)'; then
  echo "Reader unexpectedly links video libraries" >&2
  exit 1
fi
if ldd "$binary" | grep -q 'not found'; then
  echo "Reader has unresolved runtime dependencies" >&2
  exit 1
fi
cargo_version="$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "nova-reader"))')"
event_name="${GITHUB_EVENT_NAME:-local}"
if [[ "$event_name" == "pull_request" ]]; then
  artifact_tag="pr-${GITHUB_EVENT_NUMBER:-0}"
  debian_version="${cargo_version}~pr${GITHUB_EVENT_NUMBER:-0}"
else
  raw_tag="${GITHUB_REF_NAME:-local}"
  artifact_version="${raw_tag#v}"
  artifact_tag="$(printf '%s' "$artifact_version" | sed -E 's/[^A-Za-z0-9._+-]+/-/g')"
  upstream_version="${raw_tag#v}"
  upstream_version="$(printf '%s' "$upstream_version" | sed -E 's/-/~/g; s/[^A-Za-z0-9.+~]/./g')"
  if [[ "$upstream_version" =~ ^[0-9] ]]; then
    debian_version="$upstream_version"
  else
    debian_version="${cargo_version}+${upstream_version}"
  fi
fi
if [[ -z "$artifact_tag" ]]; then
  artifact_tag="local"
fi
dpkg --validate-version "$debian_version"

work="target/linux-package"
tools_dir="$work/tools"
appdir="$work/nova-reader.AppDir"
deb_work="$work/deb-work"
deb_root="$work/deb-root"
dist="$repo_root/dist"
rm -rf "$work"
mkdir -p "$tools_dir" "$appdir" "$deb_work/debian" "$deb_root/DEBIAN" "$dist"

download_verified() {
  local url="$1"
  local output="$2"
  local expected_sha256="$3"

  curl --fail --location --retry 3 --silent --show-error "$url" --output "$output"
  printf '%s  %s\n' "$expected_sha256" "$output" | sha256sum --check
  chmod 755 "$output"
}

# AppImage's continuous release assets are mutable, so pin their published
# SHA-256 digests and fail closed if the upstream binaries change.
download_verified \
  "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage" \
  "$tools_dir/linuxdeploy-x86_64.AppImage" \
  "8aea8da0f7f7039d2a2cecb14657d752a222a5e1d3825caeef186c82f751cdd1"
download_verified \
  "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/continuous/linuxdeploy-plugin-appimage-x86_64.AppImage" \
  "$tools_dir/linuxdeploy-plugin-appimage-x86_64.AppImage" \
  "49d6a17160675a6bd1781699aae6bdf7692d98552e02a3671d2183d10547842e"

appimage="$dist/Nova-Reader-Linux-x86_64-${artifact_tag}.AppImage"
deb_file="$dist/Nova-Reader-Linux-x86_64-${artifact_tag}.deb"
app_icon="$appdir/usr/share/icons/hicolor/512x512/apps/nova-reader.png"
icon_512="$work/nova-reader-512.png"

install -D -m 755 "$binary" "$appdir/usr/bin/nova-reader"
install -D -m 755 "$binary" "$deb_root/usr/bin/nova-reader"
convert assets/logo.png -resize 512x512 -background none -gravity center -extent 512x512 "$icon_512"
icon_dimensions="$(identify -format '%wx%h' "$icon_512")"
if [[ "$icon_dimensions" != "512x512" ]]; then
  echo "Expected a 512x512 AppImage icon, got $icon_dimensions" >&2
  exit 1
fi
install -D -m 644 "$icon_512" "$app_icon"
install -D -m 644 target/licenses/x86_64-unknown-linux-gnu.txt "$appdir/usr/share/doc/nova-reader/DEPENDENCY_LICENSES.txt"
install -D -m 644 target/licenses/x86_64-unknown-linux-gnu.txt "$deb_root/usr/share/doc/nova-reader/DEPENDENCY_LICENSES.txt"
install -D -m 644 LICENSE "$appdir/usr/share/doc/nova-reader/LICENSE"
install -D -m 644 THIRD_PARTY_NOTICES.md "$appdir/usr/share/doc/nova-reader/THIRD_PARTY_NOTICES.md"
install -D -m 644 .github/scripts/nova-reader.desktop "$deb_root/usr/share/applications/nova-reader.desktop"
install -D -m 644 "$icon_512" "$deb_root/usr/share/icons/hicolor/512x512/apps/nova-reader.png"
install -D -m 644 LICENSE "$deb_root/usr/share/doc/nova-reader/LICENSE"
install -D -m 644 THIRD_PARTY_NOTICES.md "$deb_root/usr/share/doc/nova-reader/THIRD_PARTY_NOTICES.md"
install -D -m 644 .github/scripts/nova-reader.desktop "$appdir/usr/share/applications/nova-reader.desktop"
desktop-file-validate .github/scripts/nova-reader.desktop
desktop-file-validate "$appdir/usr/share/applications/nova-reader.desktop"

cat > "$deb_work/debian/control" <<EOF
Source: nova-reader
Section: graphics
Priority: optional
Maintainer: Nova Reader project <50531162+misobarisic@users.noreply.github.com>
Standards-Version: 4.6.0

Package: nova-reader
Architecture: amd64
Depends: \${shlibs:Depends}, \${misc:Depends}
Description: Cross-platform comic reader
 Nova Reader is a comic reader for desktop and Android.
EOF

shlib_vars="$(cd "$deb_work" && dpkg-shlibdeps -O -e"$repo_root/$binary")"
shlib_dependencies="$(sed -n 's/^shlibs:Depends=//p' <<<"$shlib_vars")"
if [[ -z "$shlib_dependencies" ]]; then
  echo "dpkg-shlibdeps did not report runtime dependencies for $binary" >&2
  exit 1
fi
dependencies="$shlib_dependencies"

cat > "$deb_root/DEBIAN/control" <<EOF
Package: nova-reader
Version: $debian_version
Section: graphics
Priority: optional
Architecture: amd64
Maintainer: Nova Reader project <50531162+misobarisic@users.noreply.github.com>
Depends: $dependencies
Description: Cross-platform comic reader
 Nova Reader is a comic reader for desktop and Android.
EOF
chmod 644 "$deb_root/DEBIAN/control"
dpkg-deb --root-owner-group --build "$deb_root" "$deb_file"

# AppImage tools are AppImages themselves. This lets the build work on hosted
# runners without relying on FUSE being available.
export APPIMAGE_EXTRACT_AND_RUN=1
export ARCH=x86_64

linuxdeploy_args=(
  --appdir "$appdir"
  --executable "$appdir/usr/bin/nova-reader"
  --desktop-file "$appdir/usr/share/applications/nova-reader.desktop"
  --icon-file "$app_icon"
)

LINUXDEPLOY_OUTPUT_VERSION="$debian_version" \
LDAI_OUTPUT="$appimage" \
  "$tools_dir/linuxdeploy-x86_64.AppImage" \
    "${linuxdeploy_args[@]}" --output appimage

test "$(dpkg-deb -f "$deb_file" Package)" = nova-reader
dpkg-deb --contents "$deb_file" | grep './usr/bin/nova-reader$' > /dev/null
# Execute the AppImage's built-in extractor as an artifact-format smoke check.
(cd "$work" && "$appimage" --appimage-extract >/dev/null)
test -x "$work/squashfs-root/usr/bin/nova-reader"
test -s "$appimage"
chmod 755 "$appimage"
echo "Created $deb_file"
echo "Created $appimage"
