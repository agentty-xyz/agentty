//! Session API project tests covering retired-model migration, rejection
//! of cross-project inheritance, the active project's status and sync, and
//! the listed projects a session-API caller lists, reads, creates sessions
//! in, and syncs, each from its own session page and checkout.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_session::{
    AgentKind, AgentModel, AgentSelection, CreateSessionMode, CreateSessionRequest, HostProject,
    HostProjectStatus, HostProjectSync, HostProjectSyncPhase, ProjectScope, ProjectSessions,
    ReasoningLevel, SessionError as ApiSessionError, SessionId, SessionListEntry, SessionOperation,
    SessionSettingsChange, test_support as model_fixture,
};
use tempfile::TempDir;

use super::support::{
    request_project_status, request_project_sync, request_session, request_session_creation,
    request_settings,
};
use crate::app::session::{SessionGitStatus, session_branch};
use crate::app::test_support::{
    MockSyncMainRunner, ProjectSyncContext, SyncMainCompletion, SyncMainOutcome,
    SyncSessionStartError, add_listed_project, insert_loadable_session, start_fake_listener,
};
use crate::app::{App, AppEvent, ProjectSyncPhase, ProjectSyncStatus};
use crate::domain::session::Status;
use crate::domain::setting::SettingName;

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

/// Hides `project_id` in the project catalog and reloads the list.
async fn hide_project(app: &mut App, project_id: i64) {
    app.services
        .db()
        .settings()
        .upsert_project_setting(project_id, SettingName::ProjectCatalogVisible, "false")
        .await
        .expect("visibility should persist");
    app.reload_projects().await;
}

/// Returns the ids of `entries` in order.
fn entry_ids(entries: &[SessionListEntry]) -> Vec<&str> {
    entries
        .iter()
        .map(|entry| entry.summary.id.as_str())
        .collect()
}

/// Stores Claude Opus as the default agent and model of `project_id`.
async fn prefer_claude_opus(app: &App, project_id: i64) {
    app.services
        .db()
        .settings()
        .upsert_project_settings(
            project_id,
            vec![
                (SettingName::DefaultSmartAgent, "claude".to_string()),
                (
                    SettingName::DefaultSmartModel,
                    AgentModel::ClaudeOpus55.as_str().to_string(),
                ),
            ],
        )
        .await
        .expect("project settings should persist");
}

/// Checks out the new branch `branch` in the checkout of the project
/// `name`, which [`add_listed_project`] made under `directory`.
fn check_out_new_branch(directory: &TempDir, name: &str, branch: &str) {
    let status = std::process::Command::new("git")
        .args(["checkout", "--quiet", "-b", branch])
        .current_dir(directory.path().join(name))
        .status()
        .expect("git should run");

    assert!(status.success(), "git checkout should succeed");
}

/// Returns the ids of the sessions on the viewed page of `project_id`.
fn viewed_session_ids(app: &App, project_id: i64) -> Vec<String> {
    app.sessions
        .state()
        .viewed_page(project_id)
        .map_or_default(|page| {
            page.sessions()
                .iter()
                .map(|session| session.id.to_string())
                .collect()
        })
}

/// Returns the ids of the terminal's sessions and the one it selects.
fn terminal_list(app: &App) -> (Vec<String>, Option<String>) {
    let session_ids = app
        .sessions
        .sessions()
        .iter()
        .map(|session| session.id.to_string())
        .collect();
    let selected_session_id = app.selected_session().map(|session| session.id.to_string());

    (session_ids, selected_session_id)
}

/// What one started project sync was given.
struct StartedSync {
    operation: ProjectSyncContext,
    project_branch_name: Option<String>,
    review_target_ids: Vec<String>,
    session_model: AgentModel,
    working_dir: PathBuf,
}

/// Lets `app` start exactly one project sync without running Git, and
/// records what it was given.
fn record_one_project_sync(app: &mut App) -> Arc<Mutex<Vec<StartedSync>>> {
    let started = Arc::new(Mutex::new(Vec::new()));
    let mut sync_main_runner = MockSyncMainRunner::new();
    sync_main_runner
        .expect_start_sync_main()
        .times(1)
        .returning({
            let started = Arc::clone(&started);

            move |_, operation, session_model, sync_context| {
                started.lock().expect("started syncs").push(StartedSync {
                    operation,
                    project_branch_name: sync_context.project_branch_name,
                    review_target_ids: sync_context
                        .review_request_sync_targets
                        .into_iter()
                        .map(|target| target.session_id.to_string())
                        .collect(),
                    session_model,
                    working_dir: sync_context.working_dir,
                });
            }
        });
    app.sync_main_runner = Arc::new(sync_main_runner);

    started
}

