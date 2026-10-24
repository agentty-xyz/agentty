use serde_json::{Value, json};

use super::support::{
    ByteEstimator, bash_command_call, budget, read_call_of, record_read_round, request, sent,
};
use crate::compaction::summary_schema;
use crate::context::{
    ContextBudget, ContextFit, FitStep, MAX_SUMMARY_FAILURES_PER_TURN, SUMMARY_KEEP_TAIL_PERCENT,
    repeated_read_result,
};
use crate::lifecycle::ContextSummaryTrigger;
use crate::model::{ModelMessage, ModelRequest};
use crate::round_summary::{CONTINUED_SUMMARY_PREFIX, ROUND_SUMMARY_INSTRUCTIONS, RoundSummaryJob};

const PROACTIVE: Option<ContextSummaryTrigger> = Some(ContextSummaryTrigger::Proactive);

fn weigh(messages: &[ModelMessage]) -> u64 {
    messages
        .iter()
        .map(|message| u64::try_from(message.retained_bytes()).expect("bytes fit u64"))
        .sum()
}

/// Returns a request whose current turn read `count` files of `bytes` each,
/// one per round.
fn read_rounds(count: usize, bytes: usize) -> ModelRequest {
    let mut request = request(Vec::new(), Vec::new(), "list the public functions");
    for index in 0..count {
        record_read_round(&mut request, &format!("r{index}"), bytes);
    }

    request
}

fn summary(context: &str) -> Value {
    json!({"context": context, "decisions": ["keep going"], "state": "half done"})
}

/// Fits `request` and returns the summary job it asks for.
fn summary_job(
    fit: &mut ContextFit,
    budget: ContextBudget,
    request: &ModelRequest,
    trigger: Option<ContextSummaryTrigger>,
) -> RoundSummaryJob {
    match fit.fit(&ByteEstimator, budget, request, trigger) {
        Ok(FitStep::Summarize(job)) => *job,
        other => {
            std::panic::resume_unwind(Box::new(format!("summary step expected, got {other:?}")))
        }
    }
}

/// Returns the index of the tool-call message of round `round`, counted from
/// zero, in a request built by [`read_rounds`].
fn round_start(round: usize) -> usize {
    1 + 2 * round
}

fn note_text(message: &ModelMessage) -> &str {
    match message {
        ModelMessage::User(text) => text,
        other => std::panic::resume_unwind(Box::new(format!("note expected, got {other:?}"))),
    }
}

#[test]
fn an_outgrown_request_asks_to_summarize_rounds_before_the_kept_tail() {
    // Arrange
    let request = read_rounds(20, 1000);
    let budget_weight = 20_000;
    assert!(weigh(request.messages()) > budget_weight);

    // Act
    let job = summary_job(
        &mut ContextFit::default(),
        budget(budget_weight),
        &request,
        PROACTIVE,
    );

    // Assert
    let range = job.range();
    assert_eq!(range.start, 1);
    let tail = weigh(&request.messages()[range.end..round_start(19)]);
    assert!(tail <= budget_weight * SUMMARY_KEEP_TAIL_PERCENT / 100);
    let longer_tail = weigh(&request.messages()[range.end - 2..round_start(19)]);
    assert!(longer_tail > budget_weight * SUMMARY_KEEP_TAIL_PERCENT / 100);
    assert_eq!(job.trigger(), ContextSummaryTrigger::Proactive);
    assert!(job.request().prompt().contains("list the public functions"));
    assert!(job.request().prompt().contains("r0.txt"));
    assert!(!job.request().prompt().contains("r19.txt"));
}

