//! The platform's Sync E2E tests against a fake platform server: a first
//! message that takes a task, a conflict resolved by taking the task over or
//! by starting separate work, a platform that cannot answer, and an account
//! without Sync that sends nothing.

use ag_session::SettingName;
use serde_json::{Value, json};
use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;
use wiremock::matchers::{body_partial_json, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::common;
use crate::common::FeatureTest;

/// Token of the saved sign-in.
const TOKEN: &str = "agp_e2e-token";
/// First message of the scenarios.
const MESSAGE: &str = "Fix the relay reconnect";

/// Starts a fake platform whose account answer enables Sync when `sync` is
/// set, and offers no relay.
async fn fake_platform(sync: bool) -> MockServer {
    let server = MockServer::start().await;
    let mut services = json!({});
    if sync {
        services["sync"] = json!({});
    }
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "account": { "login": "octocat" },
            "services": services,
        })))
        .mount(&server)
        .await;

    server
}

/// Returns the task object the fake platform names.
fn task(uri: &str) -> Value {
    json!({
        "project": "agentty",
        "number": 12,
        "title": MESSAGE,
        "url": format!("{uri}/sync/projects/agentty/tasks/12"),
    })
}

/// Answers a start with task 12, created when `created` is set.
fn held(uri: &str, created: bool) -> ResponseTemplate {
    ResponseTemplate::new(if created { 201 } else { 200 }).set_body_json(json!({
        "task": task(uri),
        "claim_id": "clm_e2e",
        "created": created,
        "in_progress": [{
            "number": 9,
            "title": "Speed up the diff view",
            "session": "laptop 81ac04",
            "last_activity_at_ms": 0,
        }],
    }))
}

/// Answers a start that another session holds task 12.
fn conflict(uri: &str) -> ResponseTemplate {
    ResponseTemplate::new(409).set_body_json(json!({
        "error": "task_in_progress",
        "message": "Another session is working on task #12",
        "task": task(uri),
        "holder": {
            "session": "laptop 81ac04",
            "claimed_at_ms": 0,
            "last_activity_at_ms": 0,
        },
    }))
}

/// Accepts every request about task 12 of a session that holds it.
async fn accept_task_requests(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path_regex("^/api/v1/sync/projects/agentty/tasks/12/"))
        .respond_with(ResponseTemplate::new(204))
        .mount(server)
        .await;
}

/// Builds the `name` scenario signed in to `server`, in a repository whose
/// `origin` remote carries credentials that must never reach the platform.
fn sync_test(name: &str, server: &MockServer) -> FeatureTest {
    let stored = json!({ "access_token": TOKEN, "platform_url": server.uri() });

    FeatureTest::new(name)
        .with_git()
        .with_terminal_size(160, 50)
        .args(["--platform-url".to_string(), server.uri()])
        .setup(move |env| {
            Box::pin(async move {
                let output = std::process::Command::new("git")
                    .args([
                        "remote",
                        "add",
                        "origin",
                        "https://user:secret@github.com/agentty-xyz/agentty.git",
                    ])
                    .current_dir(&env.workdir)
                    .output()?;
                if !output.status.success() {
                    return Err(std::io::Error::other("git remote add failed").into());
                }
                common::open_database(env)
                    .await?
                    .settings()
                    .upsert_setting(SettingName::PlatformSignIn, &stored.to_string())
                    .await?;

                Ok(())
            })
        })
}

/// Waits for the account check, then submits `MESSAGE` as the first
/// message of a new regular session.
fn submit_first_message(scenario: Scenario) -> Scenario {
    scenario
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab_reverse("Platform"))
        .wait_for_text("Sync    On", 10000)
        .compose(&common::switch_to_tab("Projects"))
        .compose(&common::switch_to_tab("Sessions"))
        .compose(&common::open_new_session_prompt())
        .write_text(MESSAGE)
        .wait_for_text(MESSAGE, 3000)
        .press_key("Enter")
}

/// Verify an account with Sync shows it on the Platform tab, and a first
/// message takes a new task, starts the session, and shows the task and
/// the repository's other work in progress.
#[tokio::test]
async fn platform_sync_task_created() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = fake_platform(true).await;
    let uri = server.uri();
    Mock::given(method("POST"))
        .and(path("/api/v1/sync/work"))
        .and(body_partial_json(json!({
            "repository": "github.com/agentty-xyz/agentty",
            "title": MESSAGE,
            "brief": MESSAGE,
        })))
        .respond_with(held(&uri, true))
        .expect(1)
        .mount(&server)
        .await;
    accept_task_requests(&server).await;

    // Act, Assert
    sync_test("platform_sync_task_created", &server)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab_reverse("Platform"))
                    .wait_for_text("Sync    On", 10000)
                    .capture_labeled("platform", "The Platform tab shows Sync on")
                    .compose(&common::switch_to_tab("Projects"))
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_new_session_prompt())
                    .write_text(MESSAGE)
                    .wait_for_text(MESSAGE, 3000)
                    .press_key("Enter")
                    .wait_for_text("recorded this session's work as task #12", 10000)
                    .capture_labeled("held", "The session holds a new task")
            },
            |_frame, report| {
                Box::pin(async move {
                    let platform = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(platform.cols(), platform.rows());
                    assertion::assert_text_in_region(
                        &platform,
                        "each new session's first message is recorded on the platform",
                        &full,
                    );

                    let held = common::frame_from_capture(&report.captures[1]);
                    assertion::assert_text_in_region(
                        &held,
                        "#9 Speed up the diff view, held by laptop 81ac04",
                        &full,
                    );
                })
            },
        )
        .await?;
    server.verify().await;
    let requests = server.received_requests().await.unwrap_or_default();
    assert!(
        requests
            .iter()
            .all(|request| !String::from_utf8_lossy(&request.body).contains("secret"))
    );

    Ok(())
}