/// Stores a loadable review session of `project_id` with a published
/// branch, whose review request a sync of the project refreshes.
async fn insert_published_session(app: &App, session_id: &str, project_id: i64) {
    insert_loadable_session(app, session_id, Status::Review, project_id).await;
    app.services
        .db()
        .sessions()
        .update_session_published_upstream_ref(session_id, Some(format!("origin/wt/{session_id}")))
        .await
        .expect("upstream should persist");
}

/// Returns the sync phase and branch `sessions` reports for its project.
fn reported_sync(
    sessions: Result<ProjectSessions, ApiSessionError>,
) -> Option<(String, HostProjectSyncPhase)> {
    sessions
        .expect("project should list")
        .status
        .and_then(|status| status.sync)
        .map(|sync| (sync.branch, sync.phase))
}

#[tokio::test]
async fn runtime_backend_creates_sessions_in_a_viewed_project_from_its_own_checkout() {
    // Arrange
    // The active project has no branch, so nothing created elsewhere can
    // start from it.
    let (mut app, directory) = crate::test_support::new_test_app().await;
    let active_project_id = app.active_project_id();
    insert_loadable_session(&app, "active1", Status::Review, active_project_id).await;
    app.refresh_sessions_now().await;
    app.sessions.select_session_index(Some(0));
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    prefer_claude_opus(&app, other_project_id).await;
    let service = app.session_service();
    app.drive_session_request(service.project_sessions(ProjectScope::Project(other_project_id)))
        .await
        .expect("viewed project should list");
    // The checkout moves to another branch after its page loaded.
    check_out_new_branch(&directory, "other", "release");
    app.project_sync_statuses.insert(
        other_project_id,
        ProjectSyncStatus {
            context: ProjectSyncContext {
                default_branch: "release".to_string(),
                operation_id: 1,
                project_id: other_project_id,
                project_name: "other".to_string(),
            },
            phase: ProjectSyncPhase::Running,
        },
    );
    let terminal_before = terminal_list(&app);
    let default_model = app.sessions.default_session_model();
    let request = |mode| CreateSessionRequest {
        inherit_from_session_id: None,
        mode,
        project_id: other_project_id,
    };

    // Act
    let while_syncing = request_session_creation(&mut app, request(CreateSessionMode::Draft)).await;
    app.project_sync_statuses.remove(&other_project_id);
    let mut created = Vec::new();
    for mode in [CreateSessionMode::Draft, CreateSessionMode::Regular] {
        created.push(
            request_session_creation(&mut app, request(mode))
                .await
                .expect("a viewed project should take new sessions"),
        );
    }

    // Assert
    assert_eq!(
        while_syncing,
        Err(ApiSessionError::Operation(
            "Project `other` is synchronizing `release`; try this base-branch operation again \
             after sync"
                .to_string()
        ))
    );
    let mut created_rows = Vec::new();
    for session_id in &created {
        let row = app
            .services
            .db()
            .sessions()
            .load_session(session_id)
            .await
            .expect("session read")
            .expect("created session");
        created_rows.push((row.project_id, row.base_branch, row.agent, row.model));
    }
    let expected_row = (
        Some(other_project_id),
        "release".to_string(),
        "claude".to_string(),
        AgentModel::ClaudeOpus55.as_str().to_string(),
    );
    assert_eq!(created_rows, vec![expected_row.clone(), expected_row]);
    assert_eq!(
        viewed_session_ids(&app, other_project_id),
        created.iter().map(ToString::to_string).collect::<Vec<_>>()
    );
    // The regular session's worktree branch lives in the viewed checkout.
    let branches = std::process::Command::new("git")
        .args(["branch", "--list", &session_branch(&created[1])])
        .current_dir(directory.path().join("other"))
        .output()
        .expect("git should run");
    assert!(
        !branches.stdout.is_empty(),
        "the session branch should exist"
    );
    assert_eq!(terminal_list(&app), terminal_before);
    assert_eq!(terminal_before.0, vec!["active1".to_string()]);
    assert_eq!(app.sessions.default_session_model(), default_model);
}

