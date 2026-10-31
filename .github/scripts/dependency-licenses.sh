#!/usr/bin/env bash
set -euo pipefail
case "${1:?specify android/windows/linux}" in
  android) targets=(aarch64-linux-android armv7-linux-androideabi x86_64-linux-android); features=android,update-check ;;
  windows) targets=(x86_64-pc-windows-gnu); features=desktop,update-check ;;
  linux) targets=(x86_64-unknown-linux-gnu); features=desktop,update-check ;;
  *) exit 2 ;;
esac
mkdir -p target/licenses
for target in "${targets[@]}"; do
  cargo-about generate --locked --fail -c .github/scripts/about.toml --target "$target" --no-default-features --features "$features" assets/dependency-licenses.hbs > "target/licenses/$target.txt"
done
if [[ "$1" == android ]]; then
  mkdir -p android/assets/dependency-licenses
  cp target/licenses/*linux-android.txt target/licenses/armv7-linux-androideabi.txt android/assets/dependency-licenses/
fi
