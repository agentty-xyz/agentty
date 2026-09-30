//! Session timing exported by the real TUI through a local OTLP receiver.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
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
    let server = MockServer::start().await;
    let exported = Arc::new(Mutex::new(Vec::<Span>::new()));
    let received = Arc::clone(&exported);
    Mock::given(method("POST"))
        .respond_with(move |request: &wiremock::Request| {
            let payload =
                ExportTraceServiceRequest::decode(request.body.as_slice()).expect("OTLP protobuf");
            received.lock().expect("received spans").extend(
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
