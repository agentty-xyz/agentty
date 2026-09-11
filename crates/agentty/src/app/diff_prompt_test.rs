use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_agent::{
    AgentKind, AgentModel, AgentRequestKind, MockOneShotClient, OneShotError, OneShotRequest,
    OneShotSubmission, PermissionMode, ReasoningLevel, SessionStats, SpeedMode, diff_fence,
    is_input_size_error,
};
use ag_protocol::AgentResponse;

use super::{
    MAX_PROVIDER_CALLS, PROMPT_BUDGET, SUMMARY_CHUNK_BYTES, chunks, submit, summarize,
    summary_with_repair,
};

fn request() -> OneShotRequest {
    OneShotRequest {
        provider_call_budget: Some(ag_agent::ProviderCallBudget::new(MAX_PROVIDER_CALLS)),
        agent_kind: AgentKind::Claude,
        child_pid: None,
        folder: PathBuf::from("."),
        model: AgentModel::ClaudeSonnet5,
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

fn render(diff: &str, context: &str) -> String {
    format!("FINAL\n{context}\n{diff}")
}

#[tokio::test]
async fn small_diff_preserves_input_and_settings() {
    // Arrange
    let mut client = MockOneShotClient::new();
    client.expect_submit().once().returning(|request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("shared budget")
            .consume()?;
        assert_eq!(request.prompt, "FINAL\nintent\n+hello");
        assert_eq!(request.request_kind, AgentRequestKind::FocusedReview);
        assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
        Ok(answer("result"))
    });

    // Act
    let (submission, summarized) =
        submit(&client, request(), "+hello", "intent", |diff, context| {
            Ok(render(diff, context))
        })
        .await
        .expect("operation should succeed");

    // Assert
    assert_eq!(submission.response.answer, "result");
    assert!(!summarized);
}

#[tokio::test]
async fn budgets_huge_unicode_diff_history_and_fences_before_submission() {
    // Arrange
    let diff = format!(
        "diff --git a/first b/first\n{}\ndiff --git a/middle b/middle\n{}\ndiff --git a/last \
         b/last\n{}",
        "🦀".repeat(100_000),
        "`".repeat(100_000),
        "+end\n".repeat(100_000)
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_calls = Arc::clone(&seen);
    let mut client = MockOneShotClient::new();
    client.expect_submit().returning(move |request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("shared budget")
            .consume()?;
        assert!(request.prompt.len() <= PROMPT_BUDGET);
        assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
        assert_eq!(request.model, AgentModel::ClaudeSonnet5);
        seen_calls
            .lock()
            .expect("operation should succeed")
            .push(request.prompt.clone());
        if request.prompt.starts_with("FINAL") {
            return Ok(answer("result"));
        }
        assert_eq!(request.request_kind, AgentRequestKind::UtilityPrompt);
        Ok(answer("Preserve each file change and accepted decisions."))
    });

    // Act
    let (_, summarized) = submit(
        &client,
        request(),
        &diff,
        &"history decision\n".repeat(50_000),
        |diff, context| Ok(render(diff, context)),
    )
    .await
    .expect("operation should succeed");

    // Assert
    assert!(summarized);
    let seen = seen.lock().expect("operation should succeed");
    for path in ["a/first", "a/middle", "a/last", "history decision"] {
        assert!(seen.iter().any(|prompt| prompt.contains(path)), "{path}");
    }
    assert!(
        seen.last()
            .expect("operation should succeed")
            .contains("Summarized input")
    );
}

#[tokio::test]
async fn exact_size_rejection_retries_only_smaller_prompts() {
    // Arrange
    let lengths = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&lengths);
    let mut client = MockOneShotClient::new();
    client.expect_submit().returning(move |request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("shared budget")
            .consume()?;
        if request.prompt.starts_with("FINAL") {
            let mut lengths = observed.lock().expect("operation should succeed");
            lengths.push(request.prompt.len());
            if lengths.len() == 1 {
                return Err(OneShotError::new(
                    "Input exceeds the maximum length of 1048576 characters.",
                ));
            }
            return Ok(answer("result"));
        }
        Ok(answer("Change behavior."))
    });

    // Act
    let (_, summarized) = submit(
        &client,
        request(),
        &"x".repeat(40_000),
        "",
        |diff, context| Ok(render(diff, context)),
    )
    .await
    .expect("operation should succeed");

    // Assert
    assert!(summarized);
    let lengths = lengths.lock().expect("operation should succeed");
    assert_eq!(lengths.len(), 2);
    assert!(lengths[1] < lengths[0]);
}

