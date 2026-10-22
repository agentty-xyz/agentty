/// A structural graph or exact-result binding violation.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GraphContractError {
    /// A required identity or name is blank.
    #[error("graph field `{0}` must not be blank")]
    EmptyField(&'static str),
    /// A graph must contain at least one node.
    #[error("graph must contain at least one node")]
    EmptyGraph,
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
