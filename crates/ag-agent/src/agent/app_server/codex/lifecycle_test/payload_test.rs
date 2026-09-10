use super::*;

#[test]
fn build_thread_start_payload_carries_method_id_cwd_and_model() {
    // Arrange
    let folder = PathBuf::from("/tmp/agentty-codex-thread-start");
    let model = AgentModel::Gpt56Sol.as_str();

    // Act
    let payload = build_thread_start_payload(
        &folder,
        model,
        PermissionMode::AutoEdit,
        ReasoningLevel::High,
        SpeedMode::Fast,
        "thread-start-1",
    );

    // Assert
    assert_eq!(
        payload.get("method").and_then(Value::as_str),
        Some("thread/start")
    );
    assert_eq!(
        payload.get("id").and_then(Value::as_str),
        Some("thread-start-1")
    );
    let params = payload
        .get("params")
        .expect("thread/start params should be present");
    assert_eq!(params.get("model").and_then(Value::as_str), Some(model));
    assert_eq!(
        params.get("serviceTier").and_then(Value::as_str),
        Some("fast")
    );
    assert_eq!(
        params.get("cwd").and_then(Value::as_str),
        Some(folder.to_string_lossy().as_ref())
    );
    assert_eq!(
        params.get("experimentalRawEvents").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        params
            .get("persistExtendedHistory")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert!(params.get("config").is_some());
}

#[test]
fn build_thread_resume_payload_uses_thread_id_for_resume() {
    // Arrange
    let model = AgentModel::Gpt56Sol.as_str();

    // Act
    let payload = build_thread_resume_payload(
        "thread-resume-1",
        "existing-thread",
        model,
        PermissionMode::AutoEdit,
        ReasoningLevel::Medium,
        SpeedMode::default(),
    );

    // Assert
    assert_eq!(
        payload.get("method").and_then(Value::as_str),
        Some("thread/resume")
    );
    assert_eq!(
        payload.get("id").and_then(Value::as_str),
        Some("thread-resume-1")
    );
    let params = payload.get("params").expect("resume params present");
    assert_eq!(
        params.get("threadId").and_then(Value::as_str),
        Some("existing-thread")
    );
    assert_eq!(params.get("model").and_then(Value::as_str), Some(model));
    assert_eq!(
        params.get("serviceTier").and_then(Value::as_str),
        Some("default")
    );
}

#[test]
fn build_turn_start_payload_uses_full_access_for_auto_edit() {
    // Arrange
    let folder = PathBuf::from("/tmp/agentty-codex-turn-start");

    // Act
    let payload = build_turn_start_payload(&CodexTurnStartPayloadInput {
        folder: &folder,
        model: AgentModel::Gpt56Sol.as_str(),
        permission_mode: PermissionMode::AutoEdit,
        prompt: "Update the repository instructions".into(),
        protocol_profile: ProtocolRequestProfile::SessionTurn,
        reasoning_level: ReasoningLevel::Medium,
        speed_mode: SpeedMode::default(),
        thread_id: "thread-1",
        turn_start_id: "turn-start-1",
    });

    // Assert
    let sandbox_policy = payload
        .pointer("/params/sandboxPolicy")
        .expect("turn/start sandbox policy should be present");
    assert_eq!(
        sandbox_policy,
        &serde_json::json!({
            "type": "dangerFullAccess",
        })
    );
    assert_eq!(
        payload
            .pointer("/params/serviceTier")
            .and_then(Value::as_str),
        Some("default")
    );
}

#[test]
fn read_only_payloads_deny_writes_network_and_pre_action_requests() {
    // Arrange
    let folder = PathBuf::from("/tmp/agentty-codex-research");
    let approval_request = serde_json::json!({
        "id": "approval-1",
        "method": "item/commandExecution/requestApproval"
    });

    // Act
    let thread_payload = build_thread_start_payload(
        &folder,
        AgentModel::Gpt56Sol.as_str(),
        PermissionMode::ReadOnly,
        ReasoningLevel::Medium,
        SpeedMode::default(),
        "thread-start-1",
    );
    let turn_payload = build_turn_start_payload(&CodexTurnStartPayloadInput {
        folder: &folder,
        model: AgentModel::Gpt56Sol.as_str(),
        permission_mode: PermissionMode::ReadOnly,
        prompt: "Inspect the architecture".into(),
        protocol_profile: ProtocolRequestProfile::SessionTurn,
        reasoning_level: ReasoningLevel::Medium,
        speed_mode: SpeedMode::default(),
        thread_id: "thread-1",
        turn_start_id: "turn-start-1",
    });
    let approval_response =
        policy::build_server_request_response(&approval_request, PermissionMode::ReadOnly, &folder)
            .expect("approval response should be generated");

    // Assert
    assert_eq!(
        thread_payload.pointer("/params/sandbox"),
        Some(&Value::String("read-only".to_string()))
    );
    assert_eq!(
        turn_payload.pointer("/params/sandboxPolicy"),
        Some(&serde_json::json!({
            "type": "readOnly",
            "networkAccess": false
        }))
    );
    assert_eq!(
        approval_response.pointer("/result/decision"),
        Some(&Value::String("reject".to_string()))
    );
}

#[test]
fn build_turn_input_items_emits_single_text_item_when_no_attachments_present() {
    // Arrange
    let prompt = TurnPrompt::from_text("Hello, agent.".to_string());

    // Act
    let items = build_turn_input_items(&prompt);

    // Assert
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get("type").and_then(Value::as_str), Some("text"));
    assert_eq!(
        items[0].get("text").and_then(Value::as_str),
        Some("Hello, agent.")
    );
    assert!(
        items[0]
            .get("text_elements")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    );
}

#[test]
fn build_turn_input_items_interleaves_text_and_local_image_items_in_placeholder_order() {
    // Arrange
    let attachment_path = PathBuf::from("/tmp/agentty-codex-test/sample.png");
    let prompt = TurnPrompt {
        attachments: vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: attachment_path.clone(),
        }],
        text: "Describe [Image #1] please".to_string(),
        text_source: TurnPromptTextSource::UserPrompt,
    };

    // Act
    let items = build_turn_input_items(&prompt);

    // Assert
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].get("type").and_then(Value::as_str), Some("text"));
    assert_eq!(
        items[0].get("text").and_then(Value::as_str),
        Some("Describe ")
    );
    assert_eq!(
        items[1].get("type").and_then(Value::as_str),
        Some("localImage")
    );
    assert_eq!(
        items[1].get("path").and_then(Value::as_str),
        Some(attachment_path.to_string_lossy().as_ref())
    );
    assert_eq!(items[2].get("type").and_then(Value::as_str), Some("text"));
    assert_eq!(
        items[2].get("text").and_then(Value::as_str),
        Some(" please")
    );
}

#[test]
fn build_local_image_input_item_serializes_local_image_type_and_path() {
    // Arrange
    let path = PathBuf::from("/tmp/agentty-codex-test/picture.png");

    // Act
    let item = build_local_image_input_item(&path);

    // Assert
    assert_eq!(item.get("type").and_then(Value::as_str), Some("localImage"));
    assert_eq!(
        item.get("path").and_then(Value::as_str),
        Some(path.to_string_lossy().as_ref())
    );
}
