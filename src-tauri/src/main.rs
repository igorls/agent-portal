// Always a GUI process — never flash a console for eprintln! progress.
// Use `pnpm dev` / Visual Studio / RUST_LOG file sinks when debugging.
#![windows_subsystem = "windows"]

fn main() {
    portal_tauri_lib::run()
}
