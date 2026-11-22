//! The web client's relay modules, run in Node against a real relay and
//! server, so the JavaScript handshake cannot drift from `ag-noise`.

use std::process::Stdio;
use std::time::Duration;

use ag_remote::{StaticKeypair, pairing_link};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};

use crate::relay::{start_linked_server, start_relay};
use crate::support::TestResult;

/// Node script that drives the web client's relay modules.
const DEVICE_SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/web/relay_device.mjs");
/// Longest wait for the script's next step before a test fails.
const STEP_TIMEOUT: Duration = Duration::from_secs(20);
/// Message size that needs several Noise messages in each direction.
const LARGE_MESSAGE_BYTES: usize = 200_000;

/// A running device script and the steps it reports.
struct WebDevice {
    process: Child,
    steps: Lines<BufReader<ChildStdout>>,
}

impl WebDevice {
    /// Starts the script in `mode` against the relay page `link` points at.
    fn start(mode: &str, link: &str) -> TestResult<Self> {
        let mut process = Command::new("node")
            .arg(DEVICE_SCRIPT)
            .arg(mode)
            .arg(link)
            .arg(LARGE_MESSAGE_BYTES.to_string())
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
