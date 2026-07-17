//! Session lifecycle and prompt E2E tests.
//!
//! Tests cover session creation via `a` key, opening sessions with `Enter`,
//! list navigation with `j`/`k`, deletion with confirmation, prompt input
//! basics (typing, multiline via Alt+Enter and CSI-u Shift+Enter, cancel via
//! Esc), and returning to the session list from session view.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use agentty::db::{DB_DIR, DB_FILE, Database};
use agentty::domain::agent::ReasoningLevel;
use agentty::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
};
use agentty::domain::session_message::SessionMessageKind;
use agentty::test_support;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection, Executor};
use testty::assertion;
use testty::frame::TerminalFrame;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

type E2eResult = Result<(), Box<dyn std::error::Error>>;
const LOADER_SESSION_ID: &str = "loader-session-0001";

/// Stable id for the seeded running session used by stop-turn tests.
const RUNNING_STOP_SESSION_ID: &str = "running-stop-0001";

/// Stable id for the seeded rebasing session used by message-queue tests.
const REBASING_QUEUE_SESSION_ID: &str = "rebasing-queue-0001";

/// Stable id for the seeded clarification-question session used by the
/// question-resume test.
const QUESTION_RESUME_SESSION_ID: &str = "question-resume-0001";

/// First clarification question shown when the seeded question session opens.
const FIRST_QUESTION_TEXT: &str = "Use the default target branch?";

/// Second clarification question that must be shown after resuming.
const SECOND_QUESTION_TEXT: &str = "Which tests should be added?";

/// Clarification question emitted by the delayed stub while help is open.
const RECONCILE_QUESTION_TEXT: &str = "Should I add a regression test?";

/// Draft text typed into the composer by the chat-focus toggle test.
const PROMPT_FOCUS_DRAFT_TEXT: &str = "Draft kept while reading chat";

/// Focused-review output emitted when the prompt carries both the saved
/// decision and the instruction to honor it.
const RESOLVED_DECISION_REVIEW_TEXT: &str = "Resolved session decision honored.";

/// Stable policy phrase the focused-review stub expects in the prompt.
///
/// This intentionally matches only the durable concept instead of one full
/// template sentence so harmless copy edits do not masquerade as behavior
/// regressions.
const REVIEW_DECISION_CONTEXT_PROMPT_MARKER: &str = "session chat history as decision context";

/// Accepted tradeoff saved in the transcript and expected by the review stub.
const RESOLVED_DECISION_HISTORY_TEXT: &str =
    "Understood. Retaining the println call is an accepted tradeoff for the demo output.";

/// Diagnostic emitted when the focused-review prompt omits its policy marker.
const MISSING_DECISION_CONTEXT_POLICY_TEXT: &str =
    "Focused review prompt omitted decision-context guidance.";

/// Diagnostic emitted when the focused-review prompt omits saved chat context.
const MISSING_RESOLVED_DECISION_HISTORY_TEXT: &str =
    "Focused review prompt omitted the resolved session decision.";

/// Returns every scrollbar row and the subset occupied by its thumb in the
/// session output's rightmost column.
fn session_output_scrollbar_rows(frame: &TerminalFrame) -> (Vec<u16>, Vec<u16>) {
    let scrollbar_column = frame.cols().saturating_sub(2);
    let mut scrollbar_rows = Vec::new();
    let mut thumb_rows = Vec::new();

    for row in 0..frame.rows() {
        match frame.cell_text(row, scrollbar_column) {
            "█" => {
                scrollbar_rows.push(row);
                thumb_rows.push(row);
            }
            "│" => scrollbar_rows.push(row),
            _ => {}
        }
    }

    (scrollbar_rows, thumb_rows)
}

/// Seeds one review-ready session whose transcript contains a beautified
/// provider command failure.
fn seed_session_with_beautified_agent_error(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("agent-error-0001", "claude-opus-4-8", "main", "Review")
            .with_title("Readable agent error"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "agent-error-0001",
                SessionMessageKind::AssistantAnswer,
                "\
Agent command failed with exit code 1.

stdout:
```text
system | init | cwd: /tmp/test-agentty/wt/d4ab835d
proxy warning: retrying
rate_limit_event | rate_limit_status: rejected | rate_limit_reason: out_of_credits
assistant: hi
result error: rate_limit
message: You've hit your session limit - resets 12:10am (America/Los_Angeles)
api error status: 429
request id: req_011Cbfc7AF16gbH
duration: 283ms
```
",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("agent-er"))?;

    Ok(())
}

/// Seeds configured pre-commit validation without installing its Git hook.
fn seed_missing_pre_commit_hook_project(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::write(env.workdir.join(".pre-commit-config.yaml"), "repos: []\n")?;
    run_git(&env.workdir, &["add", ".pre-commit-config.yaml"])?;
    run_git(
        &env.workdir,
        &["commit", "-m", "configure pre-commit validation"],
    )?;
    run_git(
        &env.workdir,
        &["config", "core.hooksPath", ".missing-hooks"],
    )?;

    let claude_path = env.stub_bin.join("claude");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
printf 'pending change\n' > generated.txt
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Created pending worktree change"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","result":"{\"answer\":\"Created pending worktree change\",\"questions\":[],\"summary\":null}","usage":{"input_tokens":5,"output_tokens":9}}'
"#;
    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])?;

    Ok(())
}

/// Seeds one review-ready session whose transcript contains a markdown table.
fn seed_session_with_markdown_table(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("markdown-table-0001", "claude-opus-4-8", "main", "Review")
            .with_title("Markdown table output"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "markdown-table-0001",
                SessionMessageKind::AssistantAnswer,
                "\
| Message kind | Storage |
| --- | --- |
| User prompt | Session.output |
| Assistant markdown | session_message |
",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("markdown"))?;

    Ok(())
}

/// Seeds one review-ready session whose user prompt contains markdown.
fn seed_session_with_user_markdown(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("user-markdown-0001", "claude-opus-4-8", "main", "Review")
            .with_title("User markdown prompt"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "user-markdown-0001",
                SessionMessageKind::UserPrompt,
                "\
Use **bold** and `code`.

| Input | Meaning |
| --- | --- |
| User prompt | Markdown |

```text
formatted blocks in user messages without words breaking
```

```mermaid {theme=default}
flowchart TD
    A[Start] --> B[Finish]
```
",
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                "user-markdown-0001",
                SessionMessageKind::AssistantAnswer,
                "Assistant answer after the user markdown prompt.",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("user-mar"))?;

    Ok(())
}

/// Seeds one review-ready session whose transcript contains inline markdown
/// styling adjacent to punctuation.
fn seed_session_with_inline_markdown_punctuation(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("inline-md-0001", "claude-opus-4-8", "main", "Review")
            .with_title("Inline markdown punctuation"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "inline-md-0001",
                SessionMessageKind::AssistantAnswer,
                "Use (`session_messages_from_rows`), then [`Image #1`].\n",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("inline-m"))?;

    Ok(())
}

/// Seeds one review-ready session whose transcript contains mermaid flowchart,
/// entity-relationship, and sequence fenced blocks. The flowchart includes an
/// extended shape, an `&` fan-out, and bidirectional arrows, while the sequence
/// diagram includes a skipped control block.
fn seed_session_with_mermaid_output(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("mermaid-chat-0001", "claude-opus-4-8", "main", "Review")
            .with_title("Mermaid diagram output"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "mermaid-chat-0001",
                SessionMessageKind::AssistantAnswer,
                "\
Here is the merge flow and the data model:

```mermaid
flowchart TD
    A{{User starts session}} --> B{Choose action}
    B -->|Route request| C[Send prompt] & D[Open diff view]
    C <--> E[Agent works in worktree]
    E --> F[Run checks]
    F <-- G[Report result]
    D --> G
```

```mermaid
erDiagram
    CUSTOMER ||--o{ ORDER : places
    CUSTOMER ||--|| ACCOUNT : owns
```

```mermaid
sequenceDiagram
    participant User
    participant Agentty
    participant Agent
    User->>Agentty: Start new session
    alt Agent available
    Agentty->>Agent: Send prompt
    Agent-->>Agentty: Stream result
    end
```
",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("mermaid-"))?;

    Ok(())
}

/// Seeds one review-ready session whose assistant answer begins a line with a
/// workflow-notice prefix that must remain assistant text.
fn seed_session_with_typed_marker_collision(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_id = "typed-marker-0001";
    common::seed_session(
        env,
        SessionSeed::regular(session_id, "gpt-5.5", "main", "Review")
            .with_title("Typed marker collision"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                session_id,
                SessionMessageKind::UserPrompt,
                "explain merge label output",
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                session_id,
                SessionMessageKind::AssistantAnswer,
                "Assistant output before summary.\n[Merge] this is literal assistant text.",
            )
            .await?;
        database
            .sessions()
            .update_session_summary(
                session_id,
                r#"{"turn":"Kept marker-looking output in the assistant answer.","session":"Typed transcript rows preserve render grouping."}"#,
            )
            .await
    })?;

    std::fs::create_dir_all(test_support::session_folder(
        &env.agentty_root.join("wt"),
        session_id,
    ))?;

    Ok(())
}

/// Seeds one review-ready session plus its default source branch and
/// propagates setup errors to the caller.
fn seed_review_with_resolved_decision(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "review-shortcut-0001",
                SessionMessageKind::UserPrompt,
                "Keep the println call; its output is required by the demo contract.",
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                "review-shortcut-0001",
                SessionMessageKind::AssistantAnswer,
                RESOLVED_DECISION_HISTORY_TEXT,
            )
            .await
    })?;

    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r###"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
prompt=$(cat)
case "$prompt" in
  *"{REVIEW_DECISION_CONTEXT_PROMPT_MARKER}"*)
    case "$prompt" in
      *"{RESOLVED_DECISION_HISTORY_TEXT}"*)
        answer='{RESOLVED_DECISION_REVIEW_TEXT}'
        ;;
      *)
        answer='{MISSING_RESOLVED_DECISION_HISTORY_TEXT}'
        ;;
    esac
    ;;
  *)
    answer='{MISSING_DECISION_CONTEXT_POLICY_TEXT}'
    ;;
esac
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '{{"type":"result","subtype":"success","result":"{{\\"answer\\":\\"## Review\\\\n\\\\n### Project Impact\\\\n\\\\n- %s\\\\n\\\\n### Suggestions\\\\n\\\\n- None\\",\\"questions\\":[],\\"summary\\":null}}","usage":{{"input_tokens":5,"output_tokens":9}}}}\n' "$answer"
"###,
    );
    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    seed_project_settings(
        env,
        &[
            ("DefaultReviewAgent", "claude"),
            ("DefaultReviewModel", "claude-haiku-4-5-20251001"),
        ],
    )
}

/// Seeds one review-ready session plus its default source branch and
/// propagates setup errors to the caller.
fn seed_review_ready_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("review-shortcut-0001", "gpt-5.5", "main", "Review")
            .with_title("Review-ready session shortcuts"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_diff_stats(12, 3, "review-shortcut-0001", "M")
            .await
    })?;

    run_git(&env.workdir, &["branch", "wt/review-s"])?;
    std::fs::create_dir_all(env.agentty_root.join("wt").join("review-s"))?;

    Ok(())
}

/// Seeds a review-ready transcript and delays its Git rebase long enough to
/// inspect the in-progress session output ordering.
fn seed_rebase_transcript_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "review-shortcut-0001",
                SessionMessageKind::UserPrompt,
                "keep completed transcript stable",
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                "review-shortcut-0001",
                SessionMessageKind::AssistantAnswer,
                "Completed answer before rebase summary.",
            )
            .await?;
        database
            .sessions()
            .update_session_summary(
                "review-shortcut-0001",
                r#"{"turn":"Preserved transcript ordering.","session":"Rebase keeps completed output stable."}"#,
            )
            .await
    })?;

    let pre_rebase_hook = env
        .agentty_root
        .join("wt")
        .join("review-s")
        .join(".git")
        .join("hooks")
        .join("pre-rebase");
    std::fs::write(&pre_rebase_hook, "#!/bin/sh\nsleep 5\n")?;
    #[cfg(unix)]
    std::fs::set_permissions(&pre_rebase_hook, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Seeds a review-ready worktree whose delayed successful publish keeps the
/// manual task active beyond the upstream-ref refresh observed by the feature
/// scenario.
fn seed_slow_successful_review_request_publish(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;
    let session_worktree = env.agentty_root.join("wt").join("review-s");
    run_git(&session_worktree, &["branch", "-m", "wt/review-s"])?;

    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let git_path = env.stub_bin.join("git");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "push" ]; then
  sleep 3
  exit 0
fi
exec '{}' "$@"
"#,
        real_git.display()
    );
    std::fs::write(&git_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o755))?;

    let gh_path = env.stub_bin.join("gh");
    std::fs::write(
        &gh_path,
        r#"#!/bin/sh
marker_path="${0}.created"
case "$*" in
  *"auth status"*)
    exit 0
    ;;
  *"api"*"/pulls"*)
    if [ -f "$marker_path" ]; then
      printf '%s\n' '[{"number":42}]'
    else
      printf '%s\n' '[]'
    fi
    ;;
  *"pr create"*)
    # Exceeds both 3 s scenario wait budgets plus the 5.5 s mid-flight pause.
    sleep 15
    touch "$marker_path"
    ;;
  *"pr view"*)
    printf '%s\n' '{"number":42,"title":"Review-ready session shortcuts","state":"OPEN","url":"https://github.com/agentty-xyz/agentty/pull/42","baseRefName":"main","headRefName":"wt/review-s","isDraft":false,"mergeStateStatus":"CLEAN","reviewDecision":"REVIEW_REQUIRED","mergedAt":null}'
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 1
    ;;
esac
"#,
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Seeds one published review-ready session whose latest auto-push completion
/// is persisted as transcript output.
fn seed_session_with_published_branch_push_notice(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_id = "published-push-0001";

    common::seed_session(
        env,
        SessionSeed::regular(session_id, "gpt-5.5", "main", "Review")
            .with_title("Published push notice"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_published_upstream_ref(
                session_id,
                Some("origin/wt/published-push".to_string()),
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                session_id,
                SessionMessageKind::WorkflowNotice,
                "\n[Branch Push] Auto-pushed published branch after completed turn.\n",
            )
            .await
    })?;

    // Match `session_folder()` so startup loads the seeded review session.
    let worktree_name = &session_id[..8];
    std::fs::create_dir_all(env.agentty_root.join("wt").join(worktree_name))?;

    Ok(())
}

