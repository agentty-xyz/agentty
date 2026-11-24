//! Session API project tests covering retired-model migration, rejection
//! of cross-project inheritance, and the active project's status and sync.

use std::collections::HashMap;
use std::sync::Arc;

use ag_session::{
    AgentKind, AgentModel, CreateSessionMode, CreateSessionRequest, HostProjectStatus,
    HostProjectSync, HostProjectSyncPhase, SessionError as ApiSessionError, SessionId,
    SessionOperation, test_support as model_fixture,
};

use super::support::{
    request_project_status, request_project_sync, request_session, request_session_creation,
};
use crate::app::session::SessionGitStatus;
use crate::app::test_support::{
    MockSyncMainRunner, ProjectSyncContext, SyncMainCompletion, SyncMainOutcome,
    SyncSessionStartError, start_fake_listener,
};
use crate::app::{App, AppEvent, ProjectSyncPhase, ProjectSyncStatus};

/// Lets `app` start project syncs without running Git.
fn accept_project_syncs(app: &mut App, expected_starts: usize) {
    let mut sync_main_runner = MockSyncMainRunner::new();
    sync_main_runner
        .expect_start_sync_main()
        .times(expected_starts)
        .returning(|_, _, _, _| {});
    app.sync_main_runner = Arc::new(sync_main_runner);
}

/// Returns the operation of the project sync `app` shows.
fn shown_sync_operation(app: &App) -> ProjectSyncContext {
    app.active_project_sync_status()
        .map(|status| status.context.clone())
        .expect("a project sync should be shown")
}

/// Builds the completion of `operation` with `result`.
fn sync_completed(
    operation: ProjectSyncContext,
    result: Result<SyncMainOutcome, SyncSessionStartError>,
) -> AppEvent {
    AppEvent::SyncMainCompleted {
        completion: SyncMainCompletion {
            operation,
            result,
            review_request_updates: Vec::new(),
        },
    }
}

#[tokio::test]
async fn runtime_backend_reports_the_project_and_each_phase_of_its_sync() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    accept_project_syncs(&mut app, 1);
    app.projects.set_git_status(Some((1, 2)));
    let branch = app.projects.git_branch().map(str::to_string);
    let sync_branch = branch.clone().expect("test project should have a branch");
    let idle_project = HostProjectStatus {
        ahead_behind: Some((1, 2)),
        branch,
        project_name: app.projects.project_name().to_string(),
        sync: None,
    };
    let project_with = |phase: HostProjectSyncPhase| HostProjectStatus {
        sync: Some(HostProjectSync {
            branch: sync_branch.clone(),
            phase,
        }),
        ..idle_project.clone()
    };
    let mut revisions = start_fake_listener(&mut app).await;
    // Each step ends with the check the foreground cycle makes.
    let mut revision_changed = |app: &mut App| {
        app.publish_reported_state_changes();
        let is_changed = revisions
            .has_changed()
            .expect("revision sender should live");
        revisions.borrow_and_update();

        is_changed
    };

    // Act
    let service = app.session_service();
    let supports_status = service.supports(SessionOperation::ProjectStatus);
    let supports_sync = service.supports(SessionOperation::SyncProject);
    let idle = request_project_status(&mut app).await;
    let idle_changed = revision_changed(&mut app);
    let synced = request_project_sync(&mut app).await;
    let started_changed = revision_changed(&mut app);
    let running = request_project_status(&mut app).await;
    let repeated = request_project_sync(&mut app).await;
    // The repeated request is a mutation, which publishes on its own.
    revision_changed(&mut app);
    let operation = shown_sync_operation(&app);
    app.apply_app_events(AppEvent::SyncMainConflictResolutionStarted {
        conflicted_files: vec!["src/lib.rs".to_string(), "README.md".to_string()],
        operation: operation.clone(),
    })
    .await;
    let conflict_changed = revision_changed(&mut app);
    let resolving = request_project_status(&mut app).await;
    app.apply_app_events(sync_completed(
        operation,
        Ok(SyncMainOutcome {
            default_branch: sync_branch.clone(),
            pulled_commit_titles: Vec::new(),
            pulled_commits: Some(3),
            pushed_commit_titles: Vec::new(),
            pushed_commits: Some(1),
            resolved_conflict_files: vec!["src/lib.rs".to_string(), "README.md".to_string()],
        }),
    ))
    .await;
    let completion_changed = revision_changed(&mut app);
    let complete = request_project_status(&mut app).await;
    let expires_at = *app
        .project_sync_status_expirations
        .get(&app.active_project_id())
        .expect("a finished sync should expire");
    app.expire_project_sync_status(expires_at);
    let expiry_changed = revision_changed(&mut app);
    let expired = request_project_status(&mut app).await;
    let expired_changed = revision_changed(&mut app);

    // Assert
    assert!(supports_status);
    assert!(supports_sync);
    assert_eq!(idle, Ok(idle_project.clone()));
    assert_eq!(synced, Ok(()));
    assert_eq!(repeated, Ok(()));
    assert_eq!(running, Ok(project_with(HostProjectSyncPhase::Running)));
    assert_eq!(
        resolving,
        Ok(project_with(HostProjectSyncPhase::ResolvingConflicts {
            conflicted_file_count: 2,
        }))
    );
    assert_eq!(
        complete,
        Ok(project_with(HostProjectSyncPhase::Complete {
            pulled_commits: Some(3),
            pushed_commits: Some(1),
            resolved_conflict_count: 2,
        }))
    );
    assert_eq!(expired, Ok(idle_project));
    assert_eq!(
        [
            started_changed,
            conflict_changed,
            completion_changed,
            expiry_changed
        ],
        [true; 4]
    );
    // Reading the status changes nothing devices read.
    assert!(!idle_changed);
    assert!(!expired_changed);
}