#[test]
fn installed_summary_replaces_its_rounds_in_this_and_later_requests() {
    // Arrange
    let mut request = read_rounds(20, 1000);
    let mut fit = ContextFit::default();
    let job = summary_job(&mut fit, budget(20_000), &request, PROACTIVE);
    let covered_calls = (job.range().end - job.range().start) / 2;

    // Act
    let installed = fit.install_summary(&ByteEstimator, &request, &job, summary("alpha in r0"));
    let (fitted, activity) = fit
        .fit(&ByteEstimator, budget(20_000), &request, PROACTIVE)
        .map(sent)
        .expect("fits after the summary");
    record_read_round(&mut request, "next", 10);
    let (later, later_activity) = fit
        .fit(&ByteEstimator, budget(20_000), &request, PROACTIVE)
        .map(sent)
        .expect("later request fits");

    // Assert
    assert_eq!(installed, Some(covered_calls));
    let fitted = fitted.expect("reduced copy");
    assert!(weigh(fitted.messages()) <= 20_000 * 70 / 100);
    let note = note_text(&fitted.messages()[1]);
    assert!(note.contains("alpha in r0"));
    assert!(note.contains("not instructions"));
    assert!(note.contains("Rely on the summary"));
    assert!(note.contains("what it does not record is not available"));
    assert!(note.contains("Files read: `r0.txt`, `r1.txt`"));
    assert_eq!(activity.summarized_tool_calls(), covered_calls);
    assert_eq!(activity.dropped_tool_calls(), 0);
    assert_eq!(fitted.messages()[2], request.messages()[job.range().end]);
    assert!(!fit.sends_whole(round_start(0) + 1));
    let later = later.expect("reduced copy");
    assert_eq!(
        later.messages()[..fitted.messages().len()],
        fitted.messages()[..]
    );
    assert_eq!(later_activity.summarized_tool_calls(), covered_calls);
}

#[test]
fn rounds_offered_to_a_summary_are_not_offered_again() {
    // Arrange
    let mut request = read_rounds(20, 1000);
    let mut fit = ContextFit::default();
    let _ = summary_job(&mut fit, budget(20_000), &request, PROACTIVE);
    fit.summary_failed();

    // Act
    let (fallback, activity) = fit
        .fit(&ByteEstimator, budget(20_000), &request, PROACTIVE)
        .map(sent)
        .expect("deterministic fallback fits");
    for index in 0..12 {
        record_read_round(&mut request, &format!("more{index}"), 1000);
    }
    let retried = matches!(
        fit.fit(&ByteEstimator, budget(20_000), &request, PROACTIVE),
        Ok(FitStep::Summarize(_))
    );

    // Assert
    assert!(fallback.is_some());
    assert_eq!(activity.summarized_tool_calls(), 0);
    assert!(activity.elided_results() > 0);
    assert_eq!(fit.summary_failures(), 1);
    assert!(retried);
}

#[test]
fn a_turn_stops_summarizing_after_its_failure_limit() {
    // Arrange
    let request = read_rounds(20, 1000);
    let mut fit = ContextFit::default();
    for _ in 0..MAX_SUMMARY_FAILURES_PER_TURN {
        fit.summary_failed();
    }

    // Act
    let step = fit.fit(&ByteEstimator, budget(20_000), &request, PROACTIVE);

    // Assert
    assert!(matches!(step, Ok(FitStep::Send(Some(_), _))));
}

#[test]
fn no_summary_is_requested_without_a_trigger_or_while_the_request_fits() {
    // Arrange
    let request = read_rounds(20, 1000);

    // Act
    let untriggered = ContextFit::default()
        .fit(&ByteEstimator, budget(20_000), &request, None)
        .map(sent)
        .expect("deterministic reduction fits");
    let fitting = ContextFit::default()
        .fit(&ByteEstimator, budget(100_000), &request, PROACTIVE)
        .map(sent)
        .expect("canonical request fits");

    // Assert
    assert!(untriggered.0.is_some());
    assert_eq!(untriggered.1.summarized_tool_calls(), 0);
    assert_eq!(fitting.0, None);
}

#[test]
fn small_earlier_rounds_are_reduced_without_a_summary() {
    // Arrange
    let mut request = read_rounds(3, 100);
    let large = read_call_of("large", "large.txt");
    request.record_tool_call(large.clone());
    request.record_tool_result(&large, "x".repeat(20_000));

    // Act
    let sent_without_summary = matches!(
        ContextFit::default().fit(&ByteEstimator, budget(10_000), &request, PROACTIVE),
        Ok(FitStep::Send(Some(_), _))
    );

    // Assert
    assert!(sent_without_summary);
}

