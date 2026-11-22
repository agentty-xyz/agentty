//! The web client's relay modules, run in Node: against a real relay and
//! server, so the JavaScript handshake cannot drift from `ag-noise`, and
//! against an in-memory key store with the faults a browser can show.

use std::process::Stdio;
use std::time::Duration;

use ag_remote::{StaticKeypair, pairing_link};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};

use crate::support::{TestResult, start_linked_server, start_relay};

/// Node script that drives the web client's relay modules.
const DEVICE_SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/web/relay_device.mjs");
/// Node script that runs the client's key storage over a fake `IndexedDB`.
const KEY_SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/web/device_key_check.mjs"
);
/// Longest wait for the script's next step before a test fails.
const STEP_TIMEOUT: Duration = Duration::from_secs(20);
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
