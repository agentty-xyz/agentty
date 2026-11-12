use std::collections::BTreeMap;
use std::num::NonZeroU64;

use ag_protocol::{ProtocolRequestProfile, QuestionItem, TurnPrompt};
use ag_session::{
    AgentDefinition, AgentKind, AgentModel, AgentSelection, EnvId, ExecutionGraph,
    ExecutionGraphProgress, ExecutionGraphStatus, GraphBlockedReason, GraphCompletionGate,
    GraphContext, GraphDependency, GraphExecutionProfile, GraphExecutionRef, GraphNode,
    GraphNodeProgress, GraphNodeStatus, GraphOutput, GraphPostProcessing, GraphResult,
    GraphResultRef, GraphResultState, GraphWorkspaceAccess,
};

use super::{
    GraphEditKind, GraphEditor, GraphView, GraphViewOutput, outputs_awaiting_acceptance,
    takes_revision_guidance,
};
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
    view.outputs = vec![GraphViewOutput::new(result, "Exact output body".into())];

    view
}

/// [`graph_view`] whose `second` node waits for acceptance of `first`'s
/// validated `response` output while a live driver dispatches.
pub(crate) fn acceptance_gated_view() -> GraphView {
    let mut view = graph_view();
    let gate = GraphCompletionGate::ResultAccepted {
        output_name: "response".into(),
    };
    view.graphs[0].nodes[1].dependencies = vec![GraphDependency {
        gate: gate.clone(),
        node_id: "first".into(),
    }];
    let progress = view.progress.as_mut().expect("progress");
    progress.status = ExecutionGraphStatus::Blocked;
    progress.nodes[1].blocked = vec![GraphBlockedReason::Dependency {
        gate,
        node_id: "first".into(),
    }];

    view
}

/// [`acceptance_gated_view`] with a `third` node that already started from
/// `first`'s execution, so `first`'s output is final while `second` still
/// waits for its acceptance.
pub(crate) fn consumed_gated_view() -> GraphView {
    let mut view = acceptance_gated_view();
    let mut third = view.graphs[0].nodes[1].clone();
    third.id = "third".into();
    third.dependencies = vec![GraphDependency {
        gate: GraphCompletionGate::ExecutionSucceeded,
        node_id: "first".into(),
    }];
    view.graphs[0].nodes.push(third);
    let progress = view.progress.as_mut().expect("progress");
    let mut started = progress.nodes[1].clone();
    started.blocked = Vec::new();
    started.executions = vec![GraphExecutionRef::TaskAttempt {
        attempt_id: "attempt-third".into(),
        operation_id: "operation-third".into(),
        task_id: "task-third".into(),
    }];
    started.node_id = "third".into();
    started.status = GraphNodeStatus::Running;
    progress.nodes.push(started);

    view
}

#[test]
fn revision_guidance_requires_an_unaccepted_unconsumed_task_conversation() {
    // Arrange
    let gated = acceptance_gated_view();
    let consumed = consumed_gated_view();
    let mut accepted = gated.clone();
    accepted.progress.as_mut().expect("progress").nodes[0].results[0].state =
        GraphResultState::Accepted;
    let mut fresh = gated.clone();
    fresh.graphs[0].nodes[0].execution.context = GraphContext::Fresh;

    // Act
    let takes = [&gated, &consumed, &accepted, &fresh].map(|view| {
        takes_revision_guidance(
            &view.graphs[0].nodes,
            view.progress.as_ref().expect("progress"),
            "first",
        )
    });
    let consumed_awaiting =
        outputs_awaiting_acceptance(consumed.progress.as_ref().expect("progress")).len();

    // Assert
    assert_eq!(takes, [true, false, false, false]);
    assert_eq!(consumed_awaiting, 1);
}

