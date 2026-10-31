fn main() {
    // cargo-apk2 supplies CARGO_ENCODED_RUSTFLAGS, overriding target rustflags
    // from .cargo/config.toml. Emit this on the final Android cdylib link so
    // its ELF load segments remain compatible with 16 KiB Android pages.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        println!("cargo:rustc-link-arg-cdylib=-Wl,-z,max-page-size=16384");
    }
}
