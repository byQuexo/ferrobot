//! Docker backend integration test; needs a Docker/Podman engine and the runner image
//! (`FERROBOT_TEST_IMAGE`, default `ferrobot-runner:dev`): `--features docker-tests`.
#![cfg(feature = "docker-tests")]

use std::collections::BTreeMap;
use std::sync::Arc;

use bollard::query_parameters::InspectContainerOptions;
use ferrobot_sandbox::seal::SealKey;
use ferrobot_sandbox::{
    DockerBackend, DockerConfig, ExecRequest, SandboxBackend, SandboxPath, SandboxSpec,
    SandboxState, SleepLevel,
};

/// The env the daemon gives a sandbox, with the agent token under every name it uses.
fn agent_env(token: &str) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        ("FERROBOT_DAEMON_URL".into(), "http://127.0.0.1:9".into()),
        ("FERROBOT_DRIVER".into(), "mock".into()),
    ]);
    for name in [
        "FERROBOT_AGENT_TOKEN",
        "ANTHROPIC_API_KEY",
        "GEMINI_API_KEY",
        "AWS_BEARER_TOKEN_BEDROCK",
    ] {
        env.insert(name.into(), token.into());
    }
    env
}

/// Env entries of a committed image that still carry `token`.
#[allow(clippy::unwrap_used, reason = "test helper")]
async fn token_in_image(image: &str, token: &str) -> Vec<String> {
    let docker = bollard::Docker::connect_with_local_defaults().unwrap();
    let config = docker.inspect_image(image).await.unwrap().config.unwrap();
    config
        .env
        .unwrap_or_default()
        .into_iter()
        .filter(|kv| kv.contains(token))
        .collect()
}

#[tokio::test]
async fn container_lifecycle() {
    let snapshots = tempfile::tempdir().unwrap();
    let backend = DockerBackend::connect(DockerConfig {
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
        env: agent_env("test-token-lifecycle"),
        labels: BTreeMap::new(),
        image: None,
    };
    let h = backend.create(&spec).await.expect("create");
    // create is idempotent
    assert_eq!(backend.create(&spec).await.expect("re-create").id, h.id);
    assert_eq!(backend.state(&h).await.unwrap(), SandboxState::Running);
    let (image, id) = backend.default_image().await.unwrap();
    assert!(id.is_some_and(|id| id.starts_with("sha256:")), "{image}");

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
    assert_eq!(
        token_in_image(&snap.id, "test-token-lifecycle").await,
        Vec::<String>::new(),
        "no agent token in the snapshot image"
    );

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
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test helper")]
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
    let store: Arc<dyn object_store::ObjectStore> =
        Arc::new(object_store::local::LocalFileSystem::new_with_prefix(store_dir.path()).unwrap());
    let config = |key: u8| DockerConfig {
        image: std::env::var("FERROBOT_TEST_IMAGE")
            .unwrap_or_else(|_| "ferrobot-runner:dev".into()),
        name_prefix: "ferrobot-test-".into(),
        snapshot_store: Some(Arc::clone(&store)),
        archive_key: Some(SealKey::new(&[key; 32])),
        accept_plaintext: false,
        ..DockerConfig::default()
    };
    let backend = DockerBackend::connect(config(3)).expect("docker client");
    let name = format!("arch{}", std::process::id());
    let spec = SandboxSpec {
        name: name.clone(),
        env: agent_env("test-token-archive"),
        labels: BTreeMap::new(),
        image: None,
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
    for part in ["image.tar", "home.tar"] {
        let mut head = [0u8; 8];
        std::io::Read::read_exact(
            &mut std::fs::File::open(archived.join(part)).unwrap(),
            &mut head,
        )
        .unwrap();
        assert_eq!(&head, b"FBSEAL01", "{part} is encrypted");
    }
    let home = std::fs::read(archived.join("home.tar")).unwrap();
    assert!(!home.windows(20).any(|w| w == b"survives the archive"));

    let wrong_key = DockerBackend::connect(config(4)).unwrap();
    assert!(wrong_key.create(&spec).await.is_err(), "wrong key");
    assert!(
        archived.join("image.tar").exists(),
        "a failed restore keeps the archive"
    );

    let h = backend.create(&spec).await.expect("restore from archive");
    let env = backend
        .exec(
            &h,
            &ExecRequest::new(["sh", "-c", "echo $FERROBOT_AGENT_TOKEN"]),
        )
        .await
        .unwrap();
    assert_eq!(
        env.stdout.trim(),
        "test-token-archive",
        "the current token comes from the container"
    );
    assert_eq!(backend.state(&h).await.unwrap(), SandboxState::Running);
    let image = bollard::Docker::connect_with_local_defaults()
        .unwrap()
        .inspect_container(&h.id, None::<InspectContainerOptions>)
        .await
        .unwrap()
        .image
        .unwrap();
    assert_eq!(
        token_in_image(&image, "test-token-archive").await,
        Vec::<String>::new(),
        "no agent token in the archived image"
    );
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

#[tokio::test]
async fn prefetch_downloads_a_missing_image() {
    let image = "hello-world:latest";
    let docker = bollard::Docker::connect_with_local_defaults().unwrap();
    let remove = || {
        docker.remove_image(
            image,
            None::<bollard::query_parameters::RemoveImageOptions>,
            None,
        )
    };
    let _ = remove().await;
    let backend = DockerBackend::connect(DockerConfig {
        image: image.into(),
        name_prefix: "ferrobot-test-".into(),
        ..DockerConfig::default()
    })
    .expect("docker client");
    tokio::time::timeout(
        std::time::Duration::from_secs(120),
        backend.prefetch_image(),
    )
    .await
    .expect("prefetch finishes");
    assert!(docker.inspect_image(image).await.is_ok(), "{image} pulled");
    let _ = remove().await;
}
