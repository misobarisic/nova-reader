{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    fenix = { url = "github:nix-community/fenix"; inputs.nixpkgs.follows = "nixpkgs"; };
  };
  outputs = { nixpkgs, fenix, ... }: let
    systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
  in {
    devShells = nixpkgs.lib.genAttrs systems (system: let
      pkgs = import nixpkgs { inherit system; };
      androidPkgs = import nixpkgs { inherit system; config = { allowUnfree = true; android_sdk.accept_license = true; }; };
      inherit (pkgs) lib;
      f = fenix.packages.${system};
      host = f.stable;
      components = [ host.cargo host.rustc host.clippy host.rustfmt host.rust-src ];
      toolchain = targets: f.combine (components ++ map (t: f.targets.${t}.stable.rust-std) targets);
      cargo-apk2 = pkgs.rustPlatform.buildRustPackage {
        pname = "cargo-apk2";
        version = "1.4.2";
        src = pkgs.fetchFromGitHub {
          owner = "mzdk100"; repo = "cargo-apk2";
          rev = "32b91331ff2371f3f06505ea6ceea3743e423a1c";
          hash = "sha256-Sp6oc3Ee1cjpABCpV3Pj2Hmj26A+4WYDeElUCMbuBjc=";
        };
        # NativeActivity requires neither the Nova PiP/receiver patches nor its
        # migration epoch. Keep upstream's independent epoch-1 version mapping.
        patches = [ ./android/cargo-apk2-readonly-libs.patch ./android/cargo-apk2-uncompressed-libs.patch ./android/cargo-apk2-image-provider.patch ];
        cargoLock.lockFile = ./android/cargo-apk2-Cargo.lock;
        postPatch = ''
          sed -i '/^    ".*-examples",$/d' Cargo.toml
          cp ${./android/cargo-apk2-Cargo.lock} Cargo.lock
        '';
        cargoBuildFlags = [ "-p" "cargo-apk2" ];
        doCheck = false; # upstream integration tests require SDK/device
      };
    in {
      default = pkgs.mkShell {
        nativeBuildInputs = [ (toolchain []) pkgs.pkg-config pkgs.cargo-about pkgs.python3 pkgs.nodejs ];
        buildInputs = with pkgs; [ fontconfig freetype libxkbcommon libGL libx11 libxcursor libxi libxrandr vulkan-loader ] ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [ wayland ];
        LD_LIBRARY_PATH = lib.makeLibraryPath (with pkgs; [ fontconfig libxkbcommon libGL libx11 libxcursor libxi libxrandr vulkan-loader ] ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [ wayland ]);
      };
      spelling = pkgs.mkShell { buildInputs = [ (pkgs.aspellWithDicts (d: [ d.en ])) ]; };
    } // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
      android = let
        sdk = (androidPkgs.androidenv.composeAndroidPackages {
          platformVersions = [ "34" "35" ]; buildToolsVersions = [ "34.0.0" "35.0.0" ];
          abiVersions = [ "arm64-v8a" "armeabi-v7a" "x86_64" ];
          includeNDK = true; ndkVersions = [ "26.1.10909125" ];
        }).androidsdk;
      in androidPkgs.mkShell {
        nativeBuildInputs = [ sdk androidPkgs.jdk17 cargo-apk2 pkgs.python3 pkgs.android-tools pkgs.cargo-about pkgs.llvmPackages.libclang pkgs.gn pkgs.ninja pkgs.git pkgs.curl (toolchain [ "aarch64-linux-android" "armv7-linux-androideabi" "x86_64-linux-android" ]) ];
        shellHook = ''
          export ANDROID_HOME="${sdk}/libexec/android-sdk"
          export ANDROID_NDK_ROOT="$ANDROID_HOME/ndk/26.1.10909125"
          export ANDROID_NDK="$ANDROID_NDK_ROOT"
          export JAVA_HOME="${androidPkgs.jdk17}"
          export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
          # ARMv7 Skia builds from source; QuickJS bindgen needs target headers
          # through the host libclang rather than stdenv's glibc include path.
          export SKIA_GN_COMMAND="${pkgs.gn}/bin/gn"
          export SKIA_NINJA_COMMAND="${pkgs.ninja}/bin/ninja"
          ndk_bin="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin"
          reader_ndk_sysroot="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
          export BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android="--sysroot=$reader_ndk_sysroot --target=aarch64-linux-android26"
          export BINDGEN_EXTRA_CLANG_ARGS_armv7_linux_androideabi="--sysroot=$reader_ndk_sysroot --target=armv7a-linux-androideabi26"
          export BINDGEN_EXTRA_CLANG_ARGS_x86_64_linux_android="--sysroot=$reader_ndk_sysroot --target=x86_64-linux-android26"
          export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ndk_bin/aarch64-linux-android26-clang"
          export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$ndk_bin/armv7a-linux-androideabi26-clang"
          export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$ndk_bin/x86_64-linux-android26-clang"
          export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"
          export CC_armv7_linux_androideabi="$CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER"
          export CC_x86_64_linux_android="$CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER"
          export CXX_aarch64_linux_android="$ndk_bin/aarch64-linux-android26-clang++"
          export CXX_armv7_linux_androideabi="$ndk_bin/armv7a-linux-androideabi26-clang++"
          export CXX_x86_64_linux_android="$ndk_bin/x86_64-linux-android26-clang++"
          export AR_aarch64_linux_android="$ndk_bin/llvm-ar"
          export AR_armv7_linux_androideabi="$ndk_bin/llvm-ar"
          export AR_x86_64_linux_android="$ndk_bin/llvm-ar"
          unset CPATH
        '';
      };
      windows = let cc = pkgs.pkgsCross.mingwW64.stdenv.cc; in pkgs.mkShell {
        nativeBuildInputs = [ (toolchain [ "x86_64-pc-windows-gnu" ]) cc pkgs.pkg-config pkgs.python3 pkgs.cargo-about ];
        buildInputs = [ pkgs.pkgsCross.mingwW64.windows.pthreads ];
        # aws-lc's supplemental entropy source includes sched.h, absent in MinGW.
        AWS_LC_SYS_NO_JITTER_ENTROPY = "1";
        MINGW_CC = "${cc}";
        MINGW_OBJDUMP = "${cc.bintools}/bin/${cc.targetPrefix}objdump";
        CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "${cc}/bin/${cc.targetPrefix}gcc";
        CC_x86_64_pc_windows_gnu = "${cc}/bin/${cc.targetPrefix}gcc";
        CXX_x86_64_pc_windows_gnu = "${cc}/bin/${cc.targetPrefix}g++";
        AR_x86_64_pc_windows_gnu = "${cc}/bin/${cc.targetPrefix}ar";
      };
    });
  };
}