#[tokio::test]
async fn summary_rejection_splits_chunks_without_losing_the_tail() {
    // Arrange
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&prompts);
    let mut client = MockOneShotClient::new();
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
async fn stops_on_unreducible_input_or_non_size_errors() {
    // Arrange
    for diagnostic in ["network timeout", "context_window_exceeded"] {
        let mut client = MockOneShotClient::new();
        client
            .expect_submit()
            .once()
            .returning(move |_| Err(OneShotError::new(diagnostic)));

        // Act
        let error = submit(&client, request(), "tiny", "", |diff, context| {
            Ok(render(diff, context))
        })
        .await
        .expect_err("operation should fail");

        // Assert
        assert_eq!(error.to_string(), diagnostic);
    }
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
        let mut client = MockOneShotClient::new();
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
        let mut client = MockOneShotClient::new();
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
    let mut client = MockOneShotClient::new();
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
    let mut client = MockOneShotClient::new();
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
    let mut client = MockOneShotClient::new();
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
        let budget = ag_agent::ProviderCallBudget::new(budget_size);
        request.provider_call_budget = Some(budget.clone());
        let mut client = MockOneShotClient::new();
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
        let mut client = MockOneShotClient::new();
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
async fn rejects_unbudgetable_template_and_render_failure() {
    // Arrange
    let client = MockOneShotClient::new();

    // Act
    let oversized = submit(&client, request(), "", "", |_, _| {
        Ok("x".repeat(PROMPT_BUDGET + 1))
    })
    .await;
    let invalid = submit(&client, request(), "", "", |_, _| {
        Err(OneShotError::new("render"))
    })
    .await;

    // Assert
    assert!(oversized.is_err());
    assert_eq!(
        invalid.expect_err("operation should fail").to_string(),
        "render"
    );
}

#[tokio::test]
async fn rejects_reduction_without_progress() {
    // Arrange
    let mut client = MockOneShotClient::new();
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
async fn fencing_overhead_triggers_reduction_below_nominal_content_allowances() {
    // Arrange
    let diff = "`".repeat(19_000);
    let history = "h".repeat(7_000);
    let mut client = MockOneShotClient::new();
    client.expect_submit().returning(|request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("shared budget")
            .consume()?;
        assert!(request.prompt.len() <= PROMPT_BUDGET);
        Ok(answer("Preserve the documented changes."))
    });

    // Act
    let (_, summarized) = submit(&client, request(), &diff, &history, |diff, history| {
        let fence = diff_fence(diff);
        Ok(format!("{fence}diff\n{diff}\n{fence}\n{history}"))
    })
    .await
    .expect("fencing overhead should be reduced");

    // Assert
    assert!(summarized);
}

#[tokio::test]
async fn repeated_size_rejections_have_a_bounded_retry_count() {
    // Arrange
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&attempts);
    let mut client = MockOneShotClient::new();
    client.expect_submit().returning(move |request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("shared budget")
            .consume()?;
        if request.prompt.starts_with("FINAL") {
            seen.lock()
                .expect("attempts lock")
                .push(request.prompt.len());
            return Err(OneShotError::new("contextWindowExceeded"));
        }
        Ok(answer(&"`".repeat(summary_output_limit(&request))))
    });

    // Act
    let result = submit(
        &client,
        request(),
        &"d".repeat(25_000),
        &"h".repeat(25_000),
        |diff, context| {
            let diff_delimiter = diff_fence(diff);
            let context_delimiter = diff_fence(context);
            let diff = format!("{diff_delimiter}diff\n{diff}\n{diff_delimiter}");
            let context = format!("{context_delimiter}text\n{context}\n{context_delimiter}");
            Ok(format!("FINAL\n{diff}\n{context}"))
        },
    )
    .await;

    // Assert
    assert!(result.is_err());
    let attempts = attempts.lock().expect("attempts lock");
    assert_eq!(attempts.len(), 3);
    assert!(attempts.windows(2).all(|sizes| sizes[1] < sizes[0]));
}

#[tokio::test]
async fn render_failure_after_summarization_propagates() {
    // Arrange
    let mut client = MockOneShotClient::new();
    client
        .expect_submit()
        .returning(|_| Ok(answer("short summary")));

    // Act
    let result = submit(
        &client,
        request(),
        &"x".repeat(70_000),
        "",
        |diff, context| {
            if diff.contains("Summarized input") {
                return Err(OneShotError::new("summary render failed"));
            }
            Ok(render(diff, context))
        },
    )
    .await;

    // Assert
    assert_eq!(
        result.expect_err("render should fail").to_string(),
        "summary render failed"
    );
}

/// Simulates a provider returning the largest permitted summary.
fn summary_output_limit(request: &OneShotRequest) -> usize {
    request
        .prompt
        .split("within ")
        .nth(1)
        .expect("summary bound")
        .split_whitespace()
        .next()
        .expect("byte count")
        .parse()
        .expect("numeric bound")
}

