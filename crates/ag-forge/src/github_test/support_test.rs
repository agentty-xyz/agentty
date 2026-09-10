use crate::command::ForgeCommandOutput;
use crate::model::{ForgeKind, ForgeRemote, ReviewRequestMetadataFieldUpdate};

pub(super) fn github_remote() -> ForgeRemote {
    ForgeRemote {
        command_working_directory: None,
        forge_kind: ForgeKind::GitHub,
        host: "github.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty".to_string(),
    }
}

pub(super) fn github_review_threads_json() -> String {
    serde_json::json!([
        review_threads_page(vec![
            ReviewThreadFixture {
                comments: vec![
                    review_comment_node(Some("alice"), "Why aren't we handling None?"),
                    current_user_review_comment_node(
                        "No change needed.\n\n<!-- agentty review \
                         resolution:123e4567-e89b-12d3-a456-426614174000 -->",
                    ),
                ],
                diff_side: "RIGHT",
                has_next_comment_page: false,
                id: "thread-1",
                is_resolved: false,
                line: Some(42),
                path: "src/foo.rs",
                subject_type: "LINE",
            }
            .into_node()
        ]),
        review_threads_page(vec![
            ReviewThreadFixture {
                comments: vec![review_comment_node(None, "Resolved thread.")],
                diff_side: "LEFT",
                has_next_comment_page: false,
                id: "thread-2",
                is_resolved: true,
                line: Some(15),
                path: "src/bar.rs",
                subject_type: "LINE",
            }
            .into_node(),
            ReviewThreadFixture {
                comments: Vec::new(),
                diff_side: "RIGHT",
                has_next_comment_page: false,
                id: "thread-3",
                is_resolved: false,
                line: None,
                path: "Cargo.toml",
                subject_type: "FILE",
            }
            .into_node(),
        ]),
    ])
    .to_string()
}

pub(super) fn github_pull_request_comments_json() -> String {
    serde_json::json!([
        pull_request_comments_page(vec![review_comment_node(
            Some("carol"),
            "Overall looks good.",
        )]),
        pull_request_comments_page(vec![review_comment_node(
            None,
            "Ghost conversation comment.",
        )]),
    ])
    .to_string()
}

pub(super) fn github_empty_pull_request_comments_json() -> String {
    serde_json::json!([pull_request_comments_page(Vec::new())]).to_string()
}

pub(super) fn github_oversized_thread_json() -> String {
    serde_json::json!([review_threads_page(vec![
        ReviewThreadFixture {
            comments: vec![review_comment_node(Some("alice"), "First page")],
            diff_side: "RIGHT",
            has_next_comment_page: true,
            id: "thread-large",
            is_resolved: false,
            line: Some(7),
            path: "src/large.rs",
            subject_type: "LINE",
        }
        .into_node()
    ])])
    .to_string()
}

pub(super) fn github_thread_comment_pages_json() -> String {
    serde_json::json!([
        thread_comments_page(vec![review_comment_node(Some("alice"), "First page")]),
        thread_comments_page(vec![review_comment_node(Some("bob"), "Second page")]),
    ])
    .to_string()
}

fn review_threads_page(nodes: Vec<serde_json::Value>) -> serde_json::Value {
    let nodes = serde_json::Value::Array(nodes);

    serde_json::json!({
        "data": {
            "repository": {
                "pullRequest": {
                    "reviewThreads": {
                        "nodes": nodes,
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }
                }
            }
        }
    })
}

pub(super) struct ReviewThreadFixture<'a> {
    pub(super) comments: Vec<serde_json::Value>,
    pub(super) diff_side: &'a str,
    pub(super) has_next_comment_page: bool,
    pub(super) id: &'a str,
    pub(super) is_resolved: bool,
    pub(super) line: Option<u32>,
    pub(super) path: &'a str,
    pub(super) subject_type: &'a str,
}

impl ReviewThreadFixture<'_> {
    fn into_node(self) -> serde_json::Value {
        let comments = serde_json::Value::Array(self.comments);

        serde_json::json!({
            "id": self.id,
            "isResolved": self.is_resolved,
            "isOutdated": false,
            "path": self.path,
            "line": self.line,
            "startLine": null,
            "diffSide": self.diff_side,
            "subjectType": self.subject_type,
            "comments": {
                "nodes": comments,
                "pageInfo": {
                    "hasNextPage": self.has_next_comment_page,
                    "endCursor": if self.has_next_comment_page {
                        Some("cursor-1")
                    } else {
                        None
                    }
                }
            }
        })
    }
}

fn pull_request_comments_page(nodes: Vec<serde_json::Value>) -> serde_json::Value {
    let nodes = serde_json::Value::Array(nodes);

    serde_json::json!({
        "data": {
            "repository": {
                "pullRequest": {
                    "comments": {
                        "nodes": nodes,
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }
                }
            }
        }
    })
}

fn thread_comments_page(nodes: Vec<serde_json::Value>) -> serde_json::Value {
    let nodes = serde_json::Value::Array(nodes);

    serde_json::json!({
        "data": {
            "node": {
                "comments": {
                    "nodes": nodes,
                    "pageInfo": {"hasNextPage": false, "endCursor": null}
                }
            }
        }
    })
}

fn review_comment_node(author: Option<&str>, body: &str) -> serde_json::Value {
    serde_json::json!({
        "author": author.map(|login| serde_json::json!({"login": login})),
        "body": body,
        "viewerDidAuthor": false
    })
}

fn current_user_review_comment_node(body: &str) -> serde_json::Value {
    serde_json::json!({
        "author": {"login": "agentty"},
        "body": body,
        "viewerDidAuthor": true
    })
}

pub(super) fn github_view_json() -> String {
    r#"{
        "number": 42,
        "title": "Add forge review support",
        "state": "OPEN",
        "url": "https://github.com/agentty-xyz/agentty/pull/42",
        "baseRefName": "main",
        "headRefName": "feature/forge",
        "isDraft": false,
        "mergeStateStatus": "CLEAN",
        "reviewDecision": "APPROVED",
        "mergedAt": null
    }"#
    .to_string()
}

pub(super) fn github_metadata_json() -> String {
    serde_json::json!({
        "body": "Current body.",
        "title": "Add forge review support"
    })
    .to_string()
}

pub(super) fn reconciled_field(current: &str, desired: &str) -> ReviewRequestMetadataFieldUpdate {
    ReviewRequestMetadataFieldUpdate {
        current: current.to_string(),
        desired: desired.to_string(),
    }
}

pub(super) fn success_output(stdout: String) -> ForgeCommandOutput {
    ForgeCommandOutput {
        exit_code: Some(0),
        stderr: String::new(),
        stdout,
    }
}

pub(super) fn failure_output(stderr: String) -> ForgeCommandOutput {
    ForgeCommandOutput {
        exit_code: Some(1),
        stderr,
        stdout: String::new(),
    }
}
