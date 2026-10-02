//! Session timing exported by the real TUI through a local OTLP receiver.

use std::fmt::Write as _;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value;
use opentelemetry_proto::tonic::trace::v1::Span;
use prost::Message;
use testty::assertion;
use testty::region::Region;
use testty::step::Step;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::fixture::{run_git, seed_session_title_candidate_project};
use crate::common::{self, BuilderEnv, FeatureTest};

const TRACE_COMMAND: &str = "cargo test --filter 'quoted case'";
const TRACE_RESULT: &str = "Test compilation failed: 'quoted case'";

#[tokio::test]
async fn session_turn_exports_otlp_timing() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange / Act / Assert
    run_timing_scenario(false).await
}

#[tokio::test]
async fn session_turn_exports_opt_in_tool_content() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange / Act / Assert
    run_timing_scenario(true).await
}

#[tokio::test]
async fn project_sync_assistance_exports_opt_in_tool_content()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange / Act / Assert
    run_sync_assistance_scenario(true).await
}

#[tokio::test]
async fn project_sync_assistance_excludes_tool_content_by_default()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange / Act / Assert
    run_sync_assistance_scenario(false).await
}

async fn run_sync_assistance_scenario(capture: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let (server, exported) = trace_receiver().await;
    let observed = Arc::clone(&exported);
    let mut args = vec![
        "--otlp-endpoint".to_string(),
        format!("{}/v1/traces", server.uri()),
    ];
    if capture {
        args.push("--otlp-capture-content".to_string());
    }

    // Act
    FeatureTest::new(if capture {
        "project_sync_otlp_tool_content"
    } else {
        "project_sync_otlp_metadata"
    })
    .with_git()
    .with_terminal_size(120, 24)
    .setup(|env| Box::pin(seed_sync_tool_trace_project(env)))
    .args(args)
    .run(
        |scenario| {
            scenario
                .compose(&common::wait_for_agentty_startup())
                .press_key("s")
                .wait_for_text("Synced test-project/main", 15_000)
                .wait_for_text("1 conflict resolved", 5000)
                .step(Step::eventually(
                    Duration::from_secs(15),
                    Duration::from_millis(50),
                    move |frame| {
                        if observed.lock().is_ok_and(|spans| {
                            spans.iter().any(|span| {
                                span.name == "utility.run"
                                    && string_attribute(span, "agentty.purpose")
                                        == Some("sync conflict assistance")
                            })
                        }) {
                            return Ok(());
                        }
                        assertion::match_text_in_region(
                            frame,
                            "utility.run",
                            &Region::full(frame.cols(), frame.rows()),
                        )
                        .map_err(|mut failure| {
                            failure.message = "Waiting for exported sync assistance trace".into();
                            failure
                        })
                    },
                ))
        },
        |frame, _report| {
            Box::pin(async move {
                assertion::assert_text_in_region(
                    frame,
                    "1 conflict resolved",
                    &Region::full(frame.cols(), frame.rows()),
                );
            })
        },
    )
    .await?;

    // Assert
    let spans = exported
        .lock()
        .map_err(|_| io::Error::other("trace receiver poisoned"))?;
    assert_tool_content(&spans, capture)?;
    assert!(spans.iter().all(|span| span.name != "session.turn"));

    Ok(())
}

async fn seed_sync_tool_trace_project(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_tool_trace_project(env).await?;
    let claude = env.stub_bin.join("claude");
    let original = std::fs::read_to_string(&claude)?;
    let script = original.replace(
        "  *)\n",
        "  *)\n    printf 'resolved configuration\\n' > shared.txt\n",
    );
    assert_ne!(script, original, "sync conflict resolution inserted");
    std::fs::write(claude, script)?;

    std::fs::write(env.workdir.join("shared.txt"), "initial configuration\n")?;
    run_git(&env.workdir, &["add", "shared.txt"])?;
    run_git(&env.workdir, &["commit", "-m", "Add shared configuration"])?;
    let origin = env.agentty_root.join("sync-origin.git");
    let origin = origin.to_string_lossy().into_owned();
    run_git(&env.workdir, &["init", "--bare", &origin])?;
    run_git(&env.workdir, &["remote", "add", "origin", &origin])?;
    run_git(&env.workdir, &["push", "--set-upstream", "origin", "main"])?;

    let peer = env.agentty_root.join("sync-peer");
    let peer_path = peer.to_string_lossy().into_owned();
    run_git(
        &env.workdir,
        &["clone", "--branch", "main", &origin, &peer_path],
    )?;
    std::fs::write(peer.join("shared.txt"), "upstream configuration\n")?;
    run_git(
        &peer,
        &[
            "-c",
            "user.name=Agentty Test",
            "-c",
            "user.email=agentty@example.com",
            "commit",
            "-am",
            "Change upstream configuration",
        ],
    )?;
    run_git(&peer, &["push"])?;
    std::fs::write(env.workdir.join("shared.txt"), "local configuration\n")?;
    run_git(
        &env.workdir,
        &["commit", "-am", "Change local configuration"],
    )?;

    Ok(())
}

