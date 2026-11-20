//! Docker backend integration test.
//!
//! Needs a reachable Docker/Podman engine and a pullable `alpine` image:
//! `cargo test -p ferrobot-sandbox --features docker-tests`
#![cfg(feature = "docker-tests")]

use std::collections::BTreeMap;
use std::sync::Arc;

use ferrobot_sandbox::{
    DockerBackend, DockerConfig, ExecRequest, SandboxBackend, SandboxPath, SandboxSpec,
    SandboxState, SleepLevel,
};

#[tokio::test]
async fn container_lifecycle() {
    let snapshots = tempfile::tempdir().unwrap();
    let backend = DockerBackend::connect(DockerConfig {
        // Any long-running image works for the lifecycle test.
        image: std::env::var("FERROBOT_TEST_IMAGE")
            .unwrap_or_else(|_| "ferrobot-runner:dev".into()),
        name_prefix: "ferrobot-test-".into(),
        snapshot_store: Some(Arc::new(
            object_store::local::LocalFileSystem::new_with_prefix(snapshots.path()).unwrap(),
        )),
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

    warm_sleep_round_trip(&backend, &h).await;
    backend.wake(&h).await.expect("wake after pause+stop");
    // state survives stop/wake (home volume)
    assert_eq!(backend.read_file(&h, &path).await.unwrap(), b"hi from host");

    backend.destroy(&h, true).await.expect("destroy");
    assert_eq!(backend.state(&h).await.unwrap(), SandboxState::Missing);
    backend
        .delete_snapshot(&snap.id)
        .await
        .expect("delete snapshot");
    assert_eq!(
        std::fs::read_dir(snapshots.path().join("snapshots"))
            .unwrap()
            .count(),
        0
    );
}

/// Warm sleep pauses in place; wake resumes; a paused container still stops cleanly.
async fn warm_sleep_round_trip(backend: &DockerBackend, h: &ferrobot_sandbox::SandboxHandle) {
    let warm = backend.sleep(h, SleepLevel::Warm).await.expect("pause");
    assert_eq!(warm, SleepLevel::Warm);
    assert_eq!(backend.state(h).await.unwrap(), SandboxState::Paused);
    backend.wake(h).await.expect("resume");
    assert_eq!(backend.state(h).await.unwrap(), SandboxState::Running);
    backend
        .sleep(h, SleepLevel::Warm)
        .await
        .expect("pause again");
    backend.stop(h).await.expect("stop while paused");
    assert_eq!(backend.state(h).await.unwrap(), SandboxState::Stopped);
    assert_eq!(
        backend
            .sleep(h, SleepLevel::Warm)
            .await
            .expect("pause stopped"),
        SleepLevel::Cold,
        "a stopped container cannot be paused"
    );
}

#[tokio::test]
async fn archive_moves_the_sandbox_into_the_store_and_back() {
    let store_dir = tempfile::tempdir().unwrap();
    let backend = DockerBackend::connect(DockerConfig {
        image: std::env::var("FERROBOT_TEST_IMAGE")
            .unwrap_or_else(|_| "ferrobot-runner:dev".into()),
        name_prefix: "ferrobot-test-".into(),
        snapshot_store: Some(Arc::new(
            object_store::local::LocalFileSystem::new_with_prefix(store_dir.path()).unwrap(),
        )),
        ..DockerConfig::default()
    })
    .expect("docker client");
    let name = format!("arch{}", std::process::id());
    let spec = SandboxSpec {
        name: name.clone(),
        env: BTreeMap::from([
            ("FERROBOT_DAEMON_URL".into(), "http://127.0.0.1:9".into()),
            ("FERROBOT_AGENT_TOKEN".into(), "test".into()),
            ("FERROBOT_DRIVER".into(), "mock".into()),
        ]),
        labels: BTreeMap::new(),
    };
    let h = backend.create(&spec).await.expect("create");
    let note = SandboxPath::parse("/home/node/kept.txt").unwrap();
    backend
        .write_file(&h, &note, b"survives the archive")
        .await
        .unwrap();

    let level = backend
        .sleep(&h, SleepLevel::Archived)
        .await
        .expect("archive");
    assert_eq!(level, SleepLevel::Archived);
    assert_eq!(backend.state(&h).await.unwrap(), SandboxState::Missing);
    let archived = store_dir.path().join("archives").join(&name);
    assert!(archived.join("image.tar").exists() && archived.join("home.tar").exists());

    let h = backend.create(&spec).await.expect("restore from archive");
    assert_eq!(backend.state(&h).await.unwrap(), SandboxState::Running);
    assert_eq!(
        backend.read_file(&h, &note).await.unwrap(),
        b"survives the archive"
    );
    assert!(!archived.join("image.tar").exists(), "archive is consumed");

    backend.destroy(&h, true).await.expect("destroy");
    backend
        .delete_archive(&name)
        .await
        .expect("no archive left is fine");
}