#[tokio::test]
async fn runtime_backend_syncs_a_viewed_project_in_its_own_checkout_and_reports_it_there() {
    // Arrange
    let (mut app, directory) = crate::test_support::new_test_app().await;
    let active_project_id = app.active_project_id();
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    prefer_claude_opus(&app, other_project_id).await;
    // Each page holds a published session whose review request a sync
    // refreshes.
    insert_published_session(&app, "active1", active_project_id).await;
    insert_published_session(&app, "viewed1", other_project_id).await;
    app.refresh_sessions_now().await;
    let started = record_one_project_sync(&mut app);
    let service = app.session_service();
    let other = ProjectScope::Project(other_project_id);
    app.drive_session_request(service.project_sessions(other))
        .await
        .expect("viewed project should list");
    // The checkout moves to another branch after its page loaded.
    check_out_new_branch(&directory, "other", "release");

    // Act
    let synced = app
        .drive_session_request(service.sync_project_for(other))
        .await;
    let repeated = app
        .drive_session_request(service.sync_project_for(other))
        .await;
    let running = app
        .drive_session_request(service.project_sessions(other))
        .await;
    let active_while_running = app
        .drive_session_request(service.project_sessions(ProjectScope::Active))
        .await;
    let StartedSync {
        operation,
        project_branch_name,
        review_target_ids,
        session_model,
        working_dir,
    } = started.lock().expect("started syncs").remove(0);
    // The page leaves before the sync finishes.
    app.clear_viewed_pages();
    app.apply_app_events(sync_completed(
        operation.clone(),
        Ok(SyncMainOutcome {
            default_branch: "release".to_string(),
            pulled_commit_titles: Vec::new(),
            pulled_commits: Some(2),
            pushed_commit_titles: Vec::new(),
            pushed_commits: Some(0),
            resolved_conflict_files: Vec::new(),
        }),
    ))
    .await;
    let is_waiting = app
        .pending_project_sync_completions
        .contains_key(&other_project_id);
    let complete = app
        .drive_session_request(service.project_sessions(other))
        .await;
    app.merge_queue.set_active("merging".into());
    let blocked = app
        .drive_session_request(service.sync_project_for(other))
        .await;

    // Assert
    assert_eq!(synced, Ok(()));
    assert_eq!(repeated, Ok(()));
    assert_eq!(
        reported_sync(running),
        Some(("release".to_string(), HostProjectSyncPhase::Running))
    );
    assert_eq!(reported_sync(active_while_running), None);
    assert_eq!(operation.project_id, other_project_id);
    assert_eq!(operation.project_name, "other");
    assert_eq!(operation.default_branch, "release");
    assert_eq!(project_branch_name.as_deref(), Some("release"));
    assert_eq!(working_dir, directory.path().join("other"));
    assert_eq!(review_target_ids, vec!["viewed1".to_string()]);
    assert_eq!(
        App::review_request_sync_targets(&app.sessions)
            .into_iter()
            .map(|target| target.session_id.to_string())
            .collect::<Vec<_>>(),
        vec!["active1".to_string()]
    );
    assert_eq!(session_model, AgentModel::ClaudeOpus55);
    assert_ne!(app.sessions.default_session_model(), session_model);
    assert!(is_waiting);
    assert_eq!(
        reported_sync(complete),
        Some((
            "release".to_string(),
            HostProjectSyncPhase::Complete {
                pulled_commits: Some(2),
                pushed_commits: Some(0),
                resolved_conflict_count: 0,
            }
        ))
    );
    assert!(app.pending_project_sync_completions.is_empty());
    assert!(app.active_project_sync_status().is_none());
    // Merge work in any project blocks the sync of every project.
    assert_eq!(
        blocked,
        Err(ApiSessionError::Operation(
            "Sync is blocked: a merge is active or queued; try again after it finishes".to_string()
        ))
    );
}

#[tokio::test]
async fn runtime_backend_lists_projects_in_switcher_order_with_the_active_one_marked() {
    // Arrange
    let (mut app, directory) = crate::test_support::new_test_app().await;
    let active_project_id = app.active_project_id();
    let active_project_name = app.projects.project_name().to_string();
    hide_project(&mut app, active_project_id).await;
    insert_loadable_session(&app, "running", Status::Review, active_project_id).await;
    insert_loadable_session(&app, "finished", Status::Done, active_project_id).await;
    app.refresh_sessions_now().await;
    let alpha_project_id = add_listed_project(&mut app, &directory, "alpha").await;
    let beta_project_id = add_listed_project(&mut app, &directory, "beta").await;
    insert_loadable_session(&app, "beta1", Status::Review, beta_project_id).await;
    app.services
        .db()
        .projects()
        .touch_project_last_opened(beta_project_id)
        .await
        .expect("open time should persist");
    app.reload_projects().await;
    let service = app.session_service();
    let project = |id: i64, name: &str, is_active: bool, active_session_count: u32| HostProject {
        active_session_count,
        id,
        is_active,
        name: name.to_string(),
    };

    // Act
    let hidden_active = app.drive_session_request(service.list_projects()).await;
    app.switch_project(alpha_project_id)
        .await
        .expect("project switch");
    let listed_active = app.drive_session_request(service.list_projects()).await;

    // Assert
    // An active project the switcher hides comes first, counted from its
    // loaded page; the others follow, most recently opened first.
    assert_eq!(
        hidden_active,
        Ok(vec![
            project(active_project_id, &active_project_name, true, 1),
            project(beta_project_id, "beta", false, 1),
            project(alpha_project_id, "alpha", false, 0),
        ])
    );
    assert_eq!(
        listed_active,
        Ok(vec![
            project(alpha_project_id, "alpha", true, 0),
            project(beta_project_id, "beta", false, 1),
        ])
    );
}