#[tokio::test]
async fn runtime_backend_reports_a_failed_project_sync() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    accept_project_syncs(&mut app, 1);

    // Act
    let synced = request_project_sync(&mut app).await;
    let operation = shown_sync_operation(&app);
    app.apply_app_events(sync_completed(
        operation,
        Err(SyncSessionStartError::Other(
            "remote rejected the push".to_string(),
        )),
    ))
    .await;
    let failed = request_project_status(&mut app)
        .await
        .map(|project| project.sync.map(|sync| sync.phase));

    // Assert
    assert_eq!(synced, Ok(()));
    assert_eq!(
        failed,
        Ok(Some(HostProjectSyncPhase::Failed {
            message: "remote rejected the push".to_string(),
        }))
    );
}

#[tokio::test]
async fn runtime_backend_leaves_out_the_sync_of_another_project() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    accept_project_syncs(&mut app, 1);
    let other_project_id = app.active_project_id() + 1;
    app.project_sync_statuses.insert(
        other_project_id,
        ProjectSyncStatus {
            context: ProjectSyncContext {
                default_branch: "develop".to_string(),
                operation_id: 1,
                project_id: other_project_id,
                project_name: "other".to_string(),
            },
            phase: ProjectSyncPhase::Running,
        },
    );

    // Act
    let before_sync = request_project_status(&mut app).await;
    let synced = request_project_sync(&mut app).await;
    let after_sync = request_project_status(&mut app).await;

    // Assert
    assert_eq!(before_sync.map(|project| project.sync), Ok(None));
    assert_eq!(synced, Ok(()));
    assert_eq!(
        after_sync.map(|project| project.sync.map(|sync| sync.phase)),
        Ok(Some(HostProjectSyncPhase::Running))
    );
    assert!(
        app.project_sync_statuses
            .get(&other_project_id)
            .is_some_and(ProjectSyncStatus::is_running)
    );
}

