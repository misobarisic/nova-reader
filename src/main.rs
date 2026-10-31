fn main() {
    if let Err(error) = nova_reader::main_entry() {
        eprintln!("Nova Reader: {error}");
        std::process::exit(1);
    }
}
