use std::sync::Arc;

use ag_protocol::TurnPrompt;
use ag_session::test_support::graph_proposal_state;
use ag_store::{DbError, MockGraphProposalRepository};

use super::next_command_token;
use crate::app::execution_graph::tests::seed_session;
use crate::test_support::new_test_app;

#[tokio::test]
async fn graph_commands_validate_feature_attachments_and_session_ownership() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let state = graph_proposal_state("preview", "sess");
    app.services
        .db()
        .graph_proposals()
        .create(&state)
        .await
        .expect("capture");
    for (enabled, attachment, session, command, expected) in [
        (
            false,
            false,
            "sess",
            "query",
            "enable --experimental-multi-agent",
        ),
        (
            true,
            true,
            "sess",
            "query",
            "attachment paths are unsupported",
        ),
        (true, false, "other", "query", "Use /graph <request>"),
        (true, false, "other", "query preview", "different session"),
        (
            true,
            false,
            "sess",
            "new preview different",
            "different original input",
        ),
    ] {
        app.experimental_multi_agent = enabled;
        let mut prompt = TurnPrompt::from_text(format!("/graph {command}"));
        if attachment {
            prompt.attachments.push(ag_protocol::TurnPromptAttachment {
                local_image_path: "image.png".into(),
                placeholder: "[Image #1]".into(),
            });
        }

        // Act
        let result = app
            .execute_graph_command(&session.into(), command, &prompt)
            .await;

        // Assert
        let notice = result.unwrap_or_else(|error| error);
        assert!(notice.contains(expected), "{command}: {notice}");
    }
}

#[tokio::test]
async fn graph_commands_report_failed_lists_and_reads_before_and_after_actions() {
    for (command, successful_reads) in [
        ("query", 0),
        ("cancel preview 1", 0),
        ("cancel preview 1", 2),
    ] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        app.experimental_multi_agent = true;
        let mut repository = MockGraphProposalRepository::new();
        if command == "query" {
            repository.expect_list().once().return_once(|_| {
                Err(DbError::InvalidData {
                    entity: "graph proposal",
                    reason: "read unavailable".into(),
                })
            });
        } else {
            if successful_reads > 0 {
                repository
                    .expect_get()
                    .times(successful_reads)
                    .returning(|_, _| Ok(Some(graph_proposal_state("preview", "sess"))));
                repository.expect_cancel().once().returning(|_, _| Ok(()));
            }
            repository.expect_get().once().return_once(|_, _| {
                Err(DbError::InvalidData {
                    entity: "graph proposal",
                    reason: "read unavailable".into(),
                })
            });
        }
        app.services
            .set_graph_proposal_repository(Arc::new(repository));

        // Act
        let result = app
            .execute_graph_command(
                &"sess".into(),
                command,
                &TurnPrompt::from_text(format!("/graph {command}")),
            )
            .await;

        // Assert
        assert!(
            result
                .expect_err("read failure")
                .contains("read unavailable")
        );
    }
}

#[test]
fn command_tokens_skip_whitespace_and_preserve_json_answers() {
    for (command, operation, version, answers) in [
        ("approve  preview 1", "approve", Some("1"), ""),
        ("query\t preview", "query", None, ""),
        ("cancel \n preview\t 2", "cancel", Some("2"), ""),
        ("regenerate preview  3", "regenerate", Some("3"), ""),
        (
            "answer\t preview  \n1  [\"Local  only\", \"Use \\\"quotes\\\"\"]",
            "answer",
            Some("1"),
            "[\"Local  only\", \"Use \\\"quotes\\\"\"]",
        ),
    ] {
        // Arrange
        let mut remaining = command;

        // Act
        let parsed_operation = next_command_token(&mut remaining);
        let id = next_command_token(&mut remaining);
        let parsed_version = next_command_token(&mut remaining);

        // Assert
        assert_eq!(parsed_operation, Some(operation));
        assert_eq!(id, Some("preview"));
        assert_eq!(parsed_version, version);
        assert_eq!(remaining.trim_start(), answers);
        if operation == "answer" {
            assert_eq!(
                serde_json::from_str::<Vec<String>>(remaining).expect("answers"),
                ["Local  only", "Use \"quotes\""]
            );
        }
    }
}
