//! Viewed project pages: on-demand loading, leases and eviction, refresh by
//! each page's own fingerprint, promotion and demotion on a terminal project
//! switch, and the workflows that serve a viewed page's sessions from their
//! own project.

use std::collections::HashMap;
use std::fs;
use std::time::Duration;

use ag_session::{SessionError as ApiSessionError, SessionId, test_support as model_fixture};
use tempfile::TempDir;

use crate::app::project::ProjectContext;
use crate::app::session::{SessionGitStatus, session_folder};
use crate::app::sync::{SyncHandle, SyncOrchestrator};
use crate::app::test_support::{ProjectSyncContext, SyncMainCompletion, SyncMainOutcome};
use crate::app::{App, AppEvent, ProjectSyncPhase, ProjectSyncStatus};
use crate::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, Status,
};
use crate::domain::setting::SettingName;
use crate::test_support::{new_test_app, setup_test_git_repo};

/// Saves the project `name`, a checkout under `directory` on branch `main`,
/// and lists it as the project switcher would.
pub(crate) async fn add_listed_project(app: &mut App, directory: &TempDir, name: &str) -> i64 {
    let path = directory.path().join(name);
    fs::create_dir_all(&path).expect("project directory");
    setup_test_git_repo(&path);
    let project_id = app
        .services
        .db()
        .projects()
        .upsert_project(&path.to_string_lossy(), Some("main".to_string()))
        .await
        .expect("project should persist");
    app.reload_projects().await;

    project_id
}

/// Stores a `status` session of `project_id` with its worktree folder, so
/// a page of that project loads it.
pub(crate) async fn insert_loadable_session(
    app: &App,
    session_id: &str,
    status: Status,
    project_id: i64,
) {
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            model_fixture::CODEX_MODEL_ID,
            "main",
            &status.to_string(),
            project_id,
        )
        .await
        .expect("session should persist");
    fs::create_dir_all(session_folder(app.services.base_path(), session_id))
        .expect("session folder");
}

/// Gives `app` a sync handle whose orchestrator never runs, so no
/// background poll reports git statuses and a test applies its own. The
/// caller keeps the returned orchestrator, whose receivers let the handle
/// publish.
///
/// Use it with an app whose active project has no git branch, which the
/// orchestrator started with the app never polls.
#[must_use = "the handle publishes only while the orchestrator lives"]
pub(crate) fn hold_background_polls(app: &mut App) -> SyncOrchestrator {
    let context = App::sync_context_for(&app.projects, &app.services, &app.sessions);
    let (sync_handle, orchestrator) = SyncHandle::unstarted(app.services.event_sender(), context);
    app.sync_handle = sync_handle;

    orchestrator
}

/// Applies `app`'s background events one at a time until the Git status
/// that the sync orchestrator reports once at startup has been applied, so
/// no later batch of events applies it over a status the test sets.
///
/// Use it with an app whose active project has a git branch, which the
/// orchestrator polls at once and then only on its periodic cadence or
/// when asked to refresh.
pub(crate) async fn apply_startup_git_status(app: &mut App) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = app.next_app_event().await.expect("startup event");
            let is_git_status = matches!(event, AppEvent::GitStatusUpdated { .. });
            app.apply_single_app_event(event).await;
            if is_git_status {
                break;
            }
        }
    })
    .await
    .expect("startup Git status should arrive");
}

/// Builds the git-status event of `project_id` at `generation`, reporting
/// `status` for the project branch and `session_status` for `session_id`.
pub(crate) fn git_status_updated(
    project_id: i64,
    generation: u64,
    status: Option<(u32, u32)>,
    session_id: &str,
    session_status: SessionGitStatus,
) -> AppEvent {
    AppEvent::GitStatusUpdated {
        generation,
        project_id,
        session_statuses: HashMap::from([(SessionId::from(session_id), session_status)]),
        status,
    }
}

/// Returns the ahead/behind counts the context of `project_id` holds.
fn project_git_status(app: &App, project_id: i64) -> Option<(u32, u32)> {
    app.projects
        .context(project_id)
        .and_then(ProjectContext::git_status)
}