async fn run_timing_scenario(capture: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let (server, exported) = trace_receiver().await;
    let endpoint = format!("{}/v1/traces", server.uri());

    let observed = Arc::clone(&exported);
    let mut args = vec!["--otlp-endpoint".to_string(), endpoint];
    if capture {
        args.push("--otlp-capture-content".to_string());
    }

    // Act
    FeatureTest::new(if capture {
        "session_otlp_tool_content"
    } else {
        "session_otlp_timing"
    })
    .with_git()
    .setup(|env| Box::pin(seed_tool_trace_project(env)))
    .args(args)
    .run(
        |scenario| {
            scenario
                .compose(&common::wait_for_agentty_startup())
                .compose(&common::switch_to_tab("Sessions"))
                .compose(&common::create_session_with_prompt_and_return_to_list(
                    "Private timing prompt",
                ))
                .press_key("Enter")
                .wait_for_text("Got it. What would you like me to do?", 10000)
                .press_key("q")
                .wait_for_text("Review", 10000)
                .step(Step::eventually(
                    Duration::from_secs(15),
                    Duration::from_millis(50),
                    move |frame| {
                        let ready = observed
                            .lock()
                            .is_ok_and(|spans| has_completed_trace(&spans));
                        if ready {
                            return Ok(());
                        }
                        assertion::match_text_in_region(
                            frame,
                            "session.turn",
                            &Region::full(frame.cols(), frame.rows()),
                        )
                        .map_err(|mut failure| {
                            failure.message =
                                "Waiting for a complete exported session trace".into();
                            failure
                        })
                    },
                ))
        },
        |frame, _report| {
            Box::pin(async move {
                assertion::assert_text_in_region(
                    frame,
                    "Review",
                    &Region::full(frame.cols(), frame.rows()),
                );
            })
        },
    )
    .await?;

    // Assert
    let spans = exported
        .lock()
        .map_err(|_| io::Error::other("trace receiver poisoned"))?
        .clone();
    assert_timing_spans(&spans)?;
    assert_tool_content(&spans, capture)?;

    Ok(())
}

async fn seed_tool_trace_project(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_session_title_candidate_project(env).await?;
    let path = env.stub_bin.join("claude");
    let original = std::fs::read_to_string(&path)?;
    let payloads = [
        serde_json::json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "trace-tool", "name": "Bash", "input": {"command": TRACE_COMMAND}}]}}),
        serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "trace-tool", "content": TRACE_RESULT, "is_error": true}]}}),
        serde_json::json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "trace-redacted", "name": "Bash", "input": {"command": "curl --token fixture-credential"}}]}}),
        serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "trace-redacted", "content": "Authorization: Bearer fixture-credential", "is_error": true}]}}),
        serde_json::json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "trace-large", "name": "Read", "input": {"file_path": "example.rs"}}]}}),
        serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "trace-large", "content": "é".repeat(3000)}]}}),
        serde_json::json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "trace-structured", "name": "Read", "input": {"file_path": "example.rs", "context": "é".repeat(3000)}}]}}),
        serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "trace-structured", "content": [{"type": "text", "text": "é".repeat(3000)}]}]}}),
    ];
    let mut events = String::new();
    for event in payloads {
        let event = event.to_string().replace('\'', "'\\''");
        writeln!(&mut events, "    printf '%s\\n' '{event}'")?;
    }
    let script = original.replace("  *)\n    answer=", &format!("  *)\n{events}    answer="));
    assert_ne!(script, original, "tool events inserted");
    std::fs::write(&path, script)?;
    Ok(())
}

