//! Firecracker backend: one microVM per agent, driven over its API socket.
//!
//! Each VM has a directory `<state_dir>/<name>/` with the API socket, a
//! sparse home disk (formatted by the guest init on first boot), the
//! Firecracker log and `index`, the VM's slot on the host bridge (guest IP,
//! MAC, TAP name). All guests share one gateway, the bridge's `host_ip`,
//! which is also where they reach the daemon. The runner env goes in through
//! MMDS, never the kernel command line.
//!
//! Needs Linux with KVM and root for the TAP devices; see
//! `scripts/firecracker/README.md`. `exec`, files and snapshots wait for the
//! guest agent (roadmap F2) and snapshot sleep (F4b).

mod client;
mod guest;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use self::client::{Action, Client, Request, VmState};
use crate::{
    ExecOutput, ExecRequest, SandboxBackend, SandboxError, SandboxHandle, SandboxPath, SandboxSpec,
    SandboxState, SleepLevel, SnapshotRef,
};

const KIND: &str = "firecracker";
/// The VM's vsock Unix socket, through which the guest agent is reached.
const VSOCK: &str = "vsock.sock";
/// Guest context id; each VM has its own vsock device, so it can be fixed.
const GUEST_CID: u32 = 3;
const IFACE: &str = "eth0";
const SOCKET: &str = "api.sock";
const HOME_DISK: &str = "home.ext4";
const INDEX: &str = "index";
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Configuration for [`FirecrackerBackend`].
#[derive(Debug, Clone)]
pub struct FirecrackerConfig {
    pub firecracker_bin: PathBuf,
    /// Uncompressed guest kernel (`vmlinux`).
    pub kernel: PathBuf,
    /// Base root filesystem (ext4), shared read-only by all VMs.
    pub rootfs: PathBuf,
    /// Holds one directory per VM.
    pub state_dir: PathBuf,
    pub vcpus: u8,
    pub mem_mib: u32,
    /// Size of each VM's sparse home disk (`/home/node`).
    pub home_disk_mib: u64,
    /// The host's address on the bridge: every guest's gateway.
    pub host_ip: Ipv4Addr,
    /// Prefix length of the bridge network; guests get the addresses after `host_ip`.
    pub prefix_len: u8,
    /// Host bridge the TAP devices join, set up once by the operator.
    pub bridge: String,
    /// TAP device name prefix; the VM's index is appended (15 chars at most).
    pub tap_prefix: String,
    pub kvm_device: PathBuf,
}

impl Default for FirecrackerConfig {
    fn default() -> Self {
        Self {
            firecracker_bin: "firecracker".into(),
            kernel: PathBuf::new(),
            rootfs: PathBuf::new(),
            state_dir: ".ferrobot/vms".into(),
            vcpus: 2,
            mem_mib: 2048,
            home_disk_mib: 10 * 1024,
            host_ip: Ipv4Addr::new(172, 16, 0, 1),
            prefix_len: 24,
            bridge: "fcbr0".into(),
            tap_prefix: "fctap".into(),
            kvm_device: "/dev/kvm".into(),
        }
    }
}

/// One VM's place on the host and on the bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Vm {
    dir: PathBuf,
    guest_ip: Ipv4Addr,
    tap: String,
}

impl Vm {
    /// Firecracker's convention: `06:00` followed by the guest IP.
    fn mac(&self) -> String {
        let [a, b, c, d] = self.guest_ip.octets();
        format!("06:00:{a:02X}:{b:02X}:{c:02X}:{d:02X}")
    }
}

impl FirecrackerConfig {
    fn netmask(&self) -> u32 {
        u32::MAX
            .checked_shl(32 - u32::from(self.prefix_len.min(32)))
            .unwrap_or(0)
    }

