//! Where ARSY keeps things on disk, how much each takes, and the cleanups an
//! operator can run on them: `arsy storage` and the `/storage` dialog.
//!
//! Sizes come from file metadata alone. A credential under `secrets/` is
//! counted and measured, never opened.

use crate::{usage, Command, Diagnostic, Emitter, Invocation, Output};
use arsy_code::workspace;
use arsy_kernel::config::{self, CACHE_DIRECTORY, SECRETS_DIRECTORY, STATE_DIRECTORY};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command as Process;

/// Something `clean` can remove because ARSY rebuilds it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    /// `~/.arsy/cache`.
    Cache,
    /// `.arsy/state/repo-map.json`.
    RepoMap,
    /// Subagent views and eval trees no process has touched recently.
    Views,
    /// Artifacts nothing references, past `storage.artifact_retention_days`.
    Artifacts,
}

impl Target {
    fn parse(value: &str) -> Result<Self, Diagnostic> {
        match value {
            "cache" => Ok(Self::Cache),
            "repo-map" => Ok(Self::RepoMap),
            "views" => Ok(Self::Views),
            "artifacts" => Ok(Self::Artifacts),
            other => Err(usage(format!(
                "storage clean takes cache, repo-map, views, or artifacts, not `{other}`"
            ))),
        }
    }
}

/// What a row of the inventory can have done to it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Clean(Target),
    /// Delete the session store. Confirmed by typing the workspace's name.
    ResetHistory,
}

/// One location ARSY uses.
#[derive(Clone, Debug)]
pub struct Entry {
    pub label: &'static str,
    /// `global` for the configuration home, `project` for this workspace.
    pub scope: &'static str,
    pub path: PathBuf,
    pub exists: bool,
    pub bytes: u64,
    pub files: u64,
    pub action: Option<Action>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    List,
    Clean(Target),
    ResetHistory { confirm: Option<String> },
}

const HELP: &str =
    "storage takes no argument, `clean <cache|repo-map|views|artifacts>`, or `reset-history --confirm <WORKSPACE NAME>`";

pub fn parse(arguments: &crate::ParsedArguments) -> Result<Command, Diagnostic> {
    let request = match arguments
        .positional
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => Request::List,
        ["clean", target] => Request::Clean(Target::parse(target)?),
        ["reset-history"] => Request::ResetHistory {
            confirm: arguments.confirm.clone(),
        },
        _ => return Err(usage(HELP)),
    };
    Ok(Command::Storage { request })
}

/// Every location, global first, in the order a person reads a tree.
pub fn inventory(root: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut add = |label, scope, path: PathBuf, action| {
        let (bytes, files) = measure(&path);
        entries.push(Entry {
            label,
            scope,
            exists: path.exists(),
            path,
            bytes,
            files,
            action,
        });
    };
    if let Some(home) = config::config_home() {
        add("settings", "global", home.join(config::CONFIG_FILE), None);
        add("hooks", "global", home.join("guard.json"), None);
        add("credentials", "global", home.join(SECRETS_DIRECTORY), None);
        add(
            "cache",
            "global",
            home.join(CACHE_DIRECTORY),
            Some(Action::Clean(Target::Cache)),
        );
        add(
            "remembered choices",
            "global",
            home.join(STATE_DIRECTORY),
            None,
        );
    }
    let arsy = root.join(workspace::HARNESS_STATE);
    add("settings", "project", arsy.join(config::CONFIG_FILE), None);
    add("hooks", "project", arsy.join("guard.json"), None);
    add("instructions", "project", arsy.join("AGENTS.md"), None);
    add("plugins", "project", arsy.join("plugins"), None);
    add(
        "session history",
        "project",
        root.join(workspace::SESSION_STORE),
        Some(Action::ResetHistory),
    );
    add(
        "artifacts",
        "project",
        root.join(workspace::ARTIFACTS),
        Some(Action::Clean(Target::Artifacts)),
    );
    add(
        "repository map",
        "project",
        root.join(workspace::REPO_MAP),
        Some(Action::Clean(Target::RepoMap)),
    );
    add(
        "subagent views",
        "project",
        root.join(workspace::VIEWS),
        Some(Action::Clean(Target::Views)),
    );
    if let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry.label == "session history")
    {
        // The WAL sidecars are part of the store: a reset removes them too.
        for suffix in ["-wal", "-shm"] {
            let (bytes, files) = measure(&sidecar(&entry.path, suffix));
            entry.bytes += bytes;
            entry.files += files;
        }
    }
    if let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry.label == "subagent views")
    {
        for extra in view_directories(root).into_iter().skip(1) {
            let (bytes, files) = measure(&extra);
            entry.bytes += bytes;
            entry.files += files;
            entry.exists |= extra.exists();
        }
    }
    entries
}