/// Returns the ids of the sessions with a git status on `project_id`'s
/// loaded page.
fn page_git_status_ids(app: &App, project_id: i64) -> Option<Vec<String>> {
    app.sessions
        .state()
        .page_for_project(project_id)
        .map(|page| {
            page.session_git_statuses()
                .keys()
                .map(ToString::to_string)
                .collect()
        })
}

/// Hides `project_id` in the project catalog and reloads the list.
pub(crate) async fn hide_project(app: &mut App, project_id: i64) {
    app.services
        .db()
        .settings()
        .upsert_project_setting(project_id, SettingName::ProjectCatalogVisible, "false")
        .await
        .expect("visibility should persist");
    app.reload_projects().await;
}

/// Returns the session ids on the viewed page of `project_id`, if loaded.
fn viewed_session_ids(app: &App, project_id: i64) -> Option<Vec<String>> {
    app.sessions.state().viewed_page(project_id).map(|page| {
        page.sessions()
            .iter()
            .map(|session| session.id.to_string())
            .collect()
    })
}

#[tokio::test]
async fn a_listed_projects_page_loads_on_demand_beside_the_active_page() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    insert_loadable_session(&app, "viewed1", Status::Review, other_project_id).await;
    let active_project_id = app.active_project_id();

    // Act
    let loaded = app.ensure_project_page(other_project_id).await;
    let reloaded = app.ensure_project_page(other_project_id).await;
    let active = app.ensure_project_page(active_project_id).await;

    // Assert
    assert_eq!([loaded, reloaded, active], [Ok(()), Ok(()), Ok(())]);
    assert_eq!(
        viewed_session_ids(&app, other_project_id),
        Some(vec!["viewed1".to_string()])
    );
    let context = app
        .projects
        .context(other_project_id)
        .expect("viewed project should have a context");
    assert_eq!(context.git_branch(), Some("main"));
    assert_eq!(context.git_status(), None);
    assert_eq!(context.name(), "other");
    assert_eq!(context.working_dir(), directory.path().join("other"));
    assert!(app.sessions.session_for_id("viewed1").is_some());
    assert!(!app.sessions.is_in_active_page("viewed1"));
    assert!(app.sessions.sessions().is_empty());
    assert_eq!(app.active_project_id(), active_project_id);
}

#[tokio::test]
async fn unlisted_and_hidden_projects_are_not_found() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let hidden_project_id = add_listed_project(&mut app, &directory, "hidden").await;
    hide_project(&mut app, hidden_project_id).await;

    // Act
    let hidden = app.ensure_project_page(hidden_project_id).await;
    let unknown = app.ensure_project_page(hidden_project_id + 100).await;

    // Assert
    assert_eq!(hidden, Err(ApiSessionError::NotFound));
    assert_eq!(unknown, Err(ApiSessionError::NotFound));
    assert_eq!(app.sessions.state().viewed_project_ids(), Vec::<i64>::new());
}

#[tokio::test]
async fn a_page_that_cannot_load_is_reported_and_not_kept() {
    // Arrange
    let (mut app, directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    sqlx::query("DROP TABLE session_preparation")
        .execute(&pool)
        .await
        .expect("preparation reads should fail");

    // Act
    let loaded = app.ensure_project_page(other_project_id).await;

    // Assert
    assert_eq!(
        loaded,
        Err(ApiSessionError::Operation(
            "Could not load the sessions of project `other`".to_string()
        ))
    );
    assert!(app.sessions.state().viewed_page(other_project_id).is_none());
    assert!(app.projects.context(other_project_id).is_none());
}

#[tokio::test]
async fn viewed_pages_leave_when_idle_past_the_bound_or_unlisted() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let mut project_ids = Vec::new();
    for index in 0..5 {
        project_ids
            .push(add_listed_project(&mut app, &directory, &format!("project{index}")).await);
    }

    // Act
    for project_id in &project_ids {
        app.ensure_project_page(*project_id)
            .await
            .expect("listed page should load");
    }
    let after_bound = app.sessions.state().viewed_project_ids();
    hide_project(&mut app, project_ids[1]).await;
    let after_hiding = app.sessions.state().viewed_project_ids();
    let now = app.services.clock().now_instant();
    app.evict_idle_viewed_pages(now + Duration::from_mins(1));
    let after_minute = app.sessions.state().viewed_project_ids();
    app.evict_idle_viewed_pages(now + Duration::from_mins(11));

    // Assert
    // The least recently requested page leaves first.
    assert_eq!(after_bound, project_ids[1..].to_vec());
    assert_eq!(after_hiding, project_ids[2..].to_vec());
    assert_eq!(after_minute, after_hiding);
    assert_eq!(app.sessions.state().viewed_project_ids(), Vec::<i64>::new());
    for project_id in project_ids {
        assert!(app.projects.context(project_id).is_none());
    }
}