    fn vm(&self, name: &str, index: u32) -> Result<Vm, SandboxError> {
        let mask = self.netmask();
        let host = u32::from(self.host_ip);
        let broadcast = (host & mask) | !mask;
        let guest = host
            .checked_add(index)
            .filter(|ip| index > 0 && *ip < broadcast)
            .ok_or_else(|| {
                SandboxError::Other(format!(
                    "no guest address {index} after {}/{}",
                    self.host_ip, self.prefix_len
                ))
            })?;
        let tap = format!("{}{index}", self.tap_prefix);
        if tap.len() > 15 {
            return Err(SandboxError::Other(format!(
                "TAP name {tap} is longer than 15 characters"
            )));
        }
        Ok(Vm {
            dir: self.state_dir.join(name),
            guest_ip: Ipv4Addr::from(guest),
            tap,
        })
    }

    /// The API calls that configure and start `vm`, in order.
    fn boot_sequence(&self, vm: &Vm, env: &BTreeMap<String, String>) -> Vec<Request> {
        let boot_args = format!(
            "console=ttyS0 reboot=k panic=1 init=/sbin/ferrobot-init ip={}::{}:{}::{IFACE}:off",
            vm.guest_ip,
            self.host_ip,
            Ipv4Addr::from(self.netmask()),
        );
        vec![
            Request::machine_config(self.vcpus, self.mem_mib),
            Request::boot_source(&self.kernel, &boot_args),
            Request::drive("rootfs", &self.rootfs, true, true),
            Request::drive("home", &vm.dir.join(HOME_DISK), false, false),
            Request::network_interface(IFACE, &vm.tap, &vm.mac()),
            Request::mmds_config(&[IFACE]),
            Request::mmds(json!({ "ferrobot": { "env": env } })),
            Request::vsock(GUEST_CID, &vm.dir.join(VSOCK)),
            Request::action(Action::InstanceStart),
        ]
    }
}

