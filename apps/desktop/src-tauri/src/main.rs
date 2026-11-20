//! Desktop shell: hosts the web UI and hands it the operator token from `FERROBOT_API_TOKEN`,
//! else the file `FERROBOT_API_TOKEN_FILE`, else `$FERROBOT_DATA_DIR/api-token` (`.ferrobot`).
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

/// The app's own pages (bundled UI or Vite dev server) and agent-shared pages the viewer frames
/// from the daemon; wry asks this for frames too, but the token script only runs in the main frame.
fn is_app(url: &tauri::Url) -> bool {
    url.scheme() == "tauri"
        || url.host_str() == Some("tauri.localhost")
        // Only the Vite dev server (vite.config.ts): any other local server would get the token.
        || (url.scheme() == "http" && url.host_str() == Some("localhost") && url.port() == Some(1420))
        || (url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.path().starts_with("/artifacts/"))
}

/// Opens http(s) links in the system browser, never in this window (it holds the operator token).
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