#[tokio::test]
async fn mebibyte_diff_uses_large_chunks_and_bounded_recursive_reduction() {
    // Arrange
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&prompts);
    let mut client = MockOneShotClient::new();
    client.expect_submit().returning(move |request| {
        request
            .provider_call_budget
            .as_ref()
            .expect("shared budget")
            .consume()?;
        assert!(request.prompt.len() <= PROMPT_BUDGET);
        seen.lock()
            .expect("prompts lock")
            .push(request.prompt.clone());
        if request.prompt.starts_with("FINAL") {
            Ok(answer("result"))
        } else {
            Ok(answer(&"s".repeat(summary_output_limit(&request))))
        }
    });
    let diff = format!("{}TAIL", "X".repeat(1_048_576));

    // Act
    let (_, summarized) = submit(&client, request(), &diff, "", |diff, context| {
        Ok(render(diff, context))
    })
    .await
    .expect("large diff should succeed");

    // Assert
    assert!(summarized);
    let prompts = prompts.lock().expect("prompts lock");
    assert!(prompts.len() <= 24, "{} calls for one MiB", prompts.len());
    assert!(prompts[0].len() > 55_000);
    assert_eq!(
        prompts
            .iter()
            .filter(|prompt| !prompt.starts_with("FINAL"))
            .map(|prompt| prompt.matches('X').count())
            .sum::<usize>(),
        1_048_576
    );
    assert!(prompts.iter().any(|prompt| prompt.contains("TAIL")));
    assert!(prompts.last().expect("final prompt").starts_with("FINAL"));
}

#[tokio::test]
async fn provider_call_budget_is_shared_by_all_reduction_stages_and_final_submission() {
    // Arrange
    for (diff_chunks, context_chunks, full_summary) in [
        (MAX_PROVIDER_CALLS + 1, 0, false), // Initial diff chunks.
        (MAX_PROVIDER_CALLS, 0, true),      // Recursive summaries.
        (MAX_PROVIDER_CALLS - 1, 2, false), // Session context.
        (MAX_PROVIDER_CALLS, 0, false),     // Final submission.
    ] {
        let mut client = MockOneShotClient::new();
        client
            .expect_submit()
            .times(MAX_PROVIDER_CALLS)
            .returning(move |request| {
                request
                    .provider_call_budget
                    .as_ref()
                    .expect("shared budget")
                    .consume()?;
                assert_eq!(request.request_kind, AgentRequestKind::UtilityPrompt);
                Ok(answer(&"s".repeat(if full_summary {
                    summary_output_limit(&request)
                } else {
                    5
                })))
            });

        // Act
        let error = submit(
            &client,
            request(),
            &"x".repeat(SUMMARY_CHUNK_BYTES * diff_chunks),
            &"h".repeat(SUMMARY_CHUNK_BYTES * context_chunks),
            |diff, context| Ok(render(diff, context)),
        )
        .await
        .expect_err("call budget should stop generation");

        // Assert
        assert!(error.to_string().contains("provider call limit reached"));
        assert!(is_input_size_error(&error.to_string()));
    }
}

#[tokio::test]
async fn provider_size_rejections_consume_the_shared_call_budget() {
    // Arrange
    let mut client = MockOneShotClient::new();
    client
        .expect_submit()
        .times(MAX_PROVIDER_CALLS)
        .returning(|request| {
            request
                .provider_call_budget
                .as_ref()
                .expect("shared budget")
                .consume()?;
            if request.prompt.len() > 4_000 {
                Err(OneShotError::new("contextWindowExceeded"))
            } else {
                Ok(answer("short"))
            }
        });

    // Act
    let error = submit(
        &client,
        request(),
        &"x".repeat(1_048_576),
        "",
        |diff, context| Ok(render(diff, context)),
    )
    .await
    .expect_err("repeated splitting should exhaust the budget");

    // Assert
    assert!(
        error.to_string().contains("provider call limit reached"),
        "{error}"
    );
}

#[tokio::test]
async fn hidden_repair_turns_reduce_the_number_of_allowed_submissions() {
    // Arrange
    let mut client = MockOneShotClient::new();
    client
        .expect_submit()
        .times(MAX_PROVIDER_CALLS / 2)
        .returning(|request| {
            let budget = request.provider_call_budget.expect("shared budget");
            budget.consume()?; // Initial provider turn.
            budget.consume()?; // Protocol repair turn.
            Ok(answer("short"))
        });

    // Act
    let error = submit(
        &client,
        request(),
        &"x".repeat(SUMMARY_CHUNK_BYTES * 40),
        "",
        |diff, context| Ok(render(diff, context)),
    )
    .await
    .expect_err("repair turns exhaust the budget");

    // Assert
    assert!(error.to_string().contains("provider call limit reached"));
}
