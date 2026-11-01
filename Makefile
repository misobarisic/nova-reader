

.PHONY: apk android-build android-build-release android-build-release-universal \
	android-run android-run-release \
	linux-build linux-build-release linux-run linux-run-release

# Run these inside the appropriate dev shell: `nix develop` for Linux or
# `nix develop .#android` for Android. Override the target for an emulator,
# e.g. `make android-run ANDROID_TARGET=x86_64-linux-android`.
ANDROID_TARGET ?= aarch64-linux-android
# Local release builds stay quick; tagged CI selects cd-release explicitly.
RELEASE_PROFILE ?= release
# Set UPDATE_FEATURES= to build APKs without any GitHub update checks.
UPDATE_FEATURES ?= ,update-check
ANDROID_ARGS = --target $(ANDROID_TARGET) --no-default-features --features android$(UPDATE_FEATURES) --lib -p nova-reader

# cargo-apk2 (not cargo-apk) compiles Reader's Java document activity to DEX.
apk: android-build

android-build:
	cargo apk2 build $(ANDROID_ARGS)

android-build-release:
	cargo apk2 build $(ANDROID_ARGS) --profile $(RELEASE_PROFILE)

# Local three-ABI alternative to CI's parallel single-ABI builds. The release
# packager accepts either input to produce the same four APK variants.
android-build-release-universal:
	cargo apk2 build --no-default-features --features android$(UPDATE_FEATURES) --lib -p nova-reader --profile $(RELEASE_PROFILE)

android-run:
	cargo apk2 run $(ANDROID_ARGS)

android-run-release:
	cargo apk2 run $(ANDROID_ARGS) --profile $(RELEASE_PROFILE)

linux-build:
	cargo build

linux-build-release:
	cargo build --profile $(RELEASE_PROFILE)

linux-run:
	cargo run

linux-run-release:
	cargo run --profile $(RELEASE_PROFILE)

.PHONY: check check-addons check-android-java

check: check-addons
	cargo fmt --all -- --check
	cargo clippy --workspace --locked -- -D warnings
	cargo check --workspace --locked
	cargo test --workspace --locked
	python3 -B -m unittest discover -s .github/scripts -p 'test_package_*.py'

check-addons:
	node examples/addons/neutral-fixture/tests/fixture.test.mjs
	npm ci --ignore-scripts --prefix examples/addons/manhwa18cc/tests
	npm test --prefix examples/addons/manhwa18cc/tests
	node examples/addons/mangafire/tests/parser.test.mjs

check-android-java:
	mkdir -p target/android-java-check
	javac -source 8 -target 8 -Xlint:-options -cp "$(ANDROID_HOME)/platforms/android-35/android.jar" -d target/android-java-check android/java/dev/misob/nova/reader/*.java
