//! Desktop shell. All logic lives in the daemon; this window only hosts the
//! web UI, which talks to `ferrobotd` over HTTP and WebSocket.
//!
//! The one thing the shell does is hand the UI the daemon's operator token,
//! so the user doesn't have to paste it: `FERROBOT_API_TOKEN`, else the file
//! `FERROBOT_API_TOKEN_FILE`, else `$FERROBOT_DATA_DIR/api-token` (default
//! `.ferrobot/api-token`, like the daemon). Without one, the UI asks for it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

fn token() -> Option<String> {
    let token = std::env::var("FERROBOT_API_TOKEN").ok().or_else(|| {
        let path = std::env::var_os("FERROBOT_API_TOKEN_FILE").map_or_else(
            || {
                let dir =
                    std::env::var_os("FERROBOT_DATA_DIR").unwrap_or_else(|| ".ferrobot".into());
                PathBuf::from(dir).join("api-token")
            },
            PathBuf::from,
        );
        std::fs::read_to_string(path).ok()
    })?;
    let token = token.trim().to_owned();
    // Same alphabet the daemon accepts; also keeps the script below well-formed.
    let ok = !token.is_empty()
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b));
    ok.then_some(token)
}

/// The app's own pages (the bundled UI or the Vite dev server), and pages
/// agents shared, which the viewer frames from the daemon. wry asks this for
/// frames too; the token script only runs in the main frame.
fn is_app(url: &tauri::Url) -> bool {
    url.scheme() == "tauri"
        || matches!(url.host_str(), Some("tauri.localhost" | "localhost"))
        || (url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.path().starts_with("/artifacts/"))
}

/// Links open in the system browser, never in this window (which holds the
/// operator token).
fn open_external(url: &tauri::Url) {
    if !matches!(url.scheme(), "http" | "https") {
        return;
    }
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    match std::process::Command::new(program)
        .arg(url.as_str())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("cannot open {url}: {e}"),
    }
}

fn main() {
    let script = token()
        .map(|t| format!("window.__FERROBOT_TOKEN__ = \"{t}\";"))
        .unwrap_or_default();
    let result = tauri::Builder::default()
        .setup(move |app| {
            let config = app
                .config()
                .app
                .windows
                .first()
                .cloned()
                .ok_or("no window configured")?;
            tauri::WebviewWindowBuilder::from_config(app.handle(), &config)?
                .initialization_script(&script)
                .on_navigation(|url| {
                    if is_app(url) {
                        return true;
                    }
                    open_external(url);
                    false
                })
                .on_new_window(|url, _| {
                    open_external(&url);
                    tauri::webview::NewWindowResponse::Deny
                })
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!());
    if let Err(e) = result {
        eprintln!("ferrobot desktop failed to start: {e}");
        std::process::exit(1);
    }
}
