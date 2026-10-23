use std::num::NonZeroU64;

use ag_protocol::ProtocolRequestProfile;
use serde::{Deserialize, Serialize};

use super::GraphContractError;

/// Link to existing execution records; never a second execution status.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GraphExecutionRef {
    /// One task attempt and its authoritative ordered workflow operation.
    TaskAttempt {
        /// Unique attempt identity, distinct from task and operation IDs.
        attempt_id: String,
        /// Operation that includes turn execution and host post-processing.
        operation_id: String,
        /// Task owning the attempt and conversation.
        task_id: String,
    },
    /// One worker-supervised isolated utility execution.
    UtilityRun {
        /// Existing durable utility run identity; provider retries stay inside
        /// it.
        run_id: String,
    },
}

impl GraphExecutionRef {
    /// Checks required identity fields without asserting record existence.
    ///
    /// # Errors
    /// Rejects blank attempt, operation, task, or utility-run identities.
    pub fn validate(&self) -> Result<(), GraphContractError> {
        match self {
            Self::TaskAttempt {
                attempt_id,
                operation_id,
                task_id,
            } => {
                GraphContractError::require(attempt_id, "attempt_id")?;
                GraphContractError::require(operation_id, "operation_id")?;
                GraphContractError::require(task_id, "task_id")?;
            }
            Self::UtilityRun { run_id } => GraphContractError::require(run_id, "run_id")?,
        }

        Ok(())
    }
}

/// Immutable address of one persisted output version, excluding its body.
///
/// The host must retain the body and verify execution ownership. Revisions and
/// retries never retarget this reference to a newer response. Validation of a
/// caller-constructed reference is not proof that storage contains it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphResultRef {
    /// Existing task-attempt or utility-run authority that produced this
    /// result.
    pub execution: GraphExecutionRef,
    /// Owning graph instance.
    pub graph_id: String,
    /// Exact graph revision under which the producer was admitted.
    pub graph_revision: NonZeroU64,
    /// Host-allocated immutable result identity.
    pub id: String,
    /// Producing node, scoped by graph identity and revision.
    pub node_id: String,
    /// Declared output on the producing node.
    pub output_name: String,
    /// Protocol schema for the retained output body.
    pub schema: ProtocolRequestProfile,
    /// Immutable version within the producing node's named output.
    pub version: NonZeroU64,
}

impl GraphResultRef {
    /// Checks reference shape; storage ownership and lifecycle remain host
    /// checks.
    ///
    /// # Errors
    /// Returns an error when any required identity or name is blank.
    pub fn validate(&self) -> Result<(), GraphContractError> {
        GraphContractError::require(&self.graph_id, "graph_id")?;
        GraphContractError::require(&self.id, "result_id")?;
        GraphContractError::require(&self.node_id, "node_id")?;
        GraphContractError::require(&self.output_name, "output_name")?;

        self.execution.validate()
    }
}

/// Host-attested output state, separate from attempt/run lifecycle.
///
/// Advancing state requires authoritative evidence for this exact reference.
/// Caller-supplied state must never establish validation or user acceptance.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GraphResultState {
    /// Saved output, potentially incomplete or containing clarification.
    Produced,
    /// Schema-valid output from settled, successful, question-free execution.
    Validated,
    /// Explicitly accepted validated version; later results are not accepted.
    Accepted,
}

/// Snapshot supplied by the host's authoritative result store.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphResult {
    /// Immutable producing execution, revision, schema, and version.
    pub reference: GraphResultRef,
    /// Validation/acceptance evidence for this exact version.
    pub state: GraphResultState,
}

/// Exact input captured with a newly admitted execution.
///
/// Redelivery must retain this reference, even if a newer result is available.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphResolvedInput {
    /// Declared input slot on the consumer.
    pub name: String,
    /// Exact producer output; never an implicit "latest" query at execution.
    pub result: GraphResultRef,
}

#[cfg(test)]
#[path = "result_test.rs"]
mod tests;