#[tokio::test]
async fn runtime_backend_reads_a_viewed_project_with_its_branch_and_no_counts() {
    // Arrange
    let (mut app, directory) = crate::test_support::new_test_app().await;
    let active_project_id = app.active_project_id();
    app.projects.set_git_status(Some((1, 2)));
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    insert_loadable_session(&app, "viewed1", Status::Review, other_project_id).await;
    std::fs::write(directory.path().join("other/marker.txt"), "marker").expect("marker file");
    prefer_claude_opus(&app, other_project_id).await;
    let default_model = app.sessions.default_session_model();
    let service = app.session_service();
    let other = ProjectScope::Project(other_project_id);

    // Act
    let viewed = app
        .drive_session_request(service.project_sessions(other))
        .await
        .expect("viewed project should list");
    let active = app
        .drive_session_request(service.project_sessions(ProjectScope::Active))
        .await
        .expect("active project should list");
    let host = app
        .drive_session_request(service.host_context_for(other))
        .await
        .expect("viewed project should be described");
    let files = app
        .drive_session_request(service.list_project_files_for(other))
        .await
        .expect("viewed checkout should list");
    let active_entries = app
        .drive_session_request(service.list_session_entries())
        .await
        .expect("active sessions should list");

    // Assert
    assert_eq!(viewed.project_id, Some(other_project_id));
    assert_eq!(
        viewed.status,
        Some(HostProjectStatus {
            ahead_behind: None,
            branch: Some("main".to_string()),
            project_name: "other".to_string(),
            sync: None,
        })
    );
    assert_eq!(entry_ids(&viewed.entries), vec!["viewed1"]);
    let stats = viewed.entries[0]
        .stats
        .as_ref()
        .expect("viewed entries have stats");
    assert_eq!(stats.base_ahead_behind, None);
    assert_eq!(stats.upstream_ahead_behind, None);
    assert!(!stats.has_merge_conflict);
    assert_eq!(active.project_id, Some(active_project_id));
    assert_eq!(
        active.status.and_then(|status| status.ahead_behind),
        Some((1, 2))
    );
    assert_eq!(active_entries, Vec::new());
    assert_eq!(host.project_id, other_project_id);
    assert_eq!(host.project_name, "other");
    assert_eq!(host.base_branch, "main");
    assert_eq!(host.default_model, AgentModel::ClaudeOpus55);
    // Sessions are created there with its own default settings.
    assert_eq!(
        host.session_modes
            .iter()
            .map(|session_mode| (session_mode.mode.clone(), session_mode.agent))
            .collect::<Vec<_>>(),
        vec![(
            CreateSessionMode::Regular,
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55)
        )]
    );
    assert_eq!(app.sessions.default_session_model(), default_model);
    assert!(files.iter().any(|entry| entry.path == "marker.txt"));
    assert!(service.supports(SessionOperation::Projects));
    assert!(
        !app.remote_session_service()
            .supports(SessionOperation::Projects)
    );
}

#[tokio::test]
async fn runtime_backend_refuses_every_request_naming_a_project_it_does_not_list() {
    // Arrange
    let (mut app, directory) = crate::test_support::new_test_app().await;
    accept_project_syncs(&mut app, 0);
    let hidden_project_id = add_listed_project(&mut app, &directory, "hidden").await;
    hide_project(&mut app, hidden_project_id).await;
    let service = app.session_service();

    for project_id in [hidden_project_id, hidden_project_id + 100] {
        let scope = ProjectScope::Project(project_id);

        // Act
        let sessions = app
            .drive_session_request(service.project_sessions(scope))
            .await;
        let host = app
            .drive_session_request(service.host_context_for(scope))
            .await;
        let files = app
            .drive_session_request(service.list_project_files_for(scope))
            .await;
        let synced = app
            .drive_session_request(service.sync_project_for(scope))
            .await;
        let created = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await;

        // Assert
        assert_eq!(sessions, Err(ApiSessionError::NotFound), "{project_id}");
        assert_eq!(host, Err(ApiSessionError::NotFound), "{project_id}");
        assert_eq!(files, Err(ApiSessionError::NotFound), "{project_id}");
        assert_eq!(synced, Err(ApiSessionError::NotFound), "{project_id}");
        assert_eq!(created, Err(ApiSessionError::NotFound), "{project_id}");
    }
    assert_eq!(app.sessions.state().viewed_project_ids(), Vec::<i64>::new());
    assert!(app.project_sync_statuses.is_empty());
}

