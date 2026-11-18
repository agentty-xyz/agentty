//! Approval routing unit tests: asks reach their run's forwarder, answers
//! reach the waiting run, and calls no client can answer are denied.

use ag_harness::event::RunId;
use ag_harness::tool::ToolEffects;
use ag_harness::{ApprovalHandler, ApprovalRequest, Decision, RunEventKind};
use serde_json::{Value, json};

use super::{Approvals, NO_CLIENT};

/// Returns the request of a call by `run`, read from the wire form of an
/// `approval_requested` event, since requests are not constructible here.
fn approval_request(run: Option<&str>) -> ApprovalRequest {
    let kind: RunEventKind = serde_json::from_value(json!({
        "type": "approval_requested",
        "id": 0,
        "request": {
            "arguments": {"path": "a.txt"},
            "call_id": "call-1",
            "effects": "writes",
            "run": run,
            "session": "s",
            "tool": "write"
        }
    }))
    .expect("approval event should decode");
    let RunEventKind::ApprovalRequested { request, .. } = kind else {
        unreachable!("decoded as an approval event");
    };

    request
}

fn run_id(id: &str) -> RunId {
    serde_json::from_value(json!(id)).expect("run identifier should decode")
}

fn no_client() -> Decision {
    Decision::Deny {
        feedback: NO_CLIENT.to_string(),
    }
}

#[tokio::test]
async fn a_call_without_an_open_run_is_denied() {
    // Arrange
    let approvals = Approvals::default();

    // Act
    let outside_a_run = approvals.decide(&approval_request(None)).await;
    let unknown_run = approvals.decide(&approval_request(Some("gone"))).await;

    // Assert
    assert_eq!(outside_a_run, no_client());
    assert_eq!(unknown_run, no_client());
}

#[tokio::test]
async fn an_answer_reaches_the_run_waiting_for_it() {
    // Arrange
    let approvals = Approvals::default();
    let mut asks = approvals.open(run_id("run-1"));
    let waiting = {
        let approvals = approvals.clone();
        tokio::spawn(async move { approvals.decide(&approval_request(Some("run-1"))).await })
    };

    // Act
    let ask = asks.recv().await.expect("the run should ask");
    let request = approvals.request("s".to_string(), ask);
    let wire = serde_json::to_value(&request).expect("request should encode");
    approvals.answer(&json!(42), Decision::Allow);
    approvals.answer(&json!("approval-9"), Decision::Allow);
    approvals.answer(&wire["id"], Decision::AllowWith(json!({"path": "b.txt"})));
    let decision = waiting.await.expect("decide task");

    // Assert
    assert_eq!(decision, Decision::AllowWith(json!({"path": "b.txt"})));
    assert_eq!(wire["id"], json!("approval-0"));
    assert_eq!(wire["method"], json!("approval/decide"));
    assert_eq!(wire["params"]["session"], json!("s"));
    assert_eq!(wire["params"]["request"]["call_id"], json!("call-1"));
    assert_eq!(
        wire["params"]["request"]["effects"],
        serde_json::to_value(ToolEffects::Writes).expect("effects encode")
    );
}

#[tokio::test]
async fn abandoned_decisions_are_dropped_and_dropped_asks_deny() {
    // Arrange
    let approvals = Approvals::default();
    let run = run_id("run-1");
    let mut asks = approvals.open(run.clone());
    let waiting = {
        let approvals = approvals.clone();
        tokio::spawn(async move { approvals.decide(&approval_request(Some("run-1"))).await })
    };
    let abandoned = asks.recv().await.expect("the run should ask");
    let abandoned_id = serde_json::to_value(approvals.request("s".to_string(), abandoned))
        .expect("request should encode")["id"]
        .clone();
    waiting.abort();
    let _ = waiting.await;

    // Act
    let late = {
        let approvals = approvals.clone();
        tokio::spawn(async move { approvals.decide(&approval_request(Some("run-1"))).await })
    };
    let dropped = asks.recv().await.expect("the run should ask again");
    // A second request sweeps the decision its aborted run abandoned.
    approvals.request("s".to_string(), dropped);
    approvals.answer(&abandoned_id, Decision::Allow);
    approvals.close(&run);
    let after_close = approvals.decide(&approval_request(Some("run-1"))).await;
    let next = {
        let approvals = approvals.clone();
        tokio::spawn(async move {
            let mut asks = approvals.open(run_id("run-2"));
            let request = approval_request(Some("run-2"));
            let decided = approvals.decide(&request);
            let (decided, ask) = tokio::join!(decided, async { drop(asks.recv().await) });

            (decided, ask)
        })
    };
    let (dropped_ask, ()) = next.await.expect("decide task");
    late.abort();

    // Assert
    assert_eq!(abandoned_id, Value::String("approval-0".to_string()));
    assert_eq!(after_close, no_client());
    assert_eq!(
        dropped_ask,
        no_client(),
        "an ask dropped unregistered denies the call"
    );
}
