//! Platform task repository tests.

use crate::{
    Database, NewPlatformTask, PlatformTaskClaim, PlatformTaskName, PlatformTaskState,
    SettledPlatformTask,
};

/// Platform origin the fixtures ask.
const PLATFORM_URL: &str = "https://platform.example";

/// Opens a database holding a project whose sessions are named by `ids`,
/// each with the status that follows it, and returns the project.
async fn database_with_sessions(sessions: &[(&str, &str)]) -> (Database, i64) {
    let database = Database::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/project", Some("main".to_string()))
        .await
        .expect("project should insert");
    for (id, status) in sessions {
        database
            .sessions()
            .insert_session(id, "gpt-5.6-sol", "main", status, project_id)
            .await
            .expect("session should insert");
    }

    (database, project_id)
}

/// Starts a check for `session_id`.
async fn begin(database: &Database, session_id: &str) -> bool {
    database
        .platform_tasks()
        .begin_platform_task(NewPlatformTask {
            brief: "Fix the relay reconnect\n\nIt drops after a suspend.",
            platform_url: PLATFORM_URL,
            session_id,
            title: "Fix the relay reconnect",
        })
        .await
        .expect("task should begin")
}

/// Names task 12 of the `agentty` project.
fn task_twelve() -> PlatformTaskName<'static> {
    PlatformTaskName {
        number: 12,
        project: "agentty",
        title: "Fix the relay reconnect",
        url: Some("https://platform.example/sync/projects/agentty/tasks/12"),
    }
}

#[tokio::test]
async fn a_session_begins_its_check_once_and_keeps_what_it_asked() {
    // Arrange
    let (database, _) = database_with_sessions(&[("first", "Draft")]).await;

    // Act
    let first = begin(&database, "first").await;
    let second = begin(&database, "first").await;
    let task = database
        .platform_tasks()
        .load_platform_task("first")
        .await
        .expect("task should load")
        .expect("task should exist");

    // Assert
    assert!(first);
    assert!(!second);
    assert_eq!(task.state, PlatformTaskState::Checking);
    assert_eq!(task.platform_url, PLATFORM_URL);
    assert_eq!(task.title, "Fix the relay reconnect");
    assert_eq!(
        task.brief,
        "Fix the relay reconnect\n\nIt drops after a suspend."
    );
    assert_eq!(task.not_duplicates, [0_u64; 0]);
    assert_eq!(task.claim_id, None);
    assert_eq!(task.task_number, None);
}

#[tokio::test]
async fn a_held_claim_is_taken_once_and_hidden_from_debug_output() {
    // Arrange
    let (database, _) = database_with_sessions(&[("first", "Draft")]).await;
    begin(&database, "first").await;
    let tasks = database.platform_tasks();

    // Act
    let held = tasks
        .hold_platform_task("first", task_twelve(), "clm_secret")
        .await
        .expect("task should be held");
    let held_again = tasks
        .hold_platform_task("first", task_twelve(), "clm_other")
        .await
        .expect("second hold should run");
    let row = tasks
        .load_platform_task("first")
        .await
        .expect("task should load")
        .expect("task should exist");
    let other_platform = tasks
        .take_platform_task_claim("first", "https://other.example")
        .await
        .expect("other platform's take should run");
    let claim = tasks
        .take_platform_task_claim("first", PLATFORM_URL)
        .await
        .expect("claim should be taken");
    let claim_again = tasks
        .take_platform_task_claim("first", PLATFORM_URL)
        .await
        .expect("second take should run");

    // Assert
    assert!(held);
    assert!(!held_again);
    assert_eq!(other_platform, None);
    assert_eq!(row.state, PlatformTaskState::Resolved);
    assert_eq!(row.task_number, Some(12));
    assert_eq!(row.task_project.as_deref(), Some("agentty"));
    assert_eq!(row.task_title.as_deref(), Some("Fix the relay reconnect"));
    assert_eq!(
        row.task_url.as_deref(),
        Some("https://platform.example/sync/projects/agentty/tasks/12")
    );
    assert!(!format!("{row:?}").contains("clm_secret"));
    assert_eq!(
        claim,
        Some(PlatformTaskClaim {
            claim_id: "clm_secret".to_string(),
            platform_url: PLATFORM_URL.to_string(),
            task_number: 12,
            task_project: "agentty".to_string(),
        })
    );
    assert!(!format!("{claim:?}").contains("clm_secret"));
    assert_eq!(claim_again, None);
}

