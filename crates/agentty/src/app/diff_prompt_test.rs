use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentRequestKind, OneShotError, OneShotRequest, OneShotSubmission, PermissionMode,
    ReasoningLevel, SessionStats, SpeedMode, is_input_size_error,
};
use ag_protocol::AgentResponse;
use ag_session::{AgentKind, AgentModel};
use ag_worker::MockRunClient;

use super::{
    MAX_PROVIDER_CALLS, PROMPT_BUDGET, SUMMARY_CHUNK_BYTES, chunks, render_result, summarize,
    summarize_context, summary_with_repair,
};
use crate::app::AppError;

fn request() -> OneShotRequest {
    OneShotRequest {
        activity_tx: None,
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        provider_call_budget: Some(ag_contracts::ProviderCallBudget::new(MAX_PROVIDER_CALLS)),
        harness: (AgentKind::Claude).to_string(),
        child_pid: None,
        folder: PathBuf::from("."),
        model: AgentModel::ClaudeSonnet55.as_str().to_string(),
        permission_mode: PermissionMode::ReadOnly,
        prompt: String::new(),
        request_kind: AgentRequestKind::FocusedReview,
        reasoning_level: ReasoningLevel::Medium,
        speed_mode: SpeedMode::Normal,
    }
}

fn answer(text: &str) -> OneShotSubmission {
    OneShotSubmission {
        response: AgentResponse::plain(text),
        stats: SessionStats::default(),
    }
}

#[tokio::test]
async fn summary_rejection_splits_chunks_without_losing_the_tail() {
    // Arrange
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&prompts);
    let mut client = MockRunClient::new();
    client.expect_submit().returning(move |request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("shared budget")
            .consume()?;
        let mut prompts = seen.lock().expect("operation should succeed");
        prompts.push(request.prompt);
        if prompts.len() == 1 {
            return Err(OneShotError::new("contextWindowExceeded"));
        }
        Ok(answer("short"))
    });

    // Act
    let request = request();
    let summary = summarize(
        &client,
        &request,
        &format!("{}TAIL", "x".repeat(5000)),
        2000,
        request
            .provider_call_budget
            .as_ref()
            .expect("shared budget"),
    )
    .await
    .expect("operation should succeed");

    // Assert
    let prompts = prompts.lock().expect("operation should succeed");
    assert!(prompts[1].len() < prompts[0].len());
    assert!(prompts.iter().any(|prompt| prompt.contains("TAIL")));
    assert!(summary.len() <= 2000);
}

#[tokio::test]
async fn summary_failure_and_invalid_summary_stop_generation() {
    // Arrange
    for output in [
        Ok(String::new()),
        Ok("x".repeat(3000)),
        Err("network timeout".to_string()),
        Err("context window exceeded".to_string()),
    ] {
        let mut client = MockRunClient::new();
        client.expect_submit().returning(move |_| match &output {
            Ok(text) => Ok(answer(text)),
            Err(error) => Err(OneShotError::new(error)),
        });

        // Act
        let request = request();
        let result = summarize(
            &client,
            &request,
            &"x".repeat(2000),
            1000,
            request
                .provider_call_budget
                .as_ref()
                .expect("shared budget"),
        )
        .await;

        // Assert
        assert!(result.is_err());
    }
}

