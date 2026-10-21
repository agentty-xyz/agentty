//! Live execution-graph view model for task chains shown in session chat.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ag_session::{BuiltinAgentDefinition, TaskChain, TaskChainPhase};

use crate::domain::session::SessionId;

/// Maximum number of sessions whose latest graph stays cached.
pub(crate) const TASK_CHAIN_GRAPH_CACHE_LIMIT: usize = 8;

/// Delay before retrying a failed graph read for an unchanged session.
pub(crate) const TASK_CHAIN_GRAPH_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Label of the handoff edge between Planner and Implementer.
pub(crate) const ACCEPTED_PLAN_EDGE_LABEL: &str = "accepted plan";

/// User-facing status of one graph node.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum TaskChainNodeStatus {
    /// Role has not started because an upstream edge is unsatisfied.
    Waiting,
    /// Role attempt is queued or executing.
    Working,
    /// Role asked questions that need a user answer.
    NeedsInput,
    /// Role produced output that needs explicit user approval.
    AwaitingApproval,
    /// Role finished and its output was accepted or completed the chain.
    Done,
    /// Role attempt failed or was interrupted.
    Failed,
    /// Role attempt was canceled by the user.
    Canceled,
}

impl TaskChainNodeStatus {
    /// Returns the user-facing status label.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Waiting => "Waiting",
            Self::Working => "Working",
            Self::NeedsInput => "Needs your input",
            Self::AwaitingApproval => "Awaiting your approval",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Canceled => "Canceled",
        }
    }
}

/// One role node in the execution graph.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TaskChainGraphNode {
    /// Role represented by this node.
    pub(crate) agent: BuiltinAgentDefinition,
    /// Current node status.
    pub(crate) status: TaskChainNodeStatus,
}

/// Handoff edge from one node to the next node in `nodes` order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TaskChainGraphEdge {
    /// Whether the handoff condition has been met.
    pub(crate) is_satisfied: bool,
    /// Condition the downstream node waits for.
    pub(crate) label: &'static str,
}

/// Linear execution graph projected from one task-chain snapshot.
///
/// `edges[index]` connects `nodes[index]` to `nodes[index + 1]`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TaskChainGraph {
    /// Chain identity referenced by transcript graph markers.
    pub(crate) chain_id: String,
    /// Handoff edges between consecutive nodes.
    pub(crate) edges: Vec<TaskChainGraphEdge>,
    /// Role nodes in execution order.
    pub(crate) nodes: Vec<TaskChainGraphNode>,
}

impl TaskChainGraph {
    /// Projects the durable chain phase onto Planner and Implementer nodes.
    ///
    /// Failure and cancellation apply to the chain's current role; the other
    /// role is `Done` when the plan was accepted and `Waiting` otherwise.
    pub(crate) fn from_chain(chain: &TaskChain) -> Self {
        let is_plan_accepted = chain.accepted_plan.is_some();
        let (planner, implementer) = match chain.phase {
            TaskChainPhase::Planning => {
                (TaskChainNodeStatus::Working, TaskChainNodeStatus::Waiting)
            }
            TaskChainPhase::AwaitingPlanAcceptance => (
                TaskChainNodeStatus::AwaitingApproval,
                TaskChainNodeStatus::Waiting,
            ),
            TaskChainPhase::PlannerQuestion => (
                TaskChainNodeStatus::NeedsInput,
                TaskChainNodeStatus::Waiting,
            ),
            TaskChainPhase::Implementing => {
                (TaskChainNodeStatus::Done, TaskChainNodeStatus::Working)
            }
            TaskChainPhase::ImplementerQuestion => {
                (TaskChainNodeStatus::Done, TaskChainNodeStatus::NeedsInput)
            }
            TaskChainPhase::Failed => {
                Self::current_role_statuses(chain, TaskChainNodeStatus::Failed)
            }
            TaskChainPhase::Canceled => {
                Self::current_role_statuses(chain, TaskChainNodeStatus::Canceled)
            }
            TaskChainPhase::Succeeded => (TaskChainNodeStatus::Done, TaskChainNodeStatus::Done),
        };

        Self {
            chain_id: chain.id.clone(),
            edges: vec![TaskChainGraphEdge {
                is_satisfied: is_plan_accepted,
                label: ACCEPTED_PLAN_EDGE_LABEL,
            }],
            nodes: vec![
                TaskChainGraphNode {
                    agent: BuiltinAgentDefinition::Planner,
                    status: planner,
                },
                TaskChainGraphNode {
                    agent: BuiltinAgentDefinition::Implementer,
                    status: implementer,
                },
            ],
        }
    }

