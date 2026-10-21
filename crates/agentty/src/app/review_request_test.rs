use std::path::Path;
use std::sync::{Arc, Mutex};

use ag_contracts::{AgentRequestKind, OneShotSubmission, PermissionMode, SessionStats};
use ag_forge::ReviewRequestMetadata;
use ag_git::{GitClient, MockGitClient};
use ag_session::test_support as model_fixture;
use ag_worker::MockRunClient;

use super::{
    AGENTTY_FOOTER, ReviewRequestCommitMessage, ReviewRequestGenerator, html_tag,
    normalize_attribution, parse_review_request_commit_message, strip_attribution,
};
use crate::domain::session_message::SessionTranscript;
use crate::domain::setting::SettingName;
use crate::infra::db::AppRepositories;

#[test]
fn attribution_normalizes_legacy_footers_without_losing_other_content() {
    // Arrange
    let cases = [
        ("Details".to_string(), "Details"),
        ("    command  ".to_string(), "    command  "),
        (
            format!("Details\n\n{AGENTTY_FOOTER}\nNew detail\n{AGENTTY_FOOTER}\n"),
            "Details\n\nNew detail",
        ),
        (
            "Details\nco-authored-by: Agentty\nCo-Authored-By: Alice <alice@example.com>\nUser \
             note"
                .to_string(),
            "Details\nCo-Authored-By: Alice <alice@example.com>\nUser note",
        ),
        ("Co-Authored-By: Agentty\n".to_string(), ""),
        (String::new(), ""),
        (
            "Co-Authored-By: Agentty contributor\nInline Co-Authored-By: Agentty".to_string(),
            "Co-Authored-By: Agentty contributor\nInline Co-Authored-By: Agentty",
        ),
    ];
    for (current, expected) in cases {
        // Act
        let disabled = normalize_attribution(&current, false).expect("valid description");
        let enabled = normalize_attribution(&current, true).expect("valid description");

        // Assert
        assert_eq!(disabled, expected);
        assert_eq!(enabled.lines().last(), Some(AGENTTY_FOOTER));
        assert_eq!(enabled.matches(AGENTTY_FOOTER).count(), 1);
        assert_eq!(
            normalize_attribution(&enabled, true).expect("repeat"),
            enabled
        );
        assert_eq!(
            normalize_attribution(&enabled, false).expect("disable"),
            disabled
        );
        assert_eq!(strip_attribution(&enabled).trim_end_matches('\n'), expected);
    }
}

#[test]
fn attribution_preserves_literal_markdown_code_examples() {
    // Arrange
    let legacy = "Co-Authored-By: Agentty";
    let examples = [
        format!("```text\n{legacy}\n{AGENTTY_FOOTER}\n```"),
        format!("~~~text\n{AGENTTY_FOOTER}\n~~~"),
        format!("    {legacy}\n    {AGENTTY_FOOTER}"),
        format!("```text\r\n{legacy}\r\n{AGENTTY_FOOTER}\r\n```"),
        format!("<!--\n{legacy}\n{AGENTTY_FOOTER}\n-->"),
        format!("<pre>\n{legacy}\n{AGENTTY_FOOTER}\n</pre>"),
        format!("<PRE class='example > literal'>\n{legacy}\n</PRE>"),
        format!("<pre><code>\n{legacy}\n</code></pre>"),
        format!("<code>\n{legacy}\n</code>"),
        format!("<details>\n<!--\n{legacy}\n{AGENTTY_FOOTER}\n-->\n</details>"),
        format!("<div title=\"<!-- not a comment >\">\n<!--\n{legacy}\n-->\n</div>"),
        format!("Example: `\n{legacy}\n{AGENTTY_FOOTER}\n`"),
        format!("Example: ``\n{legacy}\n`literal backtick`\n``"),
        format!("Example: `\r\n{legacy}\r\n`"),
    ];
    for example in examples {
        let source = format!("## Literal examples é\n\n{example}\n\nKeep this note.");
        let expected = source.replace("\r\n", "\n");
        let current = format!("{source}\n\n{legacy}\n{AGENTTY_FOOTER}\n");

        // Act
        let disabled = normalize_attribution(&current, false).expect("valid description");
        let enabled = normalize_attribution(&current, true).expect("valid description");

        // Assert
        assert_eq!(disabled, expected);
        assert_eq!(enabled, format!("{expected}\n\n{AGENTTY_FOOTER}"));
        assert_eq!(
            normalize_attribution(&enabled, true).expect("repeat"),
            enabled
        );
        assert_eq!(
            normalize_attribution(&enabled, false).expect("disable"),
            disabled
        );
    }
}