#[test]
fn context_overflow_summarizes_every_round_before_the_latest_even_when_it_fits() {
    // Arrange
    let request = read_rounds(4, 100);

    // Act
    let job = summary_job(
        &mut ContextFit::default(),
        budget(100_000),
        &request,
        Some(ContextSummaryTrigger::ContextOverflow),
    );
    let single = ContextFit::default()
        .fit(
            &ByteEstimator,
            budget(100_000),
            &read_rounds(1, 100),
            Some(ContextSummaryTrigger::ContextOverflow),
        )
        .map(sent)
        .expect("a lone round fits");

    // Assert
    assert_eq!(job.range(), 1..round_start(3));
    assert_eq!(job.trigger(), ContextSummaryTrigger::ContextOverflow);
    let lone = single.0.expect("rebuilt copy");
    assert_eq!(lone.messages(), read_rounds(1, 100).messages());
}

#[test]
fn summary_sources_tighten_text_then_leave_out_the_oldest_rounds() {
    // Arrange
    let request = read_rounds(20, 1000);
    let overflow = Some(ContextSummaryTrigger::ContextOverflow);

    // Act
    let roomy = summary_job(
        &mut ContextFit::default(),
        budget(40_000),
        &request,
        overflow,
    );
    let tight = summary_job(
        &mut ContextFit::default(),
        budget(6_000),
        &request,
        overflow,
    );
    let tightest = summary_job(
        &mut ContextFit::default(),
        budget(3_500),
        &request,
        overflow,
    );
    let impossible = matches!(
        ContextFit::default().fit(&ByteEstimator, budget(1_200), &request, overflow),
        Ok(FitStep::Summarize(_))
    );

    // Assert
    assert!(!roomy.request().prompt().contains("bytes omitted"));
    let tight_prompt = tight.request().prompt();
    assert!(tight_prompt.contains("bytes omitted"));
    assert!(tight_prompt.contains("r0.txt"));
    let tightest_prompt = tightest.request().prompt();
    assert!(!tightest_prompt.contains("r0.txt"));
    assert!(tightest_prompt.contains("r18.txt"));
    assert_eq!(tightest.range(), 1..round_start(19));
    assert!(!impossible);
}

#[test]
fn summary_sources_show_superseded_results_as_their_notices() {
    // Arrange
    let mut request = read_rounds(20, 1000);
    let reread = read_call_of("again", "r0.txt");
    request.record_tool_call(reread.clone());
    request.record_tool_result(&reread, "y".repeat(1000));

    // Act
    let job = summary_job(
        &mut ContextFit::default(),
        budget(20_000),
        &request,
        PROACTIVE,
    );

    // Assert
    let prompt = job.request().prompt();
    assert_eq!(prompt.matches("context_cleared").count(), 1);
    assert!(prompt.contains("superseded_by"));
    assert!(prompt.contains("r1.txt"));
}

#[test]
fn a_later_summary_carries_the_earlier_one_forward() {
    // Arrange
    let mut request = read_rounds(20, 1000);
    let mut fit = ContextFit::default();
    let first = summary_job(&mut fit, budget(20_000), &request, PROACTIVE);
    let first_calls = fit
        .install_summary(&ByteEstimator, &request, &first, summary("alpha in r0"))
        .expect("first summary installed");
    for index in 0..20 {
        record_read_round(&mut request, &format!("more{index}"), 1000);
    }

    // Act
    let second = summary_job(&mut fit, budget(20_000), &request, PROACTIVE);
    let second_calls = fit
        .install_summary(&ByteEstimator, &request, &second, summary("omega in more0"))
        .expect("second summary installed");
    let (fitted, activity) = fit
        .fit(&ByteEstimator, budget(20_000), &request, PROACTIVE)
        .map(sent)
        .expect("fits after both summaries");

    // Assert
    assert_eq!(second.range().start, first.range().end);
    assert!(second.request().prompt().contains("alpha in r0"));
    let note = note_text(&fitted.expect("reduced copy").messages()[1]).to_string();
    assert!(note.contains("omega in more0"));
    assert!(!note.contains("alpha in r0"));
    assert_eq!(activity.summarized_tool_calls(), first_calls + second_calls);
}

