use std::collections::BTreeMap;
use std::num::NonZeroU64;

use ag_protocol::{ProtocolRequestProfile, QuestionItem, TurnPrompt};
use ag_session::{
    AgentDefinition, AgentKind, AgentModel, AgentSelection, EnvId, ExecutionGraph,
    ExecutionGraphProgress, ExecutionGraphStatus, GraphCompletionGate, GraphContext,
    GraphDependency, GraphExecutionProfile, GraphExecutionRef, GraphNode, GraphNodeProgress,
    GraphNodeStatus, GraphOutput, GraphPostProcessing, GraphResult, GraphResultRef,
    GraphResultState, GraphWorkspaceAccess,
};

use super::{GraphEditKind, GraphEditor, GraphView};
use crate::domain::input::InputState;
use crate::presentation::app_mode::ConfirmationViewMode;

pub(crate) fn graph_view() -> GraphView {
    let mut view = GraphView::new(ConfirmationViewMode {
        session_id: "sess".into(),
        scroll_offset: Some(3),
    });
    let node = |id: &str| GraphNode {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        definition: AgentDefinition::bundled("planner").expect("planner"),
        dependencies: Vec::new(),
        env_id: EnvId::SessionWorktree("sess".into()),
        execution: GraphExecutionProfile {
            context: GraphContext::TaskConversation {
                task_id: format!("task-{id}"),
            },
            post_processing: GraphPostProcessing::None,
            response_profile: ProtocolRequestProfile::SessionTurn,
            shared_budget_id: Some("calls".into()),
            workspace_access: GraphWorkspaceAccess::SharedRead,
        },
        id: id.into(),
        inputs: Vec::new(),
        objective: format!("Inspect {id}"),
        outputs: vec![GraphOutput {
            name: "response".into(),
            schema: ProtocolRequestProfile::SessionTurn,
        }],
        request: TurnPrompt::from_agent_data("Inspect repository".into()),
    };
    let graph = ExecutionGraph {
        id: "graph".into(),
        nodes: vec![node("first"), node("second")],
        revision: NonZeroU64::MIN,
        session_id: "sess".into(),
    };
    let reference = GraphResultRef {
        execution: GraphExecutionRef::TaskAttempt {
            attempt_id: "attempt".into(),
            operation_id: "operation".into(),
            task_id: "task-first".into(),
        },
        graph_id: graph.id.clone(),
        graph_revision: graph.revision,
        id: "result".into(),
        node_id: "first".into(),
        output_name: "response".into(),
        schema: ProtocolRequestProfile::SessionTurn,
        version: NonZeroU64::MIN,
    };
    let result = GraphResult {
        reference,
        state: GraphResultState::Validated,
    };
    let progress = ExecutionGraphProgress {
        graph_id: graph.id.clone(),
        revision: graph.revision,
        session_id: graph.session_id.clone(),
        status: ExecutionGraphStatus::Paused,
        nodes: graph
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| GraphNodeProgress {
                blocked: Vec::new(),
                executions: if index == 0 {
                    vec![result.reference.execution.clone()]
                } else {
                    Vec::new()
                },
                last_error: None,
                node_id: node.id.clone(),
                questions: Vec::new(),
                results: if index == 0 {
                    vec![result.clone()]
                } else {
                    Vec::new()
                },
                status: if index == 0 {
                    GraphNodeStatus::Succeeded
                } else {
                    GraphNodeStatus::Pending
                },
            })
            .collect(),
    };
    view.graphs = vec![graph];
    view.progress = Some(progress);
    view.outputs = vec![(result, "Exact output body".into())];

    view
}

#[test]
fn graph_actions_respect_execution_authority_and_final_outputs() {
    // Arrange
    let view = graph_view();

    // Act / Assert
    assert!(view.can_accept() && view.can_continue() && view.can_resume() && view.can_cancel());
    assert!(!view.can_retry());
    for scenario in [
        "empty",
        "missing-node",
        "missing-progress",
        "missing-state-node",
        "focused-review",
        "fresh",
        "accepted",
        "consumed",
        "running",
        "queued",
        "cancelled-graph",
        "completed",
        "no-execution",
        "failed",
        "cancelled-node",
        "produced",
        "no-output",
    ] {
        let mut changed = view.clone();
        change_view(&mut changed, scenario);
        assert_eq!(
            changed.can_continue(),
            matches!(
                scenario,
                "completed" | "failed" | "cancelled-node" | "produced" | "no-output"
            ),
            "{scenario}"
        );
        assert_eq!(
            changed.can_retry(),
            matches!(scenario, "failed" | "cancelled-node"),
            "{scenario}"
        );
        assert_eq!(
            changed.can_resume(),
            !matches!(
                scenario,
                "focused-review" | "cancelled-graph" | "completed" | "missing-progress"
            ),
            "{scenario}"
        );
        assert_eq!(
            changed.can_cancel(),
            !matches!(
                scenario,
                "cancelled-graph" | "completed" | "missing-progress"
            ),
            "{scenario}"
        );
        let _ = changed.can_accept();
    }
}

