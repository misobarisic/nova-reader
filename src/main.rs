// Launch the Windows desktop app without allocating a console window.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    if let Err(error) = nova_reader::main_entry() {
        eprintln!("Nova Reader: {error}");
        std::process::exit(1);
    }
}