/// Seeds one session that is already generating focused review output so
/// shortcut rendering can cover the transient `AgentReview` state.
fn seed_agent_review_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let canonical_workdir = env.workdir.canonicalize()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let database = Database::open(&db_path).await?;
        let project_id = database
            .projects()
            .upsert_project(
                &canonical_workdir.to_string_lossy(),
                Some("main".to_string()),
            )
            .await?;

        database
            .projects()
            .touch_project_last_opened(project_id)
            .await?;
        database
            .sessions()
            .insert_session(
                "agent-review-sync-0001",
                "gpt-5.5",
                "main",
                "AgentReview",
                project_id,
            )
            .await?;
        database
            .sessions()
            .update_session_title("agent-review-sync-0001", "Agent review sync shortcut")
            .await?;
        database
            .sessions()
            .update_session_diff_stats(8, 2, "agent-review-sync-0001", "S")
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("agent-re"))?;

    Ok(())
}

/// Seeds a one-level stack where both parent and child are review-ready.
fn seed_review_ready_parent_with_review_child(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("stack-parent-0001", "gpt-5.5", "main", "Review")
            .with_title("Parent stack review"),
    )?;
    common::seed_session(
        env,
        SessionSeed::stacked_draft(
            "stack-child-0001",
            "gpt-5.5",
            "wt/stack-pa",
            "Review",
            "stack-parent-0001",
        )
        .with_title("Child stack review"),
    )?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("stack-pa"))?;
    std::fs::create_dir_all(env.agentty_root.join("wt").join("stack-ch"))?;

    Ok(())
}

/// Seeds a parentless child session that still has pending post-merge stack
/// restack metadata and a real git branch requiring `git rebase --onto`.
fn seed_pending_post_merge_restack_child(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let child_worktree = env.agentty_root.join("wt").join("stack-re");
    let parent_tip = seed_child_worktree_for_onto_rebase(&env.workdir, &child_worktree)?;
    common::seed_session(
        env,
        SessionSeed::regular("stack-restack-child-0001", "gpt-5.5", "main", "Review")
            .with_title("Pending post-merge child sync"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_stack_base_commit_hash("stack-restack-child-0001", Some(parent_tip))
            .await
    })?;

    Ok(())
}

/// Seeds a pending post-merge restack with an invalid old parent tip so the
/// automatic startup sync reports its failure in the child session view.
fn seed_failing_pending_post_merge_restack_child(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let child_worktree = env.agentty_root.join("wt").join("stack-re");
    let _parent_tip = seed_child_worktree_for_onto_rebase(&env.workdir, &child_worktree)?;
    common::seed_session(
        env,
        SessionSeed::regular("stack-restack-failure-0001", "gpt-5.5", "main", "Review")
            .with_title("Blocked post-merge child sync"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_stack_base_commit_hash(
                "stack-restack-failure-0001",
                Some("missing-parent-tip".to_string()),
            )
            .await
    })?;

    Ok(())
}

/// Creates a child branch with one parent commit and one child commit so the
/// app can recover it using `git rebase --onto main <parent-tip>`.
fn seed_child_worktree_for_onto_rebase(
    main_worktree: &Path,
    child_worktree: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(parent) = child_worktree.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(main_worktree.join("base.txt"), "base\n")?;
    run_git(main_worktree, &["add", "."])?;
    run_git(main_worktree, &["commit", "-m", "base"])?;
    run_git(main_worktree, &["checkout", "-b", "parent"])?;
    std::fs::write(main_worktree.join("parent.txt"), "parent\n")?;
    run_git(main_worktree, &["add", "."])?;
    run_git(main_worktree, &["commit", "-m", "parent change"])?;
    let parent_tip = run_git_stdout(main_worktree, &["rev-parse", "HEAD"])?;
    run_git(main_worktree, &["checkout", "main"])?;
    std::fs::write(main_worktree.join("merged-parent.txt"), "merged parent\n")?;
    run_git(main_worktree, &["add", "."])?;
    run_git(main_worktree, &["commit", "-m", "merged parent"])?;
    let child_worktree_path = child_worktree.to_string_lossy().into_owned();
    run_git(
        main_worktree,
        &[
            "worktree",
            "add",
            "-b",
            "wt/stack-re",
            child_worktree_path.as_str(),
            "parent",
        ],
    )?;
    std::fs::write(child_worktree.join("child.txt"), "child\n")?;
    run_git(child_worktree, &["add", "."])?;
    run_git(child_worktree, &["commit", "-m", "child change"])?;

    Ok(parent_tip)
}

/// Seeds one review-ready session with a persisted reasoning level.
fn seed_session_with_reasoning_level(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_reasoning_level("review-shortcut-0001", ReasoningLevel::Medium)
            .await
    })?;

    Ok(())
}

/// Seeds sessions whose matching update times require creation-time ordering.
fn seed_sessions_with_matching_update_times(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("a-older", "gpt-5.5", "main", "Review")
            .with_title("Older created session"),
    )?;
    common::seed_session(
        env,
        SessionSeed::regular("z-newer", "gpt-5.5", "main", "Review")
            .with_title("Newer created session"),
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let mut connection = SqliteConnectOptions::new()
            .filename(&db_path)
            .connect()
            .await?;
        let query = sqlx::query(
            r"
UPDATE session
SET created_at = CASE id WHEN 'a-older' THEN 100 ELSE 200 END,
    updated_at = 300
WHERE id IN ('a-older', 'z-newer')
",
        );
        connection.execute(query).await?;
        connection.close().await?;

        Result::<(), Box<dyn std::error::Error>>::Ok(())
    })?;

    for session_id in ["a-older", "z-newer"] {
        std::fs::create_dir_all(test_support::session_folder(
            &env.agentty_root.join("wt"),
            session_id,
        ))?;
    }

    Ok(())
}

/// Filler repeated through the stub's non-protocol payload.
const PROTOCOL_FAILURE_PAYLOAD_FILLER: &str = "not-json-filler";

/// Marker placed at the far end of the stub's non-protocol payload.
///
/// Neither this nor the filler may reach the chat: a protocol failure must
/// report why the payload was rejected without reproducing the payload.
const PROTOCOL_FAILURE_TAIL_MARKER: &str = "TAILOFPAYLOAD";

/// Installs a Claude stub whose final payload is not protocol JSON.
///
/// The payload is far longer than the excerpt budget and carries a marker at
/// each end, so the resulting transcript notice can be checked for a bounded
/// failure message instead of a raw provider dump.
fn seed_invalid_protocol_output_project(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let payload = format!(
        "{} {PROTOCOL_FAILURE_TAIL_MARKER}",
        format!("{PROTOCOL_FAILURE_PAYLOAD_FILLER} ").repeat(40)
    );
    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' '{{"type":"result","subtype":"success","result":"{payload}"}}'
"#
    );
    std::fs::write(&claude_path, script)?;

    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])?;

    Ok(())
}

/// Adds a Gemini CLI stub that intentionally exits with failure.
///
/// Picker tests only need the executable to exist on `PATH`; using a failing
/// stub keeps accidental provider execution from looking successful.
fn seed_failing_gemini_cli_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let stub_agent_path = env.stub_bin.join("gemini");
    std::fs::write(&stub_agent_path, "#!/bin/sh\nexit 1\n")?;

    #[cfg(unix)]
    std::fs::set_permissions(&stub_agent_path, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Adds an Antigravity CLI stub that intentionally exits with failure.
///
/// Picker tests only need the executable to exist on `PATH`; using a failing
/// stub keeps accidental provider execution from looking successful.
fn seed_failing_antigravity_cli_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let stub_agent_path = env.stub_bin.join("agy");
    std::fs::write(&stub_agent_path, "#!/bin/sh\nexit 1\n")?;

    #[cfg(unix)]
    std::fs::set_permissions(&stub_agent_path, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Adds a Codex CLI stub that intentionally exits with failure.
///
/// Picker tests only need the executable to exist on `PATH`; using a failing
/// stub keeps accidental provider execution from looking successful.
fn seed_failing_codex_cli_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let stub_agent_path = env.stub_bin.join("codex");
    std::fs::write(&stub_agent_path, "#!/bin/sh\nexit 1\n")?;

    #[cfg(unix)]
    std::fs::set_permissions(&stub_agent_path, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Adds Gemini and Antigravity CLI stubs so both Google-backed providers
/// appear in stable `/model` picker positions.
fn seed_model_picker_cli_stubs(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_failing_gemini_cli_stub(env)?;
    seed_failing_antigravity_cli_stub(env)?;

    Ok(())
}

/// Adds all agent CLI stubs so provider picker tests have stable ordering.
fn seed_all_model_picker_cli_stubs(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_model_picker_cli_stubs(env)?;
    seed_failing_codex_cli_stub(env)?;

    Ok(())
}

/// Seeds one review-ready session with a focused review already persisted as
/// if Agentty had been restarted after review generation completed.
fn seed_review_ready_session_with_persisted_focused_review(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_focused_review(
                "review-shortcut-0001",
                Some("42".to_string()),
                Some(
                    "## Review\n\n### Project Impact\n\n- Persisted focused review \
                     finding.\n\n### Suggestions\n\n- None."
                        .to_string(),
                ),
            )
            .await
    })?;

    Ok(())
}

/// Seeds two review-ready sessions with distinct persisted focused reviews so
/// switching away and back can verify cache-backed output restoration.
fn seed_sessions_with_persisted_focused_reviews(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_persisted_focused_review(env)?;
    common::seed_session(
        env,
        SessionSeed::regular("second-review-0001", "gpt-5.5", "main", "Review")
            .with_title("Second persisted review"),
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_focused_review(
                "second-review-0001",
                Some("84".to_string()),
                Some(
                    "## Review\n\n### Project Impact\n\n- Second persisted review finding.\n\n### \
                     Suggestions\n\n- None."
                        .to_string(),
                ),
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("second-r"))?;

    Ok(())
}

/// Seeds one running session so `Ctrl+c` can exercise the turn-stop path
/// without needing a live agent backend.
fn seed_running_stop_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(RUNNING_STOP_SESSION_ID, "gpt-5.5", "main", "InProgress")
            .with_title("Running session stop"),
    )?;

    // Match `session_folder()` so the seeded row has the worktree path the
    // runtime expects for this session id.
    let worktree_name = &RUNNING_STOP_SESSION_ID[..8];
    std::fs::create_dir_all(env.agentty_root.join("wt").join(worktree_name))?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_reasoning_level(RUNNING_STOP_SESSION_ID, ReasoningLevel::High)
            .await
    })?;

    Ok(())
}

/// Seeds one rebasing session so message queueing can be exercised without a
/// live git operation or agent backend.
fn seed_rebasing_queue_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(REBASING_QUEUE_SESSION_ID, "gpt-5.5", "main", "Rebasing")
            .with_title("Rebasing message queue"),
    )?;

    let worktree_name = &REBASING_QUEUE_SESSION_ID[..8];
    std::fs::create_dir_all(env.agentty_root.join("wt").join(worktree_name))?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                REBASING_QUEUE_SESSION_ID,
                SessionMessageKind::WorkflowNotice,
                "\n[Commit] No changes to commit.\n",
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                REBASING_QUEUE_SESSION_ID,
                SessionMessageKind::WorkflowNotice,
                "\n[Sync Assist] Resolving existing conflicts.\n",
            )
            .await
    })?;

    Ok(())
}

/// Seeds one `Question`-status session with two option questions so the
/// question-resume flow can run without a live agent backend.
fn seed_question_resume_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(QUESTION_RESUME_SESSION_ID, "gpt-5.5", "main", "Question")
            .with_title("Question resume session"),
    )?;

    std::fs::create_dir_all(test_support::session_folder(
        &env.agentty_root.join("wt"),
        QUESTION_RESUME_SESSION_ID,
    ))?;

    let questions_json = format!(
        r#"[{{"options":["Yes","No"],"text":"{FIRST_QUESTION_TEXT}"}},{{"options":["Unit","Integration"],"text":"{SECOND_QUESTION_TEXT}"}}]"#
    );
    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;

        database
            .sessions()
            .update_session_questions(QUESTION_RESUME_SESSION_ID, &questions_json)
            .await
    })?;

    Ok(())
}

/// Persists project-scoped settings so feature tests route model selections
/// to deterministic backends even when additional real CLIs exist on `PATH`.
fn seed_project_settings(
    env: &BuilderEnv,
    settings: &[(&str, &str)],
) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let database = Database::open(&db_path).await?;
        let canonical_workdir = env.workdir.canonicalize()?;
        let project_id = database
            .projects()
            .upsert_project(
                &canonical_workdir.to_string_lossy(),
                Some("main".to_string()),
            )
            .await?;
        database
            .projects()
            .touch_project_last_opened(project_id)
            .await?;
        drop(database);

        let mut connection = SqliteConnectOptions::new()
            .filename(&db_path)
            .connect()
            .await?;
        for (setting_name, setting_value) in settings {
            let query = sqlx::query(
                r"
INSERT INTO project_setting (project_id, name, value)
VALUES (?, ?, ?)
ON CONFLICT(project_id, name) DO UPDATE SET value = excluded.value
",
            )
            .bind(project_id)
            .bind(setting_name)
            .bind(setting_value);
            connection.execute(query).await?;
        }
        connection.close().await?;

        Result::<(), Box<dyn std::error::Error>>::Ok(())
    })?;

    Ok(())
}

/// Installs a Claude stub that emits one structured clarification question
/// after a delay, giving the scenario time to cover the active view with help.
fn install_delayed_question_claude_stub(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
sleep 1
printf '%s\n' '{{"type":"system","subtype":"init"}}'
sleep 2
printf '%s\n' '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Need one clarification."}}]}}}}'
sleep 1
printf '%s\n' '{{"type":"result","subtype":"success","result":"{{\"answer\":\"Need one clarification.\",\"questions\":[{{\"text\":\"{RECONCILE_QUESTION_TEXT}\",\"options\":[\"Yes\",\"No\"]}}],\"summary\":null}}","usage":{{"input_tokens":5,"output_tokens":9}}}}'
"#
    );

    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Answer emitted before the queued session sync is allowed to start.
const QUEUED_SYNC_TURN_ANSWER: &str = "Running turn completed before sync";

/// Installs a delayed Claude turn so the scenario can queue sync while the
/// worker is still active, then observe the answer before the rebase result.
fn install_delayed_sync_claude_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
sleep 10
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{QUEUED_SYNC_TURN_ANSWER}"}}]}}}}'
printf '%s\n' '{{"type":"result","subtype":"success","result":"{{\"answer\":\"{QUEUED_SYNC_TURN_ANSWER}\",\"questions\":[],\"summary\":null}}","usage":{{"input_tokens":5,"output_tokens":9}}}}'
"#
    );

    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Installs a Claude stub that reports whether Agentty passed web-capable
/// Claude Code tools to the non-interactive session launch.
fn install_web_tool_reporting_claude_stub(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
case " $* " in
  *WebSearch*WebFetch*|*WebFetch*WebSearch*) answer='Claude web tools enabled' ;;
  *) answer='Claude web tools missing' ;;
esac
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"{\\"answer\\":\\"%s\\",\\"questions\\":[],\\"summary\\":null}"}]}}\n' "$answer"
printf '{"type":"result","subtype":"success","result":"{\"answer\":\"%s\",\"questions\":[],\"summary\":null}","usage":{"input_tokens":5,"output_tokens":9}}\n' "$answer"
"#;

    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Answer text emitted by the bare-layout success stub once its turn completes.
