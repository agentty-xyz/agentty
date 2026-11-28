//! Execution graph contract violations.

/// A structural graph or exact-result binding violation.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GraphContractError {
    /// A required identity or name is blank.
    #[error("graph field `{0}` must not be blank")]
    EmptyField(&'static str),
    /// A graph must contain at least one node.
    #[error("graph must contain at least one node")]
    EmptyGraph,
    /// An expansion must append at least one node.
    #[error("graph expansion must append at least one node")]
    EmptyExpansion,
    /// The graph has no revision number left for another revision.
    #[error("graph revision counter is exhausted")]
    RevisionExhausted,
    /// A scoped identity, dependency, or name occurs more than once.
    #[error("duplicate graph {kind}: {id}")]
    Duplicate {
        /// Identity scope that requires uniqueness.
        kind: &'static str,
        /// Conflicting value.
        id: String,
    },
    /// An edge or input references a missing node.
    #[error("unknown graph node: {0}")]
    UnknownNode(String),
    /// A gate or binding references an undeclared output.
    #[error("unknown output `{output}` of graph node `{node}`")]
    UnknownOutput {
        /// Producing node.
        node: String,
        /// Missing output name.
        output: String,
    },
    /// A child names a parent that is not a node of the graph.
    #[error(
        "graph node `{node}` names parent `{parent}`, which is not a node of this graph; name a \
         top-level node of this graph as its parent, or give it no parent"
    )]
    UnknownParent {
        /// Child naming the parent.
        node: String,
        /// Missing parent identity.
        parent: String,
    },
    /// A node names itself as its parent.
    #[error(
        "graph node `{0}` names itself as its parent; name another top-level node of this graph \
         as its parent, or give it no parent"
    )]
    SelfParent(String),
    /// A child names a parent that is itself a child; graphs have two levels.
    #[error(
        "graph node `{node}` cannot be a child of `{parent}`, which is itself a child; children \
         cannot have children, so name a top-level node as its parent"
    )]
    NestedParent {
        /// Child naming the parent.
        node: String,
        /// Parent that is itself a child.
        parent: String,
    },
    /// A child is bound to another Env than its parent. Proposal resolution
    /// binds children to their parent's Env, so only a host that builds a
    /// graph itself can cause this.
    #[error(
        "child node `{node}` is bound to another environment than its parent `{parent}`; a host \
         must bind a child to its parent's environment"
    )]
    ParentEnvMismatch {
        /// Child in another environment.
        node: String,
        /// Parent whose environment it must share.
        parent: String,
    },
    /// A dependency cycle, including a self-dependency, exists.
    #[error("graph dependencies contain a cycle")]
    Cycle,
    /// An input lacks a validation or acceptance gate for its exact output.
    #[error("input `{0}` requires a dependency gate for its producing output")]
    UngatedInput(String),
    /// The execution contract combines incompatible requirements.
    #[error("invalid graph execution profile: {0}")]
    InvalidProfile(&'static str),
    /// An execution environment is not owned by the graph's session.
    #[error("graph node environment belongs to a different session")]
    ForeignEnv,
    /// A node references a task worktree, which graphs cannot dispatch yet.
    #[error("graph nodes cannot run in task worktrees yet")]
    UnsupportedEnv,
    /// A runtime result does not match the declared input and producer.
    #[error("graph result does not match {0}")]
    ResultMismatch(&'static str),
    /// A result has not reached the binding's declared gate.
    #[error("graph result has not satisfied the input gate")]
    UnsatisfiedGate,
}

impl GraphContractError {
    /// Checks a required string without normalizing its durable identity.
    pub(super) fn require(value: &str, field: &'static str) -> Result<(), Self> {
        if value.trim().is_empty() {
            return Err(Self::EmptyField(field));
        }

        Ok(())
    }
}