#[tokio::test]
async fn invalid_summary_is_repaired_from_original_input_with_byte_feedback() {
    // Arrange
    for (invalid, diagnostic) in [
        ("   ".to_string(), "summary was empty"),
        ("🦀".repeat(200), "800 UTF-8 bytes"),
    ] {
        let mut attempts = 0;
        let mut client = MockRunClient::new();
        client.expect_submit().times(2).returning(move |request| {
            request
                .provider_call_budget
                .as_ref()
                .expect("budget")
                .consume()?;
            assert!(request.prompt.contains("ORIGINAL SOURCE"));
            assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
            attempts += 1;
            if attempts == 1 {
                return Ok(answer(&invalid));
            }
            assert!(request.prompt.contains(diagnostic));
            assert!(request.prompt.contains("nonempty, shorter summary"));
            assert!(!request.prompt.contains('🦀'));
            Ok(answer("Repaired summary."))
        });
        let request = request();

        // Act
        let summary = summarize(
            &client,
            &request,
            &"ORIGINAL SOURCE\n".repeat(200),
            2000,
            request.provider_call_budget.as_ref().expect("budget"),
        )
        .await
        .expect("repair");

        // Assert
        assert!(summary.contains("Repaired summary."));
    }
}

#[tokio::test]
async fn failed_summary_repair_splits_original_input_without_losing_tail() {
    // Arrange
    let mut attempts = 0;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let prompts = Arc::clone(&seen);
    let mut client = MockRunClient::new();
    client.expect_submit().returning(move |request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        prompts.lock().expect("prompts").push(request.prompt);
        attempts += 1;
        Ok(answer(if attempts <= 2 { "" } else { "Recovered." }))
    });
    let request = request();

    // Act
    let summary = summarize(
        &client,
        &request,
        &format!("{}TAIL", "x".repeat(5000)),
        2000,
        request.provider_call_budget.as_ref().expect("budget"),
    )
    .await
    .expect("split recovery");

    // Assert
    let prompts = seen.lock().expect("prompts");
    assert_eq!(prompts.len(), 4);
    assert!(prompts[2].len() < prompts[0].len());
    assert!(prompts[3].contains("TAIL"));
    assert!(summary.contains("Recovered."));
}

#[tokio::test]
async fn small_tail_is_retained_verbatim_without_a_tiny_summary_request() {
    // Arrange
    let mut client = MockRunClient::new();
    client
        .expect_submit()
        .once()
        .returning(|_| Ok(answer("Large fragment summarized.")));
    let request = request();

    // Act
    let summary = summarize(
        &client,
        &request,
        &format!("{}TAIL🦀", "x".repeat(SUMMARY_CHUNK_BYTES)),
        8000,
        request.provider_call_budget.as_ref().expect("budget"),
    )
    .await
    .expect("summary");

    // Assert
    assert!(summary.ends_with("TAIL🦀"));
}

#[tokio::test]
async fn provider_splitting_still_summarizes_fragments_below_the_output_limit() {
    // Arrange
    let mut client = MockRunClient::new();
    client.expect_submit().returning(|request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("budget")
            .consume()?;
        if request.prompt.len() > 4_000 {
            return Err(OneShotError::new("contextWindowExceeded"));
        }
        Ok(answer("Small provider fragment summarized."))
    });
    let request = request();

    // Act
    let summary = summarize(
        &client,
        &request,
        &"x".repeat(30_000),
        10_000,
        request.provider_call_budget.as_ref().expect("budget"),
    )
    .await
    .expect("smaller provider still makes progress");

    // Assert
    assert!(summary.len() < 10_000);
    assert!(summary.contains("Small provider fragment summarized."));
}

#[tokio::test]
async fn summary_repair_respects_prompt_and_shared_call_budgets() {
    // Arrange
    for (prompt_size, budget_size, diagnostic) in [
        (PROMPT_BUDGET, 2, "repair prompt budget"),
        (100, 1, "provider call limit reached"),
    ] {
        let mut request = request();
        request.prompt = "x".repeat(prompt_size);
        let budget = ag_contracts::ProviderCallBudget::new(budget_size);
        request.provider_call_budget = Some(budget.clone());
        let mut client = MockRunClient::new();
        client.expect_submit().once().returning(|request| {
            request
                .provider_call_budget
                .as_ref()
                .expect("budget")
                .consume()?;
            Ok(answer(""))
        });

        // Act
        let error = summary_with_repair(&client, &request, 100, &budget)
            .await
            .expect_err("bounded repair");

        // Assert
        assert!(error.to_string().contains(diagnostic));
    }
}