    /// Applies a terminal stage status to the chain's current role.
    fn current_role_statuses(
        chain: &TaskChain,
        status: TaskChainNodeStatus,
    ) -> (TaskChainNodeStatus, TaskChainNodeStatus) {
        match chain.current_request.assignment.definition {
            BuiltinAgentDefinition::Planner => (status, TaskChainNodeStatus::Waiting),
            BuiltinAgentDefinition::Implementer => (TaskChainNodeStatus::Done, status),
        }
    }
}

/// Bounded per-session cache of the latest task-chain graph.
///
/// Key: session id. Invalidation: an entry reloads when the session's reduced
/// update version differs from the version captured when its last load
/// started; failed reads retry after a short delay while retaining the graph.
/// Only one load per session is in flight. At most
/// [`TASK_CHAIN_GRAPH_CACHE_LIMIT`] recently loaded sessions are retained,
/// evicting the least recently started load.
#[derive(Debug, Default)]
pub(crate) struct TaskChainGraphCache {
    entries: VecDeque<TaskChainGraphEntry>,
}

impl TaskChainGraphCache {
    /// Returns the last loaded graph for one session, kept while a newer
    /// snapshot loads.
    pub(crate) fn graph(&self, session_id: &str) -> Option<&TaskChainGraph> {
        self.entry(session_id)?.graph.as_ref()
    }

    /// Returns whether a load for this update version should start.
    pub(crate) fn needs_load(&self, session_id: &str, update_version: u64, now: Instant) -> bool {
        self.entry(session_id).is_none_or(|entry| {
            !entry.is_loading
                && (entry.update_version != update_version
                    || entry.retry_at.is_some_and(|retry_at| now >= retry_at))
        })
    }

    /// Marks one session as loading the snapshot for `update_version`.
    pub(crate) fn begin_load(&mut self, session_id: &SessionId, update_version: u64) {
        let graph = self
            .entries
            .iter()
            .position(|entry| entry.session_id == *session_id)
            .and_then(|index| self.entries.remove(index))
            .and_then(|entry| entry.graph);
        self.entries.push_front(TaskChainGraphEntry {
            graph,
            is_loading: true,
            retry_at: None,
            session_id: session_id.clone(),
            update_version,
        });
        self.entries.truncate(TASK_CHAIN_GRAPH_CACHE_LIMIT);
    }

    /// Applies one finished load when it still matches the in-flight request.
    ///
    /// A failed load keeps the previous graph and schedules a retry; stale
    /// completions are ignored.
    pub(crate) fn complete_load(
        &mut self,
        session_id: &str,
        update_version: u64,
        result: Result<Option<TaskChainGraph>, String>,
        now: Instant,
    ) {
        let Some(entry) = self.entries.iter_mut().find(|entry| {
            entry.session_id == session_id
                && entry.is_loading
                && entry.update_version == update_version
        }) else {
            return;
        };
        entry.is_loading = false;
        match result {
            Ok(graph) => entry.graph = graph,
            Err(_) => entry.retry_at = Some(now + TASK_CHAIN_GRAPH_RETRY_DELAY),
        }
    }

    fn entry(&self, session_id: &str) -> Option<&TaskChainGraphEntry> {
        self.entries
            .iter()
            .find(|entry| entry.session_id == session_id)
    }
}

/// One session's latest graph and its load state.
#[derive(Debug)]
struct TaskChainGraphEntry {
    graph: Option<TaskChainGraph>,
    is_loading: bool,
    retry_at: Option<Instant>,
    session_id: SessionId,
    update_version: u64,
}

#[cfg(test)]
#[path = "task_chain_graph_test.rs"]
mod tests;