fn read_index(dir: &Path) -> Option<u32> {
    std::fs::read_to_string(dir.join(INDEX))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Creates the sparse home disk unless it exists; the guest formats it.
fn create_home_disk(path: &Path, mib: u64) -> Result<(), SandboxError> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => Ok(file.set_len(mib * 1024 * 1024)?),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

async fn ip(args: &[&str]) -> Result<(), SandboxError> {
    let out = Command::new("ip")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await?;
    if out.status.success() {
        Ok(())
    } else {
        Err(SandboxError::Other(format!(
            "ip {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

fn tap_exists(tap: &str) -> bool {
    Path::new("/sys/class/net").join(tap).exists()
}

// TODO(kvm): untested; needs root (CAP_NET_ADMIN) and the bridge from the README.
async fn ensure_tap(tap: &str, bridge: &str) -> Result<(), SandboxError> {
    if tap_exists(tap) {
        return Ok(());
    }
    ip(&["tuntap", "add", "dev", tap, "mode", "tap"]).await?;
    ip(&["link", "set", "dev", tap, "master", bridge]).await?;
    // Guests may reach the host, not each other.
    ip(&[
        "link",
        "set",
        "dev",
        tap,
        "type",
        "bridge_slave",
        "isolated",
        "on",
    ])
    .await?;
    ip(&["link", "set", "dev", tap, "up"]).await
}

/// Runner env per VM, kept to boot it again; in memory only (it holds the agent token).
#[derive(Default)]
struct Slot {
    env: BTreeMap<String, String>,
    child: Option<Child>,
}

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slot")
            .field("child", &self.child)
            .finish_non_exhaustive()
    }
}

/// One Firecracker microVM per agent.
#[derive(Debug)]
pub struct FirecrackerBackend {
    config: FirecrackerConfig,
    slots: Mutex<HashMap<String, Slot>>,
}

impl FirecrackerBackend {
    pub fn new(config: FirecrackerConfig) -> Self {
        Self {
            config,
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Fails unless this host can run Firecracker (read/write access to KVM).
    pub fn check_kvm(&self) -> Result<(), SandboxError> {
        let kvm = &self.config.kvm_device;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(kvm)
            .map(drop)
            .map_err(|e| {
                SandboxError::Other(format!(
                    "the firecracker backend needs read/write access to {} \
                     (Linux with KVM): {e}",
                    kvm.display()
                ))
            })
    }

    fn name(handle: &SandboxHandle) -> Result<&str, SandboxError> {
        if handle.backend != KIND {
            return Err(SandboxError::WrongBackend {
                expected: KIND,
                actual: handle.backend.clone(),
            });
        }
        // Names become directory names; refuse anything that could escape `state_dir`.
        if handle.id.is_empty()
            || !handle
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(SandboxError::NotFound(handle.id.clone()));
        }
        Ok(&handle.id)
    }

    fn vsock(&self, name: &str) -> PathBuf {
        self.config.state_dir.join(name).join(VSOCK)
    }

    fn client(&self, name: &str) -> Client {
        Client::new(self.config.state_dir.join(name).join(SOCKET))
    }

    /// The VM's index from its directory, else the lowest free one, recorded there.
    fn index(&self, name: &str) -> Result<u32, SandboxError> {
        let dir = self.config.state_dir.join(name);
        if let Some(index) = read_index(&dir) {
            return Ok(index);
        }
        std::fs::create_dir_all(&dir)?;
        let used: HashSet<u32> = std::fs::read_dir(&self.config.state_dir)?
            .filter_map(|entry| read_index(&entry.ok()?.path()))
            .collect();
        let mut index = 1;
        while used.contains(&index) {
            index += 1;
        }
        std::fs::write(dir.join(INDEX), index.to_string())?;
        Ok(index)
    }

    // TODO(kvm): untested; spawns firecracker and needs /dev/kvm and root.
    async fn boot(&self, name: &str) -> Result<(), SandboxError> {
        self.check_kvm()?;
        let (vm, env) = {
            let mut slots = self.slots.lock().await;
            let slot = slots
                .get_mut(name)
                .ok_or_else(|| SandboxError::NotFound(name.to_owned()))?;
            // A process that never started its VM ("Not started").
            if let Some(mut old) = slot.child.take() {
                let _ = old.kill().await;
            }
            let env = slot.env.clone();
            (self.config.vm(name, self.index(name)?)?, env)
        };
        create_home_disk(&vm.dir.join(HOME_DISK), self.config.home_disk_mib)?;
        ensure_tap(&vm.tap, &self.config.bridge).await?;
        let socket = vm.dir.join(SOCKET);
        match std::fs::remove_file(&socket) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(vm.dir.join("firecracker.log"))?;
        // TODO(kvm): start through the jailer (roadmap F3), not firecracker directly.
        let mut child = Command::new(&self.config.firecracker_bin)
            .arg("--api-sock")
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .kill_on_drop(true)
            .spawn()?;
        let client = Client::new(&socket);
        let configured = async {
            for _ in 0..100 {
                if client.instance_state().await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            for request in self.config.boot_sequence(&vm, &env) {
                client.send(&request).await?;
            }
            Ok::<_, SandboxError>(())
        }
        .await;
        if let Err(e) = configured {
            let _ = child.kill().await;
            return Err(e);
        }
        if let Some(slot) = self.slots.lock().await.get_mut(name) {
            slot.child = Some(child);
        }
        Ok(())
    }
}

#[async_trait]
impl SandboxBackend for FirecrackerBackend {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn create(&self, spec: &SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        let handle = SandboxHandle {
            backend: KIND.into(),
            id: spec.name.clone(),
        };
        let name = Self::name(&handle)?;
        self.check_kvm()?;
        self.slots
            .lock()
            .await
            .entry(name.to_owned())
            .or_default()
            .env
            .clone_from(&spec.env);
        // A running or paused VM is adopted; the new env applies from its next boot.
        if matches!(
            self.state(&handle).await?,
            SandboxState::Stopped | SandboxState::Missing
        ) {
            self.boot(name).await?;
        }
        Ok(handle)
    }

    // TODO(kvm): the guest side (socat on vsock + guest-agent.js) has not run in a VM.
    async fn exec(
        &self,
        sandbox: &SandboxHandle,
        req: &ExecRequest,
    ) -> Result<ExecOutput, SandboxError> {
        let name = Self::name(sandbox)?;
        guest::exec(&self.vsock(name), name, req).await
    }

    async fn read_file(
        &self,
        sandbox: &SandboxHandle,
        path: &SandboxPath,
    ) -> Result<Vec<u8>, SandboxError> {
        let name = Self::name(sandbox)?;
        guest::read_file(&self.vsock(name), name, &path.to_string()).await
    }

    async fn write_file(
        &self,
        sandbox: &SandboxHandle,
        path: &SandboxPath,
        contents: &[u8],
    ) -> Result<(), SandboxError> {
        let name = Self::name(sandbox)?;
        guest::write_file(&self.vsock(name), name, &path.to_string(), contents).await
    }

    async fn snapshot(
        &self,
        _sandbox: &SandboxHandle,
        _label: &str,
    ) -> Result<SnapshotRef, SandboxError> {
        // TODO(kvm): pause + `PUT /snapshot/create` (Full), with the disks (roadmap F4b).
        Err(SandboxError::NotImplemented {
            backend: KIND,
            operation: "snapshot",
        })
    }

    async fn wake(&self, sandbox: &SandboxHandle) -> Result<(), SandboxError> {
        let name = Self::name(sandbox)?;
        match self.state(sandbox).await? {
            SandboxState::Running => Ok(()),
            SandboxState::Paused => {
                self.client(name)
                    .send(&Request::vm(VmState::Resumed))
                    .await?;
                Ok(())
            }
            // Without the env (e.g. after a daemon restart) the daemon provisions it anew.
            SandboxState::Stopped | SandboxState::Missing => {
                let known = self.slots.lock().await.contains_key(name);
                if !known || !self.config.state_dir.join(name).exists() {
                    return Err(SandboxError::NotFound(name.to_owned()));
                }
                self.boot(name).await
            }
        }
    }

    async fn sleep(
        &self,
        sandbox: &SandboxHandle,
        level: SleepLevel,
    ) -> Result<SleepLevel, SandboxError> {
        let name = Self::name(sandbox)?;
        if level == SleepLevel::Warm {
            return match self.state(sandbox).await? {
                SandboxState::Running => {
                    self.client(name)
                        .send(&Request::vm(VmState::Paused))
                        .await?;
                    Ok(SleepLevel::Warm)
                }
                SandboxState::Paused => Ok(SleepLevel::Warm),
                SandboxState::Stopped | SandboxState::Missing => Ok(SleepLevel::Cold),
            };
        }
        // TODO(kvm): cold sleep should snapshot and load on wake (roadmap F4b); now it stops.
        self.stop(sandbox).await?;
        Ok(SleepLevel::Cold)
    }

    // TODO(kvm): untested against a real guest.
    async fn stop(&self, sandbox: &SandboxHandle) -> Result<(), SandboxError> {
        let name = Self::name(sandbox)?;
        let client = self.client(name);
        match self.state(sandbox).await? {
            SandboxState::Stopped | SandboxState::Missing => return Ok(()),
            // A paused guest cannot take the shutdown request.
            SandboxState::Paused => {
                client.send(&Request::vm(VmState::Resumed)).await?;
            }
            SandboxState::Running => {}
        }
        let child = if cfg!(target_arch = "x86_64") {
            client
                .send(&Request::action(Action::SendCtrlAltDel))
                .await?;
            let deadline = tokio::time::Instant::now() + STOP_TIMEOUT;
            let mut stopped = false;
            while !stopped && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
                stopped = self.state(sandbox).await? == SandboxState::Stopped;
            }
            let child = self
                .slots
                .lock()
                .await
                .get_mut(name)
                .and_then(|s| s.child.take());
            if stopped {
                if let Some(mut child) = child {
                    child.wait().await?;
                }
                return Ok(());
            }
            child
        } else {
            self.slots
                .lock()
                .await
                .get_mut(name)
                .and_then(|s| s.child.take())
        };
        match child {
            Some(mut child) => Ok(child.kill().await?),
            None => Err(SandboxError::Other(format!(
                "VM {name} did not stop and was not started by this daemon"
            ))),
        }
    }

    async fn destroy(&self, sandbox: &SandboxHandle, purge_disk: bool) -> Result<(), SandboxError> {
        let name = Self::name(sandbox)?;
        self.stop(sandbox).await?;
        self.slots.lock().await.remove(name);
        let dir = self.config.state_dir.join(name);
        if let Some(index) = read_index(&dir) {
            let tap = self.config.vm(name, index)?.tap;
            if tap_exists(&tap) {
                // TODO(kvm): untested.
                ip(&["link", "del", "dev", &tap]).await?;
            }
        }
        if purge_disk {
            match tokio::fs::remove_dir_all(&dir).await {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        Ok(())
    }

    async fn state(&self, sandbox: &SandboxHandle) -> Result<SandboxState, SandboxError> {
        let name = Self::name(sandbox)?;
        if !self.config.state_dir.join(name).exists() {
            return Ok(SandboxState::Missing);
        }
        match self.client(name).instance_state().await {
            Ok(state) => Ok(match state.as_str() {
                "Running" => SandboxState::Running,
                "Paused" => SandboxState::Paused,
                _ => SandboxState::Stopped,
            }),
            // No socket, or no process behind it.
            Err(SandboxError::Io(_)) => Ok(SandboxState::Stopped),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::client::fake::{Recorded, serve};
    use super::*;

    fn config(root: &Path) -> FirecrackerConfig {
        FirecrackerConfig {
            kernel: "/fc/vmlinux".into(),
            rootfs: "/fc/rootfs.ext4".into(),
            state_dir: root.join("vms"),
            kvm_device: root.join("no-kvm"),
            ..FirecrackerConfig::default()
        }
    }

    fn handle(name: &str) -> SandboxHandle {
        SandboxHandle {
            backend: KIND.into(),
            id: name.into(),
        }
    }

    #[test]
    fn boot_sequence_is_exact_and_env_only_goes_to_mmds() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path());
        let vm = config.vm("alice", 1).unwrap();
        let env = BTreeMap::from([
            ("FERROBOT_AGENT_TOKEN".to_owned(), "s3cret-token".to_owned()),
            (
                "FERROBOT_DAEMON_URL".to_owned(),
                "http://172.16.0.1:7879".to_owned(),
            ),
        ]);
        let home = tmp.path().join("vms/alice/home.ext4");
        let requests = config.boot_sequence(&vm, &env);
        let actual: Vec<_> = requests
            .iter()
            .map(|r| (r.method.as_str(), r.path.as_str(), r.body.clone().unwrap()))
            .collect();
        assert_eq!(
            actual,
            [
                (
                    "PUT",
                    "/machine-config",
                    json!({"vcpu_count": 2, "mem_size_mib": 2048})
                ),
                (
                    "PUT",
                    "/boot-source",
                    json!({"kernel_image_path": "/fc/vmlinux", "boot_args":
                        "console=ttyS0 reboot=k panic=1 init=/sbin/ferrobot-init \
                         ip=172.16.0.2::172.16.0.1:255.255.255.0::eth0:off"})
                ),
                (
                    "PUT",
                    "/drives/rootfs",
                    json!({"drive_id": "rootfs", "path_on_host": "/fc/rootfs.ext4",
                        "is_root_device": true, "is_read_only": true})
                ),
                (
                    "PUT",
                    "/drives/home",
                    json!({"drive_id": "home", "path_on_host": home.to_str().unwrap(),
                        "is_root_device": false, "is_read_only": false})
                ),
                (
                    "PUT",
                    "/network-interfaces/eth0",
                    json!({"iface_id": "eth0", "host_dev_name": "fctap1",
                        "guest_mac": "06:00:AC:10:00:02"})
                ),
                (
                    "PUT",
                    "/mmds/config",
                    json!({"version": "V2", "network_interfaces": ["eth0"]})
                ),
                (
                    "PUT",
                    "/mmds",
                    json!({"ferrobot": {"env": {
                        "FERROBOT_AGENT_TOKEN": "s3cret-token",
                        "FERROBOT_DAEMON_URL": "http://172.16.0.1:7879"}}})
                ),
                (
                    "PUT",
                    "/vsock",
                    json!({"guest_cid": 3,
                        "uds_path": tmp.path().join("vms/alice/vsock.sock").to_str().unwrap()})
                ),
                ("PUT", "/actions", json!({"action_type": "InstanceStart"})),
            ]
        );
        for request in requests.iter().filter(|r| r.path != "/mmds") {
            let wire = format!("{request:?} {}", request.body.as_ref().unwrap());
            assert!(!wire.contains("s3cret-token"), "{wire}");
        }
    }

    #[test]
    fn guest_addresses_follow_the_host_ip_until_the_subnet_ends() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path());
        assert!(config.vm("a", 0).is_err());
        let last = config.vm("a", 253).unwrap();
        assert_eq!(last.guest_ip, Ipv4Addr::new(172, 16, 0, 254));
        assert_eq!(last.tap, "fctap253");
        assert!(config.vm("a", 254).is_err(), "broadcast address");
        let long = FirecrackerConfig {
            tap_prefix: "a-very-long-tap".into(),
            ..config
        };
        assert!(long.vm("a", 1).is_err());
    }

    #[test]
    fn indexes_are_the_lowest_free_and_stick() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FirecrackerBackend::new(config(tmp.path()));
        assert_eq!(b.index("a").unwrap(), 1);
        assert_eq!(b.index("b").unwrap(), 2);
        assert_eq!(b.index("a").unwrap(), 1);
        std::fs::remove_dir_all(tmp.path().join("vms/a")).unwrap();
        assert_eq!(b.index("c").unwrap(), 1);
    }

    #[test]
    fn home_disk_is_sparse_and_never_recreated() {
        let tmp = tempfile::tempdir().unwrap();
        let disk = tmp.path().join("home.ext4");
        create_home_disk(&disk, 4).unwrap();
        assert_eq!(std::fs::metadata(&disk).unwrap().len(), 4 * 1024 * 1024);
        std::fs::write(&disk, b"data").unwrap();
        create_home_disk(&disk, 4).unwrap();
        assert_eq!(std::fs::read(&disk).unwrap(), b"data");
    }

    #[tokio::test]
    async fn create_without_kvm_fails_clearly_and_touches_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FirecrackerBackend::new(config(tmp.path()));
        let spec = SandboxSpec {
            name: "alice".into(),
            env: BTreeMap::new(),
            labels: BTreeMap::new(),
        };
        let err = b.create(&spec).await.unwrap_err().to_string();
        assert!(err.contains("needs read/write access to"), "{err}");
        assert!(err.contains("(Linux with KVM)"), "{err}");
        assert!(!tmp.path().join("vms").exists());
        assert_eq!(
            b.state(&handle("alice")).await.unwrap(),
            SandboxState::Missing
        );
    }

    #[tokio::test]
    async fn state_follows_the_api_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FirecrackerBackend::new(config(tmp.path()));
        assert_eq!(
            b.state(&handle("ghost")).await.unwrap(),
            SandboxState::Missing
        );
        let dir = tmp.path().join("vms/alice");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            b.state(&handle("alice")).await.unwrap(),
            SandboxState::Stopped
        );
        serve(&dir.join(SOCKET), 200, r#"{"state":"Not started"}"#);
        assert_eq!(
            b.state(&handle("alice")).await.unwrap(),
            SandboxState::Stopped
        );
        let dir = tmp.path().join("vms/bob");
        std::fs::create_dir_all(&dir).unwrap();
        serve(&dir.join(SOCKET), 200, r#"{"state":"Running"}"#);
        assert_eq!(
            b.state(&handle("bob")).await.unwrap(),
            SandboxState::Running
        );
    }

    fn patch_vm(state: &str) -> Recorded {
        Recorded {
            method: "PATCH".into(),
            path: "/vm".into(),
            content_type: Some("application/json".into()),
            body: Some(json!({ "state": state })),
        }
    }

    #[tokio::test]
    async fn warm_sleep_pauses_a_running_vm() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FirecrackerBackend::new(config(tmp.path()));
        let dir = tmp.path().join("vms/alice");
        std::fs::create_dir_all(&dir).unwrap();
        let log = serve(&dir.join(SOCKET), 200, r#"{"state":"Running"}"#);
        let reached = b.sleep(&handle("alice"), SleepLevel::Warm).await.unwrap();
        assert_eq!(reached, SleepLevel::Warm);
        assert_eq!(log.lock().unwrap().last(), Some(&patch_vm("Paused")));
    }

    #[tokio::test]
    async fn wake_resumes_a_paused_vm_without_kvm() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FirecrackerBackend::new(config(tmp.path()));
        let dir = tmp.path().join("vms/alice");
        std::fs::create_dir_all(&dir).unwrap();
        let log = serve(&dir.join(SOCKET), 200, r#"{"state":"Paused"}"#);
        b.wake(&handle("alice")).await.unwrap();
        assert_eq!(log.lock().unwrap().last(), Some(&patch_vm("Resumed")));
        assert_eq!(
            b.sleep(&handle("alice"), SleepLevel::Warm).await.unwrap(),
            SleepLevel::Warm
        );
    }

    #[tokio::test]
    async fn stopped_vms_sleep_cold_and_unknown_ones_wake_as_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FirecrackerBackend::new(config(tmp.path()));
        std::fs::create_dir_all(tmp.path().join("vms/alice")).unwrap();
        let h = handle("alice");
        assert_eq!(
            b.sleep(&h, SleepLevel::Warm).await.unwrap(),
            SleepLevel::Cold
        );
        assert_eq!(
            b.sleep(&h, SleepLevel::Cold).await.unwrap(),
            SleepLevel::Cold
        );
        // No env in memory (e.g. after a daemon restart): the daemon re-provisions.
        assert!(matches!(
            b.wake(&h).await.unwrap_err(),
            SandboxError::NotFound(_)
        ));
    }

    #[tokio::test]
    async fn destroy_keeps_or_purges_the_home_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FirecrackerBackend::new(config(tmp.path()));
        b.index("alice").unwrap();
        let disk = tmp.path().join("vms/alice").join(HOME_DISK);
        create_home_disk(&disk, 1).unwrap();
        b.destroy(&handle("alice"), false).await.unwrap();
        assert!(disk.exists());
        b.destroy(&handle("alice"), true).await.unwrap();
        assert!(!disk.exists());
        assert_eq!(
            b.state(&handle("alice")).await.unwrap(),
            SandboxState::Missing
        );
    }

    #[tokio::test]
    async fn kind_capabilities_and_guest_agent_operations() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FirecrackerBackend::new(config(tmp.path()));
        assert_eq!(b.kind(), "firecracker");
        assert!(!b.supports_snapshots());
        let err = b
            .exec(&handle("alice"), &ExecRequest::new(["true"]))
            .await
            .unwrap_err();
        assert!(
            matches!(err, SandboxError::NotFound(ref n) if n == "alice"),
            "no VM, no vsock socket: {err}"
        );
        let foreign = SandboxHandle {
            backend: "docker".into(),
            id: "alice".into(),
        };
        assert!(matches!(
            b.state(&foreign).await,
            Err(SandboxError::WrongBackend { .. })
        ));
        assert!(matches!(
            b.state(&handle("../etc")).await,
            Err(SandboxError::NotFound(_))
        ));
    }
}
