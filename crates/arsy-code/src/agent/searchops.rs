//! Repository navigation as operations: find files by name, find text inside
//! them.
//!
//! These exist so the model is not driven to `bash grep` for the two questions
//! it asks most. A shell answer to either is unbounded, unsanitised, and needs
//! `process.exec` authority to read a file — which makes read-only work
//! indistinguishable from arbitrary execution in the audit trail.

use crate::{
    operations::Directories,
    resource::{locate, Location, ResolveError, Workspace},
    search::{SearchError, SearchResults},
};
use arsy_kernel::{
    artifact::ArtifactStore,
    capability::{CapabilityAction, CapabilityGrant},
    domain::ResourceRef,
    operation::{
        ConcurrencyRule, Effect, Idempotency, InputSchema, JsonType, OperationContract,
        OperationError, OperationExecutor, OperationKind, OperationOutcome, OperationRequest,
    },
};
use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Defaults that keep one call's answer inside a turn. A caller that wants more
/// asks for more; a caller that forgets does not lose the turn to one search.
const DEFAULT_LIMIT: u64 = 100;
const MAX_LIMIT: u64 = 1_000;
const MAX_FILES_WALKED: usize = 20_000;
const MAX_SEARCHED_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchOperation {
    Text,
    Files,
}

impl SearchOperation {
    pub const ALL: [Self; 2] = [Self::Text, Self::Files];

    const fn kind(self) -> &'static str {
        match self {
            Self::Text => "search.text",
            Self::Files => "search.files",
        }
    }

    const fn query_field(self) -> &'static str {
        match self {
            Self::Text => "query",
            Self::Files => "pattern",
        }
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextHit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchOutcome {
    pub query: String,
    /// Present for `search.text`, empty for `search.files`.
    pub hits: Vec<TextHit>,
    /// Present for `search.files`, empty for `search.text`.
    pub paths: Vec<String>,
    /// Whether a limit stopped the walk before it ran out of candidates.
    pub truncated: bool,
}

pub struct SearchExecutor {
    operation: SearchOperation,
    contract: OperationContract,
    workspace: PathBuf,
    artifacts: Arc<dyn ArtifactStore>,
    retain_until_ms: u64,
    /// Directories the operator added beside the workspace; a `path` under one
    /// searches it instead of the workspace.
    directories: Directories,
}

impl SearchExecutor {
    pub fn new(
        operation: SearchOperation,
        workspace: &Workspace,
        artifacts: Arc<dyn ArtifactStore>,
        retain_until_ms: u64,
        directories: Directories,
    ) -> Arc<Self> {
        Arc::new(Self {
            operation,
            contract: OperationContract {
                kind: OperationKind::new(operation.kind()).expect("static operation kind is valid"),
                input_schema: InputSchema {
                    required: BTreeMap::from([(
                        operation.query_field().to_owned(),
                        JsonType::String,
                    )]),
                    optional: BTreeMap::from([
                        ("limit".to_owned(), JsonType::Number),
                        ("path".to_owned(), JsonType::String),
                    ]),
                    allow_extra: false,
                },
                actions: vec![CapabilityAction::FsRead],
                idempotency: Idempotency::Idempotent,
                reversible: true,
                concurrency: ConcurrencyRule::Parallel,
            },
            workspace: workspace.path().to_owned(),
            artifacts,
            retain_until_ms,
            directories,
        })
    }
}

impl OperationExecutor for SearchExecutor {
    fn contract(&self) -> &OperationContract {
        &self.contract
    }

    fn execute(
        &self,
        request: &OperationRequest,
        grants: &[CapabilityGrant],
    ) -> Result<OperationOutcome, OperationError> {
        let (workspace, prefix) = self.searched(
            request
                .input
                .get("path")
                .and_then(serde_json::Value::as_str),
            grants,
        )?;
        let query = request
            .input
            .get(self.operation.query_field())
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let limit = request
            .input
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT);

        let outcome = match self.operation {
            SearchOperation::Text => {
                let SearchResults { hits, truncated } = workspace
                    .search(
                        &query,
                        usize::try_from(limit).unwrap_or(usize::MAX),
                        MAX_FILES_WALKED,
                        MAX_SEARCHED_FILE_BYTES,
                    )
                    .map_err(search)?;
                SearchOutcome {
                    query: query.clone(),
                    hits: hits
                        .into_iter()
                        .map(|hit| TextHit {
                            path: prefixed(prefix.as_deref(), hit.resource.value()),
                            line: hit.line,
                            text: hit.text,
                        })
                        .collect(),
                    paths: Vec::new(),
                    truncated,
                }
            }
            SearchOperation::Files => {
                let (paths, truncated) = find_files(&workspace, &query, limit)?;
                let paths = paths
                    .iter()
                    .map(|path| prefixed(prefix.as_deref(), path))
                    .collect();
                SearchOutcome {
                    query: query.clone(),
                    hits: Vec::new(),
                    paths,
                    truncated,
                }
            }
        };

        let value = super::store(
            self.artifacts.as_ref(),
            &outcome,
            request.actor.clone(),
            self.retain_until_ms,
        )?;