#[tokio::test]
async fn git_status_updates_advance_the_remote_revision_only_when_they_change() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    app.sessions
        .push_session(crate::test_support::session_fixture(
            "session-1",
            crate::domain::session::Status::Review,
        ));
    let generation = app.sync_handle.current_generation();
    let update = |status| AppEvent::GitStatusUpdated {
        generation,
        session_statuses: HashMap::new(),
        status,
    };
    let conflict_update = AppEvent::GitStatusUpdated {
        generation,
        session_statuses: HashMap::from([(
            SessionId::from("session-1"),
            SessionGitStatus {
                has_merge_conflict: Some(true),
                ..SessionGitStatus::default()
            },
        )]),
        status: Some((0, 2)),
    };
    let conflict = SessionGitStatus {
        has_merge_conflict: Some(true),
        ..SessionGitStatus::default()
    };
    let unlisted_conflict_update = AppEvent::GitStatusUpdated {
        generation,
        session_statuses: HashMap::from([
            (SessionId::from("session-1"), conflict),
            (SessionId::from("unlisted-session"), conflict),
        ]),
        status: Some((0, 2)),
    };
    let mut revisions = start_fake_listener(&mut app).await;
    let initial_revision = *revisions.borrow_and_update();

    // Act: each update ends with the check the foreground cycle makes.
    app.apply_app_events(update(Some((0, 2)))).await;
    app.publish_reported_state_changes();
    let after_counts = *revisions.borrow_and_update();
    app.apply_app_events(update(Some((0, 2)))).await;
    app.publish_reported_state_changes();
    let after_same_counts = *revisions.borrow_and_update();
    app.apply_app_events(conflict_update).await;
    app.publish_reported_state_changes();
    let after_conflict = *revisions.borrow_and_update();
    app.apply_app_events(unlisted_conflict_update).await;
    app.publish_reported_state_changes();
    let after_unlisted_conflict = *revisions.borrow_and_update();

    // Assert
    assert_eq!(after_counts, initial_revision + 1);
    assert_eq!(after_same_counts, after_counts);
    assert_eq!(after_conflict, after_counts + 1);
    // A session the host does not list shows devices nothing new.
    assert_eq!(after_unlisted_conflict, after_conflict);
}

#[tokio::test]
async fn runtime_backend_migrates_retired_model_for_inactive_project_lookup() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let inactive_project_id = app
        .services
        .db()
        .projects()
        .upsert_project("/inactive-project", Some("main".to_string()))
        .await
        .expect("inactive project should persist");
    let session_id = SessionId::from("inactive-retired-session");
    app.services
        .db()
        .sessions()
        .insert_session(
            &session_id,
            "gemini-3.5-flash",
            "main",
            "Review",
            inactive_project_id,
        )
        .await
        .expect("retired-model session should persist");

    // Act
    let loaded_session = request_session(&mut app, session_id.clone())
        .await
        .expect("session lookup should succeed")
        .expect("session should exist");
    let persisted_row = app
        .services
        .db()
        .sessions()
        .load_session(&session_id)
        .await
        .expect("migrated session should load")
        .expect("migrated session should exist");

    // Assert
    assert_eq!(loaded_session.settings.project_id, inactive_project_id);
    assert_eq!(
        loaded_session.settings.agent,
        ag_session::AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash)
    );
    assert_eq!(persisted_row.agent, "antigravity");
    assert_eq!(persisted_row.model, "gemini-3.8-flash");
}

#[tokio::test]
async fn runtime_backend_rejects_cross_project_inheritance() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let project_id = app.active_project_id();
    let inactive_project_id = app
        .services
        .db()
        .projects()
        .upsert_project("/inactive-project", Some("develop".to_string()))
        .await
        .expect("inactive project should persist");
    let source_session_id = SessionId::from("inactive-source");
    app.services
        .db()
        .sessions()
        .insert_session(
            &source_session_id,
            model_fixture::CODEX_MODEL_ID,
            "develop",
            "Draft",
            inactive_project_id,
        )
        .await
        .expect("inactive source session should persist");

    // Act
    let project_mismatch_error = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: Some(source_session_id),
            mode: CreateSessionMode::Draft,
            project_id,
        },
    )
    .await
    .expect_err("cross-project inheritance should fail");

    // Assert
    assert_eq!(
        project_mismatch_error,
        ApiSessionError::Operation(format!(
            "Session `inactive-source` belongs to project `{inactive_project_id}`, not \
             `{project_id}`"
        ))
    );
}