#[test]
fn rounds_dropped_after_a_summary_keep_it_and_count_apart() {
    // Arrange
    let mut request = read_rounds(20, 1000);
    let mut fit = ContextFit::default();
    let job = summary_job(&mut fit, budget(20_000), &request, PROACTIVE);
    let summarized = fit
        .install_summary(&ByteEstimator, &request, &job, summary("alpha in r0"))
        .expect("summary installed");
    // Commands are never shortened, so only dropping rounds fits them.
    for index in 0..5 {
        let call = bash_command_call(&format!("cmd{index}"), &"echo ".repeat(800));
        request.record_tool_call(call.clone());
        request.record_tool_result(&call, r#"{"exit_code":0}"#.to_string());
    }

    // Act
    let (fitted, activity) = fit
        .fit(&ByteEstimator, budget(20_000), &request, None)
        .map(sent)
        .expect("fits after dropping rounds");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert!(weigh(fitted.messages()) <= 20_000);
    let note = note_text(&fitted.messages()[1]).to_string();
    assert!(note.contains("alpha in r0"));
    assert_eq!(activity.summarized_tool_calls(), summarized);
    assert!(activity.dropped_tool_calls() > 0);
}

#[test]
fn a_summary_heavier_than_its_rounds_changes_nothing() {
    // Arrange
    let request = read_rounds(4, 100);
    let mut fit = ContextFit::default();
    let job = summary_job(
        &mut fit,
        budget(100_000),
        &request,
        Some(ContextSummaryTrigger::ContextOverflow),
    );
    let heavy = summary(&"z".repeat(2000));

    // Act
    let installed = fit.install_summary(&ByteEstimator, &request, &job, heavy);
    let (fitted, activity) = fit
        .fit(&ByteEstimator, budget(100_000), &request, None)
        .map(sent)
        .expect("canonical request fits");

    // Assert
    assert_eq!(installed, None);
    assert_eq!(fitted, None);
    assert_eq!(activity.summarized_tool_calls(), 0);
}

#[test]
fn a_summary_keeps_request_only_messages_before_its_first_round() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    request.record_feedback("[harness] respond with JSON".to_string());
    for index in 0..20 {
        record_read_round(&mut request, &format!("r{index}"), 1000);
    }

    // Act
    let job = summary_job(
        &mut ContextFit::default(),
        budget(20_000),
        &request,
        PROACTIVE,
    );

    // Assert
    assert_eq!(job.range().start, 1);
    assert!(job.request().prompt().contains("respond with JSON"));
    assert!(job.request().prompt().contains("r0.txt"));
}

#[test]
fn reductions_after_an_overflow_summary_skip_the_summarized_rounds() {
    // Arrange
    let mut request = read_rounds(6, 1000);
    let mut fit = ContextFit::default();
    let job = summary_job(
        &mut fit,
        budget(100_000),
        &request,
        Some(ContextSummaryTrigger::ContextOverflow),
    );
    fit.install_summary(&ByteEstimator, &request, &job, summary("alpha in r0"))
        .expect("summary installed");
    for index in 0..4 {
        record_read_round(&mut request, &format!("big{index}"), 3000);
    }

    // Act
    let (fitted, activity) = fit
        .fit(&ByteEstimator, budget(8_000), &request, None)
        .map(sent)
        .expect("fits after capping and eliding");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert!(weigh(fitted.messages()) <= 8_000);
    assert!(note_text(&fitted.messages()[1]).contains("alpha in r0"));
    assert_eq!(activity.summarized_tool_calls(), 5);
}