const BARE_LAYOUT_ANSWER_TEXT: &str = "Bare worktree turn completed";

/// Rewrites the test project into a bare-repository worktree layout and
/// installs a Claude stub that completes one turn successfully.
///
/// Creates a bare shared repository as a sibling of the project directory,
/// seeds an initial `main` commit without a main working checkout, adds a
/// sibling `main` worktree, and adds `env.workdir` as the `feature` linked
/// worktree that Agentty opens as the project. This reproduces the
/// container-of-worktrees layout where the shared repository is bare and there
/// is no main working checkout for the dirty-status snapshot, which previously
/// failed the first turn with `this operation must be run in a work tree`.
fn seed_bare_repo_worktree_project(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let container = env
        .workdir
        .parent()
        .ok_or_else(|| std::io::Error::other("workdir has no parent container"))?
        .to_path_buf();
    let bare_dir = container.join("project.bare");
    std::fs::create_dir_all(&bare_dir)?;

    run_git(&bare_dir, &["init", "--bare", "."])?;
    run_git(&bare_dir, &["config", "user.email", "test@test.com"])?;
    run_git(&bare_dir, &["config", "user.name", "Test"])?;

    let empty_tree = run_git_stdout(&bare_dir, &["hash-object", "-w", "-t", "tree", "/dev/null"])?;
    let init_commit = run_git_stdout(&bare_dir, &["commit-tree", &empty_tree, "-m", "init"])?;
    run_git(&bare_dir, &["update-ref", "refs/heads/main", &init_commit])?;
    run_git(&bare_dir, &["symbolic-ref", "HEAD", "refs/heads/main"])?;

    // Sibling main worktree so the shared repo genuinely holds per-branch
    // worktrees as siblings, then the project worktree Agentty opens.
    let main_worktree = container.join("main").to_string_lossy().into_owned();
    run_git(
        &bare_dir,
        &["worktree", "add", main_worktree.as_str(), "main"],
    )?;
    let project_path = env.workdir.to_string_lossy().into_owned();
    run_git(
        &bare_dir,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            project_path.as_str(),
            "main",
        ],
    )?;

    install_bare_layout_success_claude_stub(env)
}

/// Installs a Claude stub that completes one turn with a fixed successful
/// answer so the bare-layout scenario can drive a turn without a live backend.
fn install_bare_layout_success_claude_stub(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{BARE_LAYOUT_ANSWER_TEXT}"}}]}}}}\n'
printf '{{"type":"result","subtype":"success","result":"{{\"answer\":\"{BARE_LAYOUT_ANSWER_TEXT}\",\"questions\":[],\"summary\":null}}","usage":{{"input_tokens":5,"output_tokens":9}}}}\n'
"#
    );

    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Seeds one review-ready session with a linked review request.
fn seed_review_ready_session_with_review_request(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;
    seed_github_review_request_stub(env)?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        let review_request = ReviewRequest {
            last_refreshed_at: 55,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/review-s".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("Checks passing".to_string()),
                target_branch: "main".to_string(),
                title: "Review-ready session shortcuts".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        };

        database
            .reviews()
            .update_session_review_request("review-shortcut-0001", Some(review_request.clone()))
            .await
    })?;

    Ok(())
}

/// Seeds the review session folder with a real git diff and GitHub remote so
/// diff mode and background review-comment sync can run without live services.
fn seed_review_worktree_with_diff(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let session_worktree = env.agentty_root.join("wt").join("review-s");
    std::fs::create_dir_all(session_worktree.join("src"))?;
    run_git(&session_worktree, &["init", "-b", "main"])?;
    run_git(
        &session_worktree,
        &["config", "user.email", "test@test.com"],
    )?;
    run_git(&session_worktree, &["config", "user.name", "Test"])?;
    std::fs::write(session_worktree.join("src/main.rs"), "fn main() {\n}\n")?;
    run_git(&session_worktree, &["add", "."])?;
    run_git(&session_worktree, &["commit", "-m", "init"])?;
    run_git(
        &session_worktree,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/agentty-xyz/agentty.git",
        ],
    )?;
    std::fs::write(
        session_worktree.join("src/main.rs"),
        "fn main() {\n    println!(\"review\");\n}\n",
    )?;

    Ok(())
}

/// Runs one git command in `working_directory`, returning an error with stderr
/// detail when git fails.
fn run_git(working_directory: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(working_directory)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }

    Ok(())
}

/// Runs one git command in `working_directory` and returns trimmed stdout.
fn run_git_stdout(
    working_directory: &Path,
    args: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(working_directory)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Installs a deterministic `gh` stub that returns review-request status.
fn seed_github_review_request_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let gh_path = env.stub_bin.join("gh");
    std::fs::write(
        &gh_path,
        r#"#!/bin/sh
case "$*" in
  *"auth status"*)
    exit 0
    ;;
  *"api --hostname github.com graphql"*)
    printf '%s\n' '{"data":{"repository":{"pullRequest":{"comments":{"nodes":[]},"reviewThreads":{"nodes":[{"diffSide":"RIGHT","isOutdated":false,"isResolved":false,"line":2,"path":"src/main.rs","startLine":1,"subjectType":"LINE","comments":{"nodes":[{"author":{"login":"alice"},"body":"Please explain why this review output is needed."}]}},{"diffSide":"RIGHT","isOutdated":false,"isResolved":false,"line":null,"path":"src/main.rs","startLine":null,"subjectType":"FILE","comments":{"nodes":[{"author":{"login":"bob"},"body":"Please review the whole file."}]}}]}}}}}'
    ;;
  *"pr view"*)
    printf '%s\n' '{"number":42,"title":"Review-ready session shortcuts","state":"OPEN","url":"https://github.com/agentty-xyz/agentty/pull/42","baseRefName":"main","headRefName":"wt/review-s","isDraft":false,"mergeStateStatus":"CLEAN","reviewDecision":"REVIEW_REQUIRED","mergedAt":null}'
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 1
    ;;
esac
"#,
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Seeds a merged GitHub review response and delays runtime worktree removal
/// so the feature scenario can prove terminal rendering stays responsive.
fn seed_slow_merged_review_request_status(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_review_request(env)?;

    let gh_path = env.stub_bin.join("gh");
    std::fs::write(
        &gh_path,
        r#"#!/bin/sh
case "$*" in
  *"auth status"*)
    exit 0
    ;;
  *"pr view"*)
    printf '%s\n' '{"number":42,"title":"Review-ready session shortcuts","state":"MERGED","url":"https://github.com/agentty-xyz/agentty/pull/42","baseRefName":"main","headRefName":"wt/review-s","isDraft":false,"mergeStateStatus":"CLEAN","reviewDecision":"APPROVED","mergedAt":"2026-01-01T00:00:00Z"}'
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 1
    ;;
esac
"#,
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o755))?;

    install_delayed_worktree_remove_stub(env, 4)
}

/// Installs a git wrapper that delays worktree removal while forwarding all
/// other commands to the real executable.
fn install_delayed_worktree_remove_stub(
    env: &BuilderEnv,
    delay_seconds: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let real_git = real_git.to_string_lossy().replace('\'', "'\"'\"'");
    let git_path = env.stub_bin.join("git");
    std::fs::write(
        &git_path,
        format!(
            r#"#!/bin/sh
if [ "$1" = "worktree" ] && [ "$2" = "remove" ]; then
  exec sleep {delay_seconds}
fi
exec '{real_git}' "$@"
"#
        ),
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Seeds one draft-session lookup target file into the temporary project.
fn seed_draft_at_lookup_project(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::write(
        env.workdir.join("draft_lookup_target.txt"),
        "draft lookup target\n",
    )?;

    Ok(())
}

/// Seeds one done session whose merged commit hash can drive the continuation
/// draft flow.
fn seed_done_session_for_continuation(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let merged_commit_hash = "704de31d0f4b5a1234567890abcdef1234567890";
    common::seed_session(
        env,
        SessionSeed::regular("done-continue-0001", "gpt-5.5", "main", "Done")
            .with_title("Continue terminal session"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_merged_commit_hash(
                "done-continue-0001",
                Some(merged_commit_hash.to_string()),
            )
            .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    Ok(())
}

/// Seeds one in-progress session so the session view can show the active
/// Tachyonfx loader without launching a live agent backend.
fn seed_active_loader_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(LOADER_SESSION_ID, "gpt-5.5", "main", "InProgress")
            .with_title("Loader session"),
    )?;

    std::fs::create_dir_all(test_support::session_folder(
        &env.agentty_root.join("wt"),
        LOADER_SESSION_ID,
    ))?;

    Ok(())
}

/// Seeds one review-ready session with enough output to overflow a compact
/// transcript viewport.
fn seed_session_with_scrollable_output(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    const SESSION_ID: &str = "scroll-output-0001";

    common::seed_session(
        env,
        SessionSeed::regular(SESSION_ID, "gpt-5.5", "main", "Review")
            .with_title("Scrollable output"),
    )?;

    let output = (0..60)
        .map(|line_index| format!("Transcript line {line_index:02} {}", "x".repeat(60)))
        .collect::<Vec<_>>()
        .join("\n");
    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(SESSION_ID, SessionMessageKind::AssistantAnswer, &output)
            .await
    })?;

    std::fs::create_dir_all(test_support::session_folder(
        &env.agentty_root.join("wt"),
        SESSION_ID,
    ))?;

    Ok(())
}

/// Verify that the Sessions tab hides empty groups and guides session creation.
///
/// Starts Agentty with no sessions, then creates an active session and verifies
/// that only the populated group is visible.
#[test]
fn session_list_empty_state() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_empty")
        .with_git()
        .zola(
            "Empty session state",
            "See how Agentty guides session creation and hides empty groups.",
            40,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(2000)
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("sessions_tab", "Sessions tab with no sessions")
                    .compose(&common::create_session_with_prompt_and_return_to_list(
                        "Keep only populated groups",
                    ))
                    .viewing_pause_ms(2000)
                    .capture_labeled("active_group", "Only the populated group is visible")
            },
            |frame, report| {
                let empty_frame = common::frame_from_capture(&report.captures[0]);
                let empty_full = Region::full(empty_frame.cols(), empty_frame.rows());
                assertion::assert_text_in_region(
                    &empty_frame,
                    "No sessions. Press 'a' to start one.",
                    &empty_full,
                );
                assertion::assert_not_visible(&empty_frame, "Merge queue");
                assertion::assert_not_visible(&empty_frame, "Active sessions");
                assertion::assert_not_visible(&empty_frame, "Archive");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Active sessions", &full);
                assertion::assert_not_visible(frame, "Merge queue");
                assertion::assert_not_visible(frame, "Archive");
                assertion::assert_not_visible(frame, "No sessions");
            },
        )?;

    Ok(())
}

/// Verify that provider output which fails protocol validation surfaces a
/// bounded failure notice instead of dumping the raw payload into the chat.
#[test]
fn test_session_invalid_protocol_output_is_bounded() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_invalid_protocol_output_is_bounded")
        .with_git()
        .setup(seed_invalid_protocol_output_project)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Return invalid protocol output")
                    .wait_for_text("Return invalid protocol output", 3000)
                    .press_key("Enter")
                    .wait_for_text("embedded_json_candidate", 60000)
                    .capture_labeled(
                        "invalid_protocol_output",
                        "Invalid provider output reports a failure without raw payload",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "embedded_json_candidate", &full);
                assertion::assert_not_visible(frame, PROTOCOL_FAILURE_TAIL_MARKER);
                assertion::assert_not_visible(frame, PROTOCOL_FAILURE_PAYLOAD_FILLER);
            },
        )?;

    Ok(())
}

/// Verify that configured validation without an installed hook warns before
/// session selection and after a successful normal commit.
#[test]
fn test_session_pre_commit_hook_warning() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_pre_commit_hook_warning")
        .with_git()
        .setup(seed_missing_pre_commit_hook_project)
        .zola(
            "Pre-commit hook warning",
            "Warn about missing pre-commit hooks without blocking session creation.",
            32,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Pre-commit hook warning", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "missing_hook_warning",
                        "Session creation warns about the missing pre-commit hook",
                    )
                    .press_key("Enter")
                    .wait_for_text("Select session type", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Create one worktree change")
                    .wait_for_text("Create one worktree change", 3000)
                    .press_key("Enter")
                    .wait_for_text("Created pending worktree change", 30000)
                    .wait_for_text("[Commit Warning]", 30000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "commit_warning",
                        "Auto-commit succeeds and prints the missing-hook warning",
                    )
            },
            |frame, report| {
                let warning_frame = common::frame_from_capture(&report.captures[0]);
                let warning_full = Region::full(warning_frame.cols(), warning_frame.rows());
                assertion::assert_text_in_region(
                    &warning_frame,
                    "Pre-commit hook warning",
                    &warning_full,
                );
                assertion::assert_text_in_region(
                    &warning_frame,
                    "not installed or executable.",
                    &warning_full,
                );
                assertion::assert_text_in_region(&warning_frame, "Install it", &warning_full);
                assertion::assert_text_in_region(
                    &warning_frame,
                    "become an error in a future release.",
                    &warning_full,
                );
                assertion::assert_text_in_region(&warning_frame, "prek install", &warning_full);
                assertion::assert_text_in_region(
                    &warning_frame,
                    "pre-commit install",
                    &warning_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "[Commit Warning]", &full);
                assertion::assert_text_in_region(frame, "Created pending worktree change", &full);
                assertion::assert_text_in_region(frame, "prek install", &full);
                assertion::assert_text_in_region(frame, "pre-commit install", &full);
            },
        )?;

    Ok(())
}

/// Verify that the Sessions tab model column includes the effective reasoning
/// level next to the model name.
#[test]
fn session_list_model_reasoning_level() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_list_model_reasoning_level")
        .with_git()
        .setup(seed_session_with_reasoning_level)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("gpt-5.5 [medium]", 5000)
                    .capture_labeled(
                        "model_reasoning",
                        "Session row model column with reasoning level",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gpt-5.5 [medium]", &full);
            },
        )?;

    Ok(())
}

/// Verify matching update times fall back to newest creation time first.
#[test]
fn session_list_matching_update_times_use_creation_order() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_list_creation_order")
        .with_git()
        .setup(seed_sessions_with_matching_update_times)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Newer created session", 5000)
                    .capture_labeled(
                        "creation_order",
                        "Matching update times ordered by creation time",
                    )
            },
            |frame, _report| {
                let newer_row = frame
                    .find_text("Newer created session")
                    .first()
                    .expect("missing newer session row")
                    .rect
                    .row;
                let older_row = frame
                    .find_text("Older created session")
                    .first()
                    .expect("missing older session row")
                    .rect
                    .row;

                assert!(newer_row < older_row);
            },
        )?;

    Ok(())
}

