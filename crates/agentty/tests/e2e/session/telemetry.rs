//! Session timing exported by the real TUI through a local OTLP receiver.

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

use super::fixture::seed_session_title_candidate_project;
use crate::common::{self, FeatureTest};

#[tokio::test]
async fn session_turn_exports_otlp_timing() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let (server, exported) = trace_receiver().await;
    let endpoint = format!("{}/v1/traces", server.uri());

    let observed = Arc::clone(&exported);

    // Act
    FeatureTest::new("session_otlp_timing")
        .with_git()
        .setup(|env| Box::pin(seed_session_title_candidate_project(env)))
        .args(["--otlp-endpoint".to_string(), endpoint])
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
                            let spans = observed.lock().expect("received spans");
                            if has_completed_trace(&spans) {
                                return Ok(());
                            }
                            let mut failure = assertion::match_text_in_region(
                                frame,
                                "session.turn",
                                &Region::full(frame.cols(), frame.rows()),
                            )
                            .expect_err("trace names are not rendered in this scenario");
                            failure.message = format!(
                                "Waiting for a complete exported session trace; received: {:?}",
                                spans.iter().map(|span| &span.name).collect::<Vec<_>>()
                            );
                            Err(failure)
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
    let spans = exported.lock().expect("received spans").clone();
    let turn = spans
        .iter()
        .find(|span| span.name == "session.turn")
        .expect("turn root");
    for name in EXPECTED_STEPS {
        let span = spans
            .iter()
            .find(|span| span.name == name && span.trace_id == turn.trace_id)
            .expect(name);
        assert!(span.end_time_unix_nano >= span.start_time_unix_nano);
    }
    let queue = spans
        .iter()
        .find(|span| span.name == "queue.wait" && span.trace_id == turn.trace_id)
        .expect("queue");
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
    assert!(!format!("{spans:?}").contains("Private timing prompt"));

    Ok(())
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
