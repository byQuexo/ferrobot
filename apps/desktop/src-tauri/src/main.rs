//! Desktop shell: starts the bundled daemon unless one already answers, then hosts the web UI and
//! hands it the operator token (`FERROBOT_API_TOKEN`, `FERROBOT_API_TOKEN_FILE`, `<data dir>/api-token`).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use tauri::Manager;
use tauri::webview::DownloadEvent;

/// Must match `DEFAULT_DAEMON` in `src/api.ts` and the CSP in `tauri.conf.json`.
const DAEMON: &str = "127.0.0.1:7878";
const RUNNER_IMAGE: &str = concat!(
    "ghcr.io/byquexo/ferrobot-runner:",
    env!("CARGO_PKG_VERSION")
);

/// The daemon this shell started. A resource, so it also stops when the Windows updater exits
/// the app without the usual exit events.
struct OwnDaemon(Child);

impl tauri::Resource for OwnDaemon {}

impl Drop for OwnDaemon {
    fn drop(&mut self) {
        stop_daemon(&mut self.0);
    }
}

fn token(data_dir: &Path) -> Option<String> {
    let token = std::env::var("FERROBOT_API_TOKEN").ok().or_else(|| {
        let path = std::env::var_os("FERROBOT_API_TOKEN_FILE")
            .map_or_else(|| data_dir.join("api-token"), PathBuf::from);
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

/// The body of `GET /v1/health`, or `None` when no daemon answers.
fn health() -> Option<String> {
    let addr: SocketAddr = DAEMON.parse().ok()?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream
        .write_all(b"GET /v1/health HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let (head, body) = response.split_once("\r\n\r\n")?;
    (head.split_whitespace().nth(1) == Some("200")).then(|| body.to_owned())
}

/// `ferrobotd` next to this executable: present in release bundles, absent in dev builds.
fn bundled_daemon() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "ferrobotd.exe"
    } else {
        "ferrobotd"
    };
    let path = std::env::current_exe().ok()?.parent()?.join(name);
    path.is_file().then_some(path)
}

fn start_daemon(bin: &Path, data_dir: &Path) -> std::io::Result<Child> {
    std::fs::create_dir_all(data_dir)?;
    let log = std::fs::File::create(data_dir.join("daemon.log"))?;
    let mut daemon = Command::new(bin);
    // Without this a console window opens next to the app, and closing it kills the daemon.
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut daemon, 0x0800_0000);
    daemon
        .args([
            "--backend",
            "docker",
            "--driver",
            "claude",
            "--listen",
            DAEMON,
        ])
        // Containers reach the agent API through host.docker.internal; it checks agent tokens.
        .args([
            "--agent-listen",
            "0.0.0.0:7879",
            "--runner-image",
            RUNNER_IMAGE,
        ])
        .arg("--data-dir")
        .arg(data_dir)
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
}

fn wait_for_daemon(child: &mut Child) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(body) = health() {
            return Ok(body);
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("the daemon exited ({status}); see daemon.log"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err("the daemon did not answer within 20 s; see daemon.log".into())
}

fn stop_daemon(child: &mut Child) {
    // SIGTERM lets the daemon shut down cleanly; Windows has no signal to send.
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
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
    let mut opener = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        // explorer.exe opens File Explorer instead of the browser for long query URLs (OAuth).
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        Command::new("xdg-open")
    };
    match opener.arg(url.as_str()).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("cannot open {url}: {e}"),
    }
}

/// Files the page builds itself (blob URLs, e.g. the usage CSV) go to the Downloads folder.
fn download(webview: &tauri::Webview, event: DownloadEvent<'_>) -> bool {
    let DownloadEvent::Requested { url, destination } = event else {
        return true;
    };
    let (Some(name), Ok(dir)) = (
        destination.file_name().map(ToOwned::to_owned),
        webview.path().download_dir(),
    ) else {
        return false;
    };
    *destination = dir.join(name);
    url.scheme() == "blob"
}

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            // Only release builds configure it (`tauri.release.conf.json`); without a config it fails.
            let updater = app.config().plugins.0.contains_key("updater");
            if updater {
                app.handle()
                    .plugin(tauri_plugin_updater::Builder::new().build())?;
            }
            let bundled = bundled_daemon();
            let data_dir = if bundled.is_some() {
                app.path().app_data_dir()?
            } else {
                PathBuf::from(
                    std::env::var_os("FERROBOT_DATA_DIR").unwrap_or_else(|| ".ferrobot".into()),
                )
            };
            let mut status = health();
            let own_daemon = status.is_none() && bundled.is_some();
            if let (None, Some(bin)) = (&status, &bundled) {
                let mut child = start_daemon(bin, &data_dir)?;
                match wait_for_daemon(&mut child) {
                    Ok(body) => status = Some(body),
                    Err(e) => eprintln!("{e}"),
                }
                app.resources_table().add(OwnDaemon(child));
            }
            let mut config = app
                .config()
                .app
                .windows
                .first()
                .cloned()
                .ok_or("no window configured")?;
            // Compact JSON from the daemon, so a substring check is enough here.
            if status.is_some_and(|body| body.contains("\"sandbox_ready\":false")) {
                config.url = tauri::WebviewUrl::App("docker.html".into());
            }
            let token = token(&data_dir)
                .map(|t| format!("window.__FERROBOT_TOKEN__ = \"{t}\";"))
                .unwrap_or_default();
            // Some webviews run this in every frame: only the app's own top window gets the token.
            let script = format!(
                "if (window === window.top && (location.protocol === \"tauri:\" \
                 || location.hostname === \"tauri.localhost\" \
                 || (location.hostname === \"localhost\" && location.port === \"1420\"))) {{ \
                 window.__FERROBOT_SHELL__ = {{ updater: {updater}, ownDaemon: {own_daemon} }};{token} }}"
            );
            tauri::WebviewWindowBuilder::from_config(app.handle(), &config)?
                .initialization_script(&script)
                // A frame can navigate without a click; links the operator opens go through
                // `on_new_window` (target=_blank, window.open), so nothing is opened from here.
                .on_navigation(is_app)
                .on_new_window(|url, _| {
                    open_external(&url);
                    tauri::webview::NewWindowResponse::Deny
                })
                .on_download(|webview, event| download(&webview, event))
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!());
    if let Err(e) = app {
        eprintln!("ferrobot desktop failed to start: {e}");
        std::process::exit(1);
    }
}
