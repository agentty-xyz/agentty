use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroU64;

use super::{
    GraphCompletionGate, GraphContext, GraphContractError, GraphExecutionRef, GraphNode,
    GraphPostProcessing, GraphResolvedInput, GraphResult, GraphResultState,
};
use crate::{EnvId, SessionId};

/// A revision of an arbitrary static execution graph owned by one session.
///
/// These contracts do not persist, schedule, or execute nodes. Hosts validate
/// before admission and enforce budgets, record ownership, permissions, Env
/// conflicts, and existing worker capacity. A revision is a stale-action fence,
/// not authorization to mutate started work or automatically replay after
/// restart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionGraph {
    /// Stable graph-instance identity, separate from command delivery identity.
    pub id: String,
    /// Nodes in caller order; validation never reorders or dispatches them.
    pub nodes: Vec<GraphNode>,
    /// Nonzero immutable revision captured by every admitted execution/result.
    pub revision: NonZeroU64,
    /// Session owning this graph and its currently supported shared local Env.
    pub session_id: SessionId,
}

impl ExecutionGraph {
    /// Checks identities, execution profiles, local references, and acyclicity.
    ///
    /// # Errors
    /// Rejects empty graphs, blank or duplicate identities, foreign Envs,
    /// incompatible schemas/profiles, missing nodes/outputs, ungated inputs,
    /// duplicate edges, and cycles. No storage or runtime is consulted.
    pub fn validate(&self) -> Result<(), GraphContractError> {
        GraphContractError::require(&self.id, "graph_id")?;
        GraphContractError::require(self.session_id.as_str(), "session_id")?;
        if self.nodes.is_empty() {
            return Err(GraphContractError::EmptyGraph);
        }

        let mut nodes = HashMap::new();
        let mut tasks = HashSet::new();
        for node in &self.nodes {
            Self::validate_node(node)?;
            if nodes.insert(node.id.as_str(), node).is_some() {
                return Err(Self::duplicate("node", &node.id));
            }
            if node.env_id != EnvId::SessionWorktree(self.session_id.clone()) {
                return Err(GraphContractError::ForeignEnv);
            }
            if let GraphContext::TaskConversation { task_id } = &node.execution.context
                && !tasks.insert(task_id)
            {
                return Err(Self::duplicate("task", task_id));
            }
        }
        for node in &self.nodes {
            Self::validate_dependencies(node, &nodes)?;
        }

        self.validate_acyclic()
    }

    /// Pins one declared input to a host-loaded, validated or accepted version.
    ///
    /// This checks shape and consistency only. Hosts must load result state
    /// from authoritative storage, validate all dependency gates atomically at
    /// dispatch, and persist every resolved input with the execution admission.
    ///
    /// # Errors
    /// Rejects invalid graphs, undeclared inputs, foreign or stale results,
    /// schema/execution mismatches, and unsatisfied result gates.
    pub fn bind_input(
        &self,
        node_id: &str,
        input_name: &str,
        result: &GraphResult,
    ) -> Result<GraphResolvedInput, GraphContractError> {
        self.validate()?;
        result.reference.validate()?;
        let consumer = self.node(node_id)?;
        let binding = consumer
            .inputs
            .iter()
            .find(|input| input.name == input_name)
            .ok_or(GraphContractError::ResultMismatch("declared input"))?;
        let reference = &result.reference;
        if reference.graph_id != self.id || reference.graph_revision != self.revision {
            return Err(GraphContractError::ResultMismatch(
                "graph identity or revision",
            ));
        }
        if reference.node_id != binding.producer_node_id
            || reference.output_name != binding.output_name
        {
            return Err(GraphContractError::ResultMismatch("producer or output"));
        }
        let producer = self.node(&reference.node_id)?;
        if reference.schema != producer.execution.response_profile {
            return Err(GraphContractError::ResultMismatch("output schema"));
        }
        match (&producer.execution.context, &reference.execution) {
            (GraphContext::Fresh, GraphExecutionRef::UtilityRun { .. }) => {}
            (
                GraphContext::TaskConversation { task_id },
                GraphExecutionRef::TaskAttempt {
                    task_id: producing_task,
                    ..
                },
            ) if task_id == producing_task => {}
            _ => return Err(GraphContractError::ResultMismatch("execution authority")),
        }

        for dependency in &consumer.dependencies {
            if dependency.node_id == reference.node_id
                && dependency.gate.output_name() == Some(reference.output_name.as_str())
            {
                let satisfied = match dependency.gate {
                    GraphCompletionGate::ResultAccepted { .. } => {
                        result.state == GraphResultState::Accepted
                    }
                    _ => result.state != GraphResultState::Produced,
                };
                if !satisfied {
                    return Err(GraphContractError::UnsatisfiedGate);
                }
            }
        }

        Ok(GraphResolvedInput {
            name: input_name.to_string(),
            result: reference.clone(),
        })
    }