#[tokio::test]
async fn runtime_backend_reports_sessions_listed_on_any_loaded_or_loadable_page() {
    // Arrange
    let (mut app, directory) = crate::test_support::new_test_app().await;
    let active_project_id = app.active_project_id();
    insert_loadable_session(&app, "active1", Status::Review, active_project_id).await;
    app.services
        .db()
        .sessions()
        .insert_session(
            "folderless",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Review",
            active_project_id,
        )
        .await
        .expect("session should persist");
    app.refresh_sessions_now().await;
    let viewed_project_id = add_listed_project(&mut app, &directory, "viewed").await;
    let unloaded_project_id = add_listed_project(&mut app, &directory, "unloaded").await;
    let hidden_project_id = add_listed_project(&mut app, &directory, "hidden").await;
    insert_loadable_session(&app, "viewed1", Status::Review, viewed_project_id).await;
    insert_loadable_session(&app, "unload1", Status::Review, unloaded_project_id).await;
    insert_loadable_session(&app, "hidden1", Status::Review, hidden_project_id).await;
    hide_project(&mut app, hidden_project_id).await;
    app.ensure_project_page(viewed_project_id)
        .await
        .expect("listed page should load");
    let service = app.session_service();

    // Act
    let mut listed = Vec::new();
    for session_id in [
        "active1",
        "viewed1",
        "unload1",
        "hidden1",
        "folderless",
        "missing",
    ] {
        let session_id = SessionId::from(session_id);
        listed.push(
            app.drive_session_request(service.is_session_listed(&session_id))
                .await,
        );
    }

    // Assert
    assert_eq!(
        listed,
        vec![
            Ok(true),
            Ok(true),
            Ok(true),
            Ok(false),
            Ok(false),
            Ok(false)
        ]
    );
    // Asking about a session of a listed project loads that project's page.
    assert_eq!(
        app.sessions.state().viewed_project_ids(),
        vec![viewed_project_id, unloaded_project_id]
    );
}

#[tokio::test]
async fn runtime_backend_reports_a_session_project_it_cannot_read_as_an_error() {
    // Arrange
    let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let service = app.session_service();
    pool.close().await;

    // Act
    let listed = app
        .drive_session_request(service.is_session_listed(&SessionId::from("unloaded")))
        .await;

    // Assert
    assert!(
        matches!(listed, Err(ApiSessionError::Operation(_))),
        "{listed:?}"
    );
}

#[tokio::test]
async fn runtime_backend_changes_a_viewed_session_and_leaves_the_terminals_list_alone() {
    // Arrange
    let (mut app, directory) = crate::test_support::new_test_app().await;
    let active_project_id = app.active_project_id();
    insert_loadable_session(&app, "active1", Status::Review, active_project_id).await;
    app.refresh_sessions_now().await;
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    insert_loadable_session(&app, "viewed1", Status::Review, other_project_id).await;
    let service = app.session_service();
    app.drive_session_request(service.project_sessions(ProjectScope::Project(other_project_id)))
        .await
        .expect("viewed project should list");
    let terminal_sessions = |app: &App| {
        app.sessions
            .sessions()
            .iter()
            .map(|session| (session.id.to_string(), session.reasoning_level_override))
            .collect::<Vec<_>>()
    };
    let terminal_sessions_before = terminal_sessions(&app);

    // Act
    let changed = request_settings(
        &mut app,
        SessionId::from("viewed1"),
        SessionSettingsChange {
            reasoning_level: Some(ReasoningLevel::Low),
            ..SessionSettingsChange::default()
        },
    )
    .await;

    // Assert
    assert_eq!(changed, Ok(()));
    assert_eq!(
        app.sessions
            .session_for_id("viewed1")
            .and_then(|session| session.reasoning_level_override),
        Some(ReasoningLevel::Low)
    );
    assert_eq!(
        app.services
            .db()
            .sessions()
            .load_session("viewed1")
            .await
            .expect("session read")
            .and_then(|row| row.reasoning_level_override),
        Some("low".to_string())
    );
    assert_eq!(terminal_sessions(&app), terminal_sessions_before);
    assert_eq!(terminal_sessions_before.len(), 1);
}
