# Development and packaging

Build infrastructure derives from Nova revision
`b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda`. `flake.lock` preserves
Nova's exact nixpkgs/Fenix pins. The flake exports development shells rather
than packages: `default`, `spelling`, and Linux-hosted `android` and `windows`.
Reader builds require no native video, torrent, casting or sync libraries.

Use `nix develop --command make check` for formatting, Clippy, workspace checks,
tests and packaging regressions. Cross checks are:

```sh
nix develop .#windows --command cargo check --locked --target x86_64-pc-windows-gnu
nix develop .#android --command cargo check --locked --target aarch64-linux-android --no-default-features --features android --lib
```

Copy `.cargo/local-kit.example.toml` to `.cargo/local-kit.toml` to use sibling
`../nova-kit` through `cargo dev`, `cargo dev-run`, `cargo dev-release` and
`cargo dev-release-run`. CI always uses the committed Git dependency pins.

## Android

The app uses `.ReaderActivity`, a thin framework NativeActivity subclass for
Android document import, and loads `libnova_reader.so`. `cargo-apk2` compiles
`android/java` into DEX; no Gradle project is needed. Its ID is `dev.misob.nova.reader`; the display
name is Nova Reader. Internet access is declared for source networking.
`cargo-apk2` is pinned to `32b91331ff2371f3f06505ea6ceea3743e423a1c`
with Nova's CLI dependency lock and just the read-only-library and uncompressed
native-library fixes. Nova's PiP, receiver and version migration patches are
unnecessary here.

Android version codes are independent of Nova: upstream epoch 1 followed by
one byte each for major, minor and patch. Thus 0.1.0 is 16777472. Public Android
releases require stable three-component numeric versions, each component at
most 255, in strictly increasing semver order. Prereleases and build metadata
are rejected for release tags rather than sharing an ambiguous version code.

Local builds use the intentionally public debug key in `android/keystore`:

```sh
nix develop .#android --command make android-build-release
```

Signing uses Nova's selected-profile precedence: the
`CARGO_APK_<PROFILE>_KEYSTORE` / `_PASSWORD` environment overrides take priority
over the matching Cargo metadata profile. The intentionally public debug key
remains available for personal sideload builds; store distribution requires a
private override. If `ANDROID_KEYSTORE_BASE64` and `ANDROID_KEYSTORE_PASSWORD`
repository secrets are configured, the workflow imports that same existing key
for every ABI build and the assembly job. Both must be supplied together.
No signing identity is replaced when these overrides are present.

The workflow follows Nova's three parallel `android-build` matrix jobs (ARM64,
ARMv7 and x86_64), followed by `android-package` assembly. Each uploads its
`nova_reader.apk` as a separate input. The packager merges all three
`--abi-apk ABI=PATH` inputs, requiring identical manifests, resources and shared
assets. The local `android-build-release-universal` Make target remains an
alternative. `cargo fetch --locked` checks the committed lock before invoking
cargo-apk2, whose CLI does not support Cargo's `--locked` argument. It emits ARM64, ARMv7,
x86_64 and universal APKs. Every shipped ELF library is checked against its ABI;
video libraries are rejected. ELF PT_LOAD segments are also checked for 16 KiB
compatibility. Root `build.rs` emits the page-size argument on the final Android
cdylib link: cargo-apk2 sets `CARGO_ENCODED_RUSTFLAGS`, which overrides the target
flags in `.cargo/config.toml`. Plain Cargo builds still use those target flags. Per-ABI APKs store and align libraries to 16 KiB;
the universal APK compresses/extracts them. Every result is signed, signature
verified and compared with the original signing identity and payload.

## Desktop and distribution

Tagged builds use `cd-release`. Linux release binaries are built on Ubuntu
22.04 with system development libraries and packaged as Debian and AppImage
artifacts. The AppImage tools have pinned SHA-256 checksums. Debian package
identity, installed binary, dependency resolution and AppImage extraction are
checked. Windows builds use the Nix GNU cross toolchain. Its packager uses Nova's compiler-closure DLL staging with
`nix-store -qR`, preserves read-only Nix inputs and creates a flat ZIP. Windows
system libraries such as UIAutomationCore are supplied by Windows. The upload
uses `archive: false` and publication downloads that artifact by ID with
`skip-decompress: true`, preserving the original release ZIP.

`dependency-licenses.sh` uses cargo-about 0.9.1 (Nix-provided or explicitly
installed by Linux CI) with `--locked --fail` for the exact release targets and
features. Full license texts and upstream links accompany LICENSE and project
attribution in desktop packages and Android assets. The inventory also includes
the Apache license for the bundled Roboto faces reused from Nova. The launcher artwork is
inherited from Nova under the same license; it is not a new branding claim.

The workflow is copied from Nova with reader-specific names, paths and native
dependencies. Its job conditions and dependencies match Nova exactly: Linux,
Windows and Android check jobs run on pull requests and pushes to main, while
tag pushes start the three Android ABI builds, Linux build and Windows build
directly. Tag builds do not run or depend on check jobs. Android assembly waits
for its ABI builds; publication waits for Android assembly, Linux and Windows.
The existing reader signing override and license inventory steps are retained
because these provide the signing identity and files used by the reader build.
Nova's pinned Actions, caches, disk cleanup, artifact transport and permissions
are unchanged. Tag names select the release version; version 0.1.2 carries this
correction without moving previously pushed tags.

The initial commit's Linux, Windows and Android checks passed in GitHub Actions.
The v0.1.0 packaging run exposed an unsupported cargo-apk2 flag and the custom
Windows DLL checker rejecting an OS library. Linux packaging also completed
both artifacts but failed its added Debian listing check because `grep -q`
closed the pipe early. Version 0.1.1 restores Nova's build/DLL staging paths and
consumes the complete Debian listing, with regressions covering all three failures. A complete artifact and device
smoke run is required before claiming installation and launch acceptance. Device launch, Android touch behavior and Windows execution
cannot be established by host-side archive and compile checks alone.

The Android shell includes host libclang with target-specific NDK sysroots for
QuickJS bindgen, plus GN/Ninja/Git/Curl for ARMv7 Skia source builds.
`make check-android-java` compiles the SAF activity against API 35; Android CI
runs it alongside the ARM64 Rust check with update checking enabled.

Version 0.1.5 compiled all three Android inputs but assembly rejected the ARM64
Reader library's 4 KiB ELF load-segment alignment. The same configuration remained
in versions 0.1.6 and 0.1.7. Version 0.1.8 sets alignment on the final cdylib link
instead of relying on the overridden Cargo flags. The regression links real ELF
libraries under encoded rustflags, reproduces the rejection without the Android
build directive, and verifies that the directive fixes it. This host-link test
does not replace the full Android build/sign/assembly run or device acceptance.