/// Verify that changing the project reasoning default does not relabel a turn
/// that is already in progress.
#[test]
fn existing_session_keeps_persisted_reasoning_label() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_persisted_reasoning_label")
        .with_git()
        .setup(seed_running_stop_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("gpt-5.5 [high]", 5000)
                    .compose(&common::switch_to_tab("Inbox"))
                    .compose(&common::switch_to_tab("Issues"))
                    .compose(&common::switch_to_tab("Settings"))
                    .press_key("j")
                    .press_key("Enter")
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("xhigh", 5000)
                    .press_key("BackTab")
                    .press_key("BackTab")
                    .press_key("BackTab")
                    .wait_for_text("gpt-5.5 [high]", 5000)
                    .capture_labeled(
                        "active_reasoning",
                        "Existing session retains persisted reasoning",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gpt-5.5 [high]", &full);
            },
        )?;

    Ok(())
}

/// Verify that the session chat header shows the selected agent immediately
/// before the model name.
#[test]
fn session_chat_header_agent_model() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_chat_header_agent_model")
        .with_git()
        .setup(seed_review_ready_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Agent: codex  Model: gpt-5.5", 5000)
                    .capture_labeled(
                        "agent_model_header",
                        "Session chat header showing agent before model",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Agent: codex  Model: gpt-5.5", &full);
            },
        )?;

    Ok(())
}

/// Verify that a selected session row remains readable under Dark Horizon.
///
/// The source renderer test covers the exact selected-cell background color;
/// this PTY-level test keeps the user-visible selection path covered without
/// depending on terminal background-color capture fidelity. The scenario
/// finishes by wrapping the theme back to `Agentty Default` so the persisted
/// theme is identical before the PTY proof run and the VHS replay.
#[test]
fn session_list_selected_row_remains_readable_under_dark_horizon() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_selection_highlight")
        .with_git()
        .setup(seed_review_ready_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Inbox"))
                    .compose(&common::switch_to_tab("Issues"))
                    .compose(&common::switch_to_tab("Settings"))
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_text("Agentty Green", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_text("Dark Horizon", 5000)
                    .compose(&common::switch_to_tab("Projects"))
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Review-ready session shortcuts", 5000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .wait_for_text("Enter: open", 3000)
                    .capture_labeled(
                        "selected_row_highlight",
                        "Selected session row on the dedicated selection surface",
                    )
                    .compose(&common::switch_to_tab("Inbox"))
                    .compose(&common::switch_to_tab("Issues"))
                    .compose(&common::switch_to_tab("Settings"))
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_text("Agentty Default", 5000)
            },
            |_frame, report| {
                assert_eq!(
                    report.captures.len(),
                    1,
                    "Expected 1 capture (selected session row under Dark Horizon)"
                );

                // Dark Horizon `surface_selection` and `surface` RGB values
                // from the `ThemePalette` definitions in `ui/style.rs`.
                let sessions_frame = common::frame_from_capture(&report.captures[0]);
                let selected_title = sessions_frame
                    .find_text("Review-ready session shortcuts")
                    .into_iter()
                    .next()
                    .expect("expected selected session title to be visible");
                let selected_row_region =
                    Region::new(0, selected_title.rect.row, sessions_frame.cols(), 1);
                sessions_frame
                    .find_text_in_region("gpt-5.5", &selected_row_region)
                    .into_iter()
                    .next()
                    .expect("expected selected session model to be visible");
            },
        )?;

    Ok(())
}

/// Verify that session output renders beautified provider command failures
/// with readable JSONL event summaries instead of raw event payloads.
#[test]
fn session_view_agent_error_output() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_view_agent_error_output")
        .setup(seed_session_with_beautified_agent_error)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("result error: rate_limit", 5000)
                    .capture_labeled(
                        "agent_error",
                        "Session view with beautified agent command error output",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "proxy warning: retrying", &full);
                assertion::assert_text_in_region(frame, "result error: rate_limit", &full);
                assertion::assert_text_in_region(
                    frame,
                    "message: You've hit your session limit",
                    &full,
                );
                assertion::assert_text_in_region(frame, "request id: req_011Cbfc7AF16gbH", &full);
            },
        )?;

    Ok(())
}

/// Verify that session output renders markdown pipe tables as aligned terminal
/// tables instead of showing the raw separator row.
#[test]
fn session_view_markdown_table_output() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_markdown_table_output")
        .setup(seed_session_with_markdown_table)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Session.output", 5000)
                    .capture_labeled(
                        "markdown_table",
                        "Session view with a rendered markdown table",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Message kind", &full);
                assertion::assert_text_in_region(frame, "Assistant markdown", &full);
                assertion::assert_text_in_region(frame, "Session.output", &full);
                assertion::assert_not_visible(frame, "| --- | --- |");
            },
        )?;

    Ok(())
}

/// Verify that markdown in user prompts renders like markdown output while
/// retaining the visible prompt marker.
#[test]
fn session_view_user_prompt_markdown_output() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_user_prompt_markdown_output")
        .setup(seed_session_with_user_markdown)
        .with_terminal_size(80, 32)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("User prompt", 5000)
                    .capture_labeled(
                        "user_prompt_markdown",
                        "Session view with rendered markdown in a user prompt",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Use bold and code.", &full);
                assertion::assert_text_in_region(frame, "User prompt", &full);
                assertion::assert_text_in_region(frame, "Markdown", &full);
                assertion::assert_text_in_region(frame, "without words breaking", &full);
                assertion::assert_text_in_region(frame, "Start", &full);
                assertion::assert_text_in_region(frame, "Finish", &full);
                assertion::assert_text_in_region(frame, "▼", &full);
                assertion::assert_not_visible(frame, "**bold**");
                assertion::assert_not_visible(frame, "`code`");
                assertion::assert_not_visible(frame, "| --- | --- |");
                assertion::assert_not_visible(frame, "flowchart TD");
            },
        )?;

    Ok(())
}

/// Verify that reopening a cached session after a theme switch repaints
/// transcript messages with the newly active theme.
#[test]
fn session_view_theme_switch_repaints_cached_messages() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_theme_switch_repaints_cached_messages")
        .setup(seed_session_with_user_markdown)
        .with_terminal_size(80, 32)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Use bold and code.", 5000)
                    .press_key("q")
                    .wait_for_text("User markdown prompt", 5000)
                    .compose(&common::switch_to_tab("Inbox"))
                    .compose(&common::switch_to_tab("Issues"))
                    .compose(&common::switch_to_tab("Settings"))
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_text("Agentty Green", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_text("Dark Horizon", 5000)
                    .compose(&common::switch_to_tab("Projects"))
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("User markdown prompt", 5000)
                    .press_key("Enter")
                    .wait_for_text("Use bold and code.", 5000)
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Use bold and code.", &full);
            },
        )?;

    Ok(())
}

/// Verify that inline markdown styling adjacent to punctuation does not add
/// spaces inside brackets or parentheses.
#[test]
fn session_view_inline_markdown_punctuation_spacing() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_inline_markdown_punctuation_spacing")
        .setup(seed_session_with_inline_markdown_punctuation)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("[Image #1]", 5000)
                    .capture_labeled(
                        "inline_markdown_punctuation",
                        "Session view with inline markdown punctuation spacing",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Use (session_messages_from_rows), then [Image #1].",
                    &full,
                );
                assertion::assert_not_visible(frame, "( session_messages_from_rows )");
                assertion::assert_not_visible(frame, "[ Image #1 ]");
            },
        )?;

    Ok(())
}

/// Verify that typed assistant output is not reclassified as a workflow notice
/// just because it starts a line with a notice-looking prefix.
#[test]
fn session_view_preserves_typed_assistant_marker_lines() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_typed_assistant_marker_lines")
        .setup(seed_session_with_typed_marker_collision)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("[Merge] this is literal assistant text.", 5000)
                    .capture_labeled(
                        "typed_assistant_marker",
                        "Typed assistant line that looks like a workflow notice",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let view_text = frame.text_in_region(&full);
                let assistant_notice_index = view_text
                    .find("[Merge] this is literal assistant text.")
                    .expect("assistant marker line should render");
                let summary_index = view_text
                    .find("Change Summary")
                    .expect("summary should render");

                assertion::assert_text_in_region(
                    frame,
                    "[Merge] this is literal assistant text.",
                    &full,
                );
                assert!(assistant_notice_index < summary_index);
            },
        )?;

    Ok(())
}

/// Verify the per-press LIFO `Ctrl+c` flow for queued chat messages: pressing
/// `Enter` while a session is `InProgress` opens the chat composer and queues
/// the typed message inline beneath the running turn; with two messages
/// queued, the first `Ctrl+c` pops only the most recently queued entry while
/// the older entry and the running turn remain (`Ctrl+c: stop` still
/// rendered); a follow-up `Ctrl+c` pops the remaining queued entry; and a
/// final `Ctrl+c` with an empty queue cancels the running turn and returns
/// the session to review-ready controls.
#[test]
fn session_queue_chat_messages_during_in_progress_turn() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_queue_chat_messages")
        .with_git()
        .setup(seed_running_stop_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("first queued")
                    .wait_for_text("first queued", 3000)
                    .press_key("Enter")
                    .wait_for_text("queued ›", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("second queued")
                    .wait_for_text("second queued", 3000)
                    .press_key("Enter")
                    .wait_for_text("second queued", 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "two_queued_messages_visible",
                        "Two queued chat messages rendered inline beneath the running turn",
                    )
                    .press_key("ctrl+c")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "first_ctrl_c_pops_last_queued",
                        "First Ctrl+c pops the most recently queued chat message and leaves the \
                         running turn",
                    )
                    .press_key("ctrl+c")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "second_ctrl_c_pops_remaining_queued",
                        "Second Ctrl+c pops the last remaining queued chat message and leaves the \
                         running turn",
                    )
                    .press_key("ctrl+c")
                    .wait_for_text("Enter: reply", 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "third_ctrl_c_cancels_turn",
                        "Third Ctrl+c cancels the running turn and returns the session to review",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(&queued_frame, "queued ›", &queued_full);
                assertion::assert_text_in_region(&queued_frame, "first queued", &queued_full);
                assertion::assert_text_in_region(&queued_frame, "second queued", &queued_full);

                let after_first_frame = common::frame_from_capture(&report.captures[1]);
                let after_first_full =
                    Region::full(after_first_frame.cols(), after_first_frame.rows());
                assertion::assert_text_in_region(
                    &after_first_frame,
                    "Ctrl+c: stop",
                    &after_first_full,
                );
                assertion::assert_text_in_region(&after_first_frame, "queued ›", &after_first_full);
                assertion::assert_text_in_region(
                    &after_first_frame,
                    "first queued",
                    &after_first_full,
                );
                assertion::assert_not_visible(&after_first_frame, "second queued");

                let after_second_frame = common::frame_from_capture(&report.captures[2]);
                let after_second_full =
                    Region::full(after_second_frame.cols(), after_second_frame.rows());
                assertion::assert_text_in_region(
                    &after_second_frame,
                    "Ctrl+c: stop",
                    &after_second_full,
                );
                assertion::assert_not_visible(&after_second_frame, "queued ›");
                assertion::assert_not_visible(&after_second_frame, "first queued");
                assertion::assert_not_visible(&after_second_frame, "second queued");

                let cleared_full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: reply", &cleared_full);
                assertion::assert_not_visible(frame, "Ctrl+c: stop");
                assertion::assert_not_visible(frame, "queued ›");
            },
        )?;

    Ok(())
}

/// Verify `Enter` opens the composer while a session is `Rebasing` and the
/// submitted follow-up renders below existing workflow notices as queued work
/// without replacing the active rebase status.
#[test]
fn session_queue_chat_message_during_rebase() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_queue_chat_message_during_rebase")
        .with_git()
        .setup(seed_rebasing_queue_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Rebasing...", 5000)
                    .wait_for_text("Enter: queue message", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("follow up after sync")
                    .wait_for_text("follow up after sync", 3000)
                    .press_key("Enter")
                    .wait_for_text("queued ›", 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "message_queued_during_rebase",
                        "Follow-up message queued inline while session sync remains active",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Rebasing...", &full);
                assertion::assert_text_in_region(frame, "[Commit] No changes to commit.", &full);
                assertion::assert_text_in_region(
                    frame,
                    "[Sync Assist] Resolving existing conflicts.",
                    &full,
                );
                assertion::assert_text_in_region(frame, "queued ›", &full);
                assertion::assert_text_in_region(frame, "follow up after sync", &full);
                let commit_row = frame
                    .find_text("[Commit] No changes to commit.")
                    .first()
                    .expect("missing commit notice")
                    .rect
                    .row;
                let sync_assist_row = frame
                    .find_text("[Sync Assist] Resolving existing conflicts.")
                    .first()
                    .expect("missing sync-assist notice")
                    .rect
                    .row;
                let queued_message_row = frame
                    .find_text("queued › follow up after sync")
                    .first()
                    .expect("missing queued follow-up")
                    .rect
                    .row;

                assert_eq!(sync_assist_row, commit_row + 2);
                assert_eq!(queued_message_row, sync_assist_row + 2);
            },
        )?;

    Ok(())
}

/// Verify running sessions queue `r` sync without interrupting the active
/// turn, then rebase before returning to review.
#[test]
fn session_running_turn_shows_sync_shortcut() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_running_sync_shortcut")
        .with_git()
        .setup(install_delayed_sync_claude_stub)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Keep the active turn running")
                    .wait_for_text("Keep the active turn running", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .wait_for_text("r: sync", 5000)
                    .press_key("r")
                    .wait_for_text("will rebase after the current turn finishes", 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "running_sync_queued",
                        "Running session view after queueing sync",
                    )
                    .wait_for_text(QUEUED_SYNC_TURN_ANSWER, 30000)
                    .wait_for_text("[Sync] Successfully synced", 10000)
                    .wait_for_text("Enter: reply", 5000)
                    .capture_labeled(
                        "running_sync_completed",
                        "Running turn completes before queued sync",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(
                    &queued_frame,
                    "will rebase after the current turn finishes",
                    &queued_full,
                );
                assertion::assert_text_in_region(&queued_frame, "Ctrl+c: stop", &queued_full);
                let active_turn_row = queued_frame
                    .find_text("Keep the active turn running")
                    .first()
                    .expect("missing active turn prompt")
                    .rect
                    .row;
                let queued_sync_row = queued_frame
                    .find_text("will rebase after the current turn finishes")
                    .first()
                    .expect("missing queued sync notice")
                    .rect
                    .row;
                assert!(active_turn_row < queued_sync_row);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, QUEUED_SYNC_TURN_ANSWER, &full);
                assertion::assert_text_in_region(frame, "[Sync] Successfully synced", &full);
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "[Stopped]");

                let answer_row = frame
                    .find_text(QUEUED_SYNC_TURN_ANSWER)
                    .first()
                    .expect("missing completed turn answer")
                    .rect
                    .row;
                let sync_row = frame
                    .find_text("[Sync] Successfully synced")
                    .first()
                    .expect("missing queued sync result")
                    .rect
                    .row;
                assert!(answer_row < sync_row);
            },
        )?;

    Ok(())
}

