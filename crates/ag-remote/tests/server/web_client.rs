//! The web client, run in Node: its modules' unit suites, `app.js` over a
//! fake DOM against a real server, the relay modules against a real relay
//! and server, so the JavaScript handshake cannot drift from `ag-noise`,
//! and the key store with the faults a browser can show.

use std::fs;
use std::process::Stdio;
use std::time::Duration;

use ag_remote::{StaticKeypair, pairing_link};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};

use crate::support::{TestResult, TestServer, start_linked_server, start_relay};

/// Directory of the web client's Node scripts and suites.
const WEB_TESTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/web");
/// Node script that runs `app.js` over the fake DOM.
const APP_SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/web/app_flow.mjs");
/// Node script that drives the web client's relay modules.
const DEVICE_SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/web/relay_device.mjs");
/// Node script that runs the client's key storage over a fake `IndexedDB`.
const KEY_SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/web/device_key_check.mjs"
);
/// Longest wait for the script's next step before a test fails.
const STEP_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest run of one `node --test` suite.
const SUITE_TIMEOUT: Duration = Duration::from_secs(60);
/// Message size that needs several Noise messages in each direction.
const LARGE_MESSAGE_BYTES: usize = 200_000;

/// A running Node script and the steps it reports.
struct WebDevice {
    process: Child,
    steps: Lines<BufReader<ChildStdout>>,
}

impl WebDevice {
    /// Starts the device script in `mode` against the relay page `link`
    /// points at.
    fn start(mode: &str, link: &str) -> TestResult<Self> {
        Self::run(
            DEVICE_SCRIPT,
            &[mode, link, &LARGE_MESSAGE_BYTES.to_string()],
        )
    }

    /// Starts `script` with `arguments`.
    fn run(script: &str, arguments: &[&str]) -> TestResult<Self> {
        let mut process = Command::new("node")
            .arg(script)
            .args(arguments)
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("Node.js 22 or newer must be on PATH: {error}"))?;
        let stdout = process.stdout.take().ok_or("script has no output")?;

        Ok(Self {
            process,
            steps: BufReader::new(stdout).lines(),
        })
    }

    /// Returns the next step the script reports.
    async fn next_step(&mut self) -> TestResult<Value> {
        let line = tokio::time::timeout(STEP_TIMEOUT, self.steps.next_line())
            .await??
            .ok_or("script ended early")?;

        Ok(serde_json::from_str(&line)?)
    }

    /// Waits for the script to exit and reports whether it succeeded.
    async fn succeeded(mut self) -> TestResult<bool> {
        let status = tokio::time::timeout(STEP_TIMEOUT, self.process.wait()).await??;

        Ok(status.success())
    }
}

/// Outcome of one `node --test` run.
struct SuiteRun {
    /// Whether every test in the suite passed.
    passed: bool,
    /// What Node printed, for the failure message.
    report: String,
}

impl SuiteRun {
    /// Runs the `node --test` suite `name` from [`WEB_TESTS`].
    async fn start(name: &str) -> TestResult<Self> {
        let output = tokio::time::timeout(
            SUITE_TIMEOUT,
            Command::new("node")
                .arg("--test")
                .arg(format!("{WEB_TESTS}/{name}"))
                .kill_on_drop(true)
                .output(),
        )
        .await?
        .map_err(|error| format!("Node.js 22 or newer must be on PATH: {error}"))?;

        Ok(Self {
            passed: output.status.success(),
            report: format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        })
    }
}

/// Returns the names of the `node --test` suites in [`WEB_TESTS`], sorted.
fn node_suites() -> TestResult<Vec<String>> {
    let mut suites = Vec::new();
    for entry in fs::read_dir(WEB_TESTS)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.ends_with(".test.mjs") {
            suites.push(name);
        }
    }
    suites.sort_unstable();

    Ok(suites)
}

#[tokio::test]
async fn every_node_suite_in_the_web_tests_passes() -> TestResult {
    // Arrange: every suite on disk runs, so a new one needs no runner.
    let suites = node_suites()?;

    // Act
    let mut runs = Vec::new();
    for suite in &suites {
        runs.push((suite, SuiteRun::start(suite).await?));
    }

    // Assert
    for known in ["fake_dom.test.mjs", "format.test.mjs"] {
        assert!(
            suites.iter().any(|suite| suite == known),
            "{known} is missing"
        );
    }
    for (suite, run) in runs {
        assert!(run.passed, "{suite} failed:\n{}", run.report);
    }

    Ok(())
}