fn change_view(changed: &mut GraphView, scenario: &str) {
    match scenario {
        "empty" => changed.graphs.clear(),
        "missing-node" => changed.selected_node = 9,
        "missing-progress" => changed.progress = None,
        "missing-state-node" => changed.progress.as_mut().expect("progress").nodes.clear(),
        "focused-review" => {
            changed.graphs[0].nodes[0].execution.response_profile =
                ProtocolRequestProfile::FocusedReview;
            changed.graphs[0].nodes[0].execution.workspace_access =
                GraphWorkspaceAccess::BackgroundRead;
        }
        "fresh" => changed.graphs[0].nodes[0].execution.context = GraphContext::Fresh,
        "accepted" => {
            changed.progress.as_mut().expect("progress").nodes[0].results[0].state =
                GraphResultState::Accepted;
        }
        "consumed" => {
            changed.graphs[0].nodes[1]
                .dependencies
                .push(GraphDependency {
                    node_id: "first".into(),
                    gate: GraphCompletionGate::ExecutionSucceeded,
                });
            let execution = changed.node().expect("node").executions[0].clone();
            changed.progress.as_mut().expect("progress").nodes[1]
                .executions
                .push(execution);
        }
        "running" => {
            changed.progress.as_mut().expect("progress").nodes[0].status = GraphNodeStatus::Running;
        }
        "queued" => {
            changed.progress.as_mut().expect("progress").nodes[0].status = GraphNodeStatus::Queued;
        }
        "cancelled-graph" => {
            changed.progress.as_mut().expect("progress").status = ExecutionGraphStatus::Cancelled;
        }
        "completed" => {
            changed.progress.as_mut().expect("progress").status = ExecutionGraphStatus::Completed;
        }
        "no-execution" => changed.progress.as_mut().expect("progress").nodes[0]
            .executions
            .clear(),
        "failed" => {
            changed.progress.as_mut().expect("progress").nodes[0].status = GraphNodeStatus::Failed;
        }
        "cancelled-node" => {
            changed.progress.as_mut().expect("progress").nodes[0].status =
                GraphNodeStatus::Cancelled;
        }
        "produced" => changed.outputs[0].0.state = GraphResultState::Produced,
        _ => changed.outputs.clear(),
    }
}

#[test]
fn resume_budgets_require_exact_names_and_positive_limits() {
    // Arrange
    let view = graph_view();

    // Act / Assert
    assert_eq!(
        view.parse_budgets(r#"{"calls":3}"#),
        view.parse_budgets("calls=3")
    );
    for text in ["{", r#"{"calls":0}"#, r#"{"extra":2}"#] {
        assert!(view.parse_budgets(text).is_err());
    }
    assert_eq!(
        view.parse_budgets("calls=3"),
        Ok(BTreeMap::from([("calls".into(), 3)]))
    );
    for text in [
        "calls",
        "calls=0",
        "calls=-1",
        "calls=no",
        "calls=1 calls=2",
        "other=2",
        "",
        "calls=2 extra=3",
    ] {
        assert!(view.parse_budgets(text).is_err(), "{text}");
    }
    let mut no_budgets = view;
    for node in &mut no_budgets.graphs[0].nodes {
        node.execution.shared_budget_id = None;
    }
    assert_eq!(no_budgets.parse_budgets(""), Ok(BTreeMap::new()));
}

#[test]
fn ordinary_static_review_nodes_keep_generic_lifecycle_controls() {
    // Arrange
    let mut view = graph_view();
    let node = &mut view.graphs[0].nodes[0];
    node.execution.context = GraphContext::Fresh;
    node.execution.response_profile = ProtocolRequestProfile::FocusedReview;
    view.progress.as_mut().expect("progress").nodes[0].status = GraphNodeStatus::Failed;

    // Act / Assert
    assert!(!view.is_focused_review());
    assert!(view.can_resume());
    assert!(view.can_retry());
}

#[test]
fn draft_reconciliation_preserves_the_original_graph_execution_and_questions() {
    // Arrange
    let mut original = graph_view();
    original.editor = Some(GraphEditor {
        answers: Vec::new(),
        input: InputState::with_text("Keep this guidance".into()),
        kind: GraphEditKind::Reply,
        needs_reconciliation: true,
    });

    // Act / Assert
    for scenario in [
        "same",
        "other-branch",
        "revision",
        "execution",
        "questions",
        "accepted",
        "missing-graph",
        "no-editor",
        "cancel",
        "resume",
        "cancelled",
        "running-resume",
    ] {
        let mut source = original.clone();
        let mut current = original.clone();
        match scenario {
            "other-branch" => {
                current.progress.as_mut().expect("progress").nodes[1].last_error =
                    Some("Branch progressed".into());
            }
            "revision" => current.graphs[0].revision = NonZeroU64::MIN.saturating_add(1),
            "execution" => current.progress.as_mut().expect("progress").nodes[0]
                .executions
                .push(GraphExecutionRef::TaskAttempt {
                    attempt_id: "new-attempt".into(),
                    operation_id: "new-operation".into(),
                    task_id: "task-first".into(),
                }),
            "questions" => current.progress.as_mut().expect("progress").nodes[0]
                .questions
                .push(QuestionItem::new("A different question?")),
            "accepted" => {
                current.progress.as_mut().expect("progress").nodes[0].results[0].state =
                    GraphResultState::Accepted;
            }
            "missing-graph" => current.graphs.clear(),
            "no-editor" => source.editor = None,
            "cancel" | "cancelled" => {
                source.editor.as_mut().expect("editor").kind = GraphEditKind::Cancel;
                if scenario == "cancelled" {
                    current.progress.as_mut().expect("progress").status =
                        ExecutionGraphStatus::Cancelled;
                }
            }
            "resume" | "running-resume" => {
                source.editor.as_mut().expect("editor").kind = GraphEditKind::Resume;
                if scenario == "running-resume" {
                    current.progress.as_mut().expect("progress").status =
                        ExecutionGraphStatus::Running;
                }
            }
            _ => {}
        }
        assert_eq!(
            source.can_reconcile_editor(&current),
            matches!(scenario, "same" | "other-branch" | "cancel" | "resume"),
            "{scenario}"
        );
    }
}
