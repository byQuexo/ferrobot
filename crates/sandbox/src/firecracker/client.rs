//! Thin HTTP/1.1 client for Firecracker's API socket. Paths and bodies follow
//! the official swagger (`src/firecracker/swagger/firecracker.yaml`).

use std::path::{Path, PathBuf};

#[cfg(not(unix))]
use super::NoUnixSockets as UnixStream;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Method;
use hyper::header::{ACCEPT, CONTENT_TYPE, HOST};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use serde_json::{Value, json};
#[cfg(unix)]
use tokio::net::UnixStream;

use crate::SandboxError;

/// One API call. The body can hold the runner env, so `Debug` omits it.
#[derive(Clone, PartialEq)]
pub(crate) struct Request {
    pub method: Method,
    pub path: String,
    pub body: Option<Value>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.method, self.path)
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) enum Action {
    InstanceStart,
    /// x86 only: the guest init sees it as SIGINT and shuts down.
    SendCtrlAltDel,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) enum VmState {
    Paused,
    Resumed,
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl Request {
    fn new(method: Method, path: impl Into<String>, body: Value) -> Self {
        Self {
            method,
            path: path.into(),
            body: Some(body),
        }
    }

    pub fn describe_instance() -> Self {
        Self {
            method: Method::GET,
            path: "/".into(),
            body: None,
        }
    }

    pub fn machine_config(vcpu_count: u8, mem_size_mib: u32) -> Self {
        Self::new(
            Method::PUT,
            "/machine-config",
            json!({ "vcpu_count": vcpu_count, "mem_size_mib": mem_size_mib }),
        )
    }

    pub fn boot_source(kernel_image_path: &Path, boot_args: &str) -> Self {
        Self::new(
            Method::PUT,
            "/boot-source",
            json!({ "kernel_image_path": path_str(kernel_image_path), "boot_args": boot_args }),
        )
    }

    pub fn drive(
        drive_id: &str,
        path_on_host: &Path,
        is_root_device: bool,
        read_only: bool,
    ) -> Self {
        Self::new(
            Method::PUT,
            format!("/drives/{drive_id}"),
            json!({
                "drive_id": drive_id,
                "path_on_host": path_str(path_on_host),
                "is_root_device": is_root_device,
                "is_read_only": read_only,
            }),
        )
    }

    pub fn network_interface(iface_id: &str, host_dev_name: &str, guest_mac: &str) -> Self {
        Self::new(
            Method::PUT,
            format!("/network-interfaces/{iface_id}"),
            json!({ "iface_id": iface_id, "host_dev_name": host_dev_name, "guest_mac": guest_mac }),
        )
    }

    pub fn mmds_config(network_interfaces: &[&str]) -> Self {
        Self::new(
            Method::PUT,
            "/mmds/config",
            json!({ "version": "V2", "network_interfaces": network_interfaces }),
        )
    }

    pub fn mmds(contents: Value) -> Self {
        Self::new(Method::PUT, "/mmds", contents)
    }

    pub fn vsock(guest_cid: u32, uds_path: &Path) -> Self {
        Self::new(
            Method::PUT,
            "/vsock",
            json!({ "guest_cid": guest_cid, "uds_path": path_str(uds_path) }),
        )
    }

    pub fn action(action_type: Action) -> Self {
        Self::new(
            Method::PUT,
            "/actions",
            json!({ "action_type": action_type }),
        )
    }

    pub fn vm(state: VmState) -> Self {
        Self::new(Method::PATCH, "/vm", json!({ "state": state }))
    }

    #[allow(dead_code, reason = "for cold sleep")]
    pub fn snapshot_create(snapshot_path: &Path, mem_file_path: &Path) -> Self {
        Self::new(
            Method::PUT,
            "/snapshot/create",
            json!({
                "snapshot_type": "Full",
                "snapshot_path": path_str(snapshot_path),
                "mem_file_path": path_str(mem_file_path),
            }),
        )
    }

    #[allow(dead_code, reason = "for cold sleep")]
    pub fn snapshot_load(snapshot_path: &Path, mem_file_path: &Path, resume_vm: bool) -> Self {
        Self::new(
            Method::PUT,
            "/snapshot/load",
            json!({
                "snapshot_path": path_str(snapshot_path),
                "mem_backend": { "backend_type": "File", "backend_path": path_str(mem_file_path) },
                "resume_vm": resume_vm,
            }),
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Client {
    socket: PathBuf,
}

fn http_error(e: impl std::fmt::Display) -> SandboxError {
    SandboxError::Other(format!("firecracker API: {e}"))
}

impl Client {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// Sends `req` on a fresh connection and returns the response body.
    /// Connecting fails with [`SandboxError::Io`] (no socket, or no process behind it).
    pub async fn send(&self, req: &Request) -> Result<Bytes, SandboxError> {
        let stream = UnixStream::connect(&self.socket).await?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(http_error)?;
        tokio::spawn(conn);
        let mut builder = hyper::Request::builder()
            .method(req.method.clone())
            .uri(req.path.as_str())
            .header(HOST, "localhost")
            .header(ACCEPT, "application/json");
        let body = match &req.body {
            Some(body) => {
                builder = builder.header(CONTENT_TYPE, "application/json");
                Bytes::from(body.to_string())
            }
            None => Bytes::new(),
        };
        let request = builder.body(Full::new(body)).map_err(http_error)?;
        let response = sender.send_request(request).await.map_err(http_error)?;
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(http_error)?
            .to_bytes();
        if status.is_success() {
            return Ok(bytes);
        }
        let fault_message = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.get("fault_message")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
        Err(SandboxError::Firecracker {
            status: status.as_u16(),
            fault_message,
        })
    }

    /// `GET /`: `Not started`, `Running` or `Paused`.
    pub async fn instance_state(&self) -> Result<String, SandboxError> {
        let body = self.send(&Request::describe_instance()).await?;
        let info: Value = serde_json::from_slice(&body).map_err(http_error)?;
        info.get("state")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| http_error("instance info without a state"))
    }
}

/// A fake Firecracker API on a Unix socket that records every request and
/// gives the same canned answer to all of them.
#[cfg(test)]
pub(crate) mod fake {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::body::Incoming;
    use hyper::header::CONTENT_TYPE;
    use hyper_util::rt::TokioIo;
    use serde_json::Value;

    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Recorded {
        pub method: String,
        pub path: String,
        pub content_type: Option<String>,
        pub body: Option<Value>,
    }

    pub(crate) type Log = Arc<Mutex<Vec<Recorded>>>;

    pub(crate) fn serve(socket: &Path, status: u16, answer: &'static str) -> Log {
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        let log = Log::default();
        let recorded = log.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let log = recorded.clone();
                let service = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
                    let log = log.clone();
                    async move {
                        let method = req.method().to_string();
                        let path = req.uri().path().to_owned();
                        let content_type = req
                            .headers()
                            .get(CONTENT_TYPE)
                            .map(|v| v.to_str().unwrap().to_owned());
                        let bytes = req.into_body().collect().await?.to_bytes();
                        let body =
                            (!bytes.is_empty()).then(|| serde_json::from_slice(&bytes).unwrap());
                        log.lock().unwrap().push(Recorded {
                            method,
                            path,
                            content_type,
                            body,
                        });
                        let response = hyper::Response::builder()
                            .status(status)
                            .body(Full::new(Bytes::from_static(answer.as_bytes())))
                            .unwrap();
                        Ok::<_, hyper::Error>(response)
                    }
                });
                tokio::spawn(
                    hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service),
                );
            }
        });
        log
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Recorded, serve};
    use super::*;

    fn recorded(method: &str, path: &str, body: Option<Value>) -> Recorded {
        Recorded {
            method: method.into(),
            path: path.into(),
            content_type: body.as_ref().map(|_| "application/json".into()),
            body,
        }
    }

    fn put(path: &str, body: Value) -> Recorded {
        recorded("PUT", path, Some(body))
    }

    #[tokio::test]
    async fn every_request_goes_out_as_the_swagger_describes() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("api.sock");
        let log = serve(&socket, 204, "");
        let p = Path::new;
        let cases = [
            (Request::describe_instance(), recorded("GET", "/", None)),
            (
                Request::machine_config(2, 2048),
                put(
                    "/machine-config",
                    json!({"vcpu_count": 2, "mem_size_mib": 2048}),
                ),
            ),
            (
                Request::boot_source(p("/k/vmlinux"), "console=ttyS0"),
                put(
                    "/boot-source",
                    json!({"kernel_image_path": "/k/vmlinux", "boot_args": "console=ttyS0"}),
                ),
            ),
            (
                Request::drive("rootfs", p("/r.ext4"), true, true),
                put(
                    "/drives/rootfs",
                    json!({"drive_id": "rootfs", "path_on_host": "/r.ext4",
                    "is_root_device": true, "is_read_only": true}),
                ),
            ),
            (
                Request::network_interface("eth0", "fctap1", "06:00:AC:10:00:02"),
                put(
                    "/network-interfaces/eth0",
                    json!({"iface_id": "eth0",
                    "host_dev_name": "fctap1", "guest_mac": "06:00:AC:10:00:02"}),
                ),
            ),
            (
                Request::mmds_config(&["eth0"]),
                put(
                    "/mmds/config",
                    json!({"version": "V2", "network_interfaces": ["eth0"]}),
                ),
            ),
            (
                Request::mmds(json!({"a": {"b": "c"}})),
                put("/mmds", json!({"a": {"b": "c"}})),
            ),
            (
                Request::vsock(3, p("/v.sock")),
                put("/vsock", json!({"guest_cid": 3, "uds_path": "/v.sock"})),
            ),
            (
                Request::action(Action::InstanceStart),
                put("/actions", json!({"action_type": "InstanceStart"})),
            ),
            (
                Request::action(Action::SendCtrlAltDel),
                put("/actions", json!({"action_type": "SendCtrlAltDel"})),
            ),
            (
                Request::vm(VmState::Paused),
                recorded("PATCH", "/vm", Some(json!({"state": "Paused"}))),
            ),
            (
                Request::vm(VmState::Resumed),
                recorded("PATCH", "/vm", Some(json!({"state": "Resumed"}))),
            ),
            (
                Request::snapshot_create(p("/s/state"), p("/s/mem")),
                put(
                    "/snapshot/create",
                    json!({"snapshot_type": "Full",
                    "snapshot_path": "/s/state", "mem_file_path": "/s/mem"}),
                ),
            ),
            (
                Request::snapshot_load(p("/s/state"), p("/s/mem"), true),
                put(
                    "/snapshot/load",
                    json!({"snapshot_path": "/s/state",
                    "mem_backend": {"backend_type": "File", "backend_path": "/s/mem"},
                    "resume_vm": true}),
                ),
            ),
        ];
        let client = Client::new(&socket);
        for (request, _) in &cases {
            client.send(request).await.unwrap();
        }
        let expected: Vec<_> = cases.into_iter().map(|(_, r)| r).collect();
        assert_eq!(*log.lock().unwrap(), expected);
    }

    #[tokio::test]
    async fn errors_carry_the_fault_message_or_the_raw_body() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            (
                400,
                r#"{"fault_message":"Invalid kernel path"}"#,
                "Invalid kernel path",
            ),
            (500, "boom", "boom"),
        ];
        for (status, answer, expected) in cases {
            let socket = dir.path().join(format!("{status}.sock"));
            serve(&socket, status, answer);
            let err = Client::new(&socket)
                .send(&Request::vm(VmState::Paused))
                .await
                .unwrap_err();
            let SandboxError::Firecracker {
                status: got,
                fault_message,
            } = &err
            else {
                panic!("{err}");
            };
            assert_eq!((*got, fault_message.as_str()), (status, expected));
            assert_eq!(
                err.to_string(),
                format!("firecracker API {status}: {expected}")
            );
        }
    }

    #[tokio::test]
    async fn reads_the_instance_state() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("api.sock");
        serve(
            &socket,
            200,
            r#"{"app_name":"Firecracker","id":"x","state":"Paused","vmm_version":"1.0.0"}"#,
        );
        let state = Client::new(&socket).instance_state().await.unwrap();
        assert_eq!(state, "Paused");
    }

    #[tokio::test]
    async fn a_missing_socket_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = Client::new(dir.path().join("api.sock"))
            .instance_state()
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::Io(_)), "{err}");
    }

    #[test]
    fn debug_hides_the_body() {
        let req = Request::mmds(json!({"env": {"TOKEN": "secret"}}));
        assert_eq!(format!("{req:?}"), "PUT /mmds");
    }
}