#[test]
fn attribution_rejects_unclosed_blocks_before_adding_or_removing_footers() {
    // Arrange
    for body in [
        "```sh\ncommand",
        "~~~sh\ncommand\n",
        "````sh\ncommand\n```",
        "```",
        "<!--\nHidden text",
        "<pre>\ncommand",
        "<code>command",
        "<div>\n<!--\nHidden text",
    ] {
        let legacy = format!("{body}\n\n{AGENTTY_FOOTER}\n\n{AGENTTY_FOOTER}");
        for current in [body, legacy.as_str()] {
            // Act / Assert
            for enabled in [true, true, false] {
                let error = normalize_attribution(current, enabled).expect_err("unsafe footer");
                assert!(error.to_string().contains("close the block and retry"));
                assert_eq!(strip_attribution(current), current.trim_end_matches('\n'));
            }
        }
    }
}

#[test]
fn attribution_accepts_closed_blocks_and_literal_comment_delimiters() {
    // Arrange
    for body in [
        "```sh\ncommand\n```",
        "~~~sh\ncommand\n~~~~  ",
        "````sh\n```\n````",
        "<!-- note --> Visible text.",
        "Use `<!--` literally.",
        "<span title=\"<!--\">Visible text.</span>",
        "> ```sh\n> command\n\nOutside the quote.",
        "<div title=\"<!-- not a comment >\">Visible text.</div>",
        "<details>\n<!-- one -->\nVisible text.\n<!-- two -->\n</details>",
        "<pre>\n<!-- literal comment opener\n</pre>",
        "<pre>\n<div title='</pre>'>literal tag</div>\n</pre>",
    ] {
        // Act
        let enabled = normalize_attribution(body, true).expect("footer outside literal blocks");

        // Assert
        assert_eq!(enabled, format!("{body}\n\n{AGENTTY_FOOTER}"));
        assert_eq!(
            normalize_attribution(&enabled, true).expect("repeat"),
            enabled
        );
        assert_eq!(
            normalize_attribution(&enabled, false).expect("disable"),
            body
        );
    }
}

#[test]
fn html_tags_respect_quoted_attributes_and_name_boundaries() {
    // Arrange
    for (source, expected) in [
        ("<div title='> <!-- </pre>'>", Some(("div", false))),
        ("<PRE class=\"example >\">", Some(("PRE", false))),
        ("</code>", Some(("code", true))),
        (
            "<custom-element data-x='v' />",
            Some(("custom-element", false)),
        ),
        ("<pre.text>", None),
        ("<3pre>", None),
        ("<pre title='unterminated>", None),
        ("not a tag", None),
        ("<pre", None),
    ] {
        // Act
        let parsed = html_tag(source);

        // Assert
        assert_eq!(
            parsed,
            expected.map(|(name, closing)| (name, closing, source.len()))
        );
    }
}

#[tokio::test]
async fn generator_uses_published_review_target_and_applies_current_setting() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("database");
    let project_id = db
        .projects()
        .upsert_project("project", Some("main".into()))
        .await
        .expect("project");
    db.sessions()
        .insert_session(
            "session",
            model_fixture::CODEX_MODEL_ID,
            "wt/local-parent",
            "Review",
            project_id,
        )
        .await
        .expect("session");
    db.sessions()
        .update_session_published_upstream_ref("session", Some("fork/published".into()))
        .await
        .expect("published branch");
    let mut git = MockGitClient::new();
    git.expect_fetch_remote_branch_commits()
        .times(3)
        .withf(|folder, remote, base, source| {
            folder == Path::new("project")
                && remote == "fork"
                && base == "review/parent"
                && source == "published"
        })
        .returning(|_, _, _, _| Box::pin(async { Ok(("a".repeat(40), "b".repeat(40))) }));
    git.expect_committed_diff()
        .times(3)
        .withf(|folder, base, source| {
            folder == Path::new("project") && base == &"a".repeat(40) && source == &"b".repeat(40)
        })
        .returning(|_, _, _| {
            Box::pin(async {
                Ok("diff --git a/publish.rs \
                    b/publish.rs\n+unpublished_parent_change();\n+generate_description();"
                    .into())
            })
        });
    let git: Arc<dyn GitClient> = Arc::new(git);
    let body = "## Why?\nReviewers need context.\n\n## What?\nGenerate descriptions.\n\n## \
                Impact?\nStructured review context.\n\n- [ ] Keep release note #42";
    let expected = body.to_string();
    let mut run_client = MockRunClient::new();
    run_client.expect_submit().times(3).returning(move |request| {
        assert_eq!(request.request_kind, AgentRequestKind::ReviewMetadata);
        assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
        assert!(request.provider_call_budget.is_some());
        assert!(request.prompt.contains("generate_description"));
        assert!(request.prompt.contains("unpublished_parent_change"));
        assert!(request.prompt.contains("user_intent"));
        assert!(!request.prompt.contains(AGENTTY_FOOTER));
        Ok(OneShotSubmission {
            response: ag_protocol::AgentResponse::plain(serde_json::json!({
                "title": "Ignored cosmetic title", "description": format!("{expected}\n\n{AGENTTY_FOOTER}"), "is_title_change_significant": false,
            }).to_string()),
            stats: SessionStats::default(),
        })
    });
    let transcript = Mutex::new(SessionTranscript::default());
    let generator = ReviewRequestGenerator {
        db: &db,
        folder: Path::new("project"),
        git_client: &git,
        run_client: &run_client,
        session_agent: model_fixture::codex_selection(),
        session_id: "session",
        transcript: &transcript,
    };
    let commit = ReviewRequestCommitMessage {
        body: Some(format!("Commit detail\n{AGENTTY_FOOTER}")),
        title: "Stable title".into(),
    };
    let mut current: Option<ReviewRequestMetadata> = None;
    for enabled in [false, true, false] {
        db.settings()
            .upsert_project_setting(
                project_id,
                SettingName::IncludeCoauthoredByAgentty,
                &enabled.to_string(),
            )
            .await
            .expect("setting");

        // Act
        let metadata = generator
            .generate(current.as_ref(), &commit, "review/parent")
            .await
            .expect("description");

        // Assert
        assert_eq!(metadata.title, "Stable title");
        assert_eq!(
            metadata.body,
            normalize_attribution(body, enabled).expect("valid description")
        );
        current = Some(metadata);
    }
}

