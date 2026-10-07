use serde_json::json;

use super::{AdmissionState, ModelSwitch, NewTurn, TurnAdmission};
use crate::input::TurnInput;
use crate::model::{ModelCapabilities, ModelMessage};
use crate::policy::ToolPolicy;
use crate::recovery::{
    ExecutionIdentity, HostRequest, HostTurnAcquisition, HostTurnRecord, HostTurnStatus,
};
use crate::schema_contract::OutputSchema;
use crate::store::{Admission, StoredTurnOptions};
use crate::{SessionError, Tool, TurnLimits, TurnOptions};

fn options(tool_policy: ToolPolicy) -> TurnOptions {
    TurnOptions::new(
        OutputSchema::new(json!({"type": "object"})).expect("schema"),
        tool_policy,
        TurnLimits::default(),
    )
}

fn request(fingerprint: &str) -> HostRequest {
    HostRequest::from_configuration("id".into(), json!({"request": fingerprint})).expect("request")
}

fn record(request: HostRequest) -> HostTurnRecord {
    HostTurnRecord {
        commands: Vec::new(),
        model: None,
        request,
        status: HostTurnStatus::InProgress,
        turn_position: 0,
        writes: Vec::new(),
    }
}

fn admit(admission: &TurnAdmission, state: AdmissionState) -> NewTurn {
    match admission.admit("session", state).expect("admitted") {
        Admission::Reserve(turn) => turn,
        Admission::Recorded(_) => std::panic::resume_unwind(Box::new("unexpected record")),
    }
}

fn switch(generation: i64, capabilities: ModelCapabilities) -> ModelSwitch {
    ModelSwitch::new(
        ExecutionIdentity::new("key", "revision").expect("identity"),
        None,
        capabilities,
        generation,
    )
}

#[test]
fn recorded_request_is_classified_before_generation_and_busy_state() {
    // Arrange
    let admission = TurnAdmission::new(
        TurnInput::from("retry"),
        options(ToolPolicy::default()),
        Some(request("same")),
        0,
    );
    let state = AdmissionState {
        active_turn: true,
        model_generation: 7,
        request: Some(record(request("same"))),
        ..AdmissionState::default()
    };

    // Act
    let admitted = admission.admit("session", state).expect("classified");

    // Assert
    assert_eq!(admission.host_id(), Some("id"));
    assert!(matches!(admitted, Admission::Recorded(record) if record.request == request("same")));
}

#[test]
fn reused_host_id_with_another_request_conflicts() {
    // Arrange
    let admission = TurnAdmission::new(
        TurnInput::from("retry"),
        options(ToolPolicy::default()),
        Some(request("changed")),
        0,
    );
    let state = AdmissionState {
        request: Some(record(request("original"))),
        ..AdmissionState::default()
    };

    // Act
    let result = admission.admit("session", state);

    // Assert
    assert!(matches!(result, Err(SessionError::HostTurnConflict)));
}

#[test]
fn turn_admission_requires_current_generation_and_an_idle_session() {
    // Arrange
    let admission = TurnAdmission::new(
        TurnInput::from("prompt"),
        options(ToolPolicy::default()),
        None,
        0,
    );
    let stale = AdmissionState {
        model_generation: 1,
        ..AdmissionState::default()
    };
    let active = AdmissionState {
        active_turn: true,
        ..AdmissionState::default()
    };
    let unresolved = AdmissionState {
        unresolved_commands: true,
        ..AdmissionState::default()
    };

    // Act
    let results = [stale, active, unresolved].map(|state| admission.admit("session", state));

    // Assert
    assert_eq!(admission.host_id(), None);
    assert!(matches!(&results[0], Err(SessionError::StaleModel { id }) if id == "session"));
    assert!(matches!(&results[1], Err(SessionError::Busy { id }) if id == "session"));
    assert!(matches!(&results[2], Err(SessionError::Busy { id }) if id == "session"));
}

#[test]
fn admitted_turn_keeps_continuation_only_for_compatible_options() {
    // Arrange
    let selected = options(ToolPolicy::default());
    let admission = TurnAdmission::new(
        TurnInput::from("prompt"),
        selected.clone(),
        Some(request("new")),
        0,
    );
    let state = |latest_options: Option<String>| AdmissionState {
        latest_options,
        provider_session_id: Some("continuation".into()),
        ..AdmissionState::default()
    };
    let incompatible = StoredTurnOptions::encode(&options(ToolPolicy::default().allow(Tool::Read)));

    // Act
    let compatible = admit(
        &admission,
        state(Some(StoredTurnOptions::encode(&selected))),
    );
    let changed = admit(&admission, state(Some(incompatible)));
    let first = admit(&admission, state(None));

    // Assert
    assert_eq!(compatible.continuation(), Some("continuation"));
    assert_eq!(compatible.message(), &ModelMessage::User("prompt".into()));
    assert_eq!(compatible.options(), StoredTurnOptions::encode(&selected));
    assert_eq!(compatible.request(), Some(&request("new")));
    assert_eq!(changed.continuation(), None);
    assert_eq!(first.continuation(), None);
}

#[test]
fn undecodable_latest_options_reject_admission() {
    // Arrange
    let admission = TurnAdmission::new(
        TurnInput::from("prompt"),
        options(ToolPolicy::default()),
        None,
        0,
    );
    let state = AdmissionState {
        latest_options: Some("{}".into()),
        ..AdmissionState::default()
    };

    // Act
    let result = admission.admit("session", state);

    // Assert
    assert!(matches!(result, Err(SessionError::InvalidData { .. })));
}

#[test]
fn model_switch_requires_an_idle_current_selection_and_advances_it() {
    // Arrange
    let switch = switch(2, ModelCapabilities::default());
    let busy = AdmissionState {
        active_turn: true,
        model_generation: 2,
        ..AdmissionState::default()
    };
    let stale = AdmissionState::default();
    let idle = AdmissionState {
        model_generation: 2,
        ..AdmissionState::default()
    };

    // Act
    let busy = switch.admit("session", &busy);
    let stale = switch.admit("session", &stale);
    let next = switch.admit("session", &idle);

    // Assert
    assert_eq!(switch.identity().key(), "key");
    assert!(switch.metadata().is_none());
    assert!(matches!(busy, Err(SessionError::Busy { .. })));
    assert!(matches!(stale, Err(SessionError::StaleModel { .. })));
    assert_eq!(next.expect("next generation"), 3);
}

#[test]
fn model_switch_rejects_history_the_target_cannot_replay() {
    // Arrange
    let switch = switch(0, ModelCapabilities::default());
    let portable = [ModelMessage::User("prompt".into())];
    let reasoning = [ModelMessage::AssistantReasoning {
        content: "{}".into(),
        reasoning_content: "private".into(),
    }];

    // Act
    let portable = switch.check_history(&portable);
    let reasoning = switch.check_history(&reasoning);

    // Assert
    assert!(portable.is_ok());
    assert!(matches!(
        reasoning,
        Err(SessionError::UnsupportedModelHistory { .. })
    ));
}

#[test]
fn plain_turn_rejects_a_recorded_acquisition() {
    // Arrange
    let acquisition = HostTurnAcquisition::Recorded(record(request("recorded")));

    // Act
    let result = acquisition.into_acquired();

    // Assert
    assert!(matches!(result, Err(SessionError::HostTurnConflict)));
}
