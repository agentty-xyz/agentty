use std::collections::BTreeMap;
use std::sync::Arc;

use ag_session::{GraphNodeContinuation, GraphNodeStatus};
use ag_store::{DbError, MockGraphRepository, StoredGraphResult};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::GraphViewAction;
use crate::presentation::app_mode::AppMode;
use crate::presentation::execution_graph::tests::graph_view;
use crate::runtime::mode::execution_graph;
use crate::test_support::new_test_app;

#[path = "graph_view_draft_test.rs"]
mod draft;

#[tokio::test]
async fn proposal_inspector_admits_no_actions_for_the_displayed_proposal() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    crate::app::execution_graph::tests::seed_session(&mut app.sessions, app.services.db()).await;
    app.experimental_graph = true;
    let state = ag_session::test_support::graph_proposal_state("preview", "sess");
    app.services
        .db()
        .graph_proposals()
        .create(&state)
        .await
        .expect("create");
    let mut view = graph_view();
    view.graphs.clear();
    view.progress = None;
    app.load_graph_view(&mut view).await.expect("load proposal");

    // Act
    let result = app
        .apply_graph_view_action(&view, GraphViewAction::Cancel)
        .await;

    // Assert
    assert_eq!(view.selected_proposal(), Some(&state));
    assert_eq!(result, Err("No graph selected".into()));
    let stored = app
        .services
        .db()
        .graph_proposals()
        .get("preview", None)
        .await
        .expect("read")
        .expect("proposal");
    assert_eq!(stored, state);
}

#[tokio::test]
async fn inspector_selects_the_approved_graph_and_preserves_failed_proposal_reads() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    app.experimental_graph = true;
    let mut view = graph_view();
    let mut state = ag_session::test_support::graph_proposal_state("preview", "sess");
    state.version.approval = Some(ag_session::GraphProposalApproval {
        graph_id: "graph".into(),
        limits: state.limits.clone(),
        version: 1,
    });
    let saved = state.clone();
    let mut proposals = ag_store::MockGraphProposalRepository::new();
    proposals
        .expect_latest()
        .once()
        .return_once(move |_| Ok(Some(saved)));
    proposals.expect_latest().once().return_once(|_| {
        Err(DbError::InvalidData {
            entity: "proposal",
            reason: "unavailable".into(),
        })
    });
    app.services
        .set_graph_proposal_repository(Arc::new(proposals));

    // Act / Assert
    app.load_graph_view(&mut view)
        .await
        .expect("approved proposal");
    assert!(!view.is_proposal_selected);
    assert_eq!(view.proposal, Some(state));
    let original = view.clone();
    assert!(app.load_graph_view(&mut view).await.is_err());
    assert_eq!(view, original);
}

#[tokio::test]
async fn inspector_reads_are_atomic_and_missing_results_are_not_actionable() {
    for fault in [
        "progress",
        "result",
        "missing-result",
        "empty",
        "no-progress",
    ] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        let mut view = graph_view();
        let original = view.clone();
        let graph = view.graphs[0].clone();
        let progress = view.progress.clone();
        let mut repository = MockGraphRepository::new();
        repository.expect_list_graphs().return_once(move |_| {
            Ok(if fault == "empty" {
                Vec::new()
            } else {
                vec![graph]
            })
        });
        if fault != "empty" {
            repository.expect_progress().return_once(move |_| {
                if fault == "progress" {
                    Err(unavailable())
                } else if fault == "no-progress" {
                    Ok(None)
                } else {
                    Ok(progress)
                }
            });
        }
        if matches!(fault, "result" | "missing-result") {
            repository.expect_load_result().return_once(move |_| {
                if fault == "result" {
                    Err(unavailable())
                } else {
                    Ok(None)
                }
            });
        }
        app.services.set_graph_repository(Arc::new(repository));

        // Act
        let result = app.load_graph_view(&mut view).await;

        // Assert
        if matches!(fault, "progress" | "result") {
            assert!(result.is_err());
            assert_eq!(view, original);
        } else {
            assert_eq!(result, Ok(()));
            assert_eq!(view.outputs, []);
            assert!(!view.can_accept());
        }
    }
}