#[tokio::test]
async fn generator_requires_a_published_ref_with_a_remote_before_reading_evidence() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("database");
    let git_client: Arc<dyn GitClient> = Arc::new(MockGitClient::new());
    let run_client = MockRunClient::new();
    let transcript = Mutex::new(SessionTranscript::default());
    let generator = ReviewRequestGenerator {
        db: &db,
        folder: Path::new("project"),
        git_client: &git_client,
        run_client: &run_client,
        session_agent: model_fixture::codex_selection(),
        session_id: "missing-session",
        transcript: &transcript,
    };
    let commit = ReviewRequestCommitMessage {
        body: None,
        title: "Title".into(),
    };

    // Act
    let error = generator
        .generate(None, &commit, "main")
        .await
        .expect_err("unpublished session");

    // Assert
    assert!(
        error
            .to_string()
            .contains("Published branch reference is unavailable")
    );

    // Arrange
    let project_id = db
        .projects()
        .upsert_project("project", Some("main".into()))
        .await
        .expect("project");
    db.sessions()
        .insert_session(
            "missing-session",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("session");
    db.sessions()
        .update_session_published_upstream_ref("missing-session", Some("invalid".into()))
        .await
        .expect("invalid published reference");

    // Act
    let error = generator
        .generate(None, &commit, "main")
        .await
        .expect_err("published reference must identify its remote");

    // Assert
    assert!(
        error
            .to_string()
            .contains("Published branch reference has no remote")
    );
}

/// Verifies parsing keeps the first non-empty line as the title and trims
/// the remaining description into the PR body.
#[test]
fn parse_review_request_commit_message_returns_title_and_body() {
    // Arrange
    let commit_message = "Refine session commit message\n\n- Keep title in sync\n";

    // Act
    let parsed_commit_message = parse_review_request_commit_message(commit_message);

    // Assert
    assert_eq!(
        parsed_commit_message,
        Some(ReviewRequestCommitMessage {
            body: Some("- Keep title in sync".to_string()),
            title: "Refine session commit message".to_string(),
        })
    );
}

/// Verifies parsing omits the PR body when the commit message only has a
/// title.
#[test]
fn parse_review_request_commit_message_omits_empty_body() {
    // Arrange
    let commit_message = "Refine session commit message\n\n";

    // Act
    let parsed_commit_message = parse_review_request_commit_message(commit_message);

    // Assert
    assert_eq!(
        parsed_commit_message,
        Some(ReviewRequestCommitMessage {
            body: None,
            title: "Refine session commit message".to_string(),
        })
    );
}

/// Verifies parsing rejects commit messages that do not contain a title.
#[test]
fn parse_review_request_commit_message_rejects_empty_title() {
    // Arrange
    let commit_message = "\n \n";

    // Act
    let parsed_commit_message = parse_review_request_commit_message(commit_message);

    // Assert
    assert_eq!(parsed_commit_message, None);
}