#[tokio::test]
async fn web_client_app_lists_opens_sends_answers_and_cancels_through_the_listener() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (_paired, _device_id, token) = server.pair("Node browser").await?;
    let address = server.handle.local_addr().to_string();

    // Act
    let mut app = WebDevice::run(APP_SCRIPT, &[&address, "session", &token])?;
    let listed = app.next_step().await?;
    let opened = app.next_step().await?;
    let answered = app.next_step().await?;
    server.revisions_tx.send(2)?;
    let refreshed = app.next_step().await?;
    let canceled = app.next_step().await?;
    let closed = app.next_step().await?;
    let running = app.next_step().await?;
    let queued = app.next_step().await?;
    server.revisions_tx.send(3)?;
    let shown = app.next_step().await?;
    let forgot = app.next_step().await?;
    let succeeded = app.succeeded().await?;

    // Assert
    assert_eq!(listed["connection"], "Connected · local listener");
    assert_eq!(listed["heading"], "agentty");
    assert_eq!(listed["isOnline"], true);
    assert_eq!(listed["view"], "home-view");
    assert_eq!(listed["groups"], json!(["Active2"]));
    assert_eq!(listed["sessions"][0]["title"], "Build it");
    assert_eq!(listed["sessions"][0]["status"], "Review");
    assert_eq!(listed["sessions"][1]["title"], "Plan it");
    assert_eq!(listed["sessions"][1]["status"], "In progress");
    let time = listed["sessions"][0]["time"].as_str().unwrap_or_default();
    assert!(time.ends_with('d'), "{time}");
    // The first listing counts as seen, so nothing starts unread.
    assert_eq!(listed["unread"], json!([]));
    assert_question_answered(&opened, &answered);
    assert_eq!(
        refreshed,
        json!({
            "step": "refreshed",
            // `changed` brought a newer `Plan it`; the open session stays read.
            "unread": ["session-2"],
            "unreadLabel": ["Unread"],
        })
    );
    assert_eq!(
        canceled,
        json!({
            "step": "canceled",
            "askedFirst": true,
            "error": "Session not found",
            "isDialogOpen": false,
            "isSheetOpen": false,
            "sheetOpened": true,
        })
    );
    assert_eq!(
        closed,
        json!({
            "step": "closed",
            "current": null,
            "hash": "",
            "heading": "agentty",
            "view": "home-view",
        })
    );
    assert_message_queued_while_running(&running, &queued, &shown);
    assert_eq!(
        forgot,
        json!({
            "step": "forgot",
            "hash": "",
            "isMainInert": false,
            "routed": "#s=session-1",
            "token": null,
            "view": "pair-view",
        })
    );
    assert!(succeeded);
    let mutations: Vec<String> = server
        .backend
        .calls()
        .into_iter()
        .filter(|call| !call.starts_with("list") && !call.starts_with("get:"))
        .collect();
    assert_eq!(
        mutations,
        [
            "answer:session-1:Which branch?=main",
            "cancel:session-1",
            "send:session-2:Use the main branch",
        ]
    );

    Ok(())
}

#[tokio::test]
async fn web_client_app_reopens_the_routed_session_with_its_draft_through_the_listener()
-> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (_paired, _device_id, token) = server.pair("Node browser").await?;
    let address = server.handle.local_addr().to_string();

    // Act
    let mut app = WebDevice::run(APP_SCRIPT, &[&address, "route", &token])?;
    let restored = app.next_step().await?;
    let sent = app.next_step().await?;
    let succeeded = app.succeeded().await?;

    // Assert
    assert_eq!(
        restored,
        json!({
            "step": "restored",
            "current": "session-2",
            "hash": "#s=session-2",
            "heading": "Plan it",
            "message": "Resume the plan",
            "view": "session-view",
        })
    );
    assert_eq!(sent, json!({"step": "sent", "draft": null, "error": ""}));
    assert!(succeeded);
    assert!(
        server
            .backend
            .calls()
            .iter()
            .any(|call| call == "send:session-2:Resume the plan")
    );

    Ok(())
}