/// Verify a manual rebase keeps the completed answer ahead of its summary
/// while only the workflow status tail animates.
#[test]
fn session_rebase_keeps_completed_transcript_before_summary() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_rebase_transcript_stability")
        .with_git()
        .setup(seed_rebase_transcript_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Completed answer before rebase summary.", 5000)
                    .wait_for_text("Change Summary", 5000)
                    .press_key("r")
                    .wait_for_text("Rebasing...", 5000)
                    .capture_labeled(
                        "rebase_transcript_stable",
                        "Completed transcript remains stable during rebase",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Completed answer before rebase summary.",
                    &full,
                );
                assertion::assert_text_in_region(frame, "Change Summary", &full);
                assertion::assert_text_in_region(frame, "Rebasing...", &full);

                let answer_row = frame
                    .find_text("Completed answer before rebase summary.")
                    .first()
                    .expect("missing completed answer")
                    .rect
                    .row;
                let summary_row = frame
                    .find_text("Change Summary")
                    .first()
                    .expect("missing change summary")
                    .rect
                    .row;
                assert!(answer_row < summary_row);
            },
        )?;

    Ok(())
}

/// Verify that `Ctrl+c` in a running session stops only the current turn and
/// returns the session to review-ready controls.
#[test]
fn session_stop_turn_returns_to_review() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_stop_turn_returns_to_review")
        .with_git()
        .setup(seed_running_stop_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .viewing_pause_ms(1200)
                    .capture_labeled(
                        "running_session",
                        "Running session before stopping the turn",
                    )
                    .press_key("ctrl+c")
                    .wait_for_text("Enter: reply", 5000)
                    .viewing_pause_ms(1200)
                    .capture_labeled(
                        "review_after_stop",
                        "Session view after Ctrl+c stops only the active turn",
                    )
            },
            |frame, report| {
                let running_frame = common::frame_from_capture(&report.captures[0]);
                let running_full = Region::full(running_frame.cols(), running_frame.rows());
                assertion::assert_text_in_region(&running_frame, "Ctrl+c: stop", &running_full);
                assertion::assert_not_visible(&running_frame, "o: open");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "Ctrl+c: stop");
            },
        )?;

    Ok(())
}

/// Verify that answering one clarification question, leaving question mode
/// with `q`, and reopening the session resumes at the next unanswered
/// question instead of restarting from the first.
#[test]
fn session_question_resume_after_leaving_to_list() -> E2eResult {
    // Arrange
    FeatureTest::new("session_question_resume")
        .with_git()
        .setup(seed_question_resume_session)
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Question resume session", 5000)
                    .press_key("Enter")
                    .wait_for_text("Question 1/2", 5000)
                    .wait_for_text("Tab: focus", 5000)
                    .capture_labeled(
                        "answer_focused",
                        "Question mode starts with the answer input focused",
                    )
                    .press_key("Tab")
                    .wait_for_text("j/k: scroll", 5000)
                    .press_key("ctrl+c")
                    .capture_labeled(
                        "chat_focused",
                        "Chat focus keeps Ctrl+C from ending the question turn",
                    )
                    .press_key("Tab")
                    .wait_for_text("Enter: send", 5000)
                    .press_key("Enter")
                    .wait_for_text("Question 2/2", 5000)
                    .press_key("q")
                    .wait_for_text("new session", 5000)
                    .press_key("Enter")
                    .wait_for_text("Question 2/2", 5000)
                    .capture_labeled(
                        "resumed_second_question",
                        "Reopening the session resumes at the second question",
                    )
            },
            |frame, report| {
                // Assert
                let answer_focused_frame = common::frame_from_capture(&report.captures[0]);
                let answer_focused_full =
                    Region::full(answer_focused_frame.cols(), answer_focused_frame.rows());
                assertion::assert_text_in_region(
                    &answer_focused_frame,
                    "Tab: focus | Enter: send | q: sessions | Esc/Ctrl+C: end turn",
                    &answer_focused_full,
                );

                let chat_focused_frame = common::frame_from_capture(&report.captures[1]);
                let chat_focused_full =
                    Region::full(chat_focused_frame.cols(), chat_focused_frame.rows());
                assertion::assert_text_in_region(
                    &chat_focused_frame,
                    "Tab: focus | j/k: scroll | d: diff | q: sessions",
                    &chat_focused_full,
                );
                assertion::assert_not_visible(&chat_focused_frame, "Ctrl+C");
                assertion::assert_text_in_region(
                    &chat_focused_frame,
                    "Question 1/2",
                    &chat_focused_full,
                );
                assertion::assert_text_in_region(
                    &chat_focused_frame,
                    FIRST_QUESTION_TEXT,
                    &chat_focused_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Question 2/2", &full);
                assertion::assert_text_in_region(frame, SECOND_QUESTION_TEXT, &full);
                assertion::assert_not_visible(frame, FIRST_QUESTION_TEXT);
            },
        )?;

    Ok(())
}

/// Persists the `Sessions` tab as the startup tab.
///
/// `Tab` is the composer focus toggle under test, so the scenario cannot spend
/// a `Tab` press on tab navigation: the seeded startup tab keeps every `Tab` in
/// the scenario meaningful, and keeps the PTY proof and the VHS replay (which
/// share this database) starting from the same tab.
fn seed_sessions_startup_tab(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        // Opening the database applies the migrations that create `setting`.
        common::open_database(env).await?;

        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let mut connection = SqliteConnectOptions::new()
            .filename(&db_path)
            .connect()
            .await?;
        let query = sqlx::query(
            r"
INSERT INTO setting (name, value) VALUES ('ActiveTab', 'Sessions')
ON CONFLICT(name) DO UPDATE SET value = excluded.value
",
        );
        connection.execute(query).await?;
        connection.close().await?;

        Result::<(), Box<dyn std::error::Error>>::Ok(())
    })?;

    Ok(())
}

/// Verify that `Tab` moves focus between the prompt composer and the chat
/// transcript, and that keys pressed while the transcript is focused scroll it
/// instead of editing the typed draft.
#[test]
fn session_prompt_chat_focus_toggle() -> E2eResult {
    // Arrange
    FeatureTest::new("session_prompt_chat_focus")
        .with_git()
        .setup(seed_sessions_startup_tab)
        .zola(
            "Read the chat while composing",
            "Press Tab to move focus from the message composer to the chat transcript and back \
             without losing the typed draft.",
            50,
        )
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("new session", 5000)
                    .viewing_pause_ms(1500)
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text(PROMPT_FOCUS_DRAFT_TEXT)
                    .wait_for_text(PROMPT_FOCUS_DRAFT_TEXT, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("composer_focused", "The composer accepts the typed draft")
                    .press_key("Tab")
                    .wait_for_text("j/k: scroll", 5000)
                    .press_key("j")
                    .press_key("k")
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "chat_focused",
                        "Tab focuses the chat transcript for scrolling",
                    )
                    .press_key("Tab")
                    .wait_for_text("Enter: send", 5000)
                    .viewing_pause_ms(1500)
            },
            |frame, report| {
                // Assert
                let chat_focused_frame = common::frame_from_capture(&report.captures[1]);
                let chat_focused_full =
                    Region::full(chat_focused_frame.cols(), chat_focused_frame.rows());
                assertion::assert_text_in_region(
                    &chat_focused_frame,
                    "Tab: focus",
                    &chat_focused_full,
                );
                assertion::assert_text_in_region(
                    &chat_focused_frame,
                    "j/k: scroll",
                    &chat_focused_full,
                );
                // Chat focus exposes no cancel shortcut, so the composer draft
                // cannot be lost while scrolling.
                assertion::assert_not_visible(&chat_focused_frame, "Ctrl+C");
                // Scroll keys pressed in chat focus must not reach the draft.
                assertion::assert_text_in_region(
                    &chat_focused_frame,
                    PROMPT_FOCUS_DRAFT_TEXT,
                    &chat_focused_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: send", &full);
                assertion::assert_text_in_region(frame, PROMPT_FOCUS_DRAFT_TEXT, &full);
                assertion::assert_not_visible(frame, "j/k: scroll");

                // This session's worktree name comes from a fresh UUID, so the
                // footer reads differently on every run. Pin the harness
                // redaction against the footer agentty actually paints: without
                // a match, the frame hash moves every run and the committed GIF
                // is re-recorded for a UI that never changed.
                let redacted = common::session_worktree_redaction().apply(&frame.all_text());
                let placeholder = format!(
                    "{}{}",
                    common::SESSION_WORKTREE_PREFIX,
                    common::SESSION_WORKTREE_PLACEHOLDER,
                );

                assert!(
                    redacted.contains(&placeholder),
                    "the session worktree hash must be redacted out of the footer, \
                     got:\n{redacted}",
                );

                // The footer must paint the worktree path home-collapsed. An
                // absolute temp path is truncated differently per platform
                // (macOS temp roots are far longer than Linux's `/tmp`), which
                // would make the committed freshness hash unreproducible on CI.
                assert!(
                    redacted.contains("~/.agentty/wt/"),
                    "the footer must paint the worktree path home-collapsed so frames hash \
                     identically on every platform, got:\n{redacted}",
                );
            },
        )?;

    Ok(())
}

/// Verify that Claude sessions are launched with web-capable Claude Code
/// tools so current-information prompts do not require an interactive grant.
#[test]
fn claude_session_launch_allows_web_tools() -> E2eResult {
    // Arrange
    let _test_guard = common::acquire_e2e_test_lock();
    let temp = tempfile::TempDir::new()?;
    let env = BuilderEnv::new(temp.path())?;
    env.init_git()?;
    install_web_tool_reporting_claude_stub(&env)?;

    let scenario = Scenario::new("claude_session_web_tools")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .press_key("a")
        .press_key("Enter")
        .wait_for_stable_frame(300, 5000)
        .write_text("Use the web for current package docs")
        .wait_for_text("Use the web for current package docs", 3000)
        .press_key("Enter")
        .wait_for_text("Claude web tools enabled", 30000);

    // Act
    let frame = scenario.run(env.builder())?;

    // Assert
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "Claude web tools enabled", &full);
    assertion::assert_not_visible(&frame, "Claude web tools missing");

    Ok(())
}

/// Verify that Agentty can create a session and drive one turn to review when
/// the project is a linked worktree of a bare shared repository.
///
/// The bare layout has no main working checkout, so the pre-fix dirty-status
/// snapshot failed the first turn with `this operation must be run in a work
/// tree`. This test drives a full successful turn and asserts the session
/// reaches the review-ready state instead of surfacing that error.
#[test]
fn bare_repo_worktree_layout_supports_session_turn() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("bare_repo_worktree_layout")
        .setup(seed_bare_repo_worktree_project)
        .zola(
            "Bare repository worktree layout",
            "Run sessions from a project that is a linked worktree of a bare shared repository.",
            45,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Summarize the bare worktree layout")
                    .wait_for_text("Summarize the bare worktree layout", 3000)
                    .press_key("Enter")
                    .wait_for_text(BARE_LAYOUT_ANSWER_TEXT, 30000)
                    .wait_for_text("Enter: reply", 5000)
                    .capture_labeled(
                        "bare_layout_review",
                        "Session reaches review after one turn in a bare worktree layout",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, BARE_LAYOUT_ANSWER_TEXT, &full);
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "this operation must be run in a work tree");
            },
        )?;

    Ok(())
}

/// Verify that an already-open session view enters question mode after a
/// structured question arrives while the help overlay hides the live
/// transition.
#[test]
fn session_question_reconcile_after_help_overlay() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_question_reconcile")
        .with_git()
        .setup(install_delayed_question_claude_stub)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Need clarification")
                    .wait_for_text("Need clarification", 3000)
                    .press_key("Enter")
                    .wait_for_text("q: back", 5000)
                    .press_key("?")
                    .wait_for_text("Keybindings", 5000)
                    .viewing_pause_ms(4500)
                    .capture_labeled(
                        "help_during_completion",
                        "Help overlay remains open while the turn completes",
                    )
                    .press_key("Escape")
                    .wait_for_text("Question 1/1", 30000)
                    .capture_labeled(
                        "reconciled_question",
                        "Question panel appears without reopening the session",
                    )
            },
            |frame, report| {
                let help_frame = common::frame_from_capture(&report.captures[0]);
                let help_full = Region::full(help_frame.cols(), help_frame.rows());
                assertion::assert_text_in_region(&help_frame, "Keybindings", &help_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Question 1/1", &full);
                assertion::assert_text_in_region(frame, RECONCILE_QUESTION_TEXT, &full);
                assertion::assert_text_in_region(frame, "Yes", &full);
                assertion::assert_text_in_region(frame, "No", &full);
                assertion::assert_not_visible(frame, "Keybindings");
            },
        )?;

    Ok(())
}

/// Verify that pressing `a` on the Sessions tab opens the creation selector,
/// and choosing the regular option opens prompt mode with the submit footer.
#[test]
fn session_creation_opens_prompt_mode() -> E2eResult {
    // Arrange
    FeatureTest::new("session_creation")
        .with_git()
        .zola(
            "Session creation",
            "Choose a regular, draft, or stacked session from the session creation selector.",
            30,
        )
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(1500)
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .capture_labeled("creation_selector", "Session creation selector")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("prompt_mode", "Prompt mode after choosing Regular")
            },
            |frame, report| {
                // Assert
                let selector_frame = common::frame_from_capture(&report.captures[0]);
                let selector_full = Region::full(selector_frame.cols(), selector_frame.rows());
                assertion::assert_text_in_region(&selector_frame, "Regular", &selector_full);
                assertion::assert_text_in_region(&selector_frame, "Draft", &selector_full);
                assertion::assert_text_in_region(&selector_frame, "Stacked", &selector_full);
                assertion::assert_text_in_region(
                    &selector_frame,
                    "Select parent first",
                    &selector_full,
                );
                assertion::assert_text_in_region(&selector_frame, "Enter: select", &selector_full);
                assertion::assert_text_in_region(&selector_frame, "q: close", &selector_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Tab: focus | Enter: send", &full);
            },
        )?;

    Ok(())
}

/// Verify that prompt image paste reports unavailable clipboard backends
/// inline.
#[test]
fn prompt_image_paste_unavailable_shows_inline_error() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("prompt_image_paste_unavailable")
        .with_git()
        .env("AGENTTY_DISABLE_CLIPBOARD", "1")
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("ctrl+v")
                    .wait_for_text("Paste Image Error", 5000)
                    .capture_labeled(
                        "paste_error",
                        "Prompt mode showing unavailable clipboard paste error",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Paste Image Error", &full);
                assertion::assert_text_in_region(frame, "Clipboard is unavailable", &full);
                assertion::assert_not_visible(frame, "[Image #1]");
            },
        )?;

    Ok(())
}