        Ok(OperationOutcome {
            value: Some(value),
            observed_effects: vec![Effect {
                action: CapabilityAction::FsRead,
                resource: ResourceRef::new("workspace", "*").expect("a static scheme and value"),
            }],
            evidence: Vec::new(),
            state: None,
        })
    }
}

impl SearchExecutor {
    /// The tree a call searches, and the prefix its results carry.
    ///
    /// No `path`, or one inside the workspace, searches the whole workspace
    /// with workspace-relative results, as it always has. A `path` under an
    /// added directory — or outside every root, when the operator approved
    /// that exact path — searches that directory and reports absolute paths,
    /// so `fs.read` can open a hit as written. The directory is canonicalized
    /// and must still lie under the root it was found in, which is what stops
    /// a symlinked directory from widening the search.
    fn searched(
        &self,
        path: Option<&str>,
        grants: &[CapabilityGrant],
    ) -> Result<(Workspace, Option<String>), OperationError> {
        let execution = |error: std::io::Error| OperationError::Execution(error.to_string());
        let refused = || OperationError::Schema(ResolveError::OutsideWorkspace.to_string());
        let Some(path) = path.filter(|path| crate::resource::escapes(Path::new(path))) else {
            return Ok((Workspace::open(&self.workspace).map_err(execution)?, None));
        };
        let location = locate(&self.directories.roots(true), &self.workspace, path)
            .map_err(|error| OperationError::Schema(error.to_string()))?;
        let (root, searched) = match location {
            Location::Inside => {
                return Ok((Workspace::open(&self.workspace).map_err(execution)?, None))
            }
            Location::Rooted(root, relative) => {
                let searched = Workspace::open(root.path().join(relative)).map_err(execution)?;
                (root.path().to_path_buf(), searched)
            }
            Location::Undeclared(absolute) => {
                let resource = ResourceRef::new(CapabilityAction::FsRead.default_scheme(), path)
                    .map_err(|error| OperationError::Schema(error.to_string()))?;
                if !grants.iter().any(|grant| {
                    grant.action == CapabilityAction::FsRead && grant.names_exactly(&resource)
                }) {
                    return Err(refused());
                }
                let searched = Workspace::open(&absolute).map_err(execution)?;
                (absolute, searched)
            }
        };
        if !searched.path().starts_with(&root) {
            return Err(refused());
        }
        let prefix = searched.path().to_string_lossy().replace('\\', "/");
        Ok((searched, Some(prefix)))
    }
}

/// A result path as the caller can read it back.
fn prefixed(prefix: Option<&str>, path: &str) -> String {
    prefix.map_or_else(|| path.to_owned(), |prefix| format!("{prefix}/{path}"))
}

/// Match `pattern` against workspace-relative paths.
///
/// A pattern with no `/` is matched against the file name as well, because
/// `*.rs` is what a caller means by "Rust files anywhere" and matching it only
/// against the full path would return nothing outside the root.
fn find_files(
    workspace: &Workspace,
    pattern: &str,
    limit: u64,
) -> Result<(Vec<String>, bool), OperationError> {
    if pattern.is_empty() {
        return Err(OperationError::Schema("pattern must not be empty".into()));
    }
    // `*` stops at a separator and `**` crosses one, so `src/*.rs` names the
    // files directly under `src` rather than the whole subtree — the same
    // reading `ResourcePattern` gives a policy glob, so a pattern does not mean
    // two different things depending on where it is written.
    let matcher: GlobMatcher = GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map_err(|error| OperationError::Schema(error.to_string()))?
        .compile_matcher();
    let by_name = !pattern.contains('/');

    let mut paths = Vec::new();
    let mut walked = 0usize;
    let mut truncated = false;
    for entry in crate::resource::walk(workspace.path()) {
        let entry = entry.map_err(|error| OperationError::Execution(error.to_string()))?;
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        walked += 1;
        if walked > MAX_FILES_WALKED {
            truncated = true;
            break;
        }
        let Ok(relative) = entry.path().strip_prefix(workspace.path()) else {
            continue;
        };
        let matched = matcher.is_match(relative)
            || (by_name
                && relative
                    .file_name()
                    .is_some_and(|name| matcher.is_match(name)));
        if !matched {
            continue;
        }
        if paths.len() as u64 == limit {
            truncated = true;
            break;
        }
        paths.push(relative.to_string_lossy().replace('\\', "/"));
    }
    paths.sort();
    Ok((paths, truncated))
}

fn search(error: SearchError) -> OperationError {
    match error {
        SearchError::EmptyNeedle => OperationError::Schema(error.to_string()),
        other => OperationError::Execution(other.to_string()),
    }
}

pub fn executors(
    workspace: &Workspace,
    artifacts: &Arc<dyn ArtifactStore>,
    retain_until_ms: u64,
    directories: &Directories,
) -> Vec<Arc<dyn OperationExecutor>> {
    SearchOperation::ALL
        .into_iter()
        .map(|operation| {
            SearchExecutor::new(
                operation,
                workspace,
                Arc::clone(artifacts),
                retain_until_ms,
                directories.clone(),
            ) as Arc<dyn OperationExecutor>
        })
        .collect()
}