/// Bytes and files under `path`, following no symlink.
fn measure(path: &Path) -> (u64, u64) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return (0, 0);
    };
    if !metadata.is_dir() {
        return (metadata.len(), 1);
    }
    let Ok(children) = std::fs::read_dir(path) else {
        return (0, 0);
    };
    children
        .flatten()
        .map(|child| measure(&child.path()))
        .fold((0, 0), |(bytes, files), (b, f)| (bytes + b, files + f))
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Where subagent views and eval trees are: the current two, then the two an
/// earlier release used, which the state migration leaves for this cleanup.
fn view_directories(root: &Path) -> [PathBuf; 4] {
    [
        root.join(workspace::VIEWS),
        root.join(workspace::EVAL),
        root.join(".arsy/views"),
        root.join(".arsy/eval"),
    ]
}

/// A view touched this recently may belong to a subagent still running in
/// another ARSY process, whose lease this one cannot see.
// ponytail: modification time stands in for the lease, which lives only in
// the owning process's memory; a lease file per view is the upgrade.
const VIEW_IDLE_MS: u128 = 30 * 60 * 1000;

/// Run one cleanup and say what it did.
pub fn clean(invocation: &Invocation, root: &Path, target: Target) -> Result<String, String> {
    match target {
        Target::Cache => {
            let cache = config::config_home()
                .ok_or("this platform has no ARSY configuration home")?
                .join(CACHE_DIRECTORY);
            let (bytes, _) = measure(&cache);
            remove(&cache)?;
            Ok(format!("Cleared the cache ({}).", human_bytes(bytes)))
        }
        Target::RepoMap => {
            let map = root.join(workspace::REPO_MAP);
            remove(&map)?;
            Ok("Removed the repository map; the next turn rebuilds it.".to_owned())
        }
        Target::Views => prune_views(root),
        Target::Artifacts => {
            let (references, objects) = crate::evidence::sweep(invocation, root)
                .map_err(|diagnostic| diagnostic.message)?;
            Ok(format!(
                "Removed {references} unreferenced artifact(s) and {objects} stored object(s)."
            ))
        }
    }
}

fn remove(path: &Path) -> Result<(), String> {
    let removed = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    };
    removed.map_err(|error| format!("{} could not be removed: {error}", path.display()))
}

/// Remove every view and eval tree nothing has touched for a while, through
/// git so the repository forgets the worktree too.
fn prune_views(root: &Path) -> Result<String, String> {
    let directories = view_directories(root);
    let views: Vec<std::fs::DirEntry> = directories
        .iter()
        .filter_map(|directory| std::fs::read_dir(directory).ok())
        .flat_map(|children| children.flatten())
        .collect();
    let (mut removed, mut kept) = (0, 0);
    for view in views {
        if !is_idle(&view) {
            kept += 1;
            continue;
        }
        let path = view.path();
        let through_git = Process::new("git")
            .arg("-C")
            .arg(root)
            .args(["worktree", "remove", "--force"])
            .arg(&path)
            .output()
            .is_ok_and(|output| output.status.success());
        if !through_git {
            remove(&path)?;
        }
        removed += 1;
    }
    // An emptied directory from an earlier release goes with its views.
    for legacy in &directories[2..] {
        let _ = std::fs::remove_dir(legacy);
    }
    let _ = Process::new("git")
        .arg("-C")
        .arg(root)
        .args(["worktree", "prune"])
        .output();
    Ok(match kept {
        0 => format!("Removed {removed} view(s)."),
        kept => format!(
            "Removed {removed} view(s); kept {kept} used in the last 30 minutes, which another ARSY may still be running in."
        ),
    })
}

/// Whether nothing has touched a view for [`VIEW_IDLE_MS`].
fn is_idle(view: &std::fs::DirEntry) -> bool {
    view.metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age.as_millis() >= VIEW_IDLE_MS)
}

