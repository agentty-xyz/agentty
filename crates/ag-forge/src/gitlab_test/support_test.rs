use crate::command::ForgeCommandOutput;
use crate::model::{ForgeKind, ForgeRemote, ReviewRequestMetadataFieldUpdate};

/// Builds one normalized GitLab remote for command-construction tests.
pub(super) fn gitlab_remote() -> ForgeRemote {
    ForgeRemote {
        command_working_directory: None,
        forge_kind: ForgeKind::GitLab,
        host: "gitlab.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "https://gitlab.com/agentty-xyz/agentty.git".to_string(),
        web_url: "https://gitlab.com/agentty-xyz/agentty".to_string(),
    }
}

/// Builds one successful command output with `stdout`.
pub(super) fn success_output(stdout: String) -> ForgeCommandOutput {
    ForgeCommandOutput {
        exit_code: Some(0),
        stderr: String::new(),
        stdout,
    }
}

/// Returns one representative GitLab merge-request JSON response.
pub(super) fn gitlab_view_json() -> String {
    serde_json::json!({
        "description": "Current description.",
        "detailed_merge_status": "can_be_merged",
        "draft": true,
        "iid": 42,
        "merge_status": "can_be_merged",
        "merged_at": null,
        "source_branch": "feature/forge",
        "state": "opened",
        "target_branch": "main",
        "title": "Add forge review support",
        "web_url": "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42"
    })
    .to_string()
}

pub(super) fn reconciled_field(current: &str, desired: &str) -> ReviewRequestMetadataFieldUpdate {
    ReviewRequestMetadataFieldUpdate {
        current: current.to_string(),
        desired: desired.to_string(),
    }
}

/// Returns one representative GitLab discussions API response.
pub(super) fn gitlab_discussions_json() -> String {
    r#"[
        {
            "id": "discussion-1",
            "individual_note": false,
            "notes": [
                {
                    "id": 1,
                    "type": "DiffNote",
                    "body": "Please simplify this.",
                    "author": {"id": 1, "name": "Alice", "username": "alice"},
                    "system": false,
                    "resolved": false,
                    "position": {
                        "old_path": "src/main.rs",
                        "new_path": "src/main.rs",
                        "old_line": null,
                        "new_line": 12
                    }
                },
                {
                    "id": 2,
                    "type": "DiscussionNote",
                    "body": "No change needed.\n\n<!-- agentty review resolution:123e4567-e89b-12d3-a456-426614174000 -->",
                    "author": {"id": 2, "name": "Bob", "username": "bob"},
                    "system": false,
                    "resolved": false,
                    "position": null
                }
            ]
        },
        {
            "id": "discussion-2",
            "individual_note": true,
            "notes": [
                {
                    "id": 3,
                    "type": "DiscussionNote",
                    "body": "Looks good overall.",
                    "author": {"id": 3, "name": "Carol", "username": "carol"},
                    "system": false,
                    "resolved": false,
                    "position": null
                }
            ]
        }
    ]"#
    .to_string()
}

pub(super) fn gitlab_current_user_json() -> String {
    serde_json::json!({"id": 2, "username": "bob"}).to_string()
}