    /// Looks up a node using the same scoped error for consumers and producers.
    fn node(&self, id: &str) -> Result<&GraphNode, GraphContractError> {
        self.nodes
            .iter()
            .find(|node| node.id == id)
            .ok_or_else(|| GraphContractError::UnknownNode(id.to_string()))
    }

    /// Validates fields owned by a single node without following edges.
    fn validate_node(node: &GraphNode) -> Result<(), GraphContractError> {
        GraphContractError::require(&node.id, "node_id")?;
        GraphContractError::require(&node.definition.id, "definition.id")?;
        GraphContractError::require(&node.definition.prompt, "definition.prompt")?;
        GraphContractError::require(&node.objective, "objective")?;
        GraphContractError::require(&node.request.text, "request")?;
        node.execution.validate()?;
        if node.definition.read_only
            && node.execution.post_processing == GraphPostProcessing::SessionWorkflow
        {
            return Err(GraphContractError::InvalidProfile(
                "read-only definitions cannot perform host mutation effects",
            ));
        }
        let mut outputs = HashSet::new();
        for output in &node.outputs {
            GraphContractError::require(&output.name, "output_name")?;
            if !outputs.insert(&output.name) {
                return Err(Self::duplicate("output", &output.name));
            }
            if output.schema != node.execution.response_profile {
                return Err(GraphContractError::InvalidProfile(
                    "output schema must match the producing response profile",
                ));
            }
        }
        let mut inputs = HashSet::new();
        for input in &node.inputs {
            GraphContractError::require(&input.name, "input_name")?;
            GraphContractError::require(&input.producer_node_id, "producer_node_id")?;
            GraphContractError::require(&input.output_name, "output_name")?;
            if !inputs.insert(&input.name) {
                return Err(Self::duplicate("input", &input.name));
            }
        }

        Ok(())
    }

    /// Validates each gate and requires a matching gate for every result input.
    fn validate_dependencies(
        node: &GraphNode,
        nodes: &HashMap<&str, &GraphNode>,
    ) -> Result<(), GraphContractError> {
        let mut dependencies = HashSet::new();
        for dependency in &node.dependencies {
            let producer = nodes
                .get(dependency.node_id.as_str())
                .ok_or_else(|| GraphContractError::UnknownNode(dependency.node_id.clone()))?;
            if !dependencies.insert(dependency) {
                return Err(Self::duplicate("dependency", &dependency.node_id));
            }
            if let Some(output_name) = dependency.gate.output_name() {
                Self::require_output(producer, output_name)?;
            }
        }
        for input in &node.inputs {
            let producer = nodes
                .get(input.producer_node_id.as_str())
                .ok_or_else(|| GraphContractError::UnknownNode(input.producer_node_id.clone()))?;
            Self::require_output(producer, &input.output_name)?;
            if !node.dependencies.iter().any(|dependency| {
                dependency.node_id == input.producer_node_id
                    && dependency.gate.output_name() == Some(input.output_name.as_str())
            }) {
                return Err(GraphContractError::UngatedInput(input.name.clone()));
            }
        }

        Ok(())
    }

    /// Reports undeclared outputs consistently for both gates and bindings.
    fn require_output(node: &GraphNode, name: &str) -> Result<(), GraphContractError> {
        if !node.outputs.iter().any(|output| output.name == name) {
            return Err(GraphContractError::UnknownOutput {
                node: node.id.clone(),
                output: name.to_string(),
            });
        }

        Ok(())
    }

    /// Kahn traversal avoids recursion for caller-supplied graph depth.
    fn validate_acyclic(&self) -> Result<(), GraphContractError> {
        let mut remaining: Vec<usize> = self
            .nodes
            .iter()
            .map(|node| node.dependencies.len())
            .collect();
        let mut consumers: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, node) in self.nodes.iter().enumerate() {
            for dependency in &node.dependencies {
                consumers
                    .entry(&dependency.node_id)
                    .or_default()
                    .push(index);
            }
        }
        let mut ready: VecDeque<&str> = self
            .nodes
            .iter()
            .filter(|node| node.dependencies.is_empty())
            .map(|node| node.id.as_str())
            .collect();
        let mut visited = 0;
        while let Some(node_id) = ready.pop_front() {
            visited += 1;
            for &consumer in consumers.get(node_id).into_iter().flatten() {
                remaining[consumer] -= 1;
                if remaining[consumer] == 0 {
                    ready.push_back(&self.nodes[consumer].id);
                }
            }
        }
        if visited != self.nodes.len() {
            return Err(GraphContractError::Cycle);
        }

        Ok(())
    }

    /// Constructs a scoped uniqueness error without losing the conflicting ID.
    fn duplicate(kind: &'static str, id: &str) -> GraphContractError {
        GraphContractError::Duplicate {
            kind,
            id: id.to_string(),
        }
    }
}

#[cfg(test)]
#[path = "graph_test.rs"]
mod tests;