#[test]
fn outputs_awaiting_acceptance_require_a_pending_accepted_output_gate() {
    // Arrange
    let gated = acceptance_gated_view().progress.expect("gated progress");
    let mut accepted = gated.clone();
    accepted.nodes[0].results[0].state = GraphResultState::Accepted;
    let mut other_output = gated.clone();
    other_output.nodes[1].blocked = vec![GraphBlockedReason::Dependency {
        gate: GraphCompletionGate::ResultAccepted {
            output_name: "other".into(),
        },
        node_id: "first".into(),
    }];
    let mut validated_gate = gated.clone();
    validated_gate.nodes[1].blocked = vec![GraphBlockedReason::Dependency {
        gate: GraphCompletionGate::ResultValidated {
            output_name: "response".into(),
        },
        node_id: "first".into(),
    }];
    let mut running = gated.clone();
    running.nodes[0].status = GraphNodeStatus::Running;
    let mut ungated = graph_view().progress.expect("ungated progress");
    ungated.status = ExecutionGraphStatus::Blocked;
    let mut paused = gated.clone();
    paused.status = ExecutionGraphStatus::Paused;
    let mut cancelled = gated.clone();
    cancelled.status = ExecutionGraphStatus::Cancelled;
    let mut failed = gated.clone();
    failed.status = ExecutionGraphStatus::Failed;
    let mut parallel = gated.clone();
    parallel.status = ExecutionGraphStatus::Running;

    // Act
    let awaiting = [&gated, &parallel].map(outputs_awaiting_acceptance);
    let others = [
        accepted,
        other_output,
        validated_gate,
        running,
        ungated,
        paused,
        cancelled,
        failed,
    ]
    .map(|progress| outputs_awaiting_acceptance(&progress).len());

    // Assert
    for awaiting in awaiting {
        assert_eq!(awaiting.len(), 1);
        assert_eq!(awaiting[0].0.node_id, "first");
        assert_eq!(awaiting[0].1.reference.id, "result");
    }
    assert_eq!(others, [0; 8]);
}

#[test]
fn output_display_decodes_by_result_schema_without_changing_the_original() {
    // Arrange
    let result = graph_view().outputs.remove(0).result;
    let metadata =
        r#"{"title":"Title","description":"Details","is_title_change_significant":false}"#;
    let review = r#"{"project_impact":["Readable impact"],"suggestions":[]}"#;
    let cases = [
        (
            ProtocolRequestProfile::SessionTurn,
            r###"{"answer":"## Plan\n\n**Ready**\n\n```rust\nlet ready = true;\n```"}"###,
            "## Plan\n\n**Ready**\n\n```rust\nlet ready = true;\n```".into(),
        ),
        (
            ProtocolRequestProfile::SessionTurn,
            r#"{"answer":"Plan","questions":[{"text":"Which target?","options":["Local"]}]}"#,
            "Plan\n\nWhich target?".into(),
        ),
        (
            ProtocolRequestProfile::SessionTurn,
            r#"{"questions":[{"text":"Question only?"}]}"#,
            "Question only?".into(),
        ),
        (
            ProtocolRequestProfile::UtilityPrompt,
            r#"{"answer":"First\nSecond"}"#,
            "First\nSecond".into(),
        ),
        (
            ProtocolRequestProfile::SessionTurn,
            "Legacy raw output",
            "Legacy raw output".into(),
        ),
        (
            ProtocolRequestProfile::SessionTurn,
            "{broken",
            "{broken".into(),
        ),
        (ProtocolRequestProfile::UtilityPrompt, "", String::new()),
        (
            ProtocolRequestProfile::ReviewMetadata,
            "Unparseable metadata",
            "Unparseable metadata".into(),
        ),
    ];

    for (schema, body, expected) in cases {
        let mut result = result.clone();
        result.reference.schema = schema;

        // Act
        let output = GraphViewOutput::new(result.clone(), body.into());

        // Assert
        assert_eq!(output.display(), expected, "{schema:?}: {body}");
        assert_eq!(output.body, body);
        assert_eq!(output.result, result);
    }

    for (schema, body) in [
        (ProtocolRequestProfile::ReviewMetadata, metadata),
        (ProtocolRequestProfile::FocusedReview, review),
    ] {
        // Arrange
        let mut result = result.clone();
        result.reference.schema = schema;

        // Act
        let output = GraphViewOutput::new(result.clone(), body.into());

        // Assert
        let json = output
            .display()
            .strip_prefix("```json\n")
            .and_then(|text| text.strip_suffix("\n```"))
            .expect("structured content is fenced as JSON");
        assert!(json.contains("\n  \""), "structured content is indented");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(json).expect("display JSON"),
            serde_json::from_str::<serde_json::Value>(body).expect("original JSON"),
        );
        assert_eq!(output.body, body);
        assert_eq!(output.result, result);
    }
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
        "produced" => changed.outputs[0].result.state = GraphResultState::Produced,
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
fn draft_reconciliation_preserves_the_original_graph_and_execution() {
    // Arrange
    let mut original = graph_view();
    original.editor = Some(GraphEditor {
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
        "awaiting-answers",
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
            "awaiting-answers" => {
                // Chat replies answer questions; the draft never retargets.
                let node = &mut current.progress.as_mut().expect("progress").nodes[0];
                node.status = GraphNodeStatus::AwaitingAnswers;
                node.questions.push(QuestionItem::new("A new question?"));
            }
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
