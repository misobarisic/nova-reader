.PHONY: apk android-build android-build-release android-build-release-universal android-run android-run-release linux-build linux-build-release linux-run linux-run-release check check-addons check-android-java
ANDROID_TARGET ?= aarch64-linux-android
RELEASE_PROFILE ?= release
UPDATE_FEATURES ?=
ANDROID_ARGS = --locked --target $(ANDROID_TARGET) --no-default-features --features android$(UPDATE_FEATURES) --lib -p nova-reader
apk: android-build
android-build:
	cargo apk2 build $(ANDROID_ARGS)
android-build-release:
	cargo apk2 build $(ANDROID_ARGS) --profile $(RELEASE_PROFILE)
android-build-release-universal:
	cargo apk2 build --locked --no-default-features --features android$(UPDATE_FEATURES) --lib -p nova-reader --profile $(RELEASE_PROFILE)
android-run:
	cargo apk2 run $(ANDROID_ARGS)
android-run-release:
	cargo apk2 run $(ANDROID_ARGS) --profile $(RELEASE_PROFILE)
linux-build:
	cargo build --locked
linux-build-release:
	cargo build --locked --profile $(RELEASE_PROFILE)
linux-run:
	cargo run --locked
linux-run-release:
	cargo run --locked --profile $(RELEASE_PROFILE)
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

check-android-java:
	mkdir -p target/android-java-check
	javac -source 8 -target 8 -Xlint:-options -cp "$(ANDROID_HOME)/platforms/android-35/android.jar" -d target/android-java-check android/java/dev/misob/nova/reader/ReaderActivity.java
