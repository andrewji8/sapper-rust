use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sapper"))
}

fn store(dir: &TempDir) -> PathBuf {
    dir.path().join(".sapper")
}

#[test]
fn full_flow() {
    let dir = TempDir::new().unwrap();
    let st = store(&dir);
    let run = |args: &[&str]| {
        Command::new(bin())
            .arg("--store")
            .arg(&st)
            .args(args)
            .output()
            .unwrap()
    };
    let out = run(&["init", "--goal", "capture flag", "--origin", "test"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run(&[
        "fact",
        "target host",
        "-k",
        "host",
        "-d",
        "addr=10.0.0.5",
        "--creator",
        "tester",
        "-t",
        "main-host",
    ]);
    assert!(out.status.success());
    let fid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(fid.starts_with('f'));
    let out = run(&[
        "intent",
        "--from",
        &fid,
        "scan services",
        "--creator",
        "tester",
    ]);
    assert!(out.status.success());
    let iid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(iid.starts_with('i'));
    let out = run(&[
        "done",
        &iid,
        "--fact",
        "found ssh",
        "-k",
        "service",
        "--worker",
        "tester",
    ]);
    assert!(out.status.success());
    let out = run(&["status"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("done intents"));
    let out = run(&["validate"]);
    assert!(out.status.success());
}

#[test]
fn corrupt_file_tolerated() {
    let dir = TempDir::new().unwrap();
    let st = store(&dir);
    Command::new(bin())
        .arg("--store")
        .arg(&st)
        .args(["init", "--goal", "g"])
        .output()
        .unwrap();
    fs::write(st.join("facts").join("bad.json"), "{not json").unwrap();
    let out = Command::new(bin())
        .arg("--store")
        .arg(&st)
        .arg("status")
        .output()
        .unwrap();
    assert!(out.status.success());
}

#[test]
fn json_roundtrip_keys() {
    let dir = TempDir::new().unwrap();
    let st = store(&dir);
    Command::new(bin())
        .arg("--store")
        .arg(&st)
        .args(["init", "--goal", "g"])
        .output()
        .unwrap();
    let raw = fs::read_to_string(st.join("project.json")).unwrap();
    assert!(raw.ends_with('\n'));
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["id"], "proj");
    assert_eq!(v["status"], "active");
}
#[test]
fn non_object_node_tolerated() {
    // A node file that is valid JSON but NOT an object must be skipped by the
    // read path (status/graph) with a warning, not panic.
    let dir = TempDir::new().unwrap();
    let st = store(&dir);
    let run = |args: &[&str]| {
        Command::new(bin())
            .arg("--store")
            .arg(&st)
            .args(args)
            .output()
            .unwrap()
    };
    let out = run(&["init", "--goal", "g"]);
    assert!(out.status.success());
    // valid JSON, but an array -> not a node object
    fs::write(st.join("facts").join("f999.json"), "[\"a\", \"b\"]").unwrap();
    let out = run(&["status"]);
    assert!(
        out.status.success(),
        "status must tolerate a non-object node file: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run(&["graph"]);
    assert!(
        out.status.success(),
        "graph must tolerate a non-object node file: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