/// Verify a conflict keeps the session unstarted, shows the task and the
/// session holding it, and `t` takes the task over and starts the session.
#[tokio::test]
async fn platform_sync_conflict_take_over() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = fake_platform(true).await;
    let uri = server.uri();
    Mock::given(method("POST"))
        .and(path("/api/v1/sync/work"))
        .and(body_partial_json(json!({ "takeover": 12 })))
        .respond_with(held(&uri, false))
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync/work"))
        .respond_with(conflict(&uri))
        .expect(1)
        .mount(&server)
        .await;
    accept_task_requests(&server).await;

    // Act, Assert
    sync_test("platform_sync_conflict_take_over", &server)
        .run(
            |scenario| {
                submit_first_message(scenario)
                    .wait_for_text("Held by laptop 81ac04", 10000)
                    .capture_labeled("conflict", "Another session holds the work")
                    .press_key("t")
                    .wait_for_text("gave this session the open task #12", 10000)
                    .capture_labeled("taken_over", "t takes the task over")
            },
            |_frame, report| {
                Box::pin(async move {
                    let conflict = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(conflict.cols(), conflict.rows());
                    assertion::assert_text_in_region(
                        &conflict,
                        "Task #12: Fix the relay reconnect",
                        &full,
                    );
                    assertion::assert_text_in_region(&conflict, "Press t to take", &full);
                    assertion::assert_text_in_region(&conflict, "n to start", &full);
                })
            },
        )
        .await?;
    server.verify().await;

    Ok(())
}

/// Verify `n` after a conflict starts the session as separate work, naming
/// the task it is not.
#[tokio::test]
async fn platform_sync_conflict_separate_work() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = fake_platform(true).await;
    let uri = server.uri();
    Mock::given(method("POST"))
        .and(path("/api/v1/sync/work"))
        .and(body_partial_json(json!({ "not_duplicates": [12] })))
        .respond_with(held(&uri, true))
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync/work"))
        .respond_with(conflict(&uri))
        .expect(1)
        .mount(&server)
        .await;
    accept_task_requests(&server).await;

    // Act, Assert
    sync_test("platform_sync_conflict_separate_work", &server)
        .run(
            |scenario| {
                submit_first_message(scenario)
                    .wait_for_text("Held by laptop 81ac04", 10000)
                    .press_key("n")
                    .wait_for_text("recorded this session's work as task #12", 10000)
                    .capture_labeled("separate", "n starts separate work")
            },
            |_frame, report| {
                Box::pin(async move {
                    let separate = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(separate.cols(), separate.rows());
                    assertion::assert_not_visible(&separate, "Press t to take");
                    assertion::assert_text_in_region(&separate, MESSAGE, &full);
                })
            },
        )
        .await?;
    server.verify().await;

    Ok(())
}

/// Verify a platform that cannot reach Sync starts the session without a
/// task and says so in the session.
#[tokio::test]
async fn platform_sync_unavailable_starts_without_a_task() -> Result<(), Box<dyn std::error::Error>>
{
    // Arrange
    let server = fake_platform(true).await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync/work"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": "sync_unavailable",
            "message": "Sync is unavailable.",
        })))
        .expect(1)
        .mount(&server)
        .await;

    // Act, Assert
    sync_test("platform_sync_unavailable_starts_without_a_task", &server)
        .run(
            |scenario| {
                submit_first_message(scenario)
                    .wait_for_text("started without a task", 10000)
                    .capture_labeled("without_task", "The session starts without a task")
            },
            |_frame, report| {
                Box::pin(async move {
                    let frame = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(&frame, "Sync is unavailable.", &full);
                    assertion::assert_not_visible(&frame, "Asking the platform's Sync");
                })
            },
        )
        .await?;
    server.verify().await;

    Ok(())
}

/// Verify an account without Sync shows it off and a first message starts
/// the session without any Sync request.
#[tokio::test]
async fn platform_sync_off_sends_nothing() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = fake_platform(false).await;
    Mock::given(path_regex("^/api/v1/sync/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    // Act, Assert
    sync_test("platform_sync_off_sends_nothing", &server)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab_reverse("Platform"))
                    .wait_for_text("Sync    Off", 10000)
                    .capture_labeled("platform", "The Platform tab shows Sync off")
                    .compose(&common::switch_to_tab("Projects"))
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_new_session_prompt())
                    .write_text(MESSAGE)
                    .wait_for_text(MESSAGE, 3000)
                    .press_key("Enter")
                    .step(common::wait_for_session_view())
                    .wait_for_stable_frame(500, 5000)
                    .capture_labeled("started", "The session starts")
            },
            |_frame, report| {
                Box::pin(async move {
                    let started = common::frame_from_capture(&report.captures[1]);
                    assertion::assert_not_visible(&started, "platform's Sync");
                })
            },
        )
        .await?;
    server.verify().await;

    Ok(())
}