/// Verify that choosing Stacked creates a startable draft under the selected
/// parent and renders the one-level stack with a tree connection in the
/// session list.
#[test]
fn stacked_session_creation() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_session_creation")
        .with_git()
        .setup(seed_review_ready_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Review-ready session shortcuts", 5000)
                    .viewing_pause_ms(1200)
                    .press_key("a")
                    .wait_for_text("Stacked", 5000)
                    .press_key("Down")
                    .press_key("Down")
                    .wait_for_text("[Preview] Stack on selected", 5000)
                    .capture_labeled("stacked_selector", "Stacked creation selector")
                    .press_key("Enter")
                    .wait_for_text("Enter: stage draft", 5000)
                    .capture_labeled("stacked_draft_view", "Stacked draft action footer")
                    .write_text("Stacked draft")
                    .press_key("Enter")
                    .wait_for_text("Draft Session", 5000)
                    .capture_labeled(
                        "stacked_draft_ready",
                        "Stacked draft staged with start action available",
                    )
                    .viewing_pause_ms(1200)
                    .press_key("q")
                    .wait_for_text("Stacked draft", 5000)
                    .capture_labeled("stacked_list", "Stacked draft connected in session list")
            },
            |frame, report| {
                let selector_frame = common::frame_from_capture(&report.captures[0]);
                let selector_full = Region::full(selector_frame.cols(), selector_frame.rows());
                assertion::assert_text_in_region(&selector_frame, "Stacked", &selector_full);
                assertion::assert_text_in_region(
                    &selector_frame,
                    "[Preview] Stack on selected",
                    &selector_full,
                );

                let draft_view_frame = common::frame_from_capture(&report.captures[1]);
                assertion::assert_not_visible(&draft_view_frame, "s: start");
                assertion::assert_not_visible(&draft_view_frame, "m: add to merge queue");
                assertion::assert_not_visible(&draft_view_frame, "r: sync");

                let ready_frame = common::frame_from_capture(&report.captures[2]);
                let ready_full = Region::full(ready_frame.cols(), ready_frame.rows());
                assertion::assert_text_in_region(&ready_frame, "s: start", &ready_full);
                assertion::assert_not_visible(&ready_frame, "m: add to merge queue");
                assertion::assert_not_visible(&ready_frame, "r: sync");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Review-ready ses", &full);
                assertion::assert_text_in_region(frame, "└ [XS] Stacked draft", &full);
            },
        )?;

    Ok(())
}

/// Verify that a review-ready parent can still open the reply composer, sync
/// the stack, and queue merge after its stacked child has also reached review,
/// while slash commands remain hidden until the child is terminal or unlinked.
#[test]
fn stacked_parent_merge_remains_available_with_review_child() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_parent_merge_remains_available_with_review_child")
        .with_git()
        .setup(seed_review_ready_parent_with_review_child)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Parent stack review", 5000)
                    .press_key("Up")
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .capture_labeled(
                        "parent_review",
                        "Parent review session with reply and sync available",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Parent stack review", &full);
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "/: commands menu");
                assertion::assert_text_in_region(frame, "m: add to merge queue", &full);
                assertion::assert_text_in_region(frame, "r: sync", &full);
            },
        )?;

    Ok(())
}

/// Verify that startup recovery requeues a pending post-merge stacked child
/// restack and completes the deterministic sync in the child session view.
#[test]
fn stacked_pending_post_merge_restack_recovers_on_startup() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_pending_post_merge_restack_recovers_on_startup")
        .with_git()
        .setup(seed_pending_post_merge_restack_child)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Pending post-merge child sync", 5000)
                    .press_key("Enter")
                    .wait_for_text("Successfully synced", 10000)
                    .capture_labeled(
                        "pending_restack_recovered",
                        "Pending post-merge stacked child sync recovered after startup",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Pending post-merge child sync", &full);
                assertion::assert_text_in_region(frame, "Successfully synced", &full);
                assertion::assert_not_visible(frame, "[Sync Error]");
            },
        )?;

    Ok(())
}

/// Verify that an automatic post-merge child sync failure remains visible in
/// the affected child session after startup.
#[test]
fn stacked_pending_post_merge_restack_failure_is_visible() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_pending_post_merge_restack_failure_is_visible")
        .with_git()
        .setup(seed_failing_pending_post_merge_restack_child)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Blocked post-merge child sync", 5000)
                    .press_key("Enter")
                    .wait_for_text("[Sync Error]", 10000)
                    .capture_labeled(
                        "pending_restack_failure",
                        "Pending post-merge stacked child sync failure after startup",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Blocked post-merge child sync", &full);
                assertion::assert_text_in_region(frame, "[Sync Error]", &full);
                assertion::assert_text_in_region(frame, "Failed to sync", &full);
            },
        )?;

    Ok(())
}

/// Verify that a stacked draft can keep collecting staged prompts while its
/// parent is still running, but the start shortcut stays hidden until the
/// parent returns to review.
#[test]
fn stacked_session_start_waits_for_parent_review() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_session_start_waits_for_parent_review")
        .with_git()
        .setup(seed_running_stop_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Running session stop", 5000)
                    .press_key("a")
                    .wait_for_text("Stacked", 5000)
                    .press_key("Down")
                    .press_key("Down")
                    .wait_for_text("[Preview] Stack on selected", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: stage draft", 5000)
                    .write_text("Waiting child draft")
                    .press_key("Enter")
                    .wait_for_text("Draft Session", 5000)
                    .capture_labeled(
                        "stacked_draft_waiting_parent",
                        "Stacked draft staged while parent is still running",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: add draft", &full);
                assertion::assert_not_visible(frame, "s: start");
                assertion::assert_not_visible(frame, "m: add to merge queue");
                assertion::assert_not_visible(frame, "r: sync");
            },
        )?;

    Ok(())
}

/// Verify that the prompt `/model` picker exposes the current Gemini Flash
/// model when the Gemini CLI is locally available.
#[test]
fn gemini_model_picker_includes_current_flash() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("gemini_model_picker_includes_current_flash")
        .with_git()
        .setup(seed_failing_gemini_cli_stub)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("/")
                    .write_text("model")
                    .wait_for_text("Slash Command", 3000)
                    .press_key("Enter")
                    .wait_for_text("/model Agent", 3000)
                    .press_key("Enter")
                    .wait_for_text("gemini-3.5-flash", 3000)
                    .capture_labeled(
                        "gemini_model_picker",
                        "Gemini model picker includes current Flash",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gemini-3.5-flash", &full);
            },
        )?;

    Ok(())
}

/// Verify that the prompt `/model` picker exposes the current Codex models in
/// the expected order.
#[test]
fn codex_model_picker_lists_current_models() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("codex_model_picker_lists_current_models")
        .with_git()
        .setup(seed_all_model_picker_cli_stubs)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("/")
                    .write_text("model")
                    .wait_for_text("Slash Command", 3000)
                    .press_key("Enter")
                    .wait_for_text("/model Agent", 3000)
                    .press_key("Down")
                    .press_key("Down")
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("gpt-5.6-sol", 3000)
                    .capture_labeled(
                        "codex_model_picker",
                        "Codex model picker lists current models",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gpt-5.6-sol", &full);
                assertion::assert_text_in_region(frame, "gpt-5.6-terra", &full);
                assertion::assert_text_in_region(frame, "gpt-5.6-luna", &full);
                assertion::assert_text_in_region(frame, "gpt-5.5", &full);
                assertion::assert_text_in_region(frame, "gpt-5.3-codex-spark", &full);
            },
        )?;

    Ok(())
}

/// Verify that the prompt `/model` picker exposes Gemini model choices for
/// Antigravity when `agy` is locally available.
#[test]
fn antigravity_model_picker_includes_gemini_models() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("antigravity_model_picker_includes_gemini_models")
        .with_git()
        .setup(seed_model_picker_cli_stubs)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("/")
                    .write_text("model")
                    .wait_for_text("Slash Command", 3000)
                    .press_key("Enter")
                    .wait_for_text("/model Agent", 3000)
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("gemini-3.5-flash", 3000)
                    .capture_labeled(
                        "antigravity_model_picker",
                        "Antigravity model picker includes Gemini models",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gemini-3.1-pro-preview", &full);
                assertion::assert_text_in_region(frame, "gemini-3.5-flash", &full);
            },
        )?;

    Ok(())
}

/// Verify that choosing Draft in the creation selector opens draft-session
/// staging with explicit draft guidance before any message is staged.
#[test]
fn draft_session_creation_opens_staging_mode() -> E2eResult {
    // Arrange
    FeatureTest::new("draft_session_creation")
        .with_git()
        .zola(
            "Draft session creation",
            "Create a draft session that clearly starts in local staging mode before the bundle \
             runs.",
            31,
        )
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(1500)
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("Draft Session", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "draft_session_prompt_mode",
                        "Draft-session prompt mode immediately after creation",
                    )
            },
            |frame, _report| {
                // Assert
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Draft Session", &full);
                assertion::assert_text_in_region(frame, "No draft messages staged yet.", &full);
                assertion::assert_text_in_region(frame, "Tab: focus | Enter: stage draft", &full);
                assertion::assert_text_in_region(frame, "Ctrl+V/Alt+V", &full);
            },
        )?;

    Ok(())
}

/// Verify that pasted-image shortcuts work directly from a draft session view
/// by opening the draft composer and routing through prompt image paste.
#[test]
fn draft_session_view_paste_image_opens_composer() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("draft_session_view_paste_image")
        .with_git()
        .env("AGENTTY_DISABLE_CLIPBOARD", "1")
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("Enter: stage draft", 5000)
                    .write_text("Draft with image")
                    .press_key("Enter")
                    .wait_for_text("Enter: add draft", 5000)
                    .capture_labeled(
                        "draft_view",
                        "Draft-session view showing direct image paste action",
                    )
                    .press_key("ctrl+v")
                    .wait_for_text("Paste Image Error", 5000)
                    .capture_labeled(
                        "draft_paste_error",
                        "Draft composer opened from view-mode image paste shortcut",
                    )
            },
            |frame, report| {
                let draft_view_frame = common::frame_from_capture(&report.captures[0]);
                let draft_view_full =
                    Region::full(draft_view_frame.cols(), draft_view_frame.rows());
                assertion::assert_text_in_region(
                    &draft_view_frame,
                    "Ctrl+V/Alt+V: paste image",
                    &draft_view_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Paste Image Error", &full);
                assertion::assert_text_in_region(frame, "Clipboard is unavailable", &full);
                assertion::assert_text_in_region(frame, "Enter: stage draft", &full);
                assertion::assert_not_visible(frame, "[Image #1]");
            },
        )?;

    Ok(())
}

/// Verify that pressing `Esc` in an empty prompt for a new non-draft
/// session deletes it and returns to the empty Sessions list.
#[test]
fn session_prompt_cancel_returns_to_empty_list() -> E2eResult {
    // Arrange
    FeatureTest::new("prompt_cancel")
        .with_git()
        .zola(
            "Prompt cancel",
            "Cancel prompt input with Esc to return to the session view.",
            120,
        )
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("prompt_open", "Prompt mode opened")
                    .press_key("Esc")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("back_to_list", "Sessions list after cancel")
            },
            |frame, report| {
                // Assert
                let prompt_frame = common::frame_from_capture(&report.captures[0]);
                let prompt_full = Region::full(prompt_frame.cols(), prompt_frame.rows());
                assertion::assert_text_in_region(
                    &prompt_frame,
                    "Tab: focus | Enter: send",
                    &prompt_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "No sessions", &full);
            },
        )?;

    Ok(())
}

/// Verify that draft-session prompt mode can open `@` file lookup suggestions
/// before the deferred worktree exists.
#[test]
fn draft_session_at_lookup() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("draft_session_at_lookup")
        .with_git()
        .setup(seed_draft_at_lookup_project)
        .zola(
            "Draft session @ lookup",
            "Browse project files with `@` before a draft session materializes its worktree.",
            121,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(1500)
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("Enter: stage draft", 3000)
                    .viewing_pause_ms(1000)
                    .write_text("@draft_lookup")
                    .wait_for_text("draft_lookup_target.txt", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "draft_at_lookup",
                        "Draft-session prompt mode with an active @ lookup",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "draft_lookup_target.txt", &full);
                assertion::assert_text_in_region(frame, "Enter: stage draft", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing `Enter` on a session opens the session view and
/// pressing `q` returns to the session list.
#[test]
fn session_open_and_return_to_list() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_open")
        .with_git()
        .zola(
            "Session open and return",
            "Open a session with Enter and return to the list with q.",
            42,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::create_session_and_return_to_list())
                    .viewing_pause_ms(1500)
                    .compose(&common::open_selected_session_view())
                    .viewing_pause_ms(2000)
                    .capture_labeled("session_view", "Session view after Enter")
                    .compose(&common::return_to_session_list())
                    .viewing_pause_ms(2000)
                    .capture_labeled("back_to_list", "Sessions list after q")
            },
            |frame, report| {
                let session_view_frame = common::frame_from_capture(&report.captures[0]);
                let view_full = Region::full(session_view_frame.cols(), session_view_frame.rows());
                assertion::assert_text_in_region(&session_view_frame, "q: back", &view_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "test", &full);
            },
        )?;

    Ok(())
}

/// Verify that active session output uses the Tachyonfx loader glyph instead
/// of dot-based working copy.
#[test]
fn session_active_loader_uses_tachyonfx_glyph() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_active_loader")
        .setup(seed_active_loader_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_active_loader",
                        "Active session view with Tachyonfx loader",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "▌▌▌ Working...", &full);
            },
        )?;

    Ok(())
}

/// Verify that overflowing session output shows a scrollbar in the panel's
/// rightmost column.
#[test]
fn session_output_scrollbar_is_visible() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_output_scrollbar")
        .with_terminal_size(80, 20)
        .setup(seed_session_with_scrollable_output)
        .zola(
            "Session output scrollbar",
            "Track your position while scrolling through long session transcripts.",
            43,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("g")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_output_scrollbar_top",
                        "Scrollbar thumb at the top of long session output",
                    )
                    .write_text("G")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_output_scrollbar_bottom",
                        "Scrollbar thumb at the bottom of long session output",
                    )
            },
            |_frame, report| {
                assert_eq!(report.captures.len(), 2);

                let top_frame = common::frame_from_capture(&report.captures[0]);
                let bottom_frame = common::frame_from_capture(&report.captures[1]);
                let (top_scrollbar_rows, top_thumb_rows) =
                    session_output_scrollbar_rows(&top_frame);
                let (bottom_scrollbar_rows, bottom_thumb_rows) =
                    session_output_scrollbar_rows(&bottom_frame);
                let scrollbar_padding_column = top_frame.cols().saturating_sub(3);

                assert!(top_scrollbar_rows.len() > top_thumb_rows.len());
                assert!(bottom_scrollbar_rows.len() > bottom_thumb_rows.len());
                assert!(
                    top_scrollbar_rows
                        .iter()
                        .all(|row| { top_frame.cell_text(*row, scrollbar_padding_column) == " " })
                );
                assert!(
                    bottom_scrollbar_rows.iter().all(|row| {
                        bottom_frame.cell_text(*row, scrollbar_padding_column) == " "
                    })
                );
                assert_eq!(top_thumb_rows.first(), top_scrollbar_rows.first());
                assert_eq!(bottom_thumb_rows.last(), bottom_scrollbar_rows.last());
                assert!(
                    top_thumb_rows.last() < bottom_thumb_rows.first(),
                    "expected the scrollbar thumb to move from top to bottom"
                );
            },
        )?;

    Ok(())
}