#[tokio::test]
async fn a_conflict_keeps_its_holder_until_the_session_asks_again() {
    // Arrange
    let (database, _) = database_with_sessions(&[("first", "Draft")]).await;
    begin(&database, "first").await;
    let tasks = database.platform_tasks();

    // Act
    let recorded = tasks
        .record_platform_task_conflict("first", task_twelve(), Some("laptop 81ac04"), 1_767_261_600)
        .await
        .expect("conflict should record");
    let conflict = tasks
        .load_platform_task("first")
        .await
        .expect("task should load")
        .expect("task should exist");
    let asked_again = tasks
        .check_platform_task_again("first", "https://other.example", &[12, 15])
        .await
        .expect("check should restart");
    let checking = tasks
        .load_platform_task("first")
        .await
        .expect("task should load")
        .expect("task should exist");
    let asked_while_checking = tasks
        .check_platform_task_again("first", PLATFORM_URL, &[])
        .await
        .expect("second check should run");

    // Assert
    assert!(recorded);
    assert_eq!(conflict.state, PlatformTaskState::Conflict);
    assert_eq!(conflict.holder_session.as_deref(), Some("laptop 81ac04"));
    assert_eq!(conflict.holder_last_active_at, Some(1_767_261_600));
    assert_eq!(conflict.task_number, Some(12));
    assert_eq!(conflict.claim_id, None);
    assert!(asked_again);
    assert_eq!(checking.state, PlatformTaskState::Checking);
    assert_eq!(checking.platform_url, "https://other.example");
    assert_eq!(checking.not_duplicates, [12, 15]);
    assert_eq!(checking.task_number, None);
    assert_eq!(checking.holder_session, None);
    assert!(!asked_while_checking);
}

#[tokio::test]
async fn a_prompt_is_kept_once_until_taken_and_not_after_the_start_is_answered() {
    // Arrange
    let (database, _) = database_with_sessions(&[("first", "Draft")]).await;
    begin(&database, "first").await;
    let tasks = database.platform_tasks();

    // Act
    let saved = tasks
        .save_platform_task_prompt("first", "{\"text\":\"one\"}")
        .await
        .expect("prompt should save");
    let saved_again = tasks
        .save_platform_task_prompt("first", "{\"text\":\"two\"}")
        .await
        .expect("second save should run");
    let taken = tasks
        .take_platform_task_prompt("first")
        .await
        .expect("prompt should be taken");
    let taken_again = tasks
        .take_platform_task_prompt("first")
        .await
        .expect("second take should run");
    let resolved = tasks
        .resolve_platform_task("first")
        .await
        .expect("task should resolve");
    let resolved_again = tasks
        .resolve_platform_task("first")
        .await
        .expect("second resolve should run");
    let saved_after_answer = tasks
        .save_platform_task_prompt("first", "{\"text\":\"three\"}")
        .await
        .expect("late save should run");
    let missing = tasks
        .take_platform_task_prompt("missing")
        .await
        .expect("missing take should run");

    // Assert
    assert!(saved);
    assert!(!saved_again);
    assert_eq!(taken.as_deref(), Some("{\"text\":\"one\"}"));
    assert_eq!(taken_again, None);
    assert!(resolved);
    assert!(!resolved_again);
    assert!(!saved_after_answer);
    assert_eq!(missing, None);
}

#[tokio::test]
async fn only_tasks_holding_their_sessions_back_are_waiting() {
    // Arrange
    let (database, project_id) = database_with_sessions(&[
        ("canceled", "Canceled"),
        ("checking", "Draft"),
        ("conflict", "Draft"),
        ("resolved", "InProgress"),
    ])
    .await;
    let tasks = database.platform_tasks();
    for session_id in ["canceled", "checking", "conflict", "resolved"] {
        begin(&database, session_id).await;
    }
    tasks
        .record_platform_task_conflict("conflict", task_twelve(), None, 1)
        .await
        .expect("conflict should record");
    tasks
        .resolve_platform_task("resolved")
        .await
        .expect("task should resolve");

    // Act
    let waiting = tasks
        .load_waiting_platform_tasks(project_id)
        .await
        .expect("tasks should load");
    let other_project = tasks
        .load_waiting_platform_tasks(project_id + 1)
        .await
        .expect("tasks should load");

    // Assert
    let waiting = waiting
        .iter()
        .map(|task| (task.session_id.as_str(), task.state))
        .collect::<Vec<_>>();
    assert_eq!(
        waiting,
        [
            ("checking", PlatformTaskState::Checking),
            ("conflict", PlatformTaskState::Conflict),
        ]
    );
    assert_eq!(other_project, Vec::new());
}