#[test]
fn a_summary_merges_the_notes_of_earlier_drops_into_its_note() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "loop");
    let mut fit = ContextFit::default();
    // Commands are never shortened, so only dropping rounds fits them, one
    // note per fit that drops.
    for index in 0..10 {
        let call = bash_command_call(
            &format!("c{index}"),
            &format!("echo {index} {}", "x".repeat(990)),
        );
        request.record_tool_call(call.clone());
        request.record_tool_result(&call, r#"{"exit_code":0}"#.to_string());
        fit.fit(&ByteEstimator, budget(6000), &request, None)
            .map(sent)
            .expect("fits after dropping rounds");
    }
    let (before, before_activity) = fit
        .fit(&ByteEstimator, budget(6000), &request, None)
        .map(sent)
        .expect("refits");
    let before = before.expect("reduced copy");
    assert!(matches!(&before.messages()[2], ModelMessage::User(_)));
    let job = summary_job(
        &mut fit,
        budget(6000),
        &request,
        Some(ContextSummaryTrigger::ContextOverflow),
    );

    // Act
    let summarized = fit
        .install_summary(&ByteEstimator, &request, &job, summary("all echoed"))
        .expect("summary installed");
    let (fitted, activity) = fit
        .fit(&ByteEstimator, budget(6000), &request, None)
        .map(sent)
        .expect("fits after the summary");

    // Assert
    let fitted = fitted.expect("reduced copy");
    let note = note_text(&fitted.messages()[1]);
    assert!(note.contains("all echoed"));
    let dropped = before_activity.dropped_tool_calls();
    assert!(note.starts_with(&format!("[harness] {} earlier", dropped + summarized)));
    // Commands of both drop notes and of the summarized rounds, within the
    // bound on named effects.
    for index in 0..8 {
        assert!(note.contains(&format!("`echo {index} ")));
    }
    assert!(note.contains("and 1 more"));
    assert!(!matches!(&fitted.messages()[2], ModelMessage::User(_)));
    // The summary covers only the rounds still sent; the earlier ones keep
    // counting as dropped without one.
    assert_eq!(activity.summarized_tool_calls(), summarized);
    assert_eq!(activity.dropped_tool_calls(), dropped);
}

#[test]
fn a_repeat_of_a_summarized_result_is_cleared() {
    // Arrange
    let mut request = read_rounds(20, 1000);
    let repeat = read_call_of("again", "r0.txt");
    request.record_tool_call(repeat.clone());
    request.record_tool_result(&repeat, repeated_read_result("r0"));
    record_read_round(&mut request, "latest", 10);
    let mut fit = ContextFit::default();
    let job = summary_job(&mut fit, budget(20_000), &request, PROACTIVE);
    assert!(job.range().contains(&round_start(0)));

    // Act
    fit.install_summary(&ByteEstimator, &request, &job, summary("alpha in r0"))
        .expect("summary installed");
    let (fitted, _) = fit
        .fit(&ByteEstimator, budget(20_000), &request, PROACTIVE)
        .map(sent)
        .expect("fits after the summary");

    // Assert
    let fitted = fitted.expect("reduced copy");
    let repeat_result = &fitted.messages()[fitted.messages().len() - 3];
    let ModelMessage::ToolResult {
        call_id, content, ..
    } = repeat_result
    else {
        std::panic::resume_unwind(Box::new(format!(
            "repeat result expected, got {repeat_result:?}"
        )));
    };
    assert_eq!(call_id, "again");
    assert!(content.contains("repeated an earlier read whose result was since cleared or cut"));
}

/// Returns the identifier of the last tool call before `end`.
fn last_call_before(request: &ModelRequest, end: usize) -> String {
    request.messages()[..end]
        .iter()
        .rev()
        .find_map(|message| match message {
            ModelMessage::AssistantToolCall(call) => Some(call.id().to_string()),
            _ => None,
        })
        .expect("a tool call before the end")
}

/// Splits a continued summary request into the messages it continues and
/// the text of its instruction.
fn continued(job: &RoundSummaryJob) -> (&[ModelMessage], &str) {
    let (instruction, continued) = job
        .request()
        .messages()
        .split_last()
        .expect("an instruction");

    (continued, note_text(instruction))
}

