//! `arsy storage` against the real binary, with a configuration home and a
//! workspace of the test's own.

use serde_json::Value;
use std::{path::Path, process::Command};

fn arsy(workspace: &Path, home: &Path, args: &[&str]) -> (i32, String, Value) {
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
    (output.status.code().unwrap_or(-1), stdout, payload)
}

fn owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[test]
fn the_listing_measures_credentials_without_reading_them() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let secrets = home.path().join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    let secret = "sk-never-printed-0123456789";
    std::fs::write(secrets.join("provider.key"), secret).unwrap();
    owner_only(&secrets.join("provider.key"));

    let (code, stdout, listed) = arsy(workspace.path(), home.path(), &["storage"]);

    assert_eq!(code, 0, "{stdout}");
    assert!(!stdout.contains(secret), "a credential reached the output");
    let entries = listed["entries"].as_array().unwrap();
    let credentials = entries
        .iter()
        .find(|entry| entry["label"] == "credentials")
        .unwrap();
    assert_eq!(credentials["files"], 1);
    assert_eq!(credentials["bytes"], secret.len());
    assert!(entries
        .iter()
        .any(|entry| entry["label"] == "session history" && entry["action"] == "reset-history"));
}

#[test]
fn clean_removes_the_cache_and_the_repository_map() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("cache")).unwrap();
    std::fs::write(home.path().join("cache/mcp-tools.json"), "{}").unwrap();
    let map = workspace.path().join(".arsy/state/repo-map.json");
    std::fs::create_dir_all(map.parent().unwrap()).unwrap();
    std::fs::write(&map, "{}").unwrap();

    let (code, stdout, _) = arsy(
        workspace.path(),
        home.path(),
        &["storage", "clean", "cache"],
    );
    assert_eq!(code, 0, "{stdout}");
    assert!(!home.path().join("cache").exists());

    let (code, stdout, _) = arsy(
        workspace.path(),
        home.path(),
        &["storage", "clean", "repo-map"],
    );
    assert_eq!(code, 0, "{stdout}");
    assert!(!map.exists());

    let (code, _, _) = arsy(
        workspace.path(),
        home.path(),
        &["storage", "clean", "nothing"],
    );
    assert_ne!(code, 0);
}

#[test]
fn a_history_reset_needs_the_workspace_name() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    // Any command opens the store, which creates it.
    let (code, _, _) = arsy(workspace.path(), home.path(), &["session", "list"]);
    assert_eq!(code, 0);
    let store = workspace.path().join(".arsy/state/sessions.sqlite3");
    assert!(store.is_file());
    let name = workspace
        .path()
        .canonicalize()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let (code, _, _) = arsy(workspace.path(), home.path(), &["storage", "reset-history"]);
    assert_ne!(code, 0, "no --confirm");
    let (code, _, _) = arsy(
        workspace.path(),
        home.path(),
        &["storage", "reset-history", "--confirm", "not-it"],
    );
    assert_ne!(code, 0, "the wrong name");
    assert!(store.is_file(), "nothing deleted yet");

    let (code, stdout, _) = arsy(
        workspace.path(),
        home.path(),
        &["storage", "reset-history", "--confirm", &name],
    );
    assert_eq!(code, 0, "{stdout}");
    assert!(!store.exists());
}