#[tokio::test]
async fn a_terminal_switch_promotes_the_viewed_page_and_demotes_a_leased_one() {
    for is_active_page_leased in [true, false] {
        // Arrange
        let (mut app, directory) = new_test_app().await;
        let _held_orchestrator = hold_background_polls(&mut app);
        let previous_project_id = add_listed_project(&mut app, &directory, "previous").await;
        insert_loadable_session(&app, "active1", Status::Review, previous_project_id).await;
        app.switch_project(previous_project_id)
            .await
            .expect("project switch");
        let other_project_id = add_listed_project(&mut app, &directory, "other").await;
        insert_loadable_session(&app, "viewed1", Status::Review, other_project_id).await;
        app.ensure_project_page(other_project_id)
            .await
            .expect("listed page should load");
        if is_active_page_leased {
            app.ensure_project_page(previous_project_id)
                .await
                .expect("active page is listed");
        }
        // Each project's poll reports counts in its role before the switch.
        let active_update = git_status_updated(
            previous_project_id,
            app.sync_handle.current_generation(),
            Some((1, 1)),
            "active1",
            SessionGitStatus::default(),
        );
        let viewed_update = git_status_updated(
            other_project_id,
            app.sync_handle
                .generation_for(other_project_id)
                .expect("a loaded page is polled"),
            Some((2, 2)),
            "viewed1",
            SessionGitStatus::default(),
        );
        for update in [active_update.clone(), viewed_update.clone()] {
            app.apply_single_app_event(update).await;
        }
        let before_switch = [
            project_git_status(&app, previous_project_id),
            project_git_status(&app, other_project_id),
        ];

        // Act
        app.switch_project(other_project_id)
            .await
            .expect("project switch");
        // Results computed for either project's earlier role arrive late.
        for update in [active_update, viewed_update] {
            app.apply_single_app_event(update).await;
        }

        // Assert
        let label = format!("leased: {is_active_page_leased}");
        assert_eq!(before_switch, [Some((1, 1)), Some((2, 2))], "{label}");
        // Neither project shows counts from its earlier role; each waits for
        // the poll of its new one, which a demoted page keeps.
        assert_eq!(project_git_status(&app, other_project_id), None, "{label}");
        assert_eq!(
            page_git_status_ids(&app, other_project_id),
            Some(Vec::new()),
            "{label}"
        );
        assert_eq!(
            page_git_status_ids(&app, previous_project_id),
            is_active_page_leased.then(Vec::new),
            "{label}"
        );
        assert_eq!(
            app.sync_handle
                .generation_for(previous_project_id)
                .is_some(),
            is_active_page_leased,
            "{label}"
        );
        assert_eq!(app.active_project_id(), other_project_id, "{label}");
        assert!(app.sessions.is_in_active_page("viewed1"), "{label}");
        assert!(app.sessions.state().viewed_page(other_project_id).is_none());
        assert_eq!(
            viewed_session_ids(&app, previous_project_id).is_some(),
            is_active_page_leased,
            "{label}"
        );
        assert_eq!(
            app.sessions.session_for_id("active1").is_some(),
            is_active_page_leased,
            "{label}"
        );
        assert_eq!(
            app.projects
                .context(previous_project_id)
                .map(ProjectContext::git_status),
            is_active_page_leased.then_some(None),
            "{label}"
        );
    }
}