fn assert_timing_spans(spans: &[Span]) -> io::Result<()> {
    let turn = spans
        .iter()
        .find(|span| span.name == "session.turn")
        .ok_or_else(|| io::Error::other("missing turn root"))?;
    for name in EXPECTED_STEPS {
        let span = spans
            .iter()
            .find(|span| span.name == name && span.trace_id == turn.trace_id)
            .ok_or_else(|| io::Error::other(format!("missing {name}")))?;
        assert!(span.end_time_unix_nano >= span.start_time_unix_nano);
    }
    let queue = spans
        .iter()
        .find(|span| span.name == "queue.wait" && span.trace_id == turn.trace_id)
        .ok_or_else(|| io::Error::other("missing queue"))?;
    assert_eq!(queue.parent_span_id, turn.span_id);
    for key in [
        "agentty.session.id",
        "agentty.operation.id",
        "agentty.turn.id",
        "agentty.harness",
    ] {
        assert!(
            turn.attributes.iter().any(|attribute| attribute.key == key),
            "{key}"
        );
    }

    Ok(())
}

fn assert_tool_content(spans: &[Span], capture: bool) -> io::Result<()> {
    assert!(!format!("{spans:?}").contains("Private timing prompt"));
    assert!(!format!("{spans:?}").contains("fixture-credential"));
    let tool = tool_span(spans, "trace-tool")?;
    assert_eq!(string_attribute(tool, "gen_ai.tool.name"), Some("Bash"));
    assert_eq!(
        string_attribute(tool, "process.executable.name"),
        Some("cargo")
    );
    assert_eq!(string_attribute(tool, "agentty.outcome"), Some("failed"));
    assert_eq!(
        string_attribute(tool, "agentty.tool.command"),
        capture.then_some(TRACE_COMMAND)
    );
    assert_eq!(
        string_attribute(tool, "gen_ai.tool.call.arguments"),
        capture.then_some(r#"{"command":"cargo test --filter 'quoted case'"}"#)
    );
    assert_eq!(
        string_attribute(tool, "gen_ai.tool.call.result"),
        capture.then_some(TRACE_RESULT)
    );
    let redacted = tool_span(spans, "trace-redacted")?;
    assert_eq!(
        string_attribute(redacted, "gen_ai.tool.call.arguments"),
        capture.then_some(r#""[REDACTED]""#)
    );
    assert_eq!(
        string_attribute(redacted, "gen_ai.tool.call.result"),
        capture.then_some("[REDACTED]")
    );
    let large = tool_span(spans, "trace-large")?;
    assert_eq!(
        string_attribute(large, "gen_ai.tool.call.result").map(str::len),
        capture.then_some(4096)
    );
    assert_eq!(
        large
            .attributes
            .iter()
            .find(|attribute| attribute.key == "agentty.tool.output.truncated")
            .and_then(|attribute| attribute.value.as_ref())
            .and_then(|value| value.value.as_ref()),
        capture.then_some(&any_value::Value::BoolValue(true))
    );

    assert_structured_previews(spans, capture)
}

fn assert_structured_previews(spans: &[Span], capture: bool) -> io::Result<()> {
    let structured = tool_span(spans, "trace-structured")?;
    for key in ["gen_ai.tool.call.arguments", "gen_ai.tool.call.result"] {
        assert_eq!(string_attribute(structured, key), None);
    }
    for (key, truncated) in [
        ("agentty.tool.input.preview", "agentty.tool.input.truncated"),
        (
            "agentty.tool.output.preview",
            "agentty.tool.output.truncated",
        ),
    ] {
        let preview = string_attribute(structured, key);
        assert_eq!(preview.is_some(), capture);
        if let Some(preview) = preview {
            assert!(preview.len() <= 4096);
            assert!(!preview.contains('�'));
        }
        assert_eq!(
            structured
                .attributes
                .iter()
                .find(|attribute| attribute.key == truncated)
                .and_then(|attribute| attribute.value.as_ref())
                .and_then(|value| value.value.as_ref()),
            capture.then_some(&any_value::Value::BoolValue(true))
        );
    }

    Ok(())
}

fn tool_span<'a>(spans: &'a [Span], id: &str) -> io::Result<&'a Span> {
    spans
        .iter()
        .find(|span| string_attribute(span, "gen_ai.tool.call.id") == Some(id))
        .ok_or_else(|| io::Error::other(format!("missing tool span {id}")))
}

fn string_attribute<'a>(span: &'a Span, key: &str) -> Option<&'a str> {
    span.attributes
        .iter()
        .find(|attribute| attribute.key == key)
        .and_then(|attribute| attribute.value.as_ref())
        .and_then(|value| match value.value.as_ref() {
            Some(any_value::Value::StringValue(value)) => Some(value.as_str()),
            _ => None,
        })
}

