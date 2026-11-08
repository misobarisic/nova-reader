include!("build-support.rs");

fn main() {
    enable_experimental_components();
    // `@NovaUiShared` is the compiled `nova-reader-ui-shared` library, so the
    // window and the page crates share its globals.
    slint_build::compile_with_config("app.slint", compiler_configuration())
        .expect("compile Nova Reader UI");
}