#[tokio::test]
async fn a_viewed_pages_git_status_is_polled_while_loaded_and_kept_across_reloads() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let _held_orchestrator = hold_background_polls(&mut app);
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    insert_loadable_session(&app, "viewed1", Status::Review, other_project_id).await;
    insert_loadable_session(&app, "finished1", Status::Done, other_project_id).await;
    let conflict = SessionGitStatus {
        base_status: Some((1, 1)),
        has_merge_conflict: Some(true),
        remote_status: None,
    };

    // Act
    app.ensure_project_page(other_project_id)
        .await
        .expect("listed page should load");
    let polled_contexts = app.viewed_sync_contexts();
    let loaded_generation = app.sync_handle.generation_for(other_project_id);
    app.apply_single_app_event(git_status_updated(
        other_project_id,
        loaded_generation.expect("a loaded page is polled"),
        Some((2, 3)),
        "viewed1",
        conflict,
    ))
    .await;
    app.services
        .db()
        .sessions()
        .update_session_updated_at("viewed1", 9_999_999_999)
        .await
        .expect("update should persist");
    let reloaded = app.refresh_viewed_pages().await;
    let reloaded_generation = app.sync_handle.generation_for(other_project_id);
    let reloaded_status = project_git_status(&app, other_project_id);
    let reloaded_session_statuses = page_git_status_ids(&app, other_project_id);
    let now = app.services.clock().now_instant();
    app.evict_idle_viewed_pages(now + Duration::from_mins(11));
    app.apply_single_app_event(git_status_updated(
        other_project_id,
        loaded_generation.expect("a loaded page is polled"),
        Some((4, 4)),
        "viewed1",
        conflict,
    ))
    .await;

    // Assert
    // The page's project is polled from its own checkout and branch, for
    // its unfinished sessions, without review requests.
    assert_eq!(polled_contexts.len(), 1);
    let polled_context = &polled_contexts[0];
    assert_eq!(polled_context.project_id, other_project_id);
    assert_eq!(polled_context.project_branch_name.as_deref(), Some("main"));
    assert_eq!(polled_context.working_dir, directory.path().join("other"));
    assert_eq!(polled_context.review_request_sync_targets.len(), 0);
    assert_eq!(
        polled_context
            .session_git_status_targets
            .iter()
            .map(|target| (target.session_id.as_str(), target.base_branch.as_str()))
            .collect::<Vec<_>>(),
        vec![("viewed1", "main")]
    );
    // A reload with the same targets keeps the poll and what it reported.
    assert!(reloaded);
    assert_eq!(reloaded_generation, loaded_generation);
    assert_eq!(reloaded_status, Some((2, 3)));
    assert_eq!(reloaded_session_statuses, Some(vec!["viewed1".to_string()]));
    // Dropping the page stops its poll, and a late result changes nothing.
    assert_eq!(app.sync_handle.generation_for(other_project_id), None);
    assert!(app.viewed_sync_contexts().is_empty());
    assert!(app.projects.context(other_project_id).is_none());
}

#[tokio::test]
async fn each_viewed_page_reloads_only_when_its_own_fingerprint_changes() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let changed_project_id = add_listed_project(&mut app, &directory, "changed").await;
    let still_project_id = add_listed_project(&mut app, &directory, "still").await;
    insert_loadable_session(&app, "changed1", Status::Review, changed_project_id).await;
    insert_loadable_session(&app, "still1", Status::Review, still_project_id).await;
    for project_id in [changed_project_id, still_project_id] {
        app.ensure_project_page(project_id)
            .await
            .expect("listed page should load");
    }
    for session_id in ["changed1", "still1"] {
        app.sessions
            .state_mut()
            .session_mut_for_id(session_id)
            .expect("viewed session should be loaded")
            .title = Some("in memory".to_string());
    }
    app.services
        .db()
        .sessions()
        .update_session_updated_at("changed1", 9_999_999_999)
        .await
        .expect("update should persist");

    // Act
    let first_refresh = app.refresh_viewed_pages().await;
    let second_refresh = app.refresh_viewed_pages().await;

    // Assert
    assert!(first_refresh);
    assert!(!second_refresh);
    let title = |session_id: &str| {
        app.sessions
            .session_for_id(session_id)
            .and_then(|session| session.title.clone())
    };
    assert_eq!(title("changed1"), None);
    assert_eq!(title("still1"), Some("in memory".to_string()));
}