#[test]
fn a_shared_prefix_summary_continues_the_canonical_request_last_sent() {
    // Arrange
    let mut request = read_rounds(14, 1000);
    let mut fit = ContextFit::new(true);
    let (canonical, _) = fit
        .fit(&ByteEstimator, budget(20_000), &request, PROACTIVE)
        .map(sent)
        .expect("the request fits whole");
    let last_sent = request.messages().to_vec();
    for index in 0..8 {
        record_read_round(&mut request, &format!("more{index}"), 1000);
    }

    // Act
    let job = summary_job(&mut fit, budget(20_000), &request, PROACTIVE);

    // Assert
    assert_eq!(canonical, None);
    let (messages, instruction) = continued(&job);
    assert_eq!(messages, last_sent.as_slice());
    assert!(instruction.starts_with(CONTINUED_SUMMARY_PREFIX));
    let last = last_call_before(&request, job.range().end);
    assert!(instruction.contains(&format!("from call `r0` through call `{last}`")));
    assert!(instruction.contains("never infer"));
    assert!(
        instruction.contains(
            &summary_schema()
                .expect("summary schema")
                .value()
                .to_string()
        )
    );
    assert!(!instruction.contains("Carry forward"));
    assert_eq!(job.request().prefix_schema(), Some(request.schema()));
    assert_eq!(
        job.request().schema(),
        &summary_schema().expect("summary schema")
    );
    assert_eq!(
        u64::try_from(job.source_bytes()).expect("bytes fit u64"),
        weigh(&request.messages()[job.range()])
    );
}

#[test]
fn a_shared_prefix_summary_continues_the_reduced_copy_last_sent() {
    // Arrange
    let mut request = read_rounds(20, 1000);
    let mut fit = ContextFit::new(true);
    let (reduced, _) = fit
        .fit(&ByteEstimator, budget(20_000), &request, None)
        .map(sent)
        .expect("fits reduced");
    let reduced = reduced.expect("reduced copy");
    for index in 0..12 {
        record_read_round(&mut request, &format!("more{index}"), 1000);
    }

    // Act
    let job = summary_job(&mut fit, budget(20_000), &request, PROACTIVE);

    // Assert
    let (messages, _) = continued(&job);
    assert_eq!(messages, reduced.messages());
}

#[test]
fn a_shared_prefix_summary_sends_its_own_source_when_the_continued_request_does_not_fit() {
    // Arrange
    let mut request = read_rounds(18, 1000);
    let mut fit = ContextFit::new(true);
    let (canonical, _) = fit
        .fit(&ByteEstimator, budget(20_000), &request, PROACTIVE)
        .map(sent)
        .expect("the request fits whole");
    record_read_round(&mut request, "large", 3000);

    // Act
    let job = summary_job(&mut fit, budget(20_000), &request, PROACTIVE);

    // Assert
    assert_eq!(canonical, None);
    assert_eq!(
        job.request().messages()[0],
        ModelMessage::System(ROUND_SUMMARY_INSTRUCTIONS.to_string())
    );
    assert_eq!(job.request().prefix_schema(), None);
}

#[test]
fn a_later_shared_prefix_summary_continues_the_note_of_the_earlier_one() {
    // Arrange
    let mut request = read_rounds(20, 1000);
    let mut fit = ContextFit::new(true);
    // Nothing was sent yet, so the first summary sends its own source.
    let first = summary_job(&mut fit, budget(20_000), &request, PROACTIVE);
    fit.install_summary(&ByteEstimator, &request, &first, summary("alpha in r0"))
        .expect("first summary installed");
    let (noted, _) = fit
        .fit(&ByteEstimator, budget(20_000), &request, PROACTIVE)
        .map(sent)
        .expect("fits after the summary");
    let noted = noted.expect("reduced copy");
    for index in 0..20 {
        record_read_round(&mut request, &format!("more{index}"), 1000);
    }

    // Act
    let second = summary_job(&mut fit, budget(20_000), &request, PROACTIVE);

    // Assert
    assert_eq!(first.request().prefix_schema(), None);
    let (messages, instruction) = continued(&second);
    assert_eq!(messages, noted.messages());
    assert!(note_text(&messages[1]).contains("alpha in r0"));
    assert!(instruction.contains("Carry forward everything the summary"));
    assert_eq!(second.range().start, first.range().end);
}