#[tokio::test]
async fn persistent_invalid_summary_reports_distinct_terminal_diagnostics() {
    // Arrange
    for (output, diagnostic) in [
        (String::new(), "summary was empty"),
        ("🦀".repeat(200), "800 UTF-8 bytes, exceeding 127"),
    ] {
        let mut client = MockRunClient::new();
        client
            .expect_submit()
            .times(2)
            .returning(move |_| Ok(answer(&output)));
        let request = request();

        // Act
        let error = summarize(
            &client,
            &request,
            &"x".repeat(512),
            511,
            request.provider_call_budget.as_ref().expect("budget"),
        )
        .await
        .expect_err("terminal diagnostic");

        // Assert
        // The target's quarter is the hard byte limit, with no per-tail
        // shrinkage.
        assert!(error.to_string().contains(diagnostic));
        assert!(error.to_string().contains("after repair"));
        assert!(is_input_size_error(&error.to_string()));
    }
}

#[tokio::test]
async fn rejects_reduction_without_progress() {
    // Arrange
    let mut client = MockRunClient::new();
    client
        .expect_submit()
        .returning(|_| Ok(answer("123456789012")));

    // Act
    let request = request();
    let result = summarize(
        &client,
        &request,
        &"x".repeat(50),
        49,
        request
            .provider_call_budget
            .as_ref()
            .expect("shared budget"),
    )
    .await;

    // Assert
    assert!(
        result
            .expect_err("operation should fail")
            .to_string()
            .contains("no progress")
    );
}

#[test]
fn chunks_preserve_unicode_newlines_and_file_attribution() {
    // Arrange
    let plain = format!("{}\n{}", "🦀".repeat(100), "z".repeat(100));
    let diff = format!("diff --git a/test b/test\n{}", "+line\n".repeat(100));

    // Act
    let plain_chunks = chunks(&plain, 99);
    let diff_chunks = chunks(&diff, 128);

    // Assert
    assert_eq!(plain_chunks.iter().cloned().collect::<String>(), plain);
    assert!(plain_chunks.iter().all(|chunk| chunk.len() <= 99));
    assert!(
        diff_chunks
            .iter()
            .all(|chunk| chunk.starts_with("diff --git a/test b/test\n"))
    );
    assert_eq!(
        diff_chunks
            .iter()
            .map(|chunk| chunk.matches("+line\n").count())
            .sum::<usize>(),
        100
    );
    assert!(chunks("", 128).is_empty());
}

#[tokio::test]
async fn history_reduction_preserves_verification_provenance_contract() {
    // Arrange
    let mut client = MockRunClient::new();
    client.expect_submit().returning(|request| {
        assert!(request.prompt.contains("Completed; Remaining; Checks"));
        assert!(request.prompt.contains("invalidating changes"));
        assert!(request.prompt.contains("unverified claim"));
        assert_eq!(request.request_kind, AgentRequestKind::UtilityPrompt);
        assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
        Ok(answer(
            "Checks: cargo test passed before parser edit; rerun affected checks.",
        ))
    });
    let budget = ag_contracts::ProviderCallBudget::new(MAX_PROVIDER_CALLS);

    // Act
    let summary = summarize_context(
        &client,
        &request(),
        &"history line\n".repeat(800),
        512,
        &budget,
    )
    .await
    .expect("checkpoint");

    // Assert
    assert!(summary.contains("rerun affected checks"));
}

#[test]
fn render_errors_preserve_host_diagnostics() {
    // Arrange
    let result = Err(AppError::Workflow("render failed".into()));
    // Act
    let error = render_result(result).expect_err("invalid render");
    let success = render_result(Ok::<_, AppError>("rendered".into()));
    // Assert
    assert_eq!(error.to_string(), "render failed");
    assert_eq!(success.expect("render"), "rendered");
}