#[tokio::test]
async fn settled_sessions_give_up_their_claims_once_and_sign_out_forgets_only_its_own() {
    // Arrange
    let (database, _) = database_with_sessions(&[
        ("canceled", "Canceled"),
        ("done", "Done"),
        ("merged", "Merged"),
        ("review", "Review"),
    ])
    .await;
    let tasks = database.platform_tasks();
    for session_id in ["canceled", "done", "merged", "review"] {
        begin(&database, session_id).await;
        tasks
            .hold_platform_task(session_id, task_twelve(), "clm_secret")
            .await
            .expect("task should be held");
    }
    database
        .sessions()
        .update_session_title("merged", "Fix the relay reconnect")
        .await
        .expect("title should update");
    sqlx::query(
        "INSERT INTO session_review_request (session_id, display_id, forge_kind, \
         last_refreshed_at, source_branch, state, target_branch, title, web_url) VALUES \
         ('merged', '#7', 'github', 0, 'wt/merged', 'merged', 'main', 'Fix', \
         'https://github.com/agentty-xyz/agentty/pull/7')",
    )
    .execute(database.pool())
    .await
    .expect("review request should insert");

    // Act
    let elsewhere = tasks
        .take_settled_platform_tasks("https://other.example")
        .await
        .expect("settled tasks should be taken");
    let settled = tasks
        .take_settled_platform_tasks(PLATFORM_URL)
        .await
        .expect("settled tasks should be taken");
    let settled_again = tasks
        .take_settled_platform_tasks(PLATFORM_URL)
        .await
        .expect("settled tasks should be taken");
    tasks
        .forget_platform_task_claims("https://other.example")
        .await
        .expect("other claims should be forgotten");
    let after_other_sign_out = review_claim(&database).await;
    tasks
        .forget_platform_task_claims(PLATFORM_URL)
        .await
        .expect("claims should be forgotten");
    let after_sign_out = review_claim(&database).await;

    // Assert
    let claim = PlatformTaskClaim {
        claim_id: "clm_secret".to_string(),
        platform_url: PLATFORM_URL.to_string(),
        task_number: 12,
        task_project: "agentty".to_string(),
    };
    assert_eq!(
        settled,
        [
            SettledPlatformTask {
                claim: claim.clone(),
                review_request_url: None,
                session_id: "canceled".to_string(),
                status: "Canceled".to_string(),
                title: None,
            },
            SettledPlatformTask {
                claim: claim.clone(),
                review_request_url: None,
                session_id: "done".to_string(),
                status: "Done".to_string(),
                title: None,
            },
            SettledPlatformTask {
                claim,
                review_request_url: Some(
                    "https://github.com/agentty-xyz/agentty/pull/7".to_string()
                ),
                session_id: "merged".to_string(),
                status: "Merged".to_string(),
                title: Some("Fix the relay reconnect".to_string()),
            },
        ]
    );
    assert!(!format!("{settled:?}").contains("clm_secret"));
    assert_eq!(elsewhere, Vec::new());
    assert_eq!(settled_again, Vec::new());
    assert_eq!(after_other_sign_out.as_deref(), Some("clm_secret"));
    assert_eq!(after_sign_out, None);
}

/// Returns the claim the `review` session holds.
async fn review_claim(database: &Database) -> Option<String> {
    database
        .platform_tasks()
        .load_platform_task("review")
        .await
        .expect("task should load")
        .expect("task should exist")
        .claim_id
}