#[tokio::test]
async fn a_sync_finished_without_a_loaded_page_reconciles_when_the_page_loads() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    let operation = ProjectSyncContext {
        default_branch: "main".to_string(),
        operation_id: 1,
        project_id: other_project_id,
        project_name: "other".to_string(),
    };
    app.pending_project_sync_completions.insert(
        other_project_id,
        SyncMainCompletion {
            operation,
            result: Ok(SyncMainOutcome {
                default_branch: "main".to_string(),
                pulled_commit_titles: Vec::new(),
                pulled_commits: Some(2),
                pushed_commit_titles: Vec::new(),
                pushed_commits: Some(0),
                resolved_conflict_files: Vec::new(),
            }),
            review_request_updates: Vec::new(),
        },
    );

    // Act
    app.ensure_project_page(other_project_id)
        .await
        .expect("listed page should load");

    // Assert
    assert!(app.pending_project_sync_completions.is_empty());
    assert!(matches!(
        app.project_sync_statuses
            .get(&other_project_id)
            .map(|status| &status.phase),
        Some(ProjectSyncPhase::Complete {
            pulled_commits: Some(2),
            ..
        })
    ));
}

#[tokio::test]
async fn a_viewed_sessions_start_and_mention_root_use_its_own_project() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    insert_loadable_session(&app, "draft1", Status::Draft, other_project_id).await;
    app.ensure_project_page(other_project_id)
        .await
        .expect("listed page should load");
    app.sessions
        .state_mut()
        .session_mut_for_id("draft1")
        .expect("viewed session should be loaded")
        .is_draft = true;
    fs::remove_dir_all(session_folder(app.services.base_path(), "draft1"))
        .expect("draft folder should be removable");
    app.project_sync_statuses.insert(
        other_project_id,
        ProjectSyncStatus {
            context: ProjectSyncContext {
                default_branch: "main".to_string(),
                operation_id: 1,
                project_id: other_project_id,
                project_name: "other".to_string(),
            },
            phase: ProjectSyncPhase::Running,
        },
    );
    let syncing = Err(
        "Project `other` is synchronizing `main`; try this base-branch operation again after sync"
            .to_string(),
    );

    // Act
    let started = app.start_session("draft1", "Start".to_string()).await;
    let staged = app.start_staged_session("draft1").await;
    let mention_root = app.at_mention_lookup_root("draft1");
    let unloaded_mention_root = app.at_mention_lookup_root("unloaded");

    // Assert
    // The active project is not syncing; the session's own project is.
    assert!(
        app.ensure_project_checkout_available(app.active_project_id())
            .is_ok()
    );
    assert_eq!(started.map_err(|error| error.to_string()), syncing);
    assert_eq!(staged.map_err(|error| error.to_string()), syncing);
    assert_eq!(mention_root, directory.path().join("other"));
    assert_eq!(unloaded_mention_root, app.working_dir());
}

#[tokio::test]
async fn a_sync_of_a_viewed_project_finalizes_the_merged_sessions_on_its_page() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let other_project_id = add_listed_project(&mut app, &directory, "other").await;
    insert_loadable_session(&app, "merged1", Status::Merged, other_project_id).await;
    app.ensure_project_page(other_project_id)
        .await
        .expect("listed page should load");
    app.sessions
        .state_mut()
        .session_mut_for_id("merged1")
        .expect("viewed session should be loaded")
        .review_request = Some(ReviewRequest {
        last_refreshed_at: 1,
        summary: ReviewRequestSummary {
            display_id: "#7".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/merged1".to_string(),
            state: ReviewRequestState::Merged,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Merged work".to_string(),
            web_url: "https://example.test/pull/7".to_string(),
        },
    });

    // Act
    app.apply_app_events(AppEvent::SyncMainCompleted {
        completion: SyncMainCompletion {
            operation: ProjectSyncContext {
                default_branch: "main".to_string(),
                operation_id: 1,
                project_id: other_project_id,
                project_name: "other".to_string(),
            },
            result: Ok(SyncMainOutcome {
                default_branch: "main".to_string(),
                pulled_commit_titles: Vec::new(),
                pulled_commits: Some(1),
                pushed_commit_titles: Vec::new(),
                pushed_commits: Some(0),
                resolved_conflict_files: Vec::new(),
            }),
            review_request_updates: Vec::new(),
        },
    })
    .await;

    // Assert
    assert!(app.pending_project_sync_completions.is_empty());
    assert_eq!(
        app.services
            .db()
            .sessions()
            .load_session("merged1")
            .await
            .expect("session read")
            .map(|row| row.status),
        Some("Done".to_string())
    );
}
