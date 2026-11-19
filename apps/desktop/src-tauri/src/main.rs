//! Desktop shell. All logic lives in the daemon; this window only hosts the
//! web UI, which talks to `ferrobotd` over HTTP and WebSocket.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(e) = tauri::Builder::default().run(tauri::generate_context!()) {
        eprintln!("ferrobot desktop failed to start: {e}");
        std::process::exit(1);
    }
}
