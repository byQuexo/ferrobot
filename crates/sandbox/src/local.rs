//! Development backend: the runner runs as a host child process.
//!
//! Each sandbox is a directory `<root>/<name>/` that stands in for the
//! machine's filesystem (`/` inside the sandbox maps onto it) and `$HOME` is
//! `<root>/<name>/home`. There is **no isolation**: use it for development
//! and CI, never for untrusted agents.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::{
    ExecOutput, ExecRequest, SandboxBackend, SandboxError, SandboxHandle, SandboxPath, SandboxSpec,
    SandboxState, SnapshotRef, capture,
};

const KIND: &str = "local";

#[derive(Debug, Clone)]
pub struct LocalProcessConfig {
    /// Directory holding one sub-directory per sandbox.
    pub root: PathBuf,
    /// Command that starts the runner, e.g. `["node", "/abs/runner/dist/index.js"]`.
    pub runner_cmd: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LocalProcessBackend {
    config: Arc<LocalProcessConfig>,
    /// Running runner processes plus the env needed to restart them.
    procs: Arc<Mutex<HashMap<String, Slot>>>,
}

#[derive(Debug)]
struct Slot {
    child: Option<Child>,
    env: Vec<(String, String)>,
}

impl LocalProcessBackend {
    pub fn new(config: LocalProcessConfig) -> Result<Self, SandboxError> {
        if config.runner_cmd.is_empty() {
            return Err(SandboxError::Other("runner_cmd must not be empty".into()));
        }
        std::fs::create_dir_all(&config.root)?;
        Ok(Self {
            config: Arc::new(config),
            procs: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    fn dir(&self, handle: &SandboxHandle) -> Result<PathBuf, SandboxError> {
        if handle.backend != KIND {
            return Err(SandboxError::WrongBackend {
                expected: KIND,
                actual: handle.backend.clone(),
            });
        }
        // Handle ids are agent names; refuse anything that could escape `root`.
        if handle.id.is_empty()
            || !handle
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(SandboxError::NotFound(handle.id.clone()));
        }
        Ok(self.config.root.join(&handle.id))
    }

    fn host_path(
        &self,
        handle: &SandboxHandle,
        path: &SandboxPath,
    ) -> Result<PathBuf, SandboxError> {
        Ok(self.dir(handle)?.join(path.relative()))
    }

    fn spawn(&self, dir: &Path, env: &[(String, String)]) -> Result<Child, SandboxError> {
        let home = dir.join("home");
        let mut cmd = Command::new(&self.config.runner_cmd[0]);
        cmd.args(&self.config.runner_cmd[1..])
            .current_dir(&home)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &home)
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .kill_on_drop(true);
        Ok(cmd.spawn()?)
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[async_trait]
impl SandboxBackend for LocalProcessBackend {
    fn kind(&self) -> &'static str {
        KIND
    }

    // Sandbox paths map below the sandbox dir; the runner's home is `<dir>/home`.
    fn home(&self) -> String {
        "/home".into()
    }

    async fn create(&self, spec: &SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        let handle = SandboxHandle {
            backend: KIND.into(),
            id: spec.name.clone(),
        };
        let dir = self.dir(&handle)?;
        tokio::fs::create_dir_all(dir.join("home")).await?;
        let env: Vec<_> = spec.env.clone().into_iter().collect();
        let mut procs = self.procs.lock().await;
        let slot = procs.entry(spec.name.clone()).or_insert(Slot {
            child: None,
            env: env.clone(),
        });
        slot.env = env;
        if slot.child.is_none() {
            slot.child = Some(self.spawn(&dir, &slot.env)?);
        }
        Ok(handle)
    }

    async fn exec(
        &self,
        sandbox: &SandboxHandle,
        req: &ExecRequest,
    ) -> Result<ExecOutput, SandboxError> {
        let dir = self.dir(sandbox)?;
        let Some((program, args)) = req.cmd.split_first() else {
            return Err(SandboxError::Other("empty command".into()));
        };
        let cwd = match &req.workdir {
            Some(w) => dir.join(SandboxPath::parse(w)?.relative()),
            None => dir.join("home"),
        };
        let out = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", dir.join("home"))
            .envs(&req.env)
            .stdin(Stdio::null())
            .output()
            .await?;
        let (stdout, t1) = capture(&out.stdout);
        let (stderr, t2) = capture(&out.stderr);
        Ok(ExecOutput {
            exit_code: out.status.code().map_or(-1, i64::from),
            stdout,
            stderr,
            truncated: t1 || t2,
        })
    }

    async fn read_file(
        &self,
        sandbox: &SandboxHandle,
        path: &SandboxPath,
    ) -> Result<Vec<u8>, SandboxError> {
        let p = self.host_path(sandbox, path)?;
        let not_found = |e: std::io::Error| match e.kind() {
            std::io::ErrorKind::NotFound => SandboxError::NotFound(path.to_string()),
            _ => e.into(),
        };
        let len = tokio::fs::metadata(&p).await.map_err(not_found)?.len();
        if usize::try_from(len).map_or(true, |n| n > crate::MAX_FILE_BYTES) {
            return Err(SandboxError::TooLarge(path.to_string()));
        }
        tokio::fs::read(&p).await.map_err(not_found)
    }

    async fn write_file(
        &self,
        sandbox: &SandboxHandle,
        path: &SandboxPath,
        contents: &[u8],
    ) -> Result<(), SandboxError> {
        let p = self.host_path(sandbox, path)?;
        tokio::fs::write(&p, contents).await?;
        Ok(())
    }

    async fn snapshot(
        &self,
        sandbox: &SandboxHandle,
        label: &str,
    ) -> Result<SnapshotRef, SandboxError> {
        let dir = self.dir(sandbox)?;
        let id = format!(
            "{}-{}-{}",
            sandbox.id,
            label
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect::<String>(),
            time::OffsetDateTime::now_utc().unix_timestamp()
        );
        let target = self.config.root.join(".snapshots").join(&id);
        tokio::task::spawn_blocking(move || copy_dir(&dir, &target))
            .await
            .map_err(|e| SandboxError::Other(e.to_string()))??;
        Ok(SnapshotRef {
            id,
            scope: "full sandbox directory copy".into(),
        })
    }

    async fn wake(&self, sandbox: &SandboxHandle) -> Result<(), SandboxError> {
        let dir = self.dir(sandbox)?;
        let mut procs = self.procs.lock().await;
        let slot = procs
            .get_mut(&sandbox.id)
            .ok_or_else(|| SandboxError::NotFound(sandbox.id.clone()))?;
        let alive = match slot.child.as_mut() {
            Some(child) => child.try_wait()?.is_none(),
            None => false,
        };
        if !alive {
            slot.child = Some(self.spawn(&dir, &slot.env)?);
        }
        Ok(())
    }

    async fn stop(&self, sandbox: &SandboxHandle) -> Result<(), SandboxError> {
        self.dir(sandbox)?;
        let mut procs = self.procs.lock().await;
        if let Some(mut child) = procs.get_mut(&sandbox.id).and_then(|s| s.child.take()) {
            child.kill().await?;
        }
        Ok(())
    }

    async fn destroy(&self, sandbox: &SandboxHandle, purge_disk: bool) -> Result<(), SandboxError> {
        self.stop(sandbox).await?;
        self.procs.lock().await.remove(&sandbox.id);
        if purge_disk {
            match tokio::fs::remove_dir_all(self.dir(sandbox)?).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    async fn state(&self, sandbox: &SandboxHandle) -> Result<SandboxState, SandboxError> {
        let dir = self.dir(sandbox)?;
        let mut procs = self.procs.lock().await;
        let running = match procs.get_mut(&sandbox.id).and_then(|s| s.child.as_mut()) {
            Some(child) => child.try_wait()?.is_none(),
            None => false,
        };
        Ok(if running {
            SandboxState::Running
        } else if dir.exists() {
            SandboxState::Stopped
        } else {
            SandboxState::Missing
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn backend(root: &Path) -> LocalProcessBackend {
        LocalProcessBackend::new(LocalProcessConfig {
            root: root.to_path_buf(),
            // A "runner" that just sleeps so lifecycle can be observed.
            runner_cmd: vec!["sleep".into(), "30".into()],
        })
        .unwrap()
    }

    fn spec(name: &str) -> SandboxSpec {
        SandboxSpec {
            name: name.into(),
            env: BTreeMap::from([("FOO".into(), "bar".into())]),
            labels: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn lifecycle_files_exec_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let b = backend(tmp.path());
        let h = b.create(&spec("alice")).await.unwrap();
        assert_eq!(b.state(&h).await.unwrap(), SandboxState::Running);

        let path = SandboxPath::parse("/home/note.txt").unwrap();
        b.write_file(&h, &path, b"hello").await.unwrap();
        assert_eq!(b.read_file(&h, &path).await.unwrap(), b"hello");

        let out = b
            .exec(&h, &ExecRequest::new(["cat", "note.txt"]))
            .await
            .unwrap();
        assert_eq!((out.exit_code, out.stdout.as_str()), (0, "hello"));

        let snap = b.snapshot(&h, "before change").await.unwrap();
        assert!(
            tmp.path()
                .join(".snapshots")
                .join(&snap.id)
                .join("home/note.txt")
                .exists()
        );

        b.stop(&h).await.unwrap();
        assert_eq!(b.state(&h).await.unwrap(), SandboxState::Stopped);
        b.wake(&h).await.unwrap();
        assert_eq!(b.state(&h).await.unwrap(), SandboxState::Running);

        b.destroy(&h, true).await.unwrap();
        assert_eq!(b.state(&h).await.unwrap(), SandboxState::Missing);
    }

    #[tokio::test]
    async fn rejects_traversal_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let b = backend(tmp.path());
        let evil = SandboxHandle {
            backend: KIND.into(),
            id: "../etc".into(),
        };
        let p = SandboxPath::parse("/passwd").unwrap();
        assert!(b.read_file(&evil, &p).await.is_err());
    }
}
