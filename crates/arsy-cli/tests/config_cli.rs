//! `arsy config set` and `unset` against the real binary, with a user
//! configuration home and a workspace of the test's own.

use serde_json::Value;
use std::{path::Path, process::Command};

fn arsy(workspace: &Path, home: &Path, args: &[&str]) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_arsy"))
        .args(["--workspace", workspace.to_str().unwrap()])
        .env("CLAUDE_CONFIG_DIR", home.join("no-claude"))
        .env("CODEX_HOME", home.join("no-codex"))
        .env("ARSY_CONFIG_HOME", home)
        .args(args)
        .args(["--output", "json"])
        .output()
        .expect("the binary runs");
    let stdout = String::from_utf8(output.stdout).expect("machine output is UTF-8");
    let payload = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| record["type"] == "result")
        .map(|record| record["payload"].clone())
        .unwrap_or(Value::Null);
    (output.status.code().unwrap_or(-1), payload)
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn a_setting_is_written_to_the_scope_it_names_and_removed_again() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = workspace.path().join(".arsy/arsy.json");
    let user = home.path().join("arsy.json");

    let (code, written) = arsy(
        workspace.path(),
        home.path(),
        &[
            "config",
            "set",
            "ui.style",
            "classic",
            "--scope",
            "workspace",
        ],
    );
    assert_eq!(code, 0, "{written}");
    assert_eq!(written["scope"], "workspace");
    assert_eq!(read(&project)["ui"]["style"], "classic");

    let (_, explained) = arsy(
        workspace.path(),
        home.path(),
        &["config", "explain", "ui.style"],
    );
    assert!(
        explained.to_string().contains(".arsy/arsy.json"),
        "the project file decides it: {explained}"
    );

    // Without --scope the operator's own file is the target.
    let (code, _) = arsy(
        workspace.path(),
        home.path(),
        &["config", "set", "execution.max_parallel", "2"],
    );
    assert_eq!(code, 0);
    assert_eq!(read(&user)["execution"]["max_parallel"], 2);

    let (code, _) = arsy(
        workspace.path(),
        home.path(),
        &["config", "unset", "ui.style", "--scope", "workspace"],
    );
    assert_eq!(code, 0);
    assert!(read(&project)["ui"].get("style").is_none());
}

#[test]
fn a_value_the_registry_refuses_changes_nothing() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();

    let (code, _) = arsy(
        workspace.path(),
        home.path(),
        &["config", "set", "ui.style", "loud", "--scope", "workspace"],
    );
    assert_ne!(code, 0);
    let (code, _) = arsy(
        workspace.path(),
        home.path(),
        &["config", "set", "no.such.key", "x", "--scope", "workspace"],
    );
    assert_ne!(code, 0);
    assert!(!workspace.path().join(".arsy/arsy.json").exists());
}

/// With `storage.state_gitignore` off, the state directory is created but
/// leaves the repository's ignore rules to the operator.
#[test]
fn state_gitignore_off_writes_no_ignore_file() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (code, _) = arsy(
        workspace.path(),
        home.path(),
        &[
            "config",
            "set",
            "storage.state_gitignore",
            "false",
            "--scope",
            "workspace",
        ],
    );
    assert_eq!(code, 0);

    let (code, _) = arsy(workspace.path(), home.path(), &["session", "list"]);
    assert_eq!(code, 0);
    let state = workspace.path().join(".arsy/state");
    assert!(state.is_dir());
    assert!(!state.join(".gitignore").exists());

    let fresh = tempfile::tempdir().unwrap();
    let (code, _) = arsy(fresh.path(), home.path(), &["session", "list"]);
    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(fresh.path().join(".arsy/state/.gitignore")).unwrap(),
        "*\n",
        "on by default"
    );
}