/// Checks the steps where the app opens `Build it`, which waits on a
/// question, and answers it.
fn assert_question_answered(opened: &Value, answered: &Value) {
    assert_eq!(
        opened,
        &json!({
            "step": "opened",
            // A question replaces the composer with the answer bar.
            "answerCount": "0 of 1 answered",
            "answerSendDisabled": true,
            "cancelHidden": false,
            "composerHidden": true,
            "current": "page",
            "hash": "#s=session-1",
            "heading": "Build it",
            "mergeDisabled": true,
            "mergeHidden": false,
            "moreHidden": false,
            "questions": [{
                "eyebrow": "Question 1 of 1",
                "options": ["main"],
                "text": "Which branch?",
            }],
            "status": "Question",
            "transcript": [
                {"kind": "user", "text": "build it"},
                {"kind": "assistant", "text": "which branch?"},
            ],
            "view": "session-view",
        })
    );
    assert_eq!(
        answered,
        &json!({
            "step": "answered",
            "chosen": {
                "answerCount": "1 of 1 answered",
                "answerSendDisabled": false,
                "pressed": "true",
            },
            "error": "",
            "isSending": true,
        })
    );
}

/// Checks the steps where the app opens `Plan it`, which runs a turn,
/// queues a message, and shows it once the host reports the change.
fn assert_message_queued_while_running(running: &Value, queued: &Value, shown: &Value) {
    assert_eq!(
        running,
        &json!({
            "step": "running",
            "activity": {
                "isOpen": true,
                "lines": [
                    ["tone-success", "✓ Read plan.md"],
                    ["tone-error", "✗ Run tests"],
                    ["tone-info", "… Edit plan.md"],
                ],
                "summary": "Activity3 lines",
            },
            "answerBarHidden": true,
            "composerHidden": false,
            "hint": "Sending queues the message",
            "notice": {"isError": true, "label": "Commit Error", "text": "hook failed"},
            "placeholder": "Queue a message for the next turn",
            "sendLabel": "Queue message",
            "stopHidden": true,
            "transcript": [
                {"kind": "user", "text": "plan it"},
                {
                    "kind": "activity",
                    "text": "Activity3 lines✓ Read plan.md✗ Run tests… Edit plan.md",
                },
                {"kind": "notice", "text": "Commit Errorhook failed"},
            ],
        })
    );
    assert_eq!(
        queued,
        &json!({
            "step": "queued",
            "enterPrevented": false,
            "error": "",
            "keptOnEnter": "Use the main branch",
        })
    );
    assert_eq!(
        shown,
        &json!({
            "step": "shown",
            "keptActivity": true,
            "queued": ["Use the main branch"],
            "queuedBoxHidden": false,
        })
    );
}

#[tokio::test]
async fn web_client_app_shows_the_confirmation_code_then_the_denial_through_the_listener()
-> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let code = server.handle.issue_pairing_code()?.code;
    let address = server.handle.local_addr().to_string();

    // Act
    let mut app = WebDevice::run(APP_SCRIPT, &[&address, "pair", &code])?;
    let awaiting = app.next_step().await?;
    let request = server
        .handle
        .status()
        .wait_for(|status| !status.pairing_requests.is_empty())
        .await?
        .pairing_requests[0]
        .clone();
    let denied_request = server.handle.deny_pairing(request.id);
    let denied = app.next_step().await?;
    let succeeded = app.succeeded().await?;

    // Assert
    assert_eq!(request.device_name, "Node browser");
    assert_eq!(
        awaiting,
        json!({
            "step": "awaiting",
            "cancelHidden": false,
            "confirmation": request.confirmation,
            "connecting": {
                "cancelHidden": false,
                "label": "Connecting to Agentty",
                "submitHidden": true,
            },
            "label": "Waiting for approval in Agentty",
            "connection": "Waiting for approval",
            "submitHidden": true,
            "view": "pair-view",
        })
    );
    assert!(denied_request);
    assert_eq!(
        denied,
        json!({
            "step": "denied",
            "cancelHidden": true,
            "confirmHidden": true,
            "connection": "Disconnected",
            "error": "Pairing was denied",
            "submitHidden": false,
            "view": "pair-view",
        })
    );
    assert!(succeeded);
    assert_eq!(server.devices.device_ids(), Vec::<String>::new());

    Ok(())
}

