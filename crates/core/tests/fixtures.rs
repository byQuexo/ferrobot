//! Parses the shared protocol fixtures that the TypeScript runner also parses,
//! so the two hand-written protocol mirrors cannot drift silently.

use std::fs;
use std::path::PathBuf;

use ferrobot_core::protocol::{DaemonFrame, RunnerFrame};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol/fixtures")
}

fn load(dir: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(fixtures_dir().join(dir)).expect("fixture dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|e| e == "json") {
            let text = fs::read_to_string(&path).expect("read fixture");
            out.push((path.display().to_string(), text));
        }
    }
    assert!(!out.is_empty(), "no fixtures in {dir}");
    out
}

#[test]
fn daemon_frames_parse_and_roundtrip() {
    for (path, text) in load("daemon") {
        let frame: DaemonFrame =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
        let again: DaemonFrame =
            serde_json::from_str(&serde_json::to_string(&frame).unwrap()).unwrap();
        assert_eq!(frame, again, "{path}");
    }
}

#[test]
fn runner_frames_parse_and_roundtrip() {
    for (path, text) in load("runner") {
        let frame: RunnerFrame =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
        let again: RunnerFrame =
            serde_json::from_str(&serde_json::to_string(&frame).unwrap()).unwrap();
        assert_eq!(frame, again, "{path}");
    }
}
