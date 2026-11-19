//! Docker backend integration test.
//!
//! Needs a reachable Docker/Podman engine and a pullable `alpine` image:
//! `cargo test -p ferrobot-sandbox --features docker-tests`
#![cfg(feature = "docker-tests")]

use std::collections::BTreeMap;

use ferrobot_sandbox::{
    DockerBackend, DockerConfig, ExecRequest, SandboxBackend, SandboxPath, SandboxSpec,
    SandboxState,
};

#[tokio::test]
async fn container_lifecycle() {
    let snapshots = tempfile::tempdir().unwrap();
    let backend = DockerBackend::connect(DockerConfig {
        // Any long-running image works for the lifecycle test.
        image: std::env::var("FERROBOT_TEST_IMAGE")
            .unwrap_or_else(|_| "ferrobot-runner:dev".into()),
        name_prefix: "ferrobot-test-".into(),
        snapshot_dir: Some(snapshots.path().to_owned()),
        ..DockerConfig::default()
    })
    .expect("docker client");
    backend.ping().await.expect("docker engine reachable");

    let name = format!("t{}", std::process::id());
    let spec = SandboxSpec {
        name: name.clone(),
        // No daemon is listening: the runner keeps retrying, which is fine here.
        env: BTreeMap::from([
            ("FERROBOT_DAEMON_URL".into(), "http://127.0.0.1:9".into()),
            ("FERROBOT_AGENT_TOKEN".into(), "test".into()),
            ("FERROBOT_DRIVER".into(), "mock".into()),
        ]),
        labels: BTreeMap::new(),
    };
    let h = backend.create(&spec).await.expect("create");
    // create is idempotent
    assert_eq!(backend.create(&spec).await.expect("re-create").id, h.id);
    assert_eq!(backend.state(&h).await.unwrap(), SandboxState::Running);

    let path = SandboxPath::parse("/home/node/hello.txt").unwrap();
    backend
        .write_file(&h, &path, b"hi from host")
        .await
        .expect("write");
    assert_eq!(
        backend.read_file(&h, &path).await.expect("read"),
        b"hi from host"
    );

    let out = backend
        .exec(
            &h,
            &ExecRequest::new(["sh", "-c", "id -u && cat /home/node/hello.txt"]),
        )
        .await
        .expect("exec");
    assert_eq!(out.exit_code, 0, "{out:?}");
    assert_eq!(out.stdout, "1000\nhi from host");

    let snap = backend.snapshot(&h, "test").await.expect("snapshot");
    assert!(snap.id.starts_with("ferrobot-snapshot:"));
    assert!(snap.scope.contains("home"), "{}", snap.scope);

    // Change the disk after the snapshot, then restore it.
    backend
        .write_file(&h, &path, b"changed")
        .await
        .expect("overwrite");
    let extra = SandboxPath::parse("/home/node/later.txt").unwrap();
    backend.write_file(&h, &extra, b"x").await.unwrap();
    let h = backend.restore(&h, &snap.id, &spec).await.expect("restore");
    assert_eq!(backend.state(&h).await.unwrap(), SandboxState::Running);
    assert_eq!(backend.read_file(&h, &path).await.unwrap(), b"hi from host");
    assert!(
        backend.read_file(&h, &extra).await.is_err(),
        "later file must be gone"
    );
    let owner = backend
        .exec(
            &h,
            &ExecRequest::new(["stat", "-c", "%u", "/home/node/hello.txt"]),
        )
        .await
        .unwrap();
    assert_eq!(
        owner.stdout.trim(),
        "1000",
        "restored files keep their owner"
    );
    assert!(
        backend
            .restore(&h, "ferrobot-snapshot:../../etc", &spec)
            .await
            .is_err()
    );

    backend.stop(&h).await.expect("stop");
    assert_eq!(backend.state(&h).await.unwrap(), SandboxState::Stopped);
    backend.wake(&h).await.expect("wake");
    // state survives stop/wake (home volume)
    assert_eq!(backend.read_file(&h, &path).await.unwrap(), b"hi from host");

    backend.destroy(&h, true).await.expect("destroy");
    assert_eq!(backend.state(&h).await.unwrap(), SandboxState::Missing);
    backend
        .delete_snapshot(&snap.id)
        .await
        .expect("delete snapshot");
    assert_eq!(std::fs::read_dir(snapshots.path()).unwrap().count(), 0);
}