/// Verify that pressing `c` in a terminal session opens a confirmation and,
/// after acceptance, stages the continuation message before focusing an empty
/// draft composer.
#[test]
fn terminal_session_continue_opens_seeded_prompt() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("terminal_session_continue")
        .with_git()
        .setup(seed_done_session_for_continuation)
        .zola(
            "Continue terminal session",
            "Confirm continuation from a done session and stage a merged-commit context message \
             before focusing an empty draft composer.",
            45,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("q: back", 5000)
                    .press_key("c")
                    .wait_for_text("Confirm Continue", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "continue_confirmation",
                        "Continuation confirmation for the selected done session",
                    )
                    .press_key("y")
                    .wait_for_stable_frame(500, 15000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "terminal_session_continue",
                        "Continuation draft composer with the staged merged-commit context",
                    )
            },
            |frame, report| {
                let confirmation_frame = common::frame_from_capture(&report.captures[0]);
                let confirmation_full =
                    Region::full(confirmation_frame.cols(), confirmation_frame.rows());
                assertion::assert_text_in_region(
                    &confirmation_frame,
                    "Confirm Continue",
                    &confirmation_full,
                );
                assertion::assert_text_in_region(
                    &confirmation_frame,
                    "Create a new draft sess...",
                    &confirmation_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: stage draft", &full);
                assertion::assert_text_in_region(frame, "Use 704de31d0f4b5a12", &full);
                assertion::assert_text_in_region(frame, "704de31d0f4b5a12", &full);
                assertion::assert_text_in_region(frame, "commit as an initial context", &full);
                assertion::assert_text_in_region(frame, "Type your message", &full);
            },
        )?;

    Ok(())
}

/// Verify that completed published-branch auto-push feedback is rendered as a
/// transcript message rather than as a transient status line.
#[test]
fn published_branch_push_notice_renders_as_transcript_message() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("published_branch_push_notice")
        .with_git()
        .setup(seed_session_with_published_branch_push_notice)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("[Branch Push]", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "published_branch_push_notice",
                        "Published branch auto-push completion rendered as a transcript message",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let view_text = frame.text_in_region(&full);

                assertion::assert_text_in_region(frame, "[Branch Push]", &full);
                assert_eq!(
                    view_text
                        .matches("Auto-pushed published branch after completed turn.")
                        .count(),
                    1
                );
            },
        )?;

    Ok(())
}

/// Verify that pressing `p` in a review-ready session opens the review-request
/// publish popup.
#[test]
fn review_request_publish_shortcut_opens_publish_popup() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_publish_shortcut")
        .with_git()
        .setup(seed_review_ready_session)
        .zola(
            "Review request publish shortcut",
            "Open the review-request publish popup directly from session view with `p`.",
            42,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("p")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_request_publish_popup",
                        "Review-request publish popup after pressing p",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Publish Review Request", &full);
                assertion::assert_text_in_region(frame, "Enter: publish review request", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Leave blank to push as `wt/review-s`",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Verify that confirming review-request publish returns to an interactive
/// session chat while the push runs in the background.
#[test]
fn review_request_publish_runs_in_background() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_background_publish")
        .with_git()
        .zola(
            "Background review-request publish",
            "Publish a review request in the background and receive its link in session chat.",
            41,
        )
        .setup(seed_slow_successful_review_request_publish)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("p")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("Enter")
                    .wait_for_text("Publishing review request...", 3000)
                    .capture_labeled(
                        "background_publish_started",
                        "Review-request publish progress shown inline in session chat",
                    )
                    .press_key("?")
                    .wait_for_text("Keybindings", 3000)
                    .capture_labeled(
                        "background_publish_help",
                        "Session help remains available while publishing",
                    )
                    .press_key("q")
                    .viewing_pause_ms(5500)
                    .capture_labeled(
                        "background_publish_waiting_for_url",
                        "Review-request publish progress remains until its URL is ready",
                    )
                    .wait_for_text("[Review Request] Created PR", 20000)
                    .capture_labeled(
                        "background_publish_finished",
                        "Review-request link recorded in session transcript history",
                    )
            },
            |frame, report| {
                let loading_frame = common::frame_from_capture(&report.captures[0]);
                let loading_full = Region::full(loading_frame.cols(), loading_frame.rows());
                assertion::assert_text_in_region(
                    &loading_frame,
                    "Publishing review request...",
                    &loading_full,
                );
                assertion::assert_text_in_region(&loading_frame, "q: back", &loading_full);

                let help_frame = common::frame_from_capture(&report.captures[1]);
                let help_full = Region::full(help_frame.cols(), help_frame.rows());
                assertion::assert_text_in_region(&help_frame, "Keybindings", &help_full);

                let waiting_frame = common::frame_from_capture(&report.captures[2]);
                let waiting_full = Region::full(waiting_frame.cols(), waiting_frame.rows());
                assertion::assert_text_in_region(
                    &waiting_frame,
                    "Publishing review request...",
                    &waiting_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42",
                    &full,
                );
                assertion::assert_text_in_region(frame, "q: back", &full);
            },
        )?;

    Ok(())
}

/// Verify that review-ready sessions no longer expose a manual review-request
/// sync shortcut because linked review requests refresh in the background.
#[test]
fn review_request_sync_runs_in_background() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_background_sync")
        .with_git()
        .setup(seed_review_ready_session_with_review_request)
        .zola(
            "Background review-request sync",
            "Review sessions track linked pull requests in the background instead of exposing a \
             manual sync shortcut.",
            43,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("?")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_request_background_sync",
                        "Review session help overlay without a manual sync shortcut",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let view_text = frame.text_in_region(&full);

                assertion::assert_text_in_region(frame, "p: Create or refresh", &full);
                assert!(
                    !view_text.contains("s: Sync"),
                    "manual sync help action should be absent"
                );
            },
        )?;

    Ok(())
}

/// Verify that externally merged status updates reach `Done` without waiting
/// for slow worktree cleanup and the terminal remains navigable.
#[test]
fn test_merged_review_request_cleanup_responsive() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("merged_review_request_cleanup_responsive")
        .with_git()
        .setup(seed_slow_merged_review_request_status)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Done", 2500)
                    .capture_labeled(
                        "merged_status",
                        "Merged session reaches Done before worktree cleanup finishes",
                    )
                    .press_key("Tab")
                    .wait_for_text("Review Requests", 1000)
                    .capture_labeled(
                        "responsive_navigation",
                        "Tab navigation remains responsive during worktree cleanup",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Review Requests", &full);
            },
        )?;

    Ok(())
}

/// Verify that confirming quit does not wait indefinitely for externally
/// merged worktree cleanup.
#[test]
fn merged_review_request_cleanup_does_not_block_quit() -> E2eResult {
    // Arrange
    let _test_guard = common::acquire_e2e_test_lock();
    let temp = tempfile::TempDir::new()?;
    let env = BuilderEnv::new(temp.path())?;
    env.init_git()?;
    seed_slow_merged_review_request_status(&env)?;
    install_delayed_worktree_remove_stub(&env, 30)?;
    let mut session = env.builder().spawn()?;
    let scenario = Scenario::new("merged_cleanup_quit")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .wait_for_text("Done", 2500)
        .compose(&common::open_quit_dialog())
        .press_key("y");

    // Act
    scenario.execute_in_pty(&mut session)?;
    let exited_successfully = session.wait_for_exit(Duration::from_secs(8))?;

    // Assert
    assert!(
        exited_successfully,
        "confirmed quit should cancel cleanup after the shutdown deadline"
    );

    Ok(())
}

/// Verify that linked review requests expose their browser URL in the session
/// header.
#[test]
fn review_request_url_appears_in_session_header() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_url_header")
        .with_git()
        .setup(seed_review_ready_session_with_review_request)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("https://github.com/agentty-xyz/agentty/pull/42", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_request_url_header",
                        "Linked review-request URL visible in the session header",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "https://github.com/agentty-xyz/agentty/pull/42",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Verify that opening the diff page from a review-ready session shows the
/// selected file's local changes and change totals.
#[test]
fn diff_preview_opens_from_session() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("diff_preview")
        .with_git()
        .setup(seed_review_ready_session_with_review_request)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("src/ +1 -0", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("diff_preview", "Diff preview after pressing d")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());

                assertion::assert_text_in_region(frame, "src/ +1 -0", &full);
                assertion::assert_text_in_region(frame, "println!(\"review\")", &full);
                assertion::assert_text_in_region(frame, "j/k: select file", &full);
            },
        )?;

    Ok(())
}

/// Verify that a linked review request opens a read-only split comment page
/// with the selected thread's current diff context.
#[test]
fn test_session_review_comments() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_review_comments")
        .with_git()
        .setup(seed_review_ready_session_with_review_request)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("c: comments", 5000)
                    .press_key("c")
                    .wait_for_text("Please explain why this review output is needed.", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "inline_review_comment",
                        "Multiline review comment with its attached code context",
                    )
                    .press_key("j")
                    .wait_for_text(
                        "This file-level comment is not attached to a code line.",
                        5000,
                    )
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "file_review_comment",
                        "File-level review comment without a synthetic code anchor",
                    )
            },
            |frame, report| {
                let inline_frame = common::frame_from_capture(&report.captures[0]);
                let inline_full = Region::full(inline_frame.cols(), inline_frame.rows());
                let page_text = inline_frame.text_in_region(&inline_full);
                let code_context_index = page_text
                    .find("Code context")
                    .expect("code context section should be visible");
                let conversation_index = page_text
                    .find("Conversation")
                    .expect("conversation section should be visible");

                assertion::assert_text_in_region(&inline_frame, "Comments (2)", &inline_full);
                assertion::assert_text_in_region(&inline_frame, "src/main.rs:1-2", &inline_full);
                assertion::assert_text_in_region(&inline_frame, "Conversation", &inline_full);
                assertion::assert_text_in_region(
                    &inline_frame,
                    "Please explain why this review output is needed.",
                    &inline_full,
                );
                assertion::assert_text_in_region(&inline_frame, "Code context", &inline_full);
                assertion::assert_text_in_region(
                    &inline_frame,
                    "println!(\"review\")",
                    &inline_full,
                );
                assert!(code_context_index < conversation_index);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "file  ·  1 comments", &full);
                assertion::assert_text_in_region(
                    frame,
                    "This file-level comment is not attached to a code line.",
                    &full,
                );
                assertion::assert_text_in_region(frame, "Please review the whole file.", &full);
                assertion::assert_not_visible(frame, "println!(\"review\")");
                assertion::assert_text_in_region(frame, "j/k: select comment", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing `d` while the chat transcript is focused in the reply
/// composer opens the diff preview, and that leaving it restores the composer
/// with the typed draft intact.
#[test]
fn diff_preview_opens_from_prompt_chat_focus() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("diff_preview_from_prompt")
        .with_git()
        .setup(seed_review_ready_session_with_review_request)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("follow up draft")
                    .wait_for_text("follow up draft", 3000)
                    .press_key("Tab")
                    .wait_for_text("d: diff", 5000)
                    .capture_labeled(
                        "prompt_chat_focused",
                        "Chat transcript focused in the reply composer",
                    )
                    .press_key("d")
                    .wait_for_text("src/ +1 -0", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "diff_from_prompt",
                        "Diff preview opened from the reply composer",
                    )
                    .press_key("Esc")
                    .wait_for_text("follow up draft", 5000)
                    .viewing_pause_ms(1000)
            },
            |frame, report| {
                let focused_frame = common::frame_from_capture(&report.captures[0]);
                let focused_full = Region::full(focused_frame.cols(), focused_frame.rows());
                assertion::assert_text_in_region(&focused_frame, "d: diff", &focused_full);

                let diff_frame = common::frame_from_capture(&report.captures[1]);
                let diff_full = Region::full(diff_frame.cols(), diff_frame.rows());
                assertion::assert_text_in_region(&diff_frame, "src/ +1 -0", &diff_full);
                assertion::assert_text_in_region(&diff_frame, "println!(\"review\")", &diff_full);
                assertion::assert_text_in_region(&diff_frame, "j/k: select file", &diff_full);

                // Leaving the diff restores the composer with the draft intact.
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "follow up draft", &full);
            },
        )?;

    Ok(())
}

/// Verify session output renders mermaid flowchart, entity-relationship, and
/// sequence fenced blocks as Unicode diagrams instead of raw mermaid source.
#[test]
fn session_view_mermaid_output() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_mermaid_output")
        .with_terminal_size(160, 72)
        .setup(seed_session_with_mermaid_output)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Stream result", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_mermaid_output",
                        "Session chat with rendered mermaid diagrams",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "User starts session", &full);
                assertion::assert_text_in_region(frame, "Send prompt", &full);
                assertion::assert_text_in_region(frame, "Report result", &full);
                assertion::assert_text_in_region(frame, "Open diff view", &full);
                assertion::assert_text_in_region(frame, "▲", &full);
                assertion::assert_text_in_region(frame, "▼", &full);
                assertion::assert_text_in_region(frame, "CUSTOMER", &full);
                assertion::assert_text_in_region(frame, "places", &full);
                assertion::assert_text_in_region(frame, "Start new session", &full);
                assertion::assert_text_in_region(frame, "Stream result", &full);
                assertion::assert_not_visible(frame, "flowchart TD");
                assertion::assert_not_visible(frame, "erDiagram");
                assertion::assert_not_visible(frame, "sequenceDiagram");
                assertion::assert_not_visible(frame, "Agent available");
            },
        )?;

    Ok(())
}

/// Verify that persisted focused review text is restored into the session
/// output panel after Agentty starts again.
#[test]
fn persisted_focused_review_survives_reload() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("persisted_focused_review")
        .with_git()
        .setup(seed_review_ready_session_with_persisted_focused_review)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "persisted_focused_review",
                        "Persisted focused review visible after startup",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Persisted focused review finding.", &full);
                let impact_header = frame
                    .find_text("Project Impact")
                    .into_iter()
                    .next()
                    .expect("project impact header should render");
                let impact_finding = frame
                    .find_text("Persisted focused review finding.")
                    .into_iter()
                    .next()
                    .expect("project impact finding should render");
                let suggestions_header = frame
                    .find_text("Suggestions")
                    .into_iter()
                    .next()
                    .expect("suggestions header should render");
                let empty_suggestion = frame
                    .find_text("- None.")
                    .into_iter()
                    .next()
                    .expect("empty suggestion should render");

                assert_eq!(impact_finding.rect.row, impact_header.rect.row + 1);
                assert_eq!(empty_suggestion.rect.row, suggestions_header.rect.row + 1);
                assertion::assert_not_visible(frame, "type \"/apply\" to verify and apply");
            },
        )?;

    Ok(())
}