#[tokio::test]
async fn web_client_pairs_exchanges_large_frames_and_reconnects_through_the_relay() -> TestResult {
    // Arrange
    let relay = start_relay().await?;
    let instance = StaticKeypair::generate()?;
    let server = start_linked_server(&relay, &instance).await?;
    let code = server.handle.issue_pairing_code()?.code;
    let relay_url = format!("ws://{}", relay.local_addr());
    let link = pairing_link(&relay_url, instance.public(), &code);

    // Act
    let mut device = WebDevice::start("pair", &link)?;
    let awaiting = device.next_step().await?;
    let request_id = server
        .handle
        .status()
        .wait_for(|status| !status.pairing_requests.is_empty())
        .await?
        .pairing_requests[0]
        .id;
    server.handle.approve_pairing(request_id);
    let paired = device.next_step().await?;
    let accepted = device.next_step().await?;
    let echoed = device.next_step().await?;
    let welcomed = device.next_step().await?;
    let succeeded = device.succeeded().await?;

    // Assert
    assert_eq!(awaiting["type"], "awaiting_approval");
    assert_eq!(paired["type"], "paired");
    assert_eq!(paired.get("token"), None);
    assert_eq!(
        accepted,
        json!({"type": "response", "id": 1, "result": {"ok": {"kind": "accepted"}}})
    );
    assert_eq!(echoed, json!({"echoed": true}));
    assert_eq!(welcomed["type"], "welcome");
    assert_eq!(welcomed["device_id"], paired["device_id"]);
    assert!(succeeded);
    assert_eq!(server.devices.token_hashes().len(), 1);

    Ok(())
}

#[tokio::test]
async fn web_client_with_an_unpaired_key_is_refused_through_the_relay() -> TestResult {
    // Arrange
    let relay = start_relay().await?;
    let instance = StaticKeypair::generate()?;
    let _server = start_linked_server(&relay, &instance).await?;
    let relay_url = format!("ws://{}", relay.local_addr());
    let link = pairing_link(&relay_url, instance.public(), "UNUSED");

    // Act
    let mut device = WebDevice::start("hello", &link)?;
    let refused = device.next_step().await?;
    let succeeded = device.succeeded().await?;

    // Assert
    assert_eq!(
        refused,
        json!({"type": "rejected", "reason": "Unknown device token"})
    );
    assert!(succeeded);

    Ok(())
}

#[tokio::test]
async fn web_client_keeps_its_device_key_through_storage_faults() -> TestResult {
    // Arrange
    let mut storage = WebDevice::run(KEY_SCRIPT, &[])?;

    // Act
    let key_objects = storage.next_step().await?;
    let fallback = storage.next_step().await?;
    let write_failure = storage.next_step().await?;
    let read_failure = storage.next_step().await?;
    let upgrade = storage.next_step().await?;
    let broken_copy = storage.next_step().await?;
    let forget = storage.next_step().await?;
    let unsaved = storage.next_step().await?;
    let succeeded = storage.succeeded().await?;

    // Assert
    assert_eq!(
        key_objects,
        json!({
            "scenario": "key_objects",
            "extractable": false,
            "origins": ["created", "saved"],
            "sameKey": true,
            "stored": ["device"],
        })
    );
    assert_eq!(
        fallback,
        json!({
            "scenario": "fallback",
            "extractable": false,
            "origins": ["created", "saved"],
            "sameKey": true,
            "stored": ["device-wrapped"],
        })
    );
    assert_eq!(
        write_failure,
        json!({"scenario": "write_failure", "origin": "saved", "sameKey": true})
    );
    assert_eq!(
        read_failure,
        json!({
            "scenario": "read_failure",
            "origins": ["unreadable", "saved"],
            "recoveredSameKey": true,
            "stored": ["device-wrapped"],
            "temporaryKeyDiffers": true,
        })
    );
    assert_eq!(
        upgrade,
        json!({
            "scenario": "upgrade",
            "extractable": false,
            "origin": "saved",
            "sameKey": true,
            "stored": ["device"],
        })
    );
    assert_eq!(
        broken_copy,
        json!({
            "scenario": "broken_copy",
            "newKey": true,
            "origins": ["created", "saved"],
            "sameKey": true,
        })
    );
    assert_eq!(
        forget,
        json!({
            "scenario": "forget",
            "newKey": true,
            "origin": "created",
            "storedAfterForget": [],
        })
    );
    assert_eq!(
        unsaved,
        json!({"scenario": "unsaved", "origin": "created", "stored": []})
    );
    assert!(succeeded);

    Ok(())
}