/// The name a history reset must be confirmed with: the workspace directory's.
pub fn workspace_name(root: &Path) -> String {
    root.file_name().map_or_else(
        || root.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Delete the session store, once the operator has typed the workspace's name
/// and nothing is writing to it.
pub fn reset_history(root: &Path, confirm: &str) -> Result<String, String> {
    let expected = workspace_name(root);
    if confirm != expected {
        return Err(format!(
            "type the workspace name `{expected}` to confirm; nothing was deleted"
        ));
    }
    let store = root.join(workspace::SESSION_STORE);
    // A store that cannot even be opened is often why history is being reset,
    // and nothing can be writing to it: only a busy one stops the reset.
    if arsy_kernel::sqlite::is_being_written(&store).unwrap_or(false) {
        return Err(
            "another ARSY is writing to this session history; finish or stop it first".to_owned(),
        );
    }
    for suffix in ["-wal", "-shm", ""] {
        remove(&sidecar(&store, suffix))?;
    }
    Ok(format!(
        "Deleted the session history of `{expected}`. Artifacts it referenced go on the next `arsy gc --apply`."
    ))
}

/// `1.2 MB`, for a person.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn run(
    invocation: &Invocation,
    request: &Request,
    emitter: &mut Emitter,
) -> Result<i32, Diagnostic> {
    let root = crate::workspace_root(&invocation.workspace)?;
    let refused = |reason: String| {
        Diagnostic::error(
            "ARSY-CMP-1000",
            reason,
            "`arsy storage` lists what can be cleaned",
        )
    };
    let message = match request {
        Request::List => {
            let entries = inventory(&root);
            emitter.result(if emitter.output == Output::Json {
                json!({"workspace": root, "entries": entries.iter().map(entry_json).collect::<Vec<_>>()})
            } else {
                json!({"storage": listing(&entries)})
            });
            return Ok(0);
        }
        Request::Clean(target) => clean(invocation, &root, *target).map_err(refused)?,
        Request::ResetHistory { confirm } => {
            let confirm = confirm.as_deref().ok_or_else(|| {
                usage(format!(
                    "storage reset-history requires --confirm {}",
                    workspace_name(&root)
                ))
            })?;
            reset_history(&root, confirm).map_err(refused)?
        }
    };
    emitter.result(json!({"message": message}));
    Ok(0)
}

fn entry_json(entry: &Entry) -> Value {
    json!({
        "label": entry.label,
        "scope": entry.scope,
        "path": entry.path,
        "exists": entry.exists,
        "bytes": entry.bytes,
        "files": entry.files,
        "action": entry.action.map(|action| match action {
            Action::Clean(Target::Cache) => "clean cache",
            Action::Clean(Target::RepoMap) => "clean repo-map",
            Action::Clean(Target::Views) => "clean views",
            Action::Clean(Target::Artifacts) => "clean artifacts",
            Action::ResetHistory => "reset-history",
        }),
    })
}

fn listing(entries: &[Entry]) -> String {
    let mut text = String::new();
    for entry in entries {
        let size = if entry.exists {
            human_bytes(entry.bytes)
        } else {
            "—".to_owned()
        };
        text.push_str(&format!(
            "{:<8} {:<20} {:>9}  {}\n",
            entry.scope,
            entry.label,
            size,
            entry.path.display()
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_workspace_lists_every_location_as_absent() {
        let root = tempfile::tempdir().unwrap();
        let entries = inventory(root.path());
        let project: Vec<_> = entries.iter().filter(|e| e.scope == "project").collect();
        assert_eq!(project.len(), 8);
        assert!(project
            .iter()
            .all(|entry| !entry.exists && entry.bytes == 0));
    }

    #[test]
    fn the_store_counts_its_sidecars_and_old_views_count_as_views() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join(workspace::RUNTIME_STATE);
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.path().join(workspace::SESSION_STORE), "12345").unwrap();
        std::fs::write(state.join("sessions.sqlite3-wal"), "123").unwrap();
        std::fs::create_dir_all(root.path().join(".arsy/views/old")).unwrap();
        std::fs::write(root.path().join(".arsy/views/old/file"), "12").unwrap();

        let entries = inventory(root.path());
        let find = |label: &str| entries.iter().find(|e| e.label == label).unwrap();
        assert_eq!(find("session history").bytes, 8);
        assert_eq!(find("session history").files, 2);
        assert_eq!(find("subagent views").bytes, 2);
        assert!(find("subagent views").exists);
    }

    #[test]
    fn a_reset_needs_the_workspace_name_and_takes_the_sidecars() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join(workspace::RUNTIME_STATE);
        std::fs::create_dir_all(&state).unwrap();
        let store = root.path().join(workspace::SESSION_STORE);
        std::fs::write(&store, "db").unwrap();
        std::fs::write(state.join("sessions.sqlite3-wal"), "wal").unwrap();

        assert!(reset_history(root.path(), "wrong").is_err());
        assert!(store.exists(), "a wrong name deletes nothing");

        reset_history(root.path(), &workspace_name(root.path())).unwrap();
        assert!(!store.exists());
        assert!(!state.join("sessions.sqlite3-wal").exists());
    }

    #[test]
    fn a_recent_view_is_kept_and_an_old_layout_directory_goes() {
        let root = tempfile::tempdir().unwrap();
        let recent = root.path().join(workspace::VIEWS).join("running");
        std::fs::create_dir_all(&recent).unwrap();

        let said = prune_views(root.path()).unwrap();

        assert!(recent.is_dir(), "{said}");
        assert!(said.contains("kept 1"), "{said}");
    }

    #[test]
    fn sizes_read_for_a_person() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MB");
    }
}