#[tokio::test]
async fn inspector_key_actions_show_admission_and_refresh_failures() {
    for fault in ["none", "action", "refresh"] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        let mut view = graph_view();
        view.graphs[0].nodes.truncate(1);
        view.progress.as_mut().expect("progress").nodes.truncate(1);
        let graph = view.graphs[0].clone();
        let progress = view.progress.clone();
        let result = view.outputs[0].result.clone();
        let mut repository = MockGraphRepository::new();
        repository
            .expect_progress()
            .returning(move |_| Ok(progress.clone()));
        repository.expect_cancel().once().return_once(move |_| {
            if fault == "action" {
                Err(unavailable())
            } else {
                Ok(())
            }
        });
        if fault != "action" {
            repository
                .expect_list_graphs()
                .once()
                .return_once(move |_| {
                    if fault == "refresh" {
                        Err(unavailable())
                    } else {
                        Ok(vec![graph])
                    }
                });
        }
        if fault == "none" {
            repository.expect_load_result().return_once(move |_| {
                Ok(Some(StoredGraphResult {
                    acceptance: None,
                    validation: None,
                    result,
                    body: "output".into(),
                }))
            });
        }
        app.services.set_graph_repository(Arc::new(repository));
        view.editor = Some(crate::presentation::execution_graph::GraphEditor {
            input: crate::domain::input::InputState::with_text("cancel".into()),
            kind: crate::presentation::execution_graph::GraphEditKind::Cancel,
            needs_reconciliation: false,
        });
        app.mode = AppMode::ExecutionGraph(Box::new(view));

        // Act
        execution_graph::handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;

        // Assert
        let view = match &app.mode {
            AppMode::ExecutionGraph(view) => Some(view),
            _ => None,
        }
        .expect("inspector");
        assert_eq!(view.editor.is_some(), fault == "action");
        if let Some(editor) = &view.editor {
            assert!(editor.needs_reconciliation);
            assert_eq!(editor.input.text(), "cancel");
        }
        assert!(view.notice.contains(if fault == "none" {
            "Action admitted"
        } else {
            "Read failed"
        }));
        assert!(!app.has_visible_tick_driven_ui());
    }
}

#[tokio::test]
async fn inspector_resume_enters_the_existing_host_boundary() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let mut view = graph_view();
    view.graphs[0].nodes.truncate(1);
    view.progress.as_mut().expect("progress").nodes.truncate(1);
    let graph = view.graphs[0].clone();
    let progress = view.progress.clone();
    let mut repository = MockGraphRepository::new();
    repository
        .expect_progress()
        .returning(move |_| Ok(progress.clone()));
    repository
        .expect_load_graph()
        .once()
        .return_once(move |_, _| Ok(Some(graph)));
    repository
        .expect_budget_limits()
        .once()
        .return_once(|_| Ok(BTreeMap::from([("calls".into(), 3)])));
    app.services.set_graph_repository(Arc::new(repository));

    // Act
    let result = app
        .apply_graph_view_action(&view, GraphViewAction::Resume)
        .await;

    // Assert
    assert!(result.is_err()); // Host denies dispatch without a prepared owning session.
}

fn unavailable() -> DbError {
    DbError::InvalidData {
        entity: "graph",
        reason: "Read failed".into(),
    }
}

#[tokio::test]
async fn session_graph_shortcut_opens_and_renders_a_session_scoped_snapshot() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    app.sessions.state_mut().replace_sessions(vec![
        crate::test_support::SessionFixtureBuilder::new()
            .id("sess")
            .build(),
    ]);
    app.mode = AppMode::View {
        session_id: "sess".into(),
        scroll_offset: Some(4),
    };
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).expect("terminal");
    let cache = crate::ui::RenderCacheStore::default();

    // Act
    crate::runtime::mode::session_view::handle_with_cache(
        &mut app,
        &cache,
        &mut terminal,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE),
    )
    .await
    .expect("shortcut");
    let mut project_table = ratatui::widgets::TableState::default();
    let mut session_table = ratatui::widgets::TableState::default();
    terminal
        .draw(|frame| {
            crate::ui::render_app(
                &app.view_snapshot(),
                frame,
                &mut project_table,
                &cache,
                &mut session_table,
            );
        })
        .expect("render");

    // Assert
    assert!(
        matches!(&app.mode, AppMode::ExecutionGraph(view) if view.restore.session_id == "sess" && view.restore.scroll_offset == Some(4))
    );
    let text: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(text.contains("No saved execution graphs"));
}

#[tokio::test]
async fn failed_graph_navigation_restores_the_inspected_selection() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let mut view = graph_view();
    let mut second = view.graphs[0].clone();
    second.id = "other-graph".into();
    view.graphs.push(second);
    view.scroll = 2;
    let mut repository = MockGraphRepository::new();
    repository
        .expect_list_graphs()
        .return_once(|_| Err(unavailable()));
    app.services.set_graph_repository(Arc::new(repository));
    app.mode = AppMode::ExecutionGraph(Box::new(view));

    // Act
    execution_graph::handle(
        &mut app,
        KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(
        matches!(&app.mode, AppMode::ExecutionGraph(view) if view.selected_graph == 0 && view.scroll == 2 && view.outputs[0].body == "Exact output body" && view.notice.contains("Read failed"))
    );
}

