//! The web client, run in Node: its modules' unit suites, `app.js` over a
//! fake DOM against a real server, the relay modules against a real relay
//! and server, so the JavaScript handshake cannot drift from `ag-noise`,
//! and the key store with the faults a browser can show.

use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ag_relay::{WebClientLink, WebClientOptions, WebClientPage, WebClientSection};
use ag_remote::{StaticKeypair, pairing_link};
use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::net::TcpListener;
use tokio::process::{Child, ChildStdout, Command};

use crate::support::{TestResult, TestServer, start_linked_server, start_relay};

/// Directory of the web client's Node scripts and suites.
const WEB_TESTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/web");
/// Node script that runs `app.js` over the fake DOM.
const APP_SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/web/app_flow.mjs");
/// Node script that drives the web client's relay modules.
const DEVICE_SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/web/relay_device.mjs");
/// Node script that runs `app.js` over the fake DOM through a relay.
const RELAY_APP_SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/web/relay_app_flow.mjs");
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
    /// Starts the `node --test` suite `name` from [`WEB_TESTS`], so the
    /// suites run side by side.
    fn start(name: &str) -> TestResult<Child> {
        let process = Command::new("node")
            .arg("--test")
            .arg(format!("{WEB_TESTS}/{name}"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("Node.js 22 or newer must be on PATH: {error}"))?;

        Ok(process)
    }

    /// Waits for a started suite to exit and collects its outcome.
    async fn finish(process: Child) -> TestResult<Self> {
        let output = tokio::time::timeout(SUITE_TIMEOUT, process.wait_with_output()).await??;

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

    // Act: start them all, then collect each outcome.
    let mut started = Vec::new();
    for suite in &suites {
        started.push((suite, SuiteRun::start(suite)?));
    }
    let mut runs = Vec::new();
    for (suite, process) in started {
        runs.push((suite, SuiteRun::finish(process).await?));
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
    let graph = app.next_step().await?;
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
    // Only the running session comes with figures, so only it has a size.
    assert_eq!(listed["sessions"][1]["title"], "[M] Plan it");
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
    assert_graph_requested(&graph);
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
        .filter(|call| !call.starts_with("list") && !call.starts_with("get:") && call != "host")
        .collect();
    assert_eq!(
        mutations,
        [
            "answer:session-1:Which branch?=main",
            "cancel:session-1",
            "answer:session-1:Where should the graph run?=Locally@c0ffee",
            "graph_proposal:session-1:Split the parser",
            "send:session-2:Use the main branch",
        ]
    );

    Ok(())
}

/// Checks the step where `Build it` waits on its graph work's question, the
/// page answers it, then replies in chat instead, and `/graph` asks for a
/// graph proposal.
fn assert_graph_requested(graph: &Value) {
    assert_eq!(
        graph,
        &json!({
            "step": "graph",
            "armed": {
                "markerHidden": false,
                "placeholder": "Describe the task for the execution graph",
            },
            "commands": ["/graph"],
            "composerHidden": false,
            "error": "",
            "markerHidden": true,
            "question": {
                "answerChatHidden": false,
                "answerError": "",
                "composerHidden": true,
                "source": "The execution graph asks. Answer here, or reply in chat.",
                "text": "Where should the graph run?",
            },
            "questionsHidden": true,
        })
    );
}

#[tokio::test]
async fn web_client_app_creates_sends_and_opens_a_session_from_home_through_the_listener()
-> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (_paired, _device_id, token) = server.pair("Node browser").await?;
    let address = server.handle.local_addr().to_string();

    // Act
    let mut app = WebDevice::run(APP_SCRIPT, &[&address, "create", &token])?;
    let home = app.next_step().await?;
    let created = app.next_step().await?;
    let succeeded = app.succeeded().await?;

    // Assert
    assert_eq!(
        home,
        json!({
            "step": "home",
            "eyebrow": "New session · agentty · base main",
            // The Node page has no fine pointer, as on a phone.
            "hint": "Enter adds a line",
            "placeholder": "Describe the task",
            "settings": "gpt-6.1-sol [high]·balanced·normal speed",
            "settingsButton": true,
            // The test host offers one mode, so there is nothing to choose.
            "modesHidden": true,
            "newSessionHidden": false,
            "project": "agentty",
            "title": "What should the agent work on?",
        })
    );
    // The test host creates `session-2`, titled `Plan it`.
    assert_eq!(
        created,
        json!({
            "step": "created",
            "error": "",
            "hash": "#s=session-2",
            "heading": "Plan it",
            "homeMessage": "",
            "view": "session-view",
        })
    );
    assert!(succeeded);
    let calls = server.backend.calls();
    let create = calls
        .iter()
        .position(|call| call == "create:Regular:1")
        .ok_or("no session was created")?;
    let send = calls
        .iter()
        .position(|call| call == "send:session-2:Add a relay image")
        .ok_or("no first message was sent")?;
    assert!(calls.iter().any(|call| call == "host"));
    assert!(create < send);

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
    let graphs = app.next_step().await?;
    let diff = app.next_step().await?;
    let settings = app.next_step().await?;
    let sync = app.next_step().await?;
    let project = app.next_step().await?;
    server.revisions_tx.send(2)?;
    let syncing = app.next_step().await?;
    let succeeded = app.succeeded().await?;

    // Assert
    assert_eq!(
        project,
        json!({
            "step": "project",
            "branch": "main ↓2 ↑0",
            "error": "",
        })
    );
    assert_eq!(
        syncing,
        json!({
            "step": "syncing",
            "status": "Syncing agentty/main...",
            "syncDisabled": true,
        })
    );
    assert_eq!(
        settings,
        json!({
            "step": "settings",
            "error": "",
            "models": ["gpt-6.1-sol"],
            // `Plan it` runs a turn, so the change waits for the next one.
            "noteHidden": false,
        })
    );
    assert_eq!(
        sync,
        json!({
            "step": "sync",
            "askedFirst": true,
            "error": "Session cannot sync in status `Done`",
            "label": "Sync with main",
        })
    );
    assert_eq!(
        diff,
        json!({
            "step": "diff",
            "chatHidden": true,
            "files": ["Aa.rs+1 -0"],
            "patch": [
                ["patch-line patch-line--meta", "diff --git a/a.rs b/a.rs\n"],
                ["patch-line patch-line--meta", "+new\n"],
            ],
            "summary": "1 file · +1 -0",
        })
    );
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
    assert_graphs_shown(&graphs);
    assert!(succeeded);
    let calls = server.backend.calls();
    for expected in [
        "graphs:session-2:-:-",
        "send:session-2:Resume the plan",
        "settings:session-2:low",
        "sync:session-2",
        "sync_project",
    ] {
        assert!(
            calls.iter().any(|call| call == expected),
            "{expected} in {calls:?}"
        );
    }

    Ok(())
}

/// Checks the step where `Plan it`'s graph summary opens its graphs in
/// place of the chat and the back gesture returns to the chat.
fn assert_graphs_shown(graphs: &Value) {
    assert_eq!(
        graphs,
        &json!({
            "step": "graphs",
            "chatHidden": true,
            "hash": "#s=session-2",
            "nodes": ["●inspectAwaiting your approval", "○fixWaiting"],
            "output": "Output 1/1: report v1 (validated)",
            "position": "Graph 1/1",
            "returned": true,
            "summary": [
                "Execution graph · v11/2 done",
                "●Blocked",
                "inspect awaits your approval; reply in chat to accept it",
            ],
            "title": "●graph | revision 1 | Blocked",
        })
    );
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
            // The host serves turn stops, so a running turn offers `Stop`.
            "hint": "Sending queues the message · Stop returns the session to Review",
            "header": [
                "In progress",
                "[M]",
                "+64 -9",
                // The listing carries how far the branch is from its base.
                "wt/session- → main ↓0 ↑2",
                "codex/gpt-6.1-sol [high]",
            ],
            "isStartedShown": true,
            "isTimerShown": true,
            "meta": "codex/gpt-6.1-sol [high]·balanced·normal speed",
            "notice": {"isError": true, "label": "Commit Error", "text": "hook failed"},
            "placeholder": "Queue a message for the next turn",
            "sendLabel": "Queue message",
            "stopHidden": false,
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
            "hintsHidden": [false, true, true],
            "shellHidden": true,
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

/// Approves the first pairing request `server` lists.
async fn approve_first_request(server: &TestServer) -> TestResult {
    let request_id = server
        .handle
        .status()
        .wait_for(|status| !status.pairing_requests.is_empty())
        .await?
        .pairing_requests[0]
        .id;
    assert!(server.handle.approve_pairing(request_id));

    Ok(())
}

/// Returns the pairing link of `server`, linked to `relay` as `instance`.
fn link_of(
    relay: &ag_relay::RelayHandle,
    instance: &StaticKeypair,
    server: &TestServer,
) -> TestResult<String> {
    let relay_url = format!("ws://{}", relay.local_addr());
    let code = server.handle.issue_pairing_code()?.code;

    Ok(pairing_link(&relay_url, instance.public(), &code))
}

/// Checks the step where the relay's own page, which has no sections,
/// waits for the first approval on the full-screen card in place of the
/// shell, with no way back yet and no hint, since the link says how.
fn assert_awaiting_on_the_card(awaiting: &Value) {
    assert_eq!(awaiting["backHidden"], true);
    assert_eq!(awaiting["headings"], json!(["Pair this device"]));
    assert_eq!(awaiting["hintsHidden"], json!([true, true, true]));
    assert_eq!(awaiting["shellHidden"], true);
    assert_eq!(awaiting["view"], "pair-view");
}

#[tokio::test]
async fn web_client_app_shows_two_agenttys_through_one_relay_and_keeps_them_apart() -> TestResult {
    // Arrange: both servers list the same session ids.
    let relay = start_relay().await?;
    let first_key = StaticKeypair::generate()?;
    let second_key = StaticKeypair::generate()?;
    let first = start_linked_server(&relay, &first_key).await?;
    let second = start_linked_server(&relay, &second_key).await?;
    let first_link = link_of(&relay, &first_key, &first)?;
    let second_link = link_of(&relay, &second_key, &second)?;

    // Act
    let mut app = WebDevice::run(RELAY_APP_SCRIPT, &["multi", &first_link, &second_link])?;
    let awaiting = app.next_step().await?;
    approve_first_request(&first).await?;
    let paired = app.next_step().await?;
    let awaiting_second = app.next_step().await?;
    approve_first_request(&second).await?;
    let paired_second = app.next_step().await?;
    let sent = app.next_step().await?;
    let returned = app.next_step().await?;
    let first_calls = first.backend.calls();
    let second_calls = second.backend.calls();
    second.handle.shutdown().await;
    let offline = app.next_step().await?;
    let forgot = app.next_step().await?;
    let succeeded = app.succeeded().await?;

    // Assert
    assert_awaiting_on_the_card(&awaiting);
    assert_eq!(
        paired,
        json!({
            "step": "paired",
            "connection": "Connected · relay, encrypted",
            "hash": "",
            "project": "agentty",
            "rows": ["A/session-1", "A/session-2"],
            "sections": 0,
            "slotHidden": false,
            "stored": ["A"],
        })
    );
    // The pairing card offers a way back to the Agentty already paired.
    assert_eq!(awaiting_second["backHidden"], false);
    assert_ne!(awaiting_second["code"], awaiting["code"]);
    assert_eq!(
        paired_second,
        json!({
            "step": "pairedB",
            "forgetHidden": true,
            "forgetItems": ["Forget on Agentty A", "Forget on Agentty B"],
            "heads": [
                {"name": "Agentty A", "state": "Connected"},
                {"name": "Agentty B", "state": "Connected"},
            ],
            "rows": [
                ["A/session-1", "A/session-2"],
                ["B/session-1", "B/session-2"],
            ],
            "slotHidden": true,
            "stored": ["A", "B"],
            "view": "home-view",
        })
    );
    assert_eq!(
        sent,
        json!({
            "step": "sentB",
            "current": ["B/session-2"],
            "error": "",
            "firstHash": "#i=B&s=session-1",
            "secondHash": "#i=B&s=session-2",
        })
    );
    assert_eq!(returned["hash"], "#i=B&s=session-1");
    assert!(
        second_calls
            .iter()
            .any(|call| call == "send:session-2:Sent to B")
    );
    assert!(!first_calls.iter().any(|call| call.starts_with("send:")));
    assert!(first_calls.iter().all(|call| !call.starts_with("get:")));
    let state = offline["heads"][1]["state"].as_str().unwrap_or_default();
    assert!(["Reconnecting", "Connecting"].contains(&state), "{offline}");
    assert_eq!(offline["heads"][0]["state"], "Connected");
    assert_eq!(offline["connection"], "1 of 2 connected");
    let banner = offline["banner"].as_str().unwrap_or_default();
    assert!(banner.starts_with("Agentty B · "), "{banner}");
    assert_eq!(offline["hash"], "#i=A&s=session-2");
    assert_eq!(offline["rowsA"], json!(["A/session-1", "A/session-2"]));
    assert_eq!(
        forgot,
        json!({
            "step": "forgotB",
            "asked": " to Agentty B",
            "connection": "Connected · relay, encrypted",
            "forgetHidden": false,
            "hash": "#i=A&s=session-2",
            "rows": ["A/session-1", "A/session-2"],
            "slotHidden": false,
            "stored": ["A"],
            "view": "session-view",
        })
    );
    assert!(succeeded);

    Ok(())
}

#[tokio::test]
async fn web_client_app_keeps_pairing_another_agentty_while_one_revokes_it_through_the_relay()
-> TestResult {
    // Arrange
    let relay = start_relay().await?;
    let first_key = StaticKeypair::generate()?;
    let second_key = StaticKeypair::generate()?;
    let first = start_linked_server(&relay, &first_key).await?;
    let second = start_linked_server(&relay, &second_key).await?;
    let first_link = link_of(&relay, &first_key, &first)?;
    let second_link = link_of(&relay, &second_key, &second)?;

    // Act
    let mut app = WebDevice::run(RELAY_APP_SCRIPT, &["revoke", &first_link, &second_link])?;
    let awaiting = app.next_step().await?;
    approve_first_request(&first).await?;
    let awaiting_second = app.next_step().await?;
    let device_id = first
        .devices
        .device_ids()
        .pop()
        .ok_or("A paired no device")?;
    first.handle.revoke_device(&device_id).await?;
    let revoked = app.next_step().await?;
    approve_first_request(&second).await?;
    let paired_second = app.next_step().await?;
    let succeeded = app.succeeded().await?;

    // Assert
    assert_eq!(awaiting["step"], "awaiting");
    assert_eq!(awaiting_second["step"], "awaitingB");
    // B's pairing card still waits for approval, with its code and Cancel.
    assert_eq!(
        revoked,
        json!({
            "step": "revokedA",
            "cancelHidden": false,
            "confirmation": true,
            "error": "",
            "submitHidden": true,
            "view": "pair-view",
        })
    );
    assert_eq!(
        paired_second,
        json!({
            "step": "pairedB",
            "rows": ["B/session-1", "B/session-2"],
            "sections": 0,
            "stored": ["B"],
        })
    );
    assert!(succeeded);

    Ok(())
}

/// Starts a web application that serves `page` at `/sessions/` with the
/// client's files beside it, every response with the headers the page
/// names, and an instance directory at `/api/instances`, which lists
/// `instances` while `signed_in` holds and answers `401` after. Returns
/// its address.
async fn start_embedder(
    page: &WebClientPage,
    instances: Value,
    signed_in: Arc<AtomicBool>,
) -> TestResult<SocketAddr> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let address = listener.local_addr()?;
    let directory = move || {
        let body = instances.to_string();
        let is_signed_in = signed_in.load(Ordering::SeqCst);

        async move {
            if !is_signed_in {
                return StatusCode::UNAUTHORIZED.into_response();
            }

            ([(header::CONTENT_TYPE, "application/json")], body).into_response()
        }
    };
    let html = page.html().to_string();
    let mut page_headers = axum::http::HeaderMap::new();
    for (name, value) in page.headers() {
        page_headers.insert(
            axum::http::HeaderName::try_from(name)?,
            axum::http::HeaderValue::try_from(value)?,
        );
    }
    let mut router = Router::new().route(
        "/sessions/",
        get({
            let page_headers = page_headers.clone();

            move || {
                let response = served("text/html; charset=utf-8", html.clone(), &page_headers);

                async move { response }
            }
        }),
    );
    for file in ag_relay::web_client_files() {
        let page_headers = page_headers.clone();
        router = router.route(
            &format!("/sessions/{}", file.path),
            get(move || {
                let response = served(file.content_type, file.body.to_string(), &page_headers);

                async move { response }
            }),
        );
    }
    let router = router.route("/api/instances", get(directory));
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    Ok(address)
}

/// `body` with its content type and the page's headers.
fn served(content_type: &str, body: String, page_headers: &axum::http::HeaderMap) -> Response {
    let mut response = ([(header::CONTENT_TYPE, content_type.to_string())], body).into_response();
    response.headers_mut().extend(page_headers.clone());

    response
}

/// What the sidebar shows beside the pairing flow before any Agentty is
/// paired, or with `paired` once one is: the application's links, the
/// theme switch, and the account always, the session tools and the device
/// only with an Agentty to show.
fn sidebar_parts(paired: bool) -> Value {
    json!({
        "account": true,
        "chips": paired,
        "device": paired,
        "filter": paired,
        "sections": 2,
        "sessionList": paired,
        "slot": paired,
        "theme": true,
    })
}

/// Checks that the page another web application serves loads every file
/// the client needs with a policy that reaches `relay`, that it pairs in
/// the shell's main pane, that a first link whose code does not work
/// leaves the flow ready for another link, and that the pasted real link
/// moves on to the device name and can be canceled before it pairs.
fn assert_first_pairing_by_paste(
    served: &Value,
    wrong_code: &Value,
    awaiting: &Value,
    relay: &ag_relay::RelayHandle,
) {
    assert_eq!(
        served,
        &json!({
            "step": "served",
            "connect": format!("connect-src 'self' ws://{}", relay.local_addr()),
            "failures": [],
            "fetched": ag_relay::web_client_files().len(),
        })
    );
    assert_eq!(
        wrong_code,
        &json!({
            "step": "wrongCode",
            "card": {
                "cardHidden": true,
                "headings": ["Pair this device"],
                "hint": "Start remote control in Agentty's Platform tab and open the pairing \
                         link it shows, or scan its QR code. A pairing link from the Remote \
                         tab can be pasted here.",
                "hintsHidden": [true, true, false],
                "linkHidden": false,
                "shellHidden": false,
                "sidebar": sidebar_parts(false),
                "submitHidden": true,
                "view": "pair-pane",
            },
            "error": "Invalid or expired pairing code",
            "linkHidden": false,
            "view": "pair-pane",
        })
    );
    assert_eq!(
        awaiting,
        &json!({
            "step": "awaiting",
            "badge": "cloud",
            "canceled": {
                "cancelHidden": true,
                "confirmHidden": true,
                "submitHidden": false,
                "view": "pair-pane",
            },
            "confirmation": true,
            "connecting": {
                "cancelHidden": false,
                "label": "Connecting to Agentty",
                "submitHidden": true,
            },
            "directory": ["laptop", "server"],
            "focused": "pair-name",
            "label": "Waiting for approval in Agentty",
            "sections": ["Dashboard", "Sessions2"],
            "sidebar": sidebar_parts(false),
            "view": "pair-pane",
        })
    );
}

/// Checks that `Pair another Agentty` opens the pairing flow in the main
/// pane beside A's sessions, and that `Back to sessions` and a session
/// chosen in the sidebar leave it.
fn assert_another_pairing_beside_the_sessions(other_relay: &Value, left: &Value) {
    // Agentty's link from the Remote tab opens the relay's page, so this
    // page takes it pasted.
    assert_eq!(
        other_relay,
        &json!({
            "step": "otherRelay",
            "card": {
                "backHidden": false,
                "focused": "pair-title",
                "headings": ["Pair this device"],
                "hintHidden": false,
                "linkHidden": false,
                "rows": ["A/session-1", "A/session-2"],
                "sidebar": sidebar_parts(true),
                "view": "pair-pane",
            },
            "error": "This pairing link is for another relay. Open it from the address it names.",
            "fieldsHidden": true,
            "linkHidden": false,
            "notLink": "Paste the whole pairing link from the Remote tab.",
            "submitHidden": true,
        })
    );
    assert_eq!(
        left,
        &json!({
            "step": "leftPairing",
            "back": {
                "error": "",
                "focused": "heading",
                "headings": ["agentty"],
                "view": "home-view",
            },
            "chosen": {
                "hash": "#i=A&s=session-1",
                "headings": ["Build it"],
                "view": "session-view",
            },
            "reopened": "pair-pane",
        })
    );
}

/// The directory's answer naming `first` `laptop` and `second` `server`,
/// which never connected.
fn directory_listing(first: &StaticKeypair, second: &StaticKeypair) -> Value {
    json!({"instances": [
        {
            "instance_id": first.public().fingerprint(),
            "last_connected_at": "2026-11-01T08:00:00Z",
            "name": "laptop",
        },
        {
            "instance_id": second.public().fingerprint(),
            "last_connected_at": null,
            "name": "server",
        },
    ]})
}

/// The options of a web application that serves the page beside its
/// dashboard, reaching instances through `relay`.
fn embedder_options(relay: &ag_relay::RelayHandle) -> WebClientOptions {
    WebClientOptions {
        relay_url: Some(format!("ws://{}", relay.local_addr())),
        directory_path: Some("/api/instances".to_string()),
        brand_badge: Some("cloud".to_string()),
        sections: vec![
            WebClientSection {
                label: "Dashboard".to_string(),
                href: "/dashboard".to_string(),
                current: false,
                badge: None,
            },
            WebClientSection {
                label: "Sessions".to_string(),
                href: "/sessions/".to_string(),
                current: true,
                badge: Some("2".to_string()),
            },
        ],
        account: Some(WebClientLink {
            label: "ada".to_string(),
            href: "/account".to_string(),
        }),
    }
}

#[tokio::test]
async fn web_client_served_by_another_web_application_reaches_agenttys_through_the_relay()
-> TestResult {
    // Arrange: the page's origin is the web application's, not the relay's.
    let relay = start_relay().await?;
    let first_key = StaticKeypair::generate()?;
    let second_key = StaticKeypair::generate()?;
    let first = start_linked_server(&relay, &first_key).await?;
    let second = start_linked_server(&relay, &second_key).await?;
    let first_link = link_of(&relay, &first_key, &first)?;
    let second_link = link_of(&relay, &second_key, &second)?;
    let page = WebClientPage::new(&embedder_options(&relay))?;
    let signed_in = Arc::new(AtomicBool::new(true));
    let listing = directory_listing(&first_key, &second_key);
    let embedder = start_embedder(&page, listing, Arc::clone(&signed_in)).await?;
    let page_url = format!("http://{embedder}/sessions/");

    // Act
    let mut app = WebDevice::run(
        RELAY_APP_SCRIPT,
        &["embed", &first_link, &second_link, &page_url],
    )?;
    let served = app.next_step().await?;
    let wrong_code = app.next_step().await?;
    let awaiting = app.next_step().await?;
    approve_first_request(&first).await?;
    let paired = app.next_step().await?;
    let other_relay = app.next_step().await?;
    let left = app.next_step().await?;
    let awaiting_second = app.next_step().await?;
    // The page reads the directory again only when it shows later.
    signed_in.store(false, Ordering::SeqCst);
    approve_first_request(&second).await?;
    let paired_second = app.next_step().await?;
    let signed_out = app.next_step().await?;
    let succeeded = app.succeeded().await?;

    // Assert
    assert_first_pairing_by_paste(&served, &wrong_code, &awaiting, &relay);
    assert_eq!(
        paired,
        json!({
            "step": "paired",
            "account": ["/account", ["a", "ada"]],
            "badge": "cloud",
            "directory": [[
                "server",
                "Never connected",
                "To pair, use the pairing link from its Remote tab.",
            ]],
            "headings": ["agentty"],
            "rows": ["A/session-1", "A/session-2"],
            "sections": [
                ["Dashboard", "/dashboard", null],
                ["Sessions2", "/sessions/", "page"],
            ],
            "sidebar": sidebar_parts(true),
            "stored": ["A"],
        })
    );
    assert_another_pairing_beside_the_sessions(&other_relay, &left);
    assert_eq!(
        awaiting_second,
        json!({"step": "awaitingB", "linkHidden": true, "pasted": ""})
    );
    assert_eq!(
        paired_second,
        json!({
            "step": "pairedB",
            "directoryHidden": true,
            "forgetItems": ["Forget on laptop", "Forget on server"],
            "heads": [
                {"name": "laptop", "state": "Connected"},
                {"name": "server", "state": "Connected"},
            ],
        })
    );
    assert_eq!(
        signed_out,
        json!({
            "step": "signedOut",
            "directoryHidden": false,
            "error": "You are signed out. Sign in again to see your Agenttys.",
            "heads": [
                {"name": "laptop", "state": "Connected"},
                {"name": "server", "state": "Connected"},
            ],
            "listed": 0,
        })
    );
    assert!(succeeded);

    Ok(())
}

#[tokio::test]
async fn web_client_app_keeps_a_pairing_saved_by_an_earlier_version_through_the_relay() -> TestResult
{
    // Arrange
    let relay = start_relay().await?;
    let instance = StaticKeypair::generate()?;
    let server = start_linked_server(&relay, &instance).await?;
    let link = link_of(&relay, &instance, &server)?;

    // Act
    let mut app = WebDevice::run(RELAY_APP_SCRIPT, &["migrate", &link])?;
    let awaiting = app.next_step().await?;
    approve_first_request(&server).await?;
    let migrated = app.next_step().await?;
    let succeeded = app.succeeded().await?;

    // Assert
    assert_eq!(awaiting["step"], "keyAwaiting");
    assert_eq!(
        migrated,
        json!({
            "step": "migrated",
            "connection": "Connected · relay, encrypted",
            "legacy": null,
            "rows": ["A/session-1", "A/session-2"],
            "sections": 0,
            "stored": ["A"],
        })
    );
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
