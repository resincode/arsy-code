//! Replayable Agent Hub projection.
//!
//! The hub owns no orchestration state. It renders the canonical task graph,
//! so rebuilding the graph after restart yields the same rows.

use crate::{
    domain::{AgentId, AttemptId, TaskId},
    orchestration::{AttemptState, Budget, TaskGraph, TaskState},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HubFilter {
    All,
    Blocked,
    Waiting,
    Active,
    Failed,
    Terminal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HubRow {
    pub task: TaskId,
    pub parent: Option<TaskId>,
    pub agent: Option<AgentId>,
    pub attempt: Option<AttemptId>,
    pub goal: String,
    pub role: String,
    pub task_state: TaskState,
    pub attempt_state: Option<AttemptState>,
    pub operation_class: Option<String>,
    pub elapsed_ms: u64,
    pub last_activity_ms: Option<u64>,
    pub budget: Budget,
    pub used: Budget,
    pub worktree: Option<String>,
    pub changed_files: Vec<String>,
    pub capabilities: Vec<String>,
    pub validation: String,
    pub proof: String,
    pub terminal_result: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentHub {
    pub rows: Vec<HubRow>,
}

impl AgentHub {
    pub fn project(graph: &TaskGraph, now_ms: u64, filter: HubFilter) -> Self {
        let mut rows: Vec<_> = graph
            .tasks()
            .filter_map(|task| {
                let attempt = task
                    .runtime
                    .current_attempt
                    .and_then(|id| graph.attempt(id));
                let assignment = attempt.and_then(|attempt| {
                    graph
                        .assignments()
                        .find(|assignment| assignment.attempt == attempt.id)
                });
                let writer = assignment.and_then(|assignment| graph.writer_result(assignment.id));
                let criteria = graph.criteria_of(task.id);
                let met = criteria
                    .iter()
                    .filter(|criterion| {
                        graph
                            .judgments_of(criterion.id)
                            .last()
                            .is_some_and(|judgment| judgment.met)
                    })
                    .count();
                let row = HubRow {
                    task: task.id,
                    parent: task.runtime.parent,
                    agent: attempt.map(|attempt| attempt.assignee).or(task.assignee),
                    attempt: attempt.map(|attempt| attempt.id),
                    goal: task.goal.clone(),
                    role: attempt.map_or_else(String::new, |attempt| attempt.role.clone()),
                    task_state: task.state,
                    attempt_state: attempt.map(|attempt| attempt.state),
                    operation_class: attempt.and_then(|attempt| {
                        graph.trace_of(attempt.id).last().and_then(|entry| {
                            entry
                                .get("operation")
                                .or_else(|| entry.get("kind"))
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned)
                        })
                    }),
                    elapsed_ms: attempt.map_or(0, |attempt| {
                        attempt
                            .ended_at_ms
                            .unwrap_or(now_ms)
                            .saturating_sub(attempt.started_at_ms)
                    }),
                    last_activity_ms: attempt
                        .map(|attempt| attempt.ended_at_ms.unwrap_or(attempt.started_at_ms)),
                    budget: task.budget,
                    used: task.runtime.used,
                    worktree: assignment.map(|assignment| assignment.view.clone()),
                    changed_files: writer
                        .map_or_else(Vec::new, |writer| writer.changed_files.clone()),
                    capabilities: attempt
                        .map(|attempt| {
                            attempt
                                .authority
                                .iter()
                                .map(|grant| grant.action.to_string())
                                .collect()
                        })
                        .unwrap_or_default(),
                    validation: format!("{met}/{}", criteria.len()),
                    proof: match task.state {
                        TaskState::Verified => "verified",
                        TaskState::Completed => "unverified",
                        _ => "pending",
                    }
                    .into(),
                    terminal_result: attempt.and_then(|attempt| attempt.result.clone()),
                };
                matches_filter(&row, filter).then_some(row)
            })
            .collect();
        rows.sort_by_key(sort_key);
        Self { rows }
    }
}

fn matches_filter(row: &HubRow, filter: HubFilter) -> bool {
    match filter {
        HubFilter::All => true,
        HubFilter::Blocked => row.task_state == TaskState::Blocked,
        HubFilter::Waiting => row.attempt_state == Some(AttemptState::Waiting),
        HubFilter::Active => row.attempt_state.is_some_and(AttemptState::is_live),
        HubFilter::Failed => row.task_state == TaskState::Failed,
        HubFilter::Terminal => matches!(
            row.task_state,
            TaskState::Completed | TaskState::Verified | TaskState::Failed | TaskState::Cancelled
        ),
    }
}

fn sort_key(row: &HubRow) -> (u8, TaskId) {
    let priority = if row.task_state == TaskState::Blocked {
        0
    } else if row.attempt_state == Some(AttemptState::Waiting) {
        1
    } else if row.attempt_state.is_some_and(AttemptState::is_live) {
        2
    } else if row.task_state == TaskState::Failed {
        3
    } else {
        4
    };
    (priority, row.task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{Principal, SessionId},
        event::MemoryEventStore,
        orchestration::{TaskNode, TaskRuntime, WorkspaceRequirement},
    };
    use std::sync::Arc;

    #[test]
    fn replay_builds_the_same_stably_ordered_hub() {
        let store = Arc::new(MemoryEventStore::default());
        let session = SessionId::new();
        let mut graph = TaskGraph::new(store.clone(), session, Principal::System).unwrap();
        let task = TaskId::new();
        graph
            .add(TaskNode {
                id: task,
                goal: "inspect the runtime".into(),
                dependencies: Vec::new(),
                assignee: None,
                required_output: "text".into(),
                workspace: WorkspaceRequirement::ReadOnlySnapshot,
                budget: Budget {
                    tokens: 100,
                    cost_micros: 20,
                    wall_ms: 1_000,
                },
                authority: Vec::new(),
                state: TaskState::Pending,
                lease_expires_at_ms: None,
                runtime: TaskRuntime::default(),
            })
            .unwrap();
        graph.ready().unwrap();
        graph.lease(task, AgentId::new(), 2_000).unwrap();

        let before = AgentHub::project(&graph, 500, HubFilter::All);
        let replayed = TaskGraph::new(store, session, Principal::System).unwrap();
        let after = AgentHub::project(&replayed, 500, HubFilter::All);
        assert_eq!(before, after);
        assert_eq!(after.rows.len(), 1);
        assert_eq!(after.rows[0].task_state, TaskState::Running);
    }
}