#[tokio::test]
async fn inspector_loads_exact_result_body_and_dispatches_shared_acceptance() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let mut view = graph_view();
    let graph = view.graphs[0].clone();
    let progress = view.progress.clone();
    let result = view.outputs[0].result.clone();
    let expected = result.reference.clone();
    let mut repository = MockGraphRepository::new();
    repository
        .expect_list_graphs()
        .once()
        .return_once(move |_| Ok(vec![graph]));
    repository
        .expect_progress()
        .returning(move |_| Ok(progress.clone()));
    repository
        .expect_load_result()
        .once()
        .return_once(move |_| {
            Ok(Some(StoredGraphResult {
                result,
                body: r#"{"answer":"**Persisted**\n\noutput"}"#.into(),
                acceptance: None,
                validation: None,
            }))
        });
    let graph = view.graphs[0].clone();
    repository
        .expect_load_graph()
        .once()
        .return_once(move |_, _| Ok(Some(graph)));
    repository
        .expect_accept_result()
        .once()
        .withf(move |reference, _| reference == &expected)
        .return_once(|_, _| Ok(true));
    app.services.set_graph_repository(Arc::new(repository));

    // Act
    app.load_graph_view(&mut view).await.expect("load");
    let accepted = app
        .apply_graph_view_action(&view, GraphViewAction::Accept)
        .await;

    // Assert
    assert_eq!(
        view.outputs[0].body,
        r#"{"answer":"**Persisted**\n\noutput"}"#
    );
    assert_eq!(view.outputs[0].display(), "**Persisted**\n\noutput");
    assert_eq!(accepted, Ok(()));
}

#[tokio::test]
async fn inspector_routes_cancellation_continuation_and_rejects_stale_actions() {
    for scenario in [
        "cancel",
        "reply",
        "retry",
        "stale",
        "unavailable",
        "missing",
        "progress-error",
    ] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        let mut view = graph_view();
        view.graphs[0].nodes.truncate(1);
        let mut progress = view.progress.clone().expect("progress");
        progress.nodes.truncate(1);
        if scenario == "retry" {
            progress.nodes[0].status = GraphNodeStatus::Failed;
        }
        if scenario == "unavailable" {
            progress.status = ag_session::ExecutionGraphStatus::Completed;
        }
        view.progress = Some(progress.clone());
        if scenario == "stale" {
            progress.nodes[0].last_error = Some("new event".into());
        }
        let mut repository = MockGraphRepository::new();
        repository.expect_progress().returning(move |_| {
            if scenario == "progress-error" {
                return Err(unavailable());
            }

            Ok(if scenario == "missing" {
                None
            } else {
                Some(progress.clone())
            })
        });
        if scenario == "cancel" {
            repository
                .expect_cancel()
                .once()
                .withf(|request| request.graph_id == "graph")
                .return_once(|_| Ok(()));
        }
        if matches!(scenario, "reply" | "retry") {
            repository.expect_continue_node().once().withf(move |request| {
                request.node_id == "first" && request.execution == graph_view().node().expect("node").executions[0] && if scenario == "retry" { request.kind == GraphNodeContinuation::Retry } else { matches!(&request.kind, GraphNodeContinuation::Reply { message, .. } if message == "Rework output") }
            }).return_once(|_| Ok(true));
        }
        app.services.set_graph_repository(Arc::new(repository));
        let action = match scenario {
            "reply" => GraphViewAction::Continue(GraphNodeContinuation::Reply {
                attachments: Vec::new(),
                message: "Rework output".into(),
            }),
            "retry" => GraphViewAction::Continue(GraphNodeContinuation::Retry),
            "unavailable" => GraphViewAction::Resume,
            _ => GraphViewAction::Cancel,
        };
        if scenario == "unavailable" {
            view.progress.as_mut().expect("progress").status =
                ag_session::ExecutionGraphStatus::Completed;
        }

        // Act
        let result = app.apply_graph_view_action(&view, action).await;

        // Assert
        assert_eq!(
            result.is_ok(),
            matches!(scenario, "cancel" | "reply" | "retry"),
            "{scenario}: {result:?}"
        );
        if scenario == "progress-error" {
            assert!(result.expect_err("failed read").contains("Read failed"));
        }
    }
}

#[tokio::test]
async fn inspector_refresh_and_exit_keep_errors_visible_and_restore_session() {
    for fail_read in [false, true] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        let mut repository = MockGraphRepository::new();
        repository.expect_list_graphs().returning(move |_| {
            if fail_read {
                Err(unavailable())
            } else {
                Ok(Vec::new())
            }
        });
        app.services.set_graph_repository(Arc::new(repository));
        let restore = graph_view().restore;

        // Act: a stale dispatch after the inspector closes is harmless.
        execution_graph::handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;

        // Assert
        assert!(matches!(app.mode, AppMode::List));

        // Act
        app.open_graph_view(restore.clone()).await;
        execution_graph::handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(&app.mode, AppMode::ExecutionGraph(view)
            if view.notice.contains(if fail_read { "Read failed" } else { "View refreshed" })));

        // Act
        execution_graph::handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).await;

        // Assert
        assert!(
            matches!(app.mode, AppMode::View { session_id, scroll_offset }
            if session_id == restore.session_id && scroll_offset == restore.scroll_offset)
        );
    }
}
