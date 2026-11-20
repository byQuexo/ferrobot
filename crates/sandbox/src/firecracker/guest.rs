//! Host side of the guest agent: exec and files via `CONNECT <port>` on the VM's vsock socket.
//! In the guest, socat serves `dist/guest-agent.js`: one JSON request and answer per connection.

use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::{ExecOutput, ExecRequest, SandboxError, capture};

/// Guest vsock port the agent listens on (`scripts/firecracker/ferrobot-init`).
pub const GUEST_PORT: u32 = 52;
/// Commands may run long; file calls finish well within it.
const TIMEOUT: Duration = Duration::from_secs(600);
/// Largest answer accepted (base64): ~48 MiB files; exec output is capped in the guest.
const MAX_ANSWER: u64 = 64 * 1024 * 1024;

fn bad(what: impl std::fmt::Display) -> SandboxError {
    SandboxError::Other(format!("guest agent: {what}"))
}

/// Sends one request and returns the agent's answer. `name` labels errors.
async fn call(uds: &Path, name: &str, request: &Value) -> Result<Value, SandboxError> {
    let exchange = async {
        let stream = UnixStream::connect(uds).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                SandboxError::NotFound(name.to_owned())
            }
            _ => e.into(),
        })?;
        let mut stream = BufReader::new(stream);
        stream
            .get_mut()
            .write_all(format!("CONNECT {GUEST_PORT}\n").as_bytes())
            .await?;
        let mut ack = String::new();
        stream.read_line(&mut ack).await?;
        if !ack.starts_with("OK ") {
            return Err(bad(format_args!("vsock refused: {}", ack.trim())));
        }
        let mut line = serde_json::to_vec(request).map_err(bad)?;
        line.push(b'\n');
        stream.get_mut().write_all(&line).await?;
        stream.get_mut().shutdown().await?;
        let mut answer = Vec::new();
        stream.take(MAX_ANSWER).read_to_end(&mut answer).await?;
        serde_json::from_slice::<Value>(&answer).map_err(bad)
    };
    let answer = tokio::time::timeout(TIMEOUT, exchange)
        .await
        .map_err(|_| bad("no answer in time"))??;
    match answer.get("error").and_then(Value::as_str) {
        None => Ok(answer),
        Some("not_found") => Err(SandboxError::NotFound(
            request["path"].as_str().unwrap_or(name).to_owned(),
        )),
        Some(other) => Err(bad(other)),
    }
}

fn decode(answer: &Value, field: &str) -> Result<Vec<u8>, SandboxError> {
    B64.decode(answer[field].as_str().unwrap_or_default())
        .map_err(bad)
}

pub(super) async fn exec(
    uds: &Path,
    name: &str,
    req: &ExecRequest,
) -> Result<ExecOutput, SandboxError> {
    let request = json!({"op": "exec", "cmd": req.cmd, "cwd": req.workdir, "env": req.env});
    let answer = call(uds, name, &request).await?;
    let (stdout, t1) = capture(&decode(&answer, "stdout")?);
    let (stderr, t2) = capture(&decode(&answer, "stderr")?);
    Ok(ExecOutput {
        exit_code: answer["exit_code"].as_i64().unwrap_or(-1),
        stdout,
        stderr,
        truncated: t1 || t2,
    })
}

pub(super) async fn read_file(uds: &Path, name: &str, path: &str) -> Result<Vec<u8>, SandboxError> {
    let answer = call(uds, name, &json!({"op": "read", "path": path})).await?;
    decode(&answer, "data")
}

pub(super) async fn write_file(
    uds: &Path,
    name: &str,
    path: &str,
    contents: &[u8],
) -> Result<(), SandboxError> {
    let request = json!({"op": "write", "path": path, "data": B64.encode(contents)});
    call(uds, name, &request).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::net::UnixListener;

    use super::*;

    /// A fake Firecracker vsock socket with a scripted guest agent behind it.
    fn fake_guest(dir: &Path, accept_port: u32) -> (std::path::PathBuf, Arc<Mutex<Vec<Value>>>) {
        let uds = dir.join("vsock.sock");
        let listener = UnixListener::bind(&uds).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let log = log.clone();
                tokio::spawn(async move {
                    let mut stream = BufReader::new(stream);
                    let mut connect = String::new();
                    stream.read_line(&mut connect).await.unwrap();
                    if connect.trim() != format!("CONNECT {accept_port}") {
                        stream.get_mut().write_all(b"FAILURE\n").await.unwrap();
                        return;
                    }
                    stream
                        .get_mut()
                        .write_all(b"OK 1073741824\n")
                        .await
                        .unwrap();
                    let mut line = String::new();
                    stream.read_line(&mut line).await.unwrap();
                    let req: Value = serde_json::from_str(&line).unwrap();
                    log.lock().unwrap().push(req.clone());
                    let answer = match req["op"].as_str() {
                        Some("exec") => json!({"exit_code": 3,
                            "stdout": B64.encode("hi\n"), "stderr": B64.encode("warn")}),
                        Some("read") if req["path"] == "/home/node/a.txt" => {
                            json!({"data": B64.encode([0u8, 159, 255])})
                        }
                        Some("read") => json!({"error": "not_found"}),
                        Some("write") => json!({"ok": true}),
                        _ => json!({"error": "unknown op"}),
                    };
                    let mut out = serde_json::to_vec(&answer).unwrap();
                    out.push(b'\n');
                    stream.get_mut().write_all(&out).await.unwrap();
                });
            }
        });
        (uds, seen)
    }

    #[tokio::test]
    async fn exec_and_files_go_through_the_vsock_handshake() {
        let tmp = tempfile::tempdir().unwrap();
        let (uds, seen) = fake_guest(tmp.path(), GUEST_PORT);
        let mut req = ExecRequest::new(["sh", "-c", "echo hi"]);
        req.workdir = Some("/home/node".into());
        let out = exec(&uds, "alice", &req).await.unwrap();
        assert_eq!(
            (
                out.exit_code,
                out.stdout.as_str(),
                out.stderr.as_str(),
                out.truncated
            ),
            (3, "hi\n", "warn", false)
        );
        assert_eq!(
            read_file(&uds, "alice", "/home/node/a.txt").await.unwrap(),
            [0, 159, 255],
            "binary content survives"
        );
        assert!(matches!(
            read_file(&uds, "alice", "/home/node/missing").await,
            Err(SandboxError::NotFound(p)) if p == "/home/node/missing"
        ));
        write_file(&uds, "alice", "/home/node/b.txt", b"\x00bytes")
            .await
            .unwrap();
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen[0]["cmd"], json!(["sh", "-c", "echo hi"]));
        assert_eq!(seen[0]["cwd"], "/home/node");
        assert_eq!(seen[3]["data"], B64.encode(b"\x00bytes"));
    }

    #[tokio::test]
    async fn a_refused_port_or_a_missing_socket_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let (uds, _) = fake_guest(tmp.path(), GUEST_PORT + 1);
        let err = read_file(&uds, "alice", "/x").await.unwrap_err();
        assert!(err.to_string().contains("vsock refused"), "{err}");
        let gone = tmp.path().join("nope.sock");
        assert!(matches!(
            read_file(&gone, "alice", "/x").await,
            Err(SandboxError::NotFound(n)) if n == "alice"
        ));
    }
}
