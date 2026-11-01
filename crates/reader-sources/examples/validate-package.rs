//! Inspect packages through the production Reader parser without installing them.
fn main() {
    let path = std::env::args_os()
        .nth(1)
        .expect("usage: validate-package ZIP");
    let bytes = std::fs::read(path).expect("read package");
    match reader_sources::SourceRegistry::inspect_archive(&bytes) {
        Ok((source, sha256)) => println!("{} {} {}", source.id, source.version, sha256),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