#[tokio::test]
async fn a_kept_prompt_moves_to_a_failed_preparation_unless_it_is_canceled() {
    // Arrange
    let (database, project_id) = database_with_sessions(&[]).await;
    let sessions = database.sessions();
    for id in ["reserved", "canceled"] {
        sessions
            .reserve_session(crate::PersistedSessionCreation {
                agent: "codex",
                base_branch: "main",
                id,
                is_draft: false,
                model: "gpt-5.6-sol",
                project_id,
                reasoning_level: ag_session::ReasoningLevel::default(),
                response_style: ag_session::ResponseStyle::default(),
                speed_mode: ag_session::SpeedMode::Normal,
                status: "Draft",
            })
            .await
            .expect("session should reserve");
    }
    sessions
        .insert_session("unprepared", "gpt-5.6-sol", "main", "Draft", project_id)
        .await
        .expect("session should insert");
    sessions
        .cancel_session_preparation("canceled")
        .await
        .expect("preparation should cancel");
    let tasks = database.platform_tasks();
    for session_id in ["reserved", "canceled", "unprepared", "promptless"] {
        if session_id != "promptless" {
            begin(&database, session_id).await;
            tasks
                .save_platform_task_prompt(session_id, "{\"text\":\"one\"}")
                .await
                .expect("prompt should save");
        }
    }

    // Act
    let retained = [
        tasks
            .retain_platform_task_prompt("reserved", "main", "Start failed.")
            .await
            .expect("retain should run"),
        tasks
            .retain_platform_task_prompt("canceled", "main", "Start failed.")
            .await
            .expect("retain should run"),
        tasks
            .retain_platform_task_prompt("unprepared", "main", "Start failed.")
            .await
            .expect("retain should run"),
        tasks
            .retain_platform_task_prompt("promptless", "main", "Start failed.")
            .await
            .expect("retain should run"),
    ];

    // Assert
    assert_eq!(retained, [true, false, true, false]);
    for session_id in ["reserved", "unprepared"] {
        let preparation = sessions
            .load_session_preparation(session_id)
            .await
            .expect("preparation should load")
            .expect("preparation should exist");
        assert_eq!(preparation.state, crate::SessionPreparationState::Failed);
        assert_eq!(preparation.error.as_deref(), Some("Start failed."));
        assert_eq!(preparation.prompt.as_deref(), Some("{\"text\":\"one\"}"));
        let task = tasks
            .load_platform_task(session_id)
            .await
            .expect("task should load")
            .expect("task should exist");
        assert_eq!(task.pending_prompt, None);
    }
    let canceled = tasks
        .load_platform_task("canceled")
        .await
        .expect("task should load")
        .expect("task should exist");
    assert_eq!(
        canceled.pending_prompt.as_deref(),
        Some("{\"text\":\"one\"}")
    );
}

#[tokio::test]
async fn recovery_retains_the_prompt_of_an_answered_start_that_never_ran() {
    // Arrange
    let (database, _) =
        database_with_sessions(&[("unstarted", "Draft"), ("started", "InProgress")]).await;
    let tasks = database.platform_tasks();
    for session_id in ["unstarted", "started"] {
        begin(&database, session_id).await;
        tasks
            .save_platform_task_prompt(session_id, "{\"text\":\"one\"}")
            .await
            .expect("prompt should save");
        tasks
            .hold_platform_task(session_id, task_twelve(), "clm_secret")
            .await
            .expect("task should be held");
    }

    // Act
    tasks
        .recover_platform_tasks()
        .await
        .expect("recovery should run");

    // Assert
    let preparation = database
        .sessions()
        .load_session_preparation("unstarted")
        .await
        .expect("preparation should load")
        .expect("preparation should exist");
    assert_eq!(preparation.state, crate::SessionPreparationState::Failed);
    assert_eq!(preparation.prompt.as_deref(), Some("{\"text\":\"one\"}"));
    for session_id in ["unstarted", "started"] {
        let task = tasks
            .load_platform_task(session_id)
            .await
            .expect("task should load")
            .expect("task should exist");
        assert_eq!(task.pending_prompt, None);
        assert_eq!(task.claim_id.as_deref(), Some("clm_secret"));
    }
}

#[tokio::test]
async fn recovery_interrupts_unanswered_checks_and_deletion_removes_the_task() {
    // Arrange
    let (database, _) =
        database_with_sessions(&[("checking", "Draft"), ("conflict", "Draft")]).await;
    let tasks = database.platform_tasks();
    begin(&database, "checking").await;
    begin(&database, "conflict").await;
    tasks
        .record_platform_task_conflict("conflict", task_twelve(), Some("laptop 81ac04"), 1)
        .await
        .expect("conflict should record");

    // Act
    tasks
        .recover_platform_tasks()
        .await
        .expect("recovery should run");
    let interrupted = tasks
        .load_platform_task("checking")
        .await
        .expect("task should load")
        .expect("task should exist");
    let conflict = tasks
        .load_platform_task("conflict")
        .await
        .expect("task should load")
        .expect("task should exist");
    database
        .sessions()
        .delete_session("checking")
        .await
        .expect("session should delete");
    let deleted = tasks
        .load_platform_task("checking")
        .await
        .expect("task should load");

    // Assert
    assert_eq!(interrupted.state, PlatformTaskState::Interrupted);
    assert_eq!(conflict.state, PlatformTaskState::Conflict);
    assert_eq!(deleted, None);
}