#[tokio::test]
async fn quit_exports_active_and_queued_turn_outcomes() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let (server, exported) = trace_receiver().await;
    let observed = Arc::clone(&exported);

    // Act
    FeatureTest::new("session_otlp_quit")
        .with_git()
        .setup(|env| {
            Box::pin(async move {
                seed_session_title_candidate_project(env).await?;
                let path = env.stub_bin.join("claude");
                let original_script = std::fs::read_to_string(&path)?;
                let script =
                    original_script.replace(
                        "  *)\n    answer='Got it.",
                        "  *\"Keep exit turn active\"*)\n    sleep 30\n    answer='Late \
                         response'\n    ;;\n  *)\n    answer='Got it.",
                    );
                assert_ne!(
                    script, original_script,
                    "Claude stub rewrite did not add the active-turn delay"
                );
                std::fs::write(&path, script)?;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750))?;
                Ok(())
            })
        })
        .args([
            "--otlp-endpoint".to_string(),
            format!("{}/v1/traces", server.uri()),
        ])
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::create_session_with_prompt_and_return_to_list(
                        "Keep exit turn active",
                    ))
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 10000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Queued exit prompt")
                    .press_key("Enter")
                    .wait_for_text("≡ queued ›", 5000)
                    .press_key("q")
                    .compose(&common::open_quit_dialog())
                    .press_key("y")
                    .step(Step::eventually(
                        Duration::from_secs(15),
                        Duration::from_millis(50),
                        move |frame| {
                            if observed
                                .lock()
                                .expect("spans")
                                .iter()
                                .filter(|span| {
                                    span.name == "session.turn" && has_canceled_outcome(span)
                                })
                                .count()
                                == 2
                            {
                                return Ok(());
                            }
                            let mut failure = assertion::match_text_in_region(
                                frame,
                                "session.turn",
                                &Region::full(frame.cols(), frame.rows()),
                            )
                            .expect_err("span names are not displayed");
                            failure.message =
                                "Waiting for active and queued outcomes during quit".into();
                            Err(failure)
                        },
                    ))
            },
            |_frame, _report| Box::pin(async {}),
        )
        .await?;

    // Assert
    let spans = exported.lock().expect("spans");
    let turns: Vec<_> = spans
        .iter()
        .filter(|span| span.name == "session.turn")
        .collect();
    assert_eq!(turns.len(), 2);
    for turn in turns {
        assert!(has_canceled_outcome(turn));
        assert!(
            spans
                .iter()
                .any(|span| span.name == "queue.wait" && span.trace_id == turn.trace_id)
        );
    }
    assert!(!format!("{spans:?}").contains("Queued exit prompt"));

    Ok(())
}

fn has_canceled_outcome(span: &Span) -> bool {
    span.attributes.iter().any(|attribute| {
        attribute.key == "agentty.outcome"
            && attribute.value.as_ref().is_some_and(|value| {
                matches!(&value.value, Some(any_value::Value::StringValue(outcome)) if outcome == "canceled")
            })
    })
}

fn has_completed_trace(spans: &[Span]) -> bool {
    spans.iter().any(|turn| {
        turn.name == "session.turn"
            && EXPECTED_STEPS.iter().all(|name| {
                spans
                    .iter()
                    .any(|span| span.name == *name && span.trace_id == turn.trace_id)
            })
    })
}

const EXPECTED_STEPS: [&str; 9] = [
    "queue.wait",
    "workspace.prepare",
    "context.prepare",
    "admission.wait",
    "agent.run",
    "agent.attempt",
    "response.validate",
    "turn.persist",
    "postprocess",
];

async fn trace_receiver() -> (MockServer, Arc<Mutex<Vec<Span>>>) {
    let server = MockServer::start().await;
    let exported = Arc::new(Mutex::new(Vec::<Span>::new()));
    let received = Arc::clone(&exported);
    Mock::given(method("POST"))
        .respond_with(move |request: &wiremock::Request| {
            let Ok(payload) = ExportTraceServiceRequest::decode(request.body.as_slice()) else {
                return ResponseTemplate::new(400);
            };
            let Ok(mut spans) = received.lock() else {
                return ResponseTemplate::new(500);
            };
            spans.extend(
                payload
                    .resource_spans
                    .into_iter()
                    .flat_map(|resource| resource.scope_spans)
                    .flat_map(|scope| scope.spans),
            );
            ResponseTemplate::new(200)
        })
        .mount(&server)
        .await;

    (server, exported)
}