/// Verify each session restores its own persisted focused review after users
/// switch between session views.
#[test]
fn focused_reviews_survive_session_switching() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("focused_reviews_survive_session_switching")
        .with_git()
        .setup(seed_sessions_with_persisted_focused_reviews)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Persisted focused review finding.", 5000)
                    .capture_labeled("first_review", "First session focused review")
                    .press_key("q")
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Second persisted review finding.", 5000)
                    .capture_labeled("second_review", "Second session focused review")
                    .press_key("q")
                    .press_key("k")
                    .press_key("Enter")
                    .wait_for_text("Persisted focused review finding.", 5000)
                    .capture_labeled("restored_first_review", "Restored first focused review")
            },
            |frame, report| {
                assert_eq!(report.captures.len(), 3);
                let first_frame = common::frame_from_capture(&report.captures[0]);
                let second_frame = common::frame_from_capture(&report.captures[1]);
                let restored_first_frame = common::frame_from_capture(&report.captures[2]);
                let first_full = Region::full(first_frame.cols(), first_frame.rows());
                let second_full = Region::full(second_frame.cols(), second_frame.rows());
                let restored_first_full =
                    Region::full(restored_first_frame.cols(), restored_first_frame.rows());
                let final_full = Region::full(frame.cols(), frame.rows());

                assertion::assert_text_in_region(
                    &first_frame,
                    "Persisted focused review finding.",
                    &first_full,
                );
                assertion::assert_text_in_region(
                    &second_frame,
                    "Second persisted review finding.",
                    &second_full,
                );
                assertion::assert_text_in_region(
                    &restored_first_frame,
                    "Persisted focused review finding.",
                    &restored_first_full,
                );
                assertion::assert_text_in_region(
                    frame,
                    "Persisted focused review finding.",
                    &final_full,
                );
            },
        )?;

    Ok(())
}

/// Verify focused review treats explanations and accepted tradeoffs from the
/// saved session chat as constraints instead of repeating resolved advice.
#[test]
fn focused_review_honors_resolved_session_decisions() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("focused_review_resolved_decision")
        .with_git()
        .setup(seed_review_with_resolved_decision)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Keep the println call", 5000)
                    .press_key("f")
                    .wait_for_text("Suggestions", 30000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "resolved_decision_honored",
                        "Focused review honors a decision resolved in session chat",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, RESOLVED_DECISION_REVIEW_TEXT, &full);
                assertion::assert_text_in_region(frame, "Suggestions", &full);
                assertion::assert_text_in_region(frame, "- None", &full);
                assertion::assert_not_visible(frame, MISSING_DECISION_CONTEXT_POLICY_TEXT);
                assertion::assert_not_visible(frame, MISSING_RESOLVED_DECISION_HISTORY_TEXT);
            },
        )?;

    Ok(())
}

/// Verify the `AgentReview` session footer keeps the sync shortcut visible so
/// users can start a rebase without waiting for focused review generation.
#[test]
fn agent_review_session_shows_sync_shortcut() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("agent_review_sync_shortcut")
        .with_git()
        .setup(seed_agent_review_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "agent_review_sync_shortcut",
                        "AgentReview session view with sync shortcut",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Agent review sync shortcut", &full);
                assertion::assert_text_in_region(frame, "r: sync", &full);
            },
        )?;

    Ok(())
}

/// Verify that root review-ready sessions can confirm session forking from
/// session view.
#[test]
fn session_fork_confirmation_creates_session_from_review_session() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_fork_confirmation")
        .with_git()
        .with_terminal_size(120, 24)
        .setup(seed_review_ready_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled("session_view", "Fork shortcut visible in session view")
                    .write_text("F")
                    .wait_for_text("Confirm Fork", 3000)
                    .viewing_pause_ms(1000)
                    .capture_labeled("fork_confirmation", "Fork confirmation popup")
                    .write_text("y")
                    .wait_for_text("Review-ready session shortcuts", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("forked_session_view", "Forked session opened")
                    .press_key("q")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("session_list_after_fork", "Source and fork listed")
            },
            |frame, report| {
                let session_view_frame = common::frame_from_capture(&report.captures[0]);
                let view_full = Region::full(session_view_frame.cols(), session_view_frame.rows());
                assertion::assert_text_in_region(&session_view_frame, "F: fork", &view_full);

                let confirmation_frame = common::frame_from_capture(&report.captures[1]);
                let confirmation_full =
                    Region::full(confirmation_frame.cols(), confirmation_frame.rows());
                assertion::assert_text_in_region(
                    &confirmation_frame,
                    "Confirm Fork",
                    &confirmation_full,
                );
                assertion::assert_text_in_region(
                    &confirmation_frame,
                    "Fork this session",
                    &confirmation_full,
                );

                let forked_view_frame = common::frame_from_capture(&report.captures[2]);
                let forked_view_full =
                    Region::full(forked_view_frame.cols(), forked_view_frame.rows());
                assertion::assert_text_in_region(
                    &forked_view_frame,
                    "Review-ready session shortcuts",
                    &forked_view_full,
                );
                assertion::assert_not_visible(&forked_view_frame, "Confirm Fork");

                let full = Region::full(frame.cols(), frame.rows());
                let session_list_text = frame.text_in_region(&full);
                let session_title_count = session_list_text
                    .matches("Review-ready session shortcuts")
                    .count();
                assert!(
                    session_title_count >= 2,
                    "expected source and fork rows in session list, got \
                     {session_title_count}:\n{session_list_text}"
                );
            },
        )?;

    Ok(())
}

/// Verify that review-ready stacked children do not expose session forking.
#[test]
fn stacked_child_hides_session_fork_shortcut() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_child_hides_session_fork_shortcut")
        .with_git()
        .with_terminal_size(120, 24)
        .setup(seed_review_ready_parent_with_review_child)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Child stack review", 5000)
                    .press_key("Enter")
                    .wait_for_text("Child stack review", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled("child_view", "Fork shortcut hidden for stacked child")
                    .write_text("F")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled("after_f", "No fork confirmation opens for stacked child")
            },
            |frame, report| {
                let child_view_frame = common::frame_from_capture(&report.captures[0]);
                let view_full = Region::full(child_view_frame.cols(), child_view_frame.rows());
                assertion::assert_text_in_region(
                    &child_view_frame,
                    "Child stack review",
                    &view_full,
                );
                assertion::assert_not_visible(&child_view_frame, "F: fork");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Child stack review", &full);
                assertion::assert_not_visible(frame, "F: fork");
                assertion::assert_not_visible(frame, "Confirm Fork");
                assertion::assert_not_visible(frame, "Reviewing changes with");
                assertion::assert_not_visible(frame, "No diff changes found for review.");
            },
        )?;

    Ok(())
}

/// Verify that typing `/apply` in a review-ready session keeps the command
/// text visible when no actionable focused-review cache is available.
#[test]
fn apply_slash_command_unavailable_without_review_cache() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("apply_slash_command_no_review")
        .with_git()
        .setup(seed_review_ready_session)
        .zola(
            "Apply slash command",
            "Type unavailable `/apply` in a review-ready session and keep the prompt intact.",
            42,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("/")
                    .wait_for_text("/model", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "slash_commands_visible",
                        "Slash command suggestion list omits unavailable /apply",
                    )
                    .write_text("apply")
                    .wait_for_text("/apply", 3000)
                    .wait_for_stable_frame(300, 3000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "apply_not_applied_without_review_cache",
                        "Session stays in prompt mode with `/apply` unchanged",
                    )
            },
            |frame, report| {
                let suggestion_frame = common::frame_from_capture(&report.captures[0]);
                let suggestion_full =
                    Region::full(suggestion_frame.cols(), suggestion_frame.rows());
                assertion::assert_text_in_region(&suggestion_frame, "/model", &suggestion_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "/apply", &full);
                let full_text = frame.text_in_region(&full);
                assert!(
                    !full_text.contains("Run a focused review first"),
                    "session without actionable review cache should not show apply guidance"
                );
            },
        )?;

    Ok(())
}

/// Verify that slash-command filtering can match text contained inside a
/// command name, not only command prefixes.
#[test]
fn model_slash_command_contains_match_is_visible() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("model_slash_command_contains_match")
        .with_git()
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("/")
                    .write_text("o")
                    .wait_for_text("/model", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "model_slash_command_contains_match",
                        "`/model` appears for contained slash-command input",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "/model", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Choose an agent and model for this session.",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Verify that `j` and `k` navigate the session list and that `Enter`
/// opens the currently selected session.
///
/// Creates two sessions ("alpha" and "beta"), navigates down with `j`,
/// opens the selection with `Enter`, returns with `q`, navigates back
/// up with `k`, and opens again. Asserts that the list shows display-only
/// size prefixes and that both navigations still land on openable session
/// views after moving the cursor in the list.
#[test]
fn session_list_jk_navigation() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_navigation")
        .with_git()
        .zola(
            "Session list navigation",
            "Navigate sessions with j/k keys to select and open different entries.",
            44,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::create_session_with_prompt_and_return_to_list(
                        "alpha",
                    ))
                    .compose(&common::create_session_with_prompt_and_return_to_list(
                        "beta",
                    ))
                    .viewing_pause_ms(2000)
                    .capture_labeled("two_sessions", "Two sessions in list")
                    // Navigate down with j, open the selection, and capture.
                    .press_key("j")
                    .wait_for_stable_frame(300, 3000)
                    .compose(&common::open_selected_session_view())
                    .viewing_pause_ms(2000)
                    .capture_labeled("opened_after_j", "Session opened after pressing j")
                    .compose(&common::return_to_session_list())
                    // Navigate back up with k, open the selection, and capture.
                    .press_key("k")
                    .wait_for_stable_frame(300, 3000)
                    .compose(&common::open_selected_session_view())
                    .viewing_pause_ms(2000)
                    .capture_labeled("opened_after_k", "Session opened after pressing k")
            },
            |_frame, report| {
                assert_eq!(
                    report.captures.len(),
                    3,
                    "Expected 3 captures (list, opened_after_j, opened_after_k)"
                );

                // Both sessions visible in the initial list.
                let initial_frame = common::frame_from_capture(&report.captures[0]);
                let initial_full = Region::full(initial_frame.cols(), initial_frame.rows());
                let initial_text = initial_frame.text_in_region(&initial_full);
                assert!(
                    initial_text.contains("alpha") && initial_text.contains("beta"),
                    "Expected both session prompts visible in list"
                );
                assert!(
                    initial_text.contains("[XS]"),
                    "Expected session-size prefix visible in list"
                );

                // Extract text from the two opened-session captures.
                let session_after_down_navigation_frame =
                    common::frame_from_capture(&report.captures[1]);
                let session_after_up_navigation_frame =
                    common::frame_from_capture(&report.captures[2]);

                let down_navigation_full = Region::full(
                    session_after_down_navigation_frame.cols(),
                    session_after_down_navigation_frame.rows(),
                );
                let up_navigation_full = Region::full(
                    session_after_up_navigation_frame.cols(),
                    session_after_up_navigation_frame.rows(),
                );

                let down_navigation_text =
                    session_after_down_navigation_frame.text_in_region(&down_navigation_full);
                let up_navigation_text =
                    session_after_up_navigation_frame.text_in_region(&up_navigation_full);

                // Each opened view must contain one of the session prompts.
                assert!(
                    down_navigation_text.contains("alpha") || down_navigation_text.contains("beta"),
                    "Session opened after j must contain alpha or beta"
                );
                assert!(
                    up_navigation_text.contains("alpha") || up_navigation_text.contains("beta"),
                    "Session opened after k must contain alpha or beta"
                );
            },
        )?;

    Ok(())
}
/// Verify that typed text appears in the prompt input.
#[test]
fn prompt_typing_shows_text() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("prompt_typing")
        .with_git()
        .zola(
            "Prompt typing",
            "Type text into the prompt input and see it appear in real time.",
            115,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("empty_prompt", "Empty prompt input")
                    .write_text("hello world")
                    .wait_for_text("hello world", 3000)
                    .viewing_pause_ms(2500)
                    .capture_labeled("typed_text", "Prompt input with typed text")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "hello world", &full);
            },
        )?;

    Ok(())
}

/// Verify that Alt+Enter inserts a newline in the prompt input,
/// producing multiline content.
///
/// Alt+Enter is sent as ESC (0x1b) followed by CR (0x0d) which crossterm
/// interprets as `KeyCode::Enter` with `KeyModifiers::ALT`.
#[test]
fn prompt_multiline_via_alt_enter() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("prompt_multiline")
        .with_git()
        .zola(
            "Multiline prompt",
            "Insert newlines with Alt+Enter to compose multiline prompts.",
            125,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .write_text("first line")
                    .wait_for_text("first line", 3000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("first_line", "First line typed")
                    // Alt+Enter: ESC (0x1b) followed by CR (0x0d).
                    .write_text("\x1b\r")
                    .wait_for_stable_frame(300, 3000)
                    .write_text("second line")
                    .wait_for_text("second line", 3000)
                    .viewing_pause_ms(2500)
                    .capture_labeled("multiline", "Multiline prompt with both lines")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "first line", &full);
                assertion::assert_text_in_region(frame, "second line", &full);
            },
        )?;

    Ok(())
}

/// Verify bracketed paste keeps leading indentation after prompt submission.
#[test]
fn prompt_paste_indentation() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("prompt_paste_indentation")
        .with_git()
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("\x1b[200~    indented prompt\x1b[201~")
                    .wait_for_text("indented prompt", 3000)
                    .press_key("Enter")
                    .wait_for_text("q: back", 10000)
                    .capture_labeled(
                        "submitted_indentation",
                        "Submitted pasted prompt retaining leading indentation",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let text = frame.text_in_region(&full);

                assert!(text.contains(" ›     indented prompt"));
            },
        )?;

    Ok(())
}

/// Verify that CSI-u `Shift+Enter` inserts a newline in the prompt input.
#[test]
fn prompt_multiline_via_csi_u_shift_enter() -> E2eResult {
    // Arrange
    let _test_guard = common::acquire_e2e_test_lock();
    let temp = tempfile::TempDir::new()?;
    let env = BuilderEnv::new(temp.path())?;
    env.init_git()?;
    let scenario = Scenario::new("prompt_multiline_shift_enter")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .press_key("a")
        .press_key("Enter")
        .wait_for_stable_frame(300, 5000)
        .write_text("first line")
        .wait_for_text("first line", 3000)
        .write_text("\x1b[13;2u")
        .wait_for_stable_frame(300, 3000)
        .write_text("second line")
        .wait_for_text("second line", 3000);

    // Act
    let frame = scenario.run(env.builder())?;

    // Assert
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "first line", &full);
    assertion::assert_text_in_region(&frame, "second line", &full);
    assertion::assert_not_visible(&frame, "first linesecond line");

    Ok(())
}
