//! Session lifecycle and prompt E2E tests.
//!
//! Tests cover session creation via `a` key, opening sessions with `Enter`,
//! list navigation with `j`/`k`, deletion with confirmation, prompt input
//! basics (typing, multiline via Alt+Enter and CSI-u Shift+Enter, cancel via
//! Esc), and returning to the session list from session view.

use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

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
use testty::proof::report::ProofReport;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

type E2eResult = Result<(), Box<dyn std::error::Error>>;
const LOADER_SESSION_ID: &str = "loader-session-0001";
/// Stable id for the session whose branch conflicts with `main`.
const MERGE_CONFLICT_SESSION_ID: &str = "merge-conflict-0001";

/// Stable id for the Antigravity session whose replay exceeds the safe
/// command-argument limit.
const ANTIGRAVITY_OVERSIZED_REPLAY_SESSION_ID: &str = "antigravity-oversized-replay";

/// Stable id for the seeded running session used by stop-turn tests.
const RUNNING_STOP_SESSION_ID: &str = "running-stop-0001";

/// Stable id for the seeded rebasing session used by message-queue tests.
const REBASING_QUEUE_SESSION_ID: &str = "rebasing-queue-0001";

/// Stable id for the seeded binary-only diff session.
const BINARY_DIFF_SESSION_ID: &str = "binary-diff-0001";

/// Stable id for the unrelated row selected before the refresh regression
/// creates its question session.
const QUESTION_REFRESH_SELECTED_SESSION_ID: &str = "question-refresh-selected";

/// Initial prompt that must survive the title-generation refresh.
const QUESTION_REFRESH_INITIAL_PROMPT: &str = "Implement the initial task";

/// Generated title that must survive clarification submission.
const QUESTION_REFRESH_TITLE: &str = "Keep the original task title";

/// Final answer emitted after the clarification response resumes the worker.
const QUESTION_REFRESH_FINAL_ANSWER: &str = "Clarifications accepted.";

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
/// Review-request notice body used by the timeline-order regression.
const REVIEW_REQUEST_TIMELINE_NOTICE_TEXT: &str =
    "Created PR https://github.com/agentty-xyz/agentty/pull/42";

/// User-authored prompt retained in composer history after review resolution.
const REVIEW_HISTORY_PROMPT_TEXT: &str = "Explain the review status loader";

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

/// Wall-clock budget for one seeded git invocation before it is killed.
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_mins(1);

/// Poll interval used while waiting for a seeded git invocation to exit.
const GIT_COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(25);

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
        SessionSeed::regular("agent-error-0001", "claude-opus-5", "main", "Review")
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
        SessionSeed::regular("markdown-table-0001", "claude-opus-5", "main", "Review")
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
        SessionSeed::regular("user-markdown-0001", "claude-opus-5", "main", "Review")
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
Use **bold** and `code`. Review @crates/agentty/src/ui/markdown.rs.

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
        SessionSeed::regular("inline-md-0001", "claude-opus-5", "main", "Review")
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
        SessionSeed::regular("mermaid-chat-0001", "claude-opus-5", "main", "Review")
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

/// Seeds one review-ready session with the cyclic orchestration flow that
/// previously fell back to a plain code block.
fn seed_session_with_cyclic_mermaid_output(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("cyclic-mermaid-0001", "claude-opus-5", "main", "Review")
            .with_title("Cyclic Mermaid output"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "cyclic-mermaid-0001",
                SessionMessageKind::AssistantAnswer,
                "\
```mermaid
flowchart LR
    U[User and TUI] --> C[Orchestrator controller]
    M[Agent model] --> P[Typed command response]
    P --> C
    C --> S[ag-session service]
    S --> A[Agentty host adapter]
    A --> W[Session workers]
    W --> E[Session events]
    E --> C
    C --> M
```
",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("cyclic-m"))?;

    Ok(())
}

/// Seeds one review-ready session with a left-to-right telemetry flow that is
/// wider than its session output panel and must use the compact layout.
fn seed_session_with_compact_mermaid_output(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("compact-mermaid-0001", "qwen3-coder-plus", "main", "Review")
            .with_title("Compact Mermaid output"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "compact-mermaid-0001",
                SessionMessageKind::AssistantAnswer,
                "\
```mermaid
flowchart LR
    Q[Qwen complete] --> T[Tracing spans and events]
    Q --> M[OTel metrics API]
    T --> S[Trace and log providers]
    M --> P[Meter provider]
    S --> O[OTLP HTTP protobuf]
    P --> O
    O --> C[Collector on port 4318]
    C --> B[Telemetry backends]
    B --> G[Grafana on port 3000]
```
",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("compact-"))?;

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
        SessionSeed::regular(session_id, "gpt-5.6-sol", "main", "Review")
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

/// Installs a deterministic Claude stub for stable-context title generation
/// and resumed review turns.
fn seed_session_title_candidate_project(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
prompt=$(cat)
case "$prompt" in
  *"Generate a concise, commit-style title"*)
    case "$prompt" in
      *'\<latest_request> Improve session title generation. \</latest_request>'*)
        answer=''
        ;;
      *'\<original_request> Improve session title generation. \</original_request>'*'\<latest_request> Also reject punctuation-only copies. \</latest_request>'*)
        answer='Stabilize session title generation'
        ;;
      *)
        answer='Assess project quality'
        ;;
    esac
    ;;
  *"Also reject punctuation-only copies."*)
    answer='Follow-up complete. No files were changed.'
    ;;
  *"Improve session title generation."*)
    answer='The session title workflow is ready for a focused follow-up.'
    ;;
  *)
    answer='Got it. What would you like me to do?'
    ;;
esac
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '{"type":"result","subtype":"success","result":"{\\"answer\\":\\"%s\\",\\"questions\\":[],\\"review_comment_outcomes\\":[],\\"summary\\":null}","usage":{"input_tokens":5,"output_tokens":9}}\n' "$answer"
"#;
    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "claude"),
            ("DefaultSmartModel", "claude-haiku-4-5-20251001"),
            ("DefaultFastAgent", "claude"),
            ("DefaultFastModel", "claude-haiku-4-5-20251001"),
        ],
    )?;

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

/// Seeds a Codex focused review with commentary, a valid final item, and a
/// blank duplicate final item in `turn/completed`.
fn seed_codex_review_with_blank_completed_fallback(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;

    let codex_path = env.stub_bin.join("codex");
    let script = r###"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'codex-cli 0.146.0\n'; exit 0; fi

extract_id() {
    printf '%s\n' "$1" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'
}

while IFS= read -r request; do
    case "$request" in
        *'"method":"initialize"'*)
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{}}\n' "$request_id"
            ;;
        *'"method":"thread/start"'*)
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{"thread":{"id":"review-thread"}}}\n' "$request_id"
            ;;
        *'"method":"turn/start"'*)
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{"turn":{"id":"review-turn"}}}\n' "$request_id"
            printf '%s\n' '{"method":"turn/started","params":{"turn":{"id":"review-turn"}}}'
            printf '%s\n' '{"method":"item/completed","params":{"threadId":"review-thread","turnId":"review-turn","item":{"type":"agentMessage","id":"commentary-item","text":"I will inspect the current code.","phase":"commentary"}}}'
            printf '%s\n' '{"method":"item/completed","params":{"threadId":"review-thread","turnId":"review-turn","item":{"type":"agentMessage","id":"final-item","text":"{\"answer\":\"## Review\\n\\n### Project Impact\\n\\n- Final focused review result.\\n\\n### Suggestions\\n\\n- None.\",\"questions\":[],\"summary\":null}","phase":"final_answer"}}}'
            printf '%s\n' '{"method":"turn/completed","params":{"threadId":"review-thread","turn":{"id":"review-turn","status":"completed","items":[{"type":"agentMessage","id":"blank-final-item","text":"   ","phase":"final_answer"}]}}}'
            ;;
    esac
done
"###;
    std::fs::write(&codex_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&codex_path, std::fs::Permissions::from_mode(0o755))?;

    seed_project_settings(
        env,
        &[
            ("DefaultReviewAgent", "codex"),
            ("DefaultReviewModel", "gpt-5.6-sol"),
        ],
    )
}

/// Seeds one review-ready session plus its default source branch and
/// propagates setup errors to the caller.
fn seed_review_ready_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("review-shortcut-0001", "gpt-5.6-sol", "main", "Review")
            .with_title("Review-ready session shortcuts"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_diff_stats(12, 3, true, "review-shortcut-0001", "M")
            .await
    })?;

    run_git(&env.workdir, &["branch", "wt/review-s"])?;
    std::fs::create_dir_all(env.agentty_root.join("wt").join("review-s"))?;

    Ok(())
}

/// Seeds one review-ready session whose latest diff refresh found no changes.
fn seed_clean_review_ready_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_clean_review_worktree(env)?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_diff_stats(0, 0, false, "review-shortcut-0001", "XS")
            .await
    })?;

    Ok(())
}

/// Seeds a review-ready worktree whose committed change conflicts with a
/// newer commit on the stored base branch.
fn seed_merge_conflict_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(MERGE_CONFLICT_SESSION_ID, "gpt-5.6-sol", "main", "Review")
            .with_title("Update shared configuration"),
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        test_support::persist_active_tab_for_test(&database, agentty::app::Tab::Sessions).await
    })?;

    std::fs::write(env.workdir.join("shared.txt"), "initial\n")?;
    run_git(&env.workdir, &["add", "shared.txt"])?;
    run_git(&env.workdir, &["commit", "-m", "Add shared configuration"])?;

    let session_worktree =
        test_support::session_folder(&env.agentty_root.join("wt"), MERGE_CONFLICT_SESSION_ID);
    let session_worktree_text = session_worktree
        .to_str()
        .ok_or_else(|| std::io::Error::other("session worktree path is not UTF-8"))?;
    run_git(
        &env.workdir,
        &[
            "worktree",
            "add",
            "-b",
            "wt/merge-co",
            session_worktree_text,
            "main",
        ],
    )?;
    std::fs::write(session_worktree.join("shared.txt"), "session change\n")?;
    run_git(&session_worktree, &["add", "shared.txt"])?;
    run_git(
        &session_worktree,
        &["commit", "-m", "Change session configuration"],
    )?;

    std::fs::write(env.workdir.join("shared.txt"), "main change\n")?;
    run_git(&env.workdir, &["add", "shared.txt"])?;
    run_git(&env.workdir, &["commit", "-m", "Change main configuration"])?;

    Ok(())
}

/// Seeds one review-ready session with a worktree-local personality.
fn seed_session_personality(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;

    let session_folder =
        test_support::session_folder(&env.agentty_root.join("wt"), "review-shortcut-0001");
    let personality_directory = session_folder
        .join(".agents")
        .join("agents")
        .join("reviewer");
    std::fs::create_dir_all(&personality_directory)?;
    std::fs::write(
        personality_directory.join("agent.md"),
        "---\nid: reviewer\nname: Code Reviewer\ndescription: Reviews code carefully\nrole: \
         delegation-target\nenabled: true\n---\nReview every change for correctness.",
    )?;

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

/// Seeds one synced published session whose next completed turn appends a
/// durable commit notice and then starts a delayed auto-push, leaving the
/// earlier sync result, the turn commit notice, and the auto-push progress row
/// visible in one frame.
fn seed_published_session_output_chronology(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_session_title_candidate_project(env)?;

    let session_id = "review-shortcut-0001";
    common::seed_session(
        env,
        SessionSeed::regular(session_id, "claude-haiku-4-5-20251001", "main", "Review")
            .with_title("Chronological session output"),
    )?;

    // The session worktree must stay a linked worktree of the project
    // checkout; a standalone repository trips the session isolation guard and
    // the turn never starts.
    let remote_path = env.agentty_root.join("chronology-remote.git");
    let remote_path_text = remote_path.to_string_lossy().into_owned();
    run_git(&env.workdir, &["init", "--bare", remote_path_text.as_str()])?;
    run_git(
        &env.workdir,
        &["remote", "add", "origin", remote_path_text.as_str()],
    )?;
    run_git(&env.workdir, &["push", "--set-upstream", "origin", "main"])?;

    let session_worktree = test_support::session_folder(&env.agentty_root.join("wt"), session_id);
    let session_worktree_text = session_worktree.to_string_lossy().into_owned();
    run_git(
        &env.workdir,
        &[
            "worktree",
            "add",
            "-b",
            "wt/review-s",
            session_worktree_text.as_str(),
        ],
    )?;
    run_git(
        &session_worktree,
        &["push", "--set-upstream", "origin", "wt/review-s"],
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_published_upstream_ref(
                session_id,
                Some("origin/wt/review-s".to_string()),
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                session_id,
                SessionMessageKind::WorkflowNotice,
                "\n[Sync] Successfully synced wt/review-s onto origin/main\n",
            )
            .await
    })?;

    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let git_path = env.stub_bin.join("git");
    let script = format!(
        r#"#!/bin/sh
case "$1" in
  push)
    sleep 8
    ;;
esac
exec '{}' "$@"
"#,
        real_git.display()
    );
    std::fs::write(&git_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o755))?;

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

    seed_successful_review_request_publish(env, 3, 15)
}

/// Seeds a live focused review that completes before a delayed review-request
/// publish, reproducing the cross-source transcript ordering boundary.
fn seed_review_request_timeline(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_with_resolved_decision(env)?;

    seed_successful_review_request_publish(env, 0, 0)
}

/// Configures the review-ready worktree and forge stubs for one successful
/// review-request publish with deterministic command delays.
fn seed_successful_review_request_publish(
    env: &BuilderEnv,
    push_delay_seconds: u64,
    create_delay_seconds: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_worktree = env.agentty_root.join("wt").join("review-s");
    run_git(&session_worktree, &["branch", "-m", "wt/review-s"])?;
    run_git(&session_worktree, &["branch", "main", "HEAD"])?;

    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let git_path = env.stub_bin.join("git");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "push" ]; then
  sleep {push_delay_seconds}
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
    let gh_script = r#"#!/bin/sh
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
    sleep __CREATE_DELAY_SECONDS__
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
"#
    .replace(
        "__CREATE_DELAY_SECONDS__",
        &create_delay_seconds.to_string(),
    );
    std::fs::write(&gh_path, gh_script)?;
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
        SessionSeed::regular(session_id, "gpt-5.6-sol", "main", "Review")
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
                "gpt-5.6-sol",
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
            .update_session_diff_stats(8, 2, true, "agent-review-sync-0001", "S")
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
        SessionSeed::regular("stack-parent-0001", "gpt-5.6-sol", "main", "Review")
            .with_title("Parent stack review"),
    )?;
    common::seed_session(
        env,
        SessionSeed::stacked_draft(
            "stack-child-0001",
            "gpt-5.6-sol",
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
        SessionSeed::regular("stack-restack-child-0001", "gpt-5.6-sol", "main", "Review")
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
        SessionSeed::regular(
            "stack-restack-failure-0001",
            "gpt-5.6-sol",
            "main",
            "Review",
        )
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
        SessionSeed::regular("a-older", "gpt-5.6-sol", "main", "Review")
            .with_title("Older created session"),
    )?;
    common::seed_session(
        env,
        SessionSeed::regular("z-newer", "gpt-5.6-sol", "main", "Review")
            .with_title("Newer created session"),
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let mut connection = SqliteConnectOptions::new()
            .filename(&db_path)
            .connect()
            .await?;
        let query = sqlx::query!(
            r"
UPDATE session
SET created_at = CASE id WHEN 'a-older' THEN 100 ELSE 200 END,
    updated_at = 300
WHERE id IN ('a-older', 'z-newer')
"
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

/// Answer returned through Claude's schema-validated result field.
const CLAUDE_STRUCTURED_RESPONSE_TEXT: &str = "Claude structured response rendered";

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

/// Installs a Claude stub that returns its final protocol reply through
/// `structured_output`, matching current Claude Code schema-validated turns.
fn seed_claude_structured_output_project(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"StructuredOutput","input":{{"answer":"{CLAUDE_STRUCTURED_RESPONSE_TEXT}"}}}}]}}}}'
printf '%s\n' '{{"type":"result","subtype":"success","result":"","structured_output":{{"answer":"{CLAUDE_STRUCTURED_RESPONSE_TEXT}","questions":[],"summary":null}},"usage":{{"input_tokens":8,"output_tokens":6}}}}'
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
    std::fs::write(
        &stub_agent_path,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'agy 1.2.0\\n'; exit 0; fi\nexit \
         1\n",
    )?;

    #[cfg(unix)]
    std::fs::set_permissions(&stub_agent_path, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Adds an outdated Antigravity CLI stub and one supported fallback provider.
fn seed_outdated_antigravity_cli_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let antigravity_path = env.stub_bin.join("agy");
    std::fs::write(
        &antigravity_path,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'agy 1.1.6\\n'; exit 0; fi\nexit \
         1\n",
    )?;
    let codex_path = env.stub_bin.join("codex");
    std::fs::write(&codex_path, "#!/bin/sh\nexit 1\n")?;

    #[cfg(unix)]
    {
        std::fs::set_permissions(&antigravity_path, std::fs::Permissions::from_mode(0o755))?;
        std::fs::set_permissions(&codex_path, std::fs::Permissions::from_mode(0o755))?;
    }

    Ok(())
}

/// Adds a supported Antigravity stub that requires structured output flags and
/// a prompt argument immediately after `--print`.
fn seed_antigravity_argv_prompt_project(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let stub_agent_path = env.stub_bin.join("agy");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'agy 1.2.0\n'; exit 0; fi
output_format=''
schema=''
prompt=''
valid_print=false
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output-format)
      output_format=$2
      shift 2
      ;;
    --json-schema)
      schema=$2
      shift 2
      ;;
    --print)
      if [ "$#" -eq 2 ]; then
        prompt=$2
        valid_print=true
      fi
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
if [ "$valid_print" != true ]; then
  answer='Antigravity invocation did not satisfy the CLI contract: print.'
elif [ "$output_format" != stream-json ]; then
  answer='Antigravity invocation did not satisfy the CLI contract: output.'
elif ! printf '%s' "$schema" | grep -q '"required"' ||
     ! printf '%s' "$schema" | grep -q '"answer"'; then
  answer='Antigravity invocation did not satisfy the CLI contract: schema.'
elif ! printf '%s' "$prompt" | grep -q 'Hi from Agentty argv'; then
  answer='Antigravity invocation did not satisfy the CLI contract: prompt.'
else
  answer='Antigravity received the argv prompt.'
fi
printf '%s\n' '{"event":"reasoning","value":"Reading the Agentty prompt"}'
printf '{"event":"result","result":{"conversation_id":"stub-conversation","status":"SUCCESS","response":"{\\"answer\\":\\"%s\\",\\"questions\\":[],\\"review_comment_outcomes\\":[],\\"summary\\":{\\"session\\":\\"Antigravity prompt delivery verified.\\",\\"turn\\":\\"Verified argv prompt delivery.\\"}}","error":"","duration_seconds":0.1,"num_turns":1,"usage":{"input_tokens":7,"output_tokens":6,"thinking_tokens":1,"cache_read_tokens":2,"total_tokens":16}}}\n' "$answer"
"#;
    std::fs::write(&stub_agent_path, script)?;

    #[cfg(unix)]
    std::fs::set_permissions(&stub_agent_path, std::fs::Permissions::from_mode(0o755))?;

    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "antigravity"),
            ("DefaultSmartModel", "gemini-3.1-pro-preview"),
        ],
    )?;

    Ok(())
}

/// Seeds a review-ready Antigravity session with an oversized replay
/// transcript and a valid worktree.
fn seed_antigravity_oversized_replay_project(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_antigravity_argv_prompt_project(env)?;
    common::seed_session(
        env,
        SessionSeed::regular(
            ANTIGRAVITY_OVERSIZED_REPLAY_SESSION_ID,
            "gemini-3.1-pro-preview",
            "main",
            "Review",
        )
        .with_title("Oversized Antigravity replay"),
    )?;

    let runtime = common::seed_runtime()?;
    let oversized_answer = "x".repeat(40 * 1024);
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                ANTIGRAVITY_OVERSIZED_REPLAY_SESSION_ID,
                SessionMessageKind::UserPrompt,
                "Complete the initial task.",
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                ANTIGRAVITY_OVERSIZED_REPLAY_SESSION_ID,
                SessionMessageKind::AssistantAnswer,
                &oversized_answer,
            )
            .await?;
        database
            .sessions()
            .update_session_prompt(
                ANTIGRAVITY_OVERSIZED_REPLAY_SESSION_ID,
                "Complete the initial task.",
            )
            .await
    })?;

    let session_worktree = test_support::session_folder(
        &env.agentty_root.join("wt"),
        ANTIGRAVITY_OVERSIZED_REPLAY_SESSION_ID,
    );
    let session_worktree = session_worktree.to_string_lossy();
    run_git(
        &env.workdir,
        &["worktree", "add", "-b", "wt/antigrav", &session_worktree],
    )?;

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
                Some(agentty::domain::review::FocusedReviewStatus::Ready),
                Some("42".to_string()),
                Some(
                    "## Review\n\n### Project Impact\n\n- Persisted focused review \
                     finding.\n\n### Suggestions\n\n- None."
                        .to_string(),
                ),
            )
            .await?;
        database
            .sessions()
            .update_session_summary(
                "review-shortcut-0001",
                r#"{"turn":"Persisted the focused review.","session":"Review output survives session reloads."}"#,
            )
            .await
    })?;

    Ok(())
}

/// Seeds one persisted focused review plus a second project so the review can
/// be restored after switching away from its owning project and back.
fn seed_cross_project_focused_review(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_persisted_focused_review(env)?;
    common::seed_mru_first_second_project(env)
}

/// Seeds two review-ready sessions with distinct persisted focused reviews so
/// switching away and back can verify cache-backed output restoration.
fn seed_sessions_with_persisted_focused_reviews(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    // The session list orders by `updated_at DESC, created_at DESC, id`, and
    // both timestamps have one-second resolution. Seed `second-review-0001`
    // first so `review-shortcut-0001` is row 0 under either outcome: when both
    // seeds land in the same second the `id` tiebreak selects it, and when a
    // second boundary falls between them its newer `updated_at` selects it.
    // Seeding in the other order makes row 0 depend on that boundary.
    common::seed_session(
        env,
        SessionSeed::regular("second-review-0001", "gpt-5.6-sol", "main", "Review")
            .with_title("Second persisted review"),
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_focused_review(
                "second-review-0001",
                Some(agentty::domain::review::FocusedReviewStatus::Ready),
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

    seed_review_ready_session_with_persisted_focused_review(env)?;

    Ok(())
}

/// Seeds one running session so `Ctrl+c` can exercise the turn-stop path
/// without needing a live agent backend.
fn seed_running_stop_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(RUNNING_STOP_SESSION_ID, "gpt-5.6-sol", "main", "InProgress")
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
        SessionSeed::regular(REBASING_QUEUE_SESSION_ID, "gpt-5.6-sol", "main", "Rebasing")
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

/// Seeds an unrelated selected row and a prompt-aware agent stub for the
/// question-refresh regression.
fn seed_question_refresh_project(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(
            QUESTION_REFRESH_SELECTED_SESSION_ID,
            "claude-haiku-4-5-20251001",
            "main",
            "Done",
        )
        .with_title("Other session"),
    )?;

    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
input=$(cat)
case "$input" in
  *"Generate a concise, commit-style title"*)
    sleep 2
    result='{{\"answer\":\"{QUESTION_REFRESH_TITLE}\",\"questions\":[],\"review_comment_outcomes\":[],\"summary\":null}}'
    ;;
  *"Clarifications:"*)
    result='{{\"answer\":\"{QUESTION_REFRESH_FINAL_ANSWER}\",\"questions\":[],\"review_comment_outcomes\":[],\"summary\":null}}'
    ;;
  *)
    result='{{\"answer\":\"Need two clarifications.\",\"questions\":[{{\"text\":\"{FIRST_QUESTION_TEXT}\",\"options\":[\"Yes\",\"No\"]}},{{\"text\":\"{SECOND_QUESTION_TEXT}\",\"options\":[\"Unit\",\"Integration\"]}}],\"review_comment_outcomes\":[],\"summary\":null}}'
    ;;
esac
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' "{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"$result\",\"usage\":{{\"input_tokens\":5,\"output_tokens\":9}}}}"
"#
    );
    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "claude"),
            ("DefaultSmartModel", "claude-haiku-4-5-20251001"),
            ("DefaultFastAgent", "claude"),
            ("DefaultFastModel", "claude-haiku-4-5-20251001"),
        ],
    )?;

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
            let query = sqlx::query!(
                r"
INSERT INTO project_setting (project_id, name, value)
VALUES (?, ?, ?)
ON CONFLICT(project_id, name) DO UPDATE SET value = excluded.value
",
                project_id,
                setting_name,
                setting_value
            );
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

/// Installs a prompt-aware Claude stub for the full orchestration feature
/// journey: plan, approval, concurrent child completion, and roll-up.
fn install_orchestration_claude_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
input=$(cat)
case "$input" in
  *"Generate a concise, commit-style title"*)
    result='{\"answer\":\"Coordinate parallel work\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[],\"summary\":null}'
    ;;
  *"The user or coordinator message follows:"*"Implement the protocol review suggestions"*)
    result='{\"answer\":\"I will continue the protocol worker with the review findings.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[{\"task_key\":\"protocol\",\"title\":\"Protocol worker\",\"prompt\":\"Implement the protocol findings on the same worker branch.\",\"touched_areas\":[\"crates/ag-protocol/\"],\"acceptance_criteria\":[\"Protocol review findings are implemented and checked\"]}],\"summary\":null}'
    ;;
  *"Orchestration verification gate"*)
    result='{\"answer\":\"All workers finished. Review and merge protocol before UI.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[],\"summary\":{\"session\":\"Orchestration finished.\",\"turn\":\"Rolled up both worker results.\"},\"verification_verdicts\":[{\"reason\":\"Protocol criteria pass\",\"task_key\":\"protocol\",\"verdict\":\"pass\"},{\"reason\":\"UI criteria pass\",\"task_key\":\"ui\",\"verdict\":\"pass\"}]}'
    ;;
  *"The user or coordinator message follows:"*"Continue protocol beyond its expected areas"*)
    result='{\"answer\":\"I will route that feedback to the existing worker.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[{\"task_key\":\"protocol\",\"title\":\"Protocol worker\",\"prompt\":\"Continue protocol beyond its expected areas.\",\"touched_areas\":[\"docs/\"],\"acceptance_criteria\":[\"Apply the requested feedback\"]}],\"summary\":null}'
    ;;
  *"The user or coordinator message follows:"*"Build protocol and UI in parallel"*)
    result='{\"answer\":\"I propose independent protocol and UI workers, merged in that order.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[{\"task_key\":\"protocol\",\"title\":\"Protocol worker\",\"prompt\":\"Implement the protocol slice.\",\"touched_areas\":[\"crates/shared/\"],\"acceptance_criteria\":[\"Protocol worker completes\"]},{\"task_key\":\"ui\",\"title\":\"UI worker\",\"prompt\":\"Implement the UI slice.\",\"touched_areas\":[\"crates/shared/\"],\"acceptance_criteria\":[\"UI worker completes\"]}],\"summary\":null}'
    ;;
  *"Implement the protocol findings on the same worker branch"*)
    sleep 4
    result='{\"answer\":\"Protocol review suggestions implemented.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[],\"summary\":{\"session\":\"Protocol review suggestions implemented.\",\"turn\":\"Continued the existing worker and checked the findings.\"}}'
    ;;
  *"Continue protocol beyond its expected areas"*"Expected touched areas (planning references): [\"docs/\"]"*)
    sleep 4
    result='{\"answer\":\"Protocol feedback implemented beyond the expected areas.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[],\"summary\":{\"session\":\"Protocol feedback implemented.\",\"turn\":\"Continued the existing worker beyond its planning references.\"}}'
    ;;
  *"Task key: protocol"*)
    sleep 4
    result='{\"answer\":\"Protocol worker completed.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[],\"summary\":{\"session\":\"Protocol worker completed.\",\"turn\":\"Implemented and checked the protocol slice.\"}}'
    ;;
  *"Task key: ui"*)
    sleep 4
    result='{\"answer\":\"UI worker completed.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[],\"summary\":{\"session\":\"UI worker completed.\",\"turn\":\"Implemented and checked the UI slice.\"}}'
    ;;
  *)
    result='{\"answer\":\"Ready\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[],\"summary\":null}'
    ;;
esac
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '{"type":"result","subtype":"success","result":"%s","usage":{"input_tokens":5,"output_tokens":9}}\n' "$result"
"#;

    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Answer emitted before the queued session sync is allowed to start.
const QUEUED_SYNC_TURN_ANSWER: &str = "Running turn completed before sync";

/// Answer emitted before queued review-request creation begins.
const QUEUED_REVIEW_REQUEST_TURN_ANSWER: &str =
    "Running turn completed before review request creation";
/// Answer emitted for the chat message queued before review-request creation.
const QUEUED_REVIEW_FOLLOW_UP_ANSWER: &str = "Queued review follow-up completed before publish";
/// Answer emitted for one-shot utility prompts in the mixed FIFO scenario.
///
/// Title generation, commit-message generation, and review-request metadata
/// run as detached one-shot commands alongside session turns, so they need a
/// response that no scenario assertion looks for.
const QUEUED_FIFO_UTILITY_ANSWER: &str = "Queued FIFO helper response";

/// Installs a delayed Claude turn so the scenario can queue sync while the
/// worker is still active, optionally forcing its later validation to fail.
fn install_delayed_sync_claude_stub(
    env: &BuilderEnv,
    fail_sync_validation: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let validation_failure_marker = env.stub_bin.join("queued-sync-validation-failure");
    let mark_validation_failure = if fail_sync_validation {
        format!("touch '{}'; ", validation_failure_marker.display())
    } else {
        String::new()
    };
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
sleep 10
{mark_validation_failure}printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{QUEUED_SYNC_TURN_ANSWER}"}}]}}}}'
printf '%s\n' '{{"type":"result","subtype":"success","result":"{{\"answer\":\"{QUEUED_SYNC_TURN_ANSWER}\",\"questions\":[],\"summary\":null}}","usage":{{"input_tokens":5,"output_tokens":9}}}}'
"#
    );

    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    if fail_sync_validation {
        install_sync_validation_failure_git_stub(env, &validation_failure_marker)?;
    }

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Installs a Git wrapper that fails only the marked queued-sync validation.
fn install_sync_validation_failure_git_stub(
    env: &BuilderEnv,
    validation_failure_marker: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let git_path = env.stub_bin.join("git");
    let script = format!(
        r#"#!/bin/sh
if [ -f '{}' ] && [ "$1" = "rev-parse" ] && [ "$2" = "--is-bare-repository" ]; then
  printf '%s\n' 'forced queued-sync validation failure' >&2
  exit 1
fi
exec '{}' "$@"
"#,
        validation_failure_marker.display(),
        real_git.display()
    );
    std::fs::write(&git_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Installs delayed agent, Git, and GitHub stubs so review-request creation
/// can be queued during a live turn and observed after that turn completes.
fn install_queued_review_request_stubs(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    run_git(
        &env.workdir,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/agentty-xyz/agentty.git",
        ],
    )?;

    let claude_path = env.stub_bin.join("claude");
    let claude_script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
sleep 8
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{QUEUED_REVIEW_REQUEST_TURN_ANSWER}"}}]}}}}'
printf '%s\n' '{{"type":"result","subtype":"success","result":"{{\"answer\":\"{QUEUED_REVIEW_REQUEST_TURN_ANSWER}\",\"questions\":[],\"summary\":null}}","usage":{{"input_tokens":5,"output_tokens":9}}}}'
"#
    );
    std::fs::write(&claude_path, claude_script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let git_path = env.stub_bin.join("git");
    let git_script = format!(
        r#"#!/bin/sh
if [ "$1" = "push" ]; then
  sleep 2
  exit 0
fi
exec '{}' "$@"
"#,
        real_git.display()
    );
    std::fs::write(&git_path, git_script)?;
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
    touch "$marker_path"
    ;;
  *"pr view"*)
    printf '%s\n' '{"number":42,"title":"Queued review request","state":"OPEN","url":"https://github.com/agentty-xyz/agentty/pull/42","baseRefName":"main","headRefName":"wt/queued-review","isDraft":false,"mergeStateStatus":"CLEAN","reviewDecision":"REVIEW_REQUIRED","mergedAt":null}'
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

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Extends the queued-review stubs with enough turn latency for a deliberate
/// cancellation after the publish action has been queued.
fn install_cancelled_queued_review_request_stubs(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    install_queued_review_request_stubs(env)?;

    let claude_path = env.stub_bin.join("claude");
    let claude_script = std::fs::read_to_string(&claude_path)?;
    let delayed_script = claude_script.replacen("sleep 8", "sleep 30", 1);
    if delayed_script == claude_script {
        return Err("queued review-request stub is missing its turn delay".into());
    }
    std::fs::write(&claude_path, delayed_script)?;

    Ok(())
}

/// Extends the queued-review stubs with a distinct second agent turn so the
/// mixed FIFO feature can prove the chat message executes before publishing.
///
/// Each response is selected from the prompt on stdin rather than from
/// invocation order: Agentty runs detached one-shot utility commands (title
/// generation first of all) concurrently with the opening turn, so an
/// order-based stub can hand the delayed first-turn response to a helper
/// command and answer the real turn immediately.
fn install_fifo_queued_review_request_stubs(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    install_queued_review_request_stubs(env)?;

    let claude_path = env.stub_bin.join("claude");
    let claude_script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
emit_answer() {{
  printf '%s\n' '{{"type":"system","subtype":"init"}}'
  printf '%s\n' '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"'"$1"'"}}]}}}}'
  printf '%s\n' '{{"type":"result","subtype":"success","result":"{{\"answer\":\"'"$1"'\",\"questions\":[],\"summary\":null}}","usage":{{"input_tokens":5,"output_tokens":9}}}}'
}}
input=$(cat)
case "$input" in
  *"For this one-shot utility prompt"*)
    emit_answer '{QUEUED_FIFO_UTILITY_ANSWER}'
    ;;
  *"Review once more"*)
    emit_answer '{QUEUED_REVIEW_FOLLOW_UP_ANSWER}'
    ;;
  *"Queue mixed work"*)
    # Keep the first turn active while the PTY driver queues both items, even
    # when the E2E test group is running four scenarios concurrently.
    sleep 20
    emit_answer '{QUEUED_REVIEW_REQUEST_TURN_ANSWER}'
    ;;
  *)
    emit_answer '{QUEUED_FIFO_UTILITY_ANSWER}'
    ;;
esac
"#
    );
    std::fs::write(&claude_path, claude_script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
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
    seed_clean_review_worktree(env)?;

    let session_worktree = env.agentty_root.join("wt").join("review-s");
    std::fs::write(
        session_worktree.join("src/main.rs"),
        "fn main() {\n    println!(\"review\");\n}\n",
    )?;

    Ok(())
}

/// Seeds the review session as a real linked worktree so submitting its line
/// comment can exercise the next agent turn without an isolation warning.
fn seed_linked_review_worktree_with_diff(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_worktree = env.agentty_root.join("wt").join("review-s");
    std::fs::remove_dir(&session_worktree)?;
    let session_worktree_path = session_worktree
        .to_str()
        .ok_or("session worktree path must be valid UTF-8")?;
    run_git(
        &env.workdir,
        &["worktree", "add", session_worktree_path, "wt/review-s"],
    )?;
    std::fs::create_dir_all(session_worktree.join("src"))?;
    std::fs::write(
        session_worktree.join("src/main.rs"),
        "fn main() {\n    println!(\"review\");\n}\n",
    )?;
    run_git(&session_worktree, &["add", "."])?;
    run_git(&session_worktree, &["commit", "-m", "add main"])?;

    Ok(())
}

/// Installs a deterministic Codex app-server stub for the submitted line
/// comment turn.
fn seed_line_comment_codex_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let codex_path = env.stub_bin.join("codex");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'codex-cli 0.146.0\n'; exit 0; fi

extract_id() {
    printf '%s\n' "$1" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'
}

while IFS= read -r request; do
    case "$request" in
        *'"method":"initialize"'*)
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{}}\n' "$request_id"
            ;;
        *'"method":"thread/start"'*|*'"method":"thread/resume"'*)
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{"thread":{"id":"line-comment-thread"}}}\n' "$request_id"
            ;;
        *'"method":"turn/start"'*)
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{"turn":{"id":"line-comment-turn"}}}\n' "$request_id"
            printf '%s\n' '{"method":"turn/started","params":{"turn":{"id":"line-comment-turn"}}}'
            sleep 1
            printf '%s\n' '{"method":"item/completed","params":{"threadId":"line-comment-thread","turnId":"line-comment-turn","item":{"type":"agentMessage","id":"line-comment-answer","text":"{\"answer\":\"Line comment received.\",\"questions\":[],\"summary\":null}","phase":"final_answer"}}}'
            printf '%s\n' '{"method":"turn/completed","params":{"threadId":"line-comment-thread","turn":{"id":"line-comment-turn","status":"completed","items":[]}}}'
            ;;
    esac
done
"#;
    std::fs::write(&codex_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&codex_path, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Installs a deterministic review provider so the automatic post-turn review
/// reaches a visible terminal state before the final feature capture.
fn seed_line_comment_review_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '%s\n' '{"type":"result","subtype":"success","result":"{\"answer\":\"No review findings.\",\"questions\":[],\"summary\":null}","usage":{"input_tokens":5,"output_tokens":9}}'
"#;
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

/// Seeds the review session folder as a clean Git worktree.
fn seed_clean_review_worktree(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
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

    Ok(())
}

/// Installs a tmux stub that edits the clean review worktree when opened.
fn install_worktree_edit_tmux_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let edited_file = env
        .agentty_root
        .join("wt")
        .join("review-s")
        .join("src/main.rs")
        .to_string_lossy()
        .replace('\'', "'\\''");
    let tmux_path = env.stub_bin.join("tmux");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "new-window" ]; then
  printf '%s\n' 'external worktree edit' > '{edited_file}'
  printf '%s\n' '@42'
fi
"#
    );
    std::fs::write(&tmux_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&tmux_path, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Adds a changed file below a single-child folder chain for compact-tree
/// rendering coverage.
fn seed_review_session_with_compact_diff_tree(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_review_request(env)?;

    let session_worktree = env.agentty_root.join("wt").join("review-s");
    let nested_directory = session_worktree.join("src/app/session");
    let nested_file = nested_directory.join("handler.rs");
    std::fs::create_dir_all(&nested_directory)?;
    std::fs::write(&nested_file, "fn handle() {\n}\n")?;
    run_git(&session_worktree, &["add", "."])?;
    run_git(&session_worktree, &["commit", "-m", "add nested handler"])?;
    std::fs::write(
        &nested_file,
        "fn handle() {\n    println!(\"review\");\n}\n",
    )?;

    Ok(())
}

/// Seeds a clean review session whose worktree-open action creates an edit.
fn seed_clean_review_session_with_worktree_edit(env: &BuilderEnv) -> E2eResult {
    seed_clean_review_ready_session(env)?;
    install_worktree_edit_tmux_stub(env)
}

/// Seeds a review diff whose external driver stays busy long enough to prove
/// that the TUI renders and accepts cancellation before Git completes.
fn seed_slow_review_diff(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;

    let session_worktree = env.agentty_root.join("wt").join("review-s");
    let slow_diff_driver = session_worktree.join("slow-diff.sh");
    std::fs::write(&slow_diff_driver, "#!/bin/sh\nsleep 3\nexit 0\n")?;
    #[cfg(unix)]
    std::fs::set_permissions(&slow_diff_driver, std::fs::Permissions::from_mode(0o755))?;
    let slow_diff_driver = slow_diff_driver
        .to_str()
        .ok_or("slow diff driver path must be valid UTF-8")?;
    run_git(
        &session_worktree,
        &["config", "diff.external", slow_diff_driver],
    )
}

/// Seeds enough changed lines to demonstrate right-pane cursor navigation and
/// viewport scrolling without a live agent backend.
fn seed_scrollable_diff_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;

    let session_worktree = env.agentty_root.join("wt").join("review-s");
    let changed_lines = (0..80)
        .map(|line_index| format!("    println!(\"changed line {line_index:02}\");"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        session_worktree.join("src/main.rs"),
        format!("fn main() {{\n{changed_lines}\n}}\n"),
    )?;

    Ok(())
}

/// Seeds a review-ready worktree whose only change is previewable markdown.
fn seed_markdown_diff_preview(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;

    let session_worktree = env.agentty_root.join("wt").join("review-s");
    run_git(&session_worktree, &["init", "-b", "main"])?;
    run_git(
        &session_worktree,
        &["config", "user.email", "test@test.com"],
    )?;
    run_git(&session_worktree, &["config", "user.name", "Test"])?;
    std::fs::create_dir_all(session_worktree.join("docs"))?;
    std::fs::write(session_worktree.join("docs/日本.md"), "# Before\n")?;
    run_git(&session_worktree, &["add", "."])?;
    run_git(&session_worktree, &["commit", "-m", "init"])?;
    std::fs::write(
        session_worktree.join("docs/日本.md"),
        concat!(
            "# Rendered Markdown Preview\n\n",
            "| Mode | Output |\n| --- | --- |\n| Preview | Markdown |\n\n",
            "```mermaid\ngraph TD\nSource[Source] --> Preview[Preview]\n```\n",
        ),
    )?;

    Ok(())
}

/// Seeds a review-ready fork source whose persisted and actual worktree diff
/// must not be inherited by the forked branch-tip worktree.
fn seed_dirty_fork_source_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)
}

/// Seeds a review-ready session whose only worktree change is binary and
/// whose persisted diff presence remains conservatively unknown.
fn seed_binary_diff_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(BINARY_DIFF_SESSION_ID, "gpt-5.6-sol", "main", "Review")
            .with_title("Binary diff session"),
    )?;

    let session_worktree =
        test_support::session_folder(&env.agentty_root.join("wt"), BINARY_DIFF_SESSION_ID);
    std::fs::create_dir_all(&session_worktree)?;
    run_git(&session_worktree, &["init", "-b", "main"])?;
    run_git(
        &session_worktree,
        &["config", "user.email", "test@test.com"],
    )?;
    run_git(&session_worktree, &["config", "user.name", "Test"])?;
    std::fs::write(session_worktree.join("asset.bin"), [0_u8, 1, 2, 3])?;
    run_git(&session_worktree, &["add", "."])?;
    run_git(&session_worktree, &["commit", "-m", "init"])?;
    std::fs::write(session_worktree.join("asset.bin"), [0_u8, 4, 5, 6, 7])?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .mark_session_diff_unknown(BINARY_DIFF_SESSION_ID)
            .await
    })?;

    Ok(())
}

/// Runs one git command in `working_directory`, returning an error with stderr
/// detail when git fails and discarding its stdout.
fn run_git(working_directory: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    run_git_stdout(working_directory, args)?;

    Ok(())
}

/// Runs one git command in `working_directory` and returns trimmed stdout.
///
/// The child is killed and reported as an error once `GIT_COMMAND_TIMEOUT`
/// elapses so a stalled git process fails the test instead of hanging CI.
fn run_git_stdout(
    working_directory: &Path,
    args: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(working_directory)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Drain both pipes from reader threads so a chatty git command cannot fill
    // a pipe buffer and block while the timeout loop polls for exit.
    let mut child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("git stdout pipe missing"))?;
    let mut child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("git stderr pipe missing"))?;
    let stdout_reader = thread::spawn(move || {
        let mut stdout = Vec::new();
        child_stdout.read_to_end(&mut stdout).map(|_| stdout)
    });
    let stderr_reader = thread::spawn(move || {
        let mut stderr = Vec::new();
        child_stderr.read_to_end(&mut stderr).map(|_| stderr)
    });

    let deadline = Instant::now() + GIT_COMMAND_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }

        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;

            return Err(std::io::Error::other(format!(
                "git {} timed out after {GIT_COMMAND_TIMEOUT:?}",
                args.join(" ")
            ))
            .into());
        }

        thread::sleep(GIT_COMMAND_POLL_INTERVAL);
    };

    let stdout = stdout_reader
        .join()
        .map_err(|_| std::io::Error::other("git stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| std::io::Error::other("git stderr reader panicked"))??;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&stderr)
        ))
        .into());
    }

    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
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
  *"addPullRequestReviewThreadReply"*)
    printf '%s\n' '{"data":{"addPullRequestReviewThreadReply":{"comment":{"id":"reply-1"}}}}'
    ;;
  *"resolveReviewThread"*)
    printf '%s\n' '{"data":{"resolveReviewThread":{"thread":{"id":"thread-inline","isResolved":true}}}}'
    ;;
  *"reviewThreads(first:"*)
    cat <<'JSON'
[{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[{"id":"thread-inline","diffSide":"RIGHT","isOutdated":false,"isResolved":false,"line":2,"path":"src/main.rs","startLine":1,"subjectType":"LINE","comments":{"nodes":[{"author":{"login":"alice"},"body":"<!-- hidden reviewer note --><p>Please <strong>explain</strong> why this review output is needed.<br>Use <code>stdout</code> context.</p>"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}},{"id":"thread-file","diffSide":"RIGHT","isOutdated":false,"isResolved":false,"line":null,"path":"src/main.rs","startLine":null,"subjectType":"FILE","comments":{"nodes":[{"author":{"login":"bob"},"body":"Please review the whole file."}],"pageInfo":{"hasNextPage":false,"endCursor":null}}},{"id":"thread-outdated","diffSide":"RIGHT","isOutdated":true,"isResolved":false,"line":2,"path":"old.rs","startLine":null,"subjectType":"LINE","comments":{"nodes":[{"author":{"login":"erin"},"body":"This comment refers to an earlier diff."}],"pageInfo":{"hasNextPage":false,"endCursor":null}}},{"id":"thread-resolved","diffSide":"RIGHT","isOutdated":false,"isResolved":true,"line":3,"path":"src/main.rs","startLine":null,"subjectType":"LINE","comments":{"nodes":[{"author":{"login":"dana"},"body":"This thread is complete."}],"pageInfo":{"hasNextPage":false,"endCursor":null}}},{"id":"thread-resolved-outdated","diffSide":"LEFT","isOutdated":true,"isResolved":true,"line":4,"path":"ro.rs","startLine":null,"subjectType":"LINE","comments":{"nodes":[{"author":{"login":"frank"},"body":"This resolved thread refers to an earlier diff."}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}]
JSON
    ;;
  *"comments(first:"*)
    cat <<'JSON'
[{"data":{"repository":{"pullRequest":{"comments":{"nodes":[{"author":{"login":"carol"},"body":"Thanks for documenting the behavior."}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}]
JSON
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

/// Seeds the linked-review fixture with a delayed Claude turn so the feature
/// scenario can observe the review-resolution loader in progress.
fn seed_review_comment_agent_resolution(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_review_request(env)?;
    let session_worktree = env.agentty_root.join("wt").join("review-s");
    std::fs::remove_dir_all(&session_worktree)?;
    let session_worktree_path = session_worktree.to_string_lossy().into_owned();
    run_git(
        &env.workdir,
        &[
            "worktree",
            "add",
            session_worktree_path.as_str(),
            "wt/review-s",
        ],
    )?;
    run_git(
        &session_worktree,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/agentty-xyz/agentty.git",
        ],
    )?;
    std::fs::create_dir_all(session_worktree.join("src"))?;
    std::fs::write(
        session_worktree.join("src/main.rs"),
        "fn main() {\n    println!(\"review\");\n}\n",
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_model("review-shortcut-0001", "claude-haiku-4-5-20251001")
            .await?;
        database
            .sessions()
            .append_session_message(
                "review-shortcut-0001",
                SessionMessageKind::UserPrompt,
                REVIEW_HISTORY_PROMPT_TEXT,
            )
            .await
    })?;

    let claude_path = env.stub_bin.join("claude");
    std::fs::write(
        &claude_path,
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
sleep 10
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Processed the selected review threads."}]}}'
printf '%s\n' '{"type":"result","subtype":"success","result":"{\"answer\":\"Processed the selected review threads.\",\"questions\":[],\"review_comment_outcomes\":[{\"reply\":\"Added the explanation.\",\"resolution\":\"fixed\",\"thread_id\":\"thread-inline\"},{\"reply\":\"The whole-file change is not needed because the scoped update covers the request.\",\"resolution\":\"fixed\",\"thread_id\":\"thread-file\"}],\"summary\":null}","usage":{"input_tokens":5,"output_tokens":9}}'
"#,
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Seeds a merged GitHub review response and delays runtime worktree removal
/// so the feature scenario can prove terminal rendering stays responsive.
fn seed_slow_merged_review_request_status(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_review_request(env)?;

    let sync_origin = env.agentty_root.join("sync-origin.git");
    std::fs::create_dir_all(&sync_origin)?;
    run_git(&sync_origin, &["init", "--bare", "."])?;
    let sync_origin_path = sync_origin.to_string_lossy().into_owned();
    run_git(
        &env.workdir,
        &["remote", "add", "origin", sync_origin_path.as_str()],
    )?;
    run_git(&env.workdir, &["push", "--set-upstream", "origin", "main"])?;

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

/// Seeds a merged parent and merged stacked child whose review target still
/// names the parent branch, plus a remote for the manual main sync.
fn seed_merged_stacked_review_requests(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("stack-parent-0001", "gpt-5.6-sol", "main", "Merged")
            .with_title("Merged stack parent"),
    )?;
    common::seed_session(
        env,
        SessionSeed::stacked_draft(
            "stack-child-0001",
            "gpt-5.6-sol",
            "wt/stack-pa",
            "Merged",
            "stack-parent-0001",
        )
        .with_title("Merged stack child"),
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        let parent_review_request = ReviewRequest {
            last_refreshed_at: 55,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/stack-pa".to_string(),
                state: ReviewRequestState::Merged,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Merged stack parent".to_string(),
                web_url: "https://github.com/example/project/pull/42".to_string(),
            },
        };
        let child_review_request = ReviewRequest {
            last_refreshed_at: 55,
            summary: ReviewRequestSummary {
                display_id: "#43".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/stack-ch".to_string(),
                state: ReviewRequestState::Merged,
                status_summary: None,
                target_branch: "wt/stack-pa".to_string(),
                title: "Merged stack child".to_string(),
                web_url: "https://github.com/example/project/pull/43".to_string(),
            },
        };

        database
            .reviews()
            .update_session_review_request("stack-parent-0001", Some(parent_review_request))
            .await?;
        database
            .reviews()
            .update_session_review_request("stack-child-0001", Some(child_review_request))
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("stack-pa"))?;
    std::fs::create_dir_all(env.agentty_root.join("wt").join("stack-ch"))?;
    let sync_origin = env.agentty_root.join("sync-origin.git");
    std::fs::create_dir_all(&sync_origin)?;
    run_git(&sync_origin, &["init", "--bare", "."])?;
    let sync_origin_path = sync_origin.to_string_lossy().into_owned();
    run_git(
        &env.workdir,
        &["remote", "add", "origin", sync_origin_path.as_str()],
    )?;
    run_git(&env.workdir, &["push", "--set-upstream", "origin", "main"])?;

    install_delayed_worktree_remove_stub(env, 1)
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

/// Seeds an unmaterialized stacked draft whose parent worktree contains a new
/// file that is absent from the project checkout.
fn seed_stacked_at_lookup_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let parent_session_id = "atparent-0001";
    let child_session_id = "atchildx-0001";
    common::seed_session(
        env,
        SessionSeed::regular(parent_session_id, "gpt-5.6-sol", "main", "Review")
            .with_title("Parent with lookup file"),
    )?;
    common::seed_session(
        env,
        SessionSeed::stacked_draft(
            child_session_id,
            "gpt-5.6-sol",
            "wt/atparent",
            "Draft",
            parent_session_id,
        )
        .with_title("Stacked lookup child"),
    )?;

    let parent_worktree = env.agentty_root.join("wt").join("atparent");
    std::fs::create_dir_all(&parent_worktree)?;
    std::fs::write(
        parent_worktree.join("parent_lookup_target.txt"),
        "parent lookup target\n",
    )?;

    Ok(())
}

/// Seeds one done session whose merged commit hash can drive the continuation
/// draft flow.
fn seed_done_session_for_continuation(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let merged_commit_hash = "704de31d0f4b5a1234567890abcdef1234567890";
    common::seed_session(
        env,
        SessionSeed::regular("done-continue-0001", "gpt-5.6-sol", "main", "Done")
            .with_title("Continue terminal session"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        let review_request = ReviewRequest {
            last_refreshed_at: 55,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/done-continue".to_string(),
                state: ReviewRequestState::Merged,
                status_summary: Some("Merged".to_string()),
                target_branch: "main".to_string(),
                title: "Completed review request".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        };
        database
            .sessions()
            .update_session_merged_commit_hash(
                "done-continue-0001",
                Some(merged_commit_hash.to_string()),
            )
            .await?;
        database
            .reviews()
            .update_session_review_request("done-continue-0001", Some(review_request))
            .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    Ok(())
}

/// Seeds one canceled session whose saved summary can drive the continuation
/// draft flow.
fn seed_canceled_session_for_continuation(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("canceled-continue-0001", "gpt-5.6-sol", "main", "Canceled")
            .with_title("Continue canceled session"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_summary(
                "canceled-continue-0001",
                "# Summary\n\nResume the remaining work.",
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
        SessionSeed::regular(LOADER_SESSION_ID, "gpt-5.6-sol", "main", "InProgress")
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
        SessionSeed::regular(SESSION_ID, "gpt-5.6-sol", "main", "Review")
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
                assertion::assert_not_visible(&empty_frame, "MERGE QUEUE");
                assertion::assert_not_visible(&empty_frame, "ACTIVE");
                assertion::assert_not_visible(&empty_frame, "ARCHIVE");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "ACTIVE —— 1", &full);
                assertion::assert_not_visible(frame, "MERGE QUEUE");
                assertion::assert_not_visible(frame, "ARCHIVE");
                assertion::assert_not_visible(frame, "No sessions");
            },
        )?;

    Ok(())
}

/// Verify that a later narrow follow-up is titled from stable session context
/// instead of becoming the whole visible session title.
#[test]
fn test_session_title_uses_stable_context() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_title_uses_stable_context")
        .with_git()
        .setup(seed_session_title_candidate_project)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Improve session title generation.")
                    .wait_for_text("Improve session title generation.", 3000)
                    .press_key("Enter")
                    .wait_for_text(
                        "The session title workflow is ready for a focused follow-up.",
                        30000,
                    )
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Also reject punctuation-only copies.")
                    .wait_for_text("Also reject punctuation-only copies.", 3000)
                    .press_key("Enter")
                    .wait_for_text("Follow-up complete. No files were changed.", 30000)
                    .wait_for_text("Stabilize session title generation", 30000)
                    .capture_labeled(
                        "stable_context_title",
                        "The session title preserves the durable overall goal",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Stabilize session title generation",
                    &full,
                );
                assertion::assert_text_in_region(
                    frame,
                    "Follow-up complete. No files were changed.",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Verify that a session branch conflicting with its stored base is marked in
/// both the Sessions list and the open session header.
#[test]
fn test_session_merge_conflict_alert() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_merge_conflict_alert")
        .with_git()
        .setup(seed_merge_conflict_session)
        .zola(
            "See merge conflicts before syncing",
            "Agentty marks sessions whose branch conflicts with its base branch in the list and \
             session view.",
            45,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("[merge conflict]", 10000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_list_alert",
                        "The conflicting session is marked beside its title",
                    )
                    .press_key("Enter")
                    .wait_for_text("Merge conflict with main", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_view_alert",
                        "The session header names the conflicting base branch",
                    )
            },
            |frame, report| {
                let list_frame = common::frame_from_capture(&report.captures[0]);
                let list_region = Region::full(list_frame.cols(), list_frame.rows());
                assertion::assert_text_in_region(&list_frame, "[merge conflict]", &list_region);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Merge conflict with main", &full);
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

/// Verify that schema-validated Claude results render their final answer
/// instead of leaving a transient structured-output tool event in chat.
#[test]
fn test_claude_structured_output_response() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("claude_structured_output_response")
        .with_git()
        .setup(seed_claude_structured_output_project)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Return a structured response")
                    .wait_for_text("Return a structured response", 3000)
                    .press_key("Enter")
                    .wait_for_text(CLAUDE_STRUCTURED_RESPONSE_TEXT, 30000)
                    .capture_labeled(
                        "structured_response",
                        "Claude schema-validated response renders in chat",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, CLAUDE_STRUCTURED_RESPONSE_TEXT, &full);
                assertion::assert_not_visible(
                    frame,
                    "Agent output did not match the required JSON schema",
                );
                assertion::assert_not_visible(frame, "Working: tool use");
            },
        )?;

    Ok(())
}

/// Verify Antigravity receives the typed user prompt as the value of its
/// string-valued `--print` flag.
#[test]
fn session_antigravity_receives_argv_prompt() -> E2eResult {
    // Arrange
    FeatureTest::new("session_antigravity_argv_prompt")
        .with_git()
        .setup(seed_antigravity_argv_prompt_project)
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Hi from Agentty argv")
                    .wait_for_text("Hi from Agentty argv", 3000)
                    .press_key("Enter")
                    .wait_for_text("Antigravity received the argv prompt.", 60000)
                    .capture_labeled(
                        "antigravity_response",
                        "Antigravity response to the argv prompt",
                    )
            },
            |frame, _report| {
                // Assert
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Antigravity received the argv prompt.",
                    &full,
                );
                assertion::assert_not_visible(
                    frame,
                    "Antigravity invocation did not satisfy the CLI contract:",
                );
            },
        )?;

    Ok(())
}

/// Verify oversized resumed transcripts fail before Agentty attempts to spawn
/// Antigravity with an unsafe command argument.
#[test]
fn session_antigravity_rejects_oversized_replay_prompt() -> E2eResult {
    // Arrange
    FeatureTest::new("session_antigravity_oversized_replay")
        .with_git()
        .setup(seed_antigravity_oversized_replay_project)
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("Continue work")
                    .wait_for_text("Continue work", 3000)
                    .press_key("Enter")
                    .wait_for_text("32768-byte", 30000)
                    .capture_labeled(
                        "oversized_replay_error",
                        "Oversized Antigravity replay is rejected safely",
                    )
            },
            |frame, _report| {
                // Assert
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "32768-byte", &full);
                assertion::assert_text_in_region(frame, "Antigravity prompt is", &full);
                assertion::assert_not_visible(frame, "Antigravity received the argv prompt.");
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
                    .wait_for_text("gpt-5.6-sol [medium]", 5000)
                    .capture_labeled(
                        "model_reasoning",
                        "Session row model column with reasoning level",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gpt-5.6-sol [medium]", &full);
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
                    .wait_for_text("Running session stop", 5000)
                    .compose(&common::switch_to_tab("Settings"))
                    .press_key("j")
                    .press_key("j")
                    .press_key("j")
                    .press_key("Enter")
                    .press_key("Enter")
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("[xhigh]", 5000)
                    .press_key("BackTab")
                    .wait_for_text("gpt-5.6-sol [high]", 5000)
                    .capture_labeled(
                        "active_reasoning",
                        "Existing session retains persisted reasoning",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gpt-5.6-sol [high]", &full);
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
                    .wait_for_text("Agent: codex  Model: gpt-5.6-sol", 5000)
                    .capture_labeled(
                        "agent_model_header",
                        "Session chat header showing agent before model",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Agent: codex  Model: gpt-5.6-sol", &full);
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
                    .find_text_in_region("gpt-5.6-sol", &selected_row_region)
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
                assertion::assert_text_in_region(
                    frame,
                    "@crates/agentty/src/ui/markdown.rs",
                    &full,
                );
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
                    .wait_for_text("≡ queued ›", 5000)
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
                assertion::assert_text_in_region(&queued_frame, "≡ queued ›", &queued_full);
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
                assertion::assert_text_in_region(
                    &after_first_frame,
                    "≡ queued ›",
                    &after_first_full,
                );
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
                    .wait_for_text("≡ queued ›", 5000)
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
                assertion::assert_text_in_region(frame, "≡ queued ›", &full);
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
        .setup(|env| install_delayed_sync_claude_stub(env, false))
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
                    .wait_for_text("rebase onto the base branch after this turn", 5000)
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
                    "≡ sync — rebase onto the base branch after this turn",
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
                    .find_text("rebase onto the base branch after this turn")
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
                assertion::assert_not_visible(frame, "≡ sync —");

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

/// Verify stopping an active turn also removes its canceled queued-sync row
/// without promoting the skipped command to active rebase work.
#[test]
fn session_queued_sync_clears_when_turn_is_cancelled() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_queued_sync_cancelled_with_turn")
        .with_git()
        .setup(|env| install_delayed_sync_claude_stub(env, false))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Cancel the turn and queued sync")
                    .wait_for_text("Cancel the turn and queued sync", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .press_key("r")
                    .wait_for_text("rebase onto the base branch after this turn", 5000)
                    .capture_labeled(
                        "queued_sync_before_turn_cancel",
                        "Sync waits behind the active turn before cancellation",
                    )
                    .press_key("ctrl+c")
                    .eventually(
                        Duration::from_secs(10),
                        Duration::from_millis(100),
                        |frame| {
                            let full = Region::full(frame.cols(), frame.rows());
                            assertion::match_text_in_region(frame, "Enter: reply", &full)?;

                            assertion::match_not_visible(frame, "≡ sync —")
                        },
                    )
                    .capture_labeled(
                        "queued_sync_cleared_after_turn_cancel",
                        "Canceled queued sync disappears without starting rebase",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(
                    &queued_frame,
                    "≡ sync — rebase onto the base branch after this turn",
                    &queued_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "≡ sync —");
                assertion::assert_not_visible(frame, "Rebasing...");
                assertion::assert_not_visible(frame, "[Sync] Successfully synced");
                assertion::assert_not_visible(frame, QUEUED_SYNC_TURN_ANSWER);
            },
        )?;

    Ok(())
}

/// Verify queued sync validation failures replace waiting state with a
/// durable error without briefly presenting the sync as active work.
#[test]
fn session_queued_sync_validation_failure_is_visible() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_queued_sync_validation_failure")
        .with_git()
        .setup(|env| install_delayed_sync_claude_stub(env, true))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Fail queued sync validation")
                    .wait_for_text("Fail queued sync validation", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .press_key("r")
                    .wait_for_text("rebase onto the base branch after this turn", 5000)
                    .capture_labeled(
                        "queued_sync_waiting",
                        "Sync remains queued behind the active turn",
                    )
                    .wait_for_text(QUEUED_SYNC_TURN_ANSWER, 30000)
                    .wait_for_text("[Sync Error] Session isolation violation", 10000)
                    .wait_for_text("Enter: reply", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "queued_sync_validation_failed",
                        "Validation failure replaces queued sync with a durable error",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(
                    &queued_frame,
                    "≡ sync — rebase onto the base branch after this turn",
                    &queued_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, QUEUED_SYNC_TURN_ANSWER, &full);
                assertion::assert_text_in_region(
                    frame,
                    "[Sync Error] Session isolation violation",
                    &full,
                );
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "≡ sync —");
                assertion::assert_not_visible(frame, "Rebasing...");
                let answer_row = frame
                    .find_text(QUEUED_SYNC_TURN_ANSWER)
                    .first()
                    .expect("missing completed turn answer")
                    .rect
                    .row;
                let error_row = frame
                    .find_text("[Sync Error] Session isolation violation")
                    .first()
                    .expect("missing queued sync validation error")
                    .rect
                    .row;
                assert!(answer_row < error_row);
            },
        )?;

    Ok(())
}

/// Verify review-request creation queues behind a running turn, begins only
/// after the answer is complete, and records the created forge link.
#[test]
fn review_request_creation_queues_during_running_turn() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_queued_creation")
        .with_git()
        .setup(install_queued_review_request_stubs)
        .zola(
            "Queued review-request creation",
            "Queue review-request creation behind a running session turn and publish it next.",
            41,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Queue the review request")
                    .wait_for_text("Queue the review request", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .wait_for_text("p: PR", 5000)
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .press_key("Enter")
                    .wait_for_text("≡ review request — publish after this turn", 5000)
                    .viewing_pause_ms(1200)
                    .capture_labeled(
                        "review_request_queued",
                        "Review-request creation queued behind the active turn",
                    )
                    .wait_for_text(QUEUED_REVIEW_REQUEST_TURN_ANSWER, 30000)
                    .wait_for_text("Publishing review request...", 5000)
                    .capture_labeled(
                        "review_request_started",
                        "Queued review-request creation starts after the turn completes",
                    )
                    .wait_for_text("[Review Request] Created PR", 15000)
                    .capture_labeled(
                        "review_request_created",
                        "Created review-request link recorded after the completed turn",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(
                    &queued_frame,
                    "≡ review request — publish after this turn",
                    &queued_full,
                );
                assertion::assert_text_in_region(
                    &queued_frame,
                    "Queue the review request",
                    &queued_full,
                );
                assertion::assert_text_in_region(&queued_frame, "Ctrl+c: stop", &queued_full);

                let started_frame = common::frame_from_capture(&report.captures[1]);
                let started_full = Region::full(started_frame.cols(), started_frame.rows());
                assertion::assert_text_in_region(
                    &started_frame,
                    QUEUED_REVIEW_REQUEST_TURN_ANSWER,
                    &started_full,
                );
                assertion::assert_text_in_region(
                    &started_frame,
                    "Publishing review request...",
                    &started_full,
                );
                assertion::assert_not_visible(&started_frame, "≡ review request —");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42",
                    &full,
                );
                assertion::assert_not_visible(frame, "≡ review request —");
            },
        )?;

    Ok(())
}

/// Verify stopping an active turn also removes its canceled queued
/// review-request row without promoting the skipped command to publish work.
#[test]
fn review_request_queued_creation_clears_when_turn_is_cancelled() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_queued_creation_cancelled_with_turn")
        .with_git()
        .setup(install_cancelled_queued_review_request_stubs)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Cancel the turn and queued review request")
                    .wait_for_text("Cancel the turn and queued review request", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .wait_for_text("p: PR", 5000)
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .press_key("Enter")
                    .wait_for_text("≡ review request — publish after this turn", 5000)
                    .capture_labeled(
                        "queued_review_request_before_turn_cancel",
                        "Review-request creation waits behind the active turn",
                    )
                    .press_key("ctrl+c")
                    .eventually(
                        Duration::from_secs(10),
                        Duration::from_millis(100),
                        |frame| {
                            let full = Region::full(frame.cols(), frame.rows());
                            assertion::match_text_in_region(frame, "Enter: reply", &full)?;

                            assertion::match_not_visible(frame, "≡ review request —")
                        },
                    )
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .capture_labeled(
                        "queued_review_request_cleared_after_turn_cancel",
                        "Canceled review-request row disappears and publish opens again",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(
                    &queued_frame,
                    "≡ review request — publish after this turn",
                    &queued_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Publish Review Request", &full);
                assertion::assert_not_visible(frame, "≡ review request —");
                assertion::assert_not_visible(frame, "Publishing review request...");
                assertion::assert_not_visible(frame, "[Review Request] Created PR");
                assertion::assert_not_visible(frame, QUEUED_REVIEW_REQUEST_TURN_ANSWER);
            },
        )?;

    Ok(())
}

/// Verify mixed chat and workflow work renders and executes from top to
/// bottom in the order each item was submitted.
#[test]
fn session_queued_work_uses_fifo_display_and_execution_order() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_queued_work_fifo")
        .with_git()
        // Tall enough to hold both completed turns plus the review-request
        // notice, so execution order never depends on transcript scrolling.
        .with_terminal_size(80, 44)
        .setup(install_fifo_queued_review_request_stubs)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Queue mixed work")
                    .wait_for_text("Queue mixed work", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .wait_for_text("p: PR", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("Review once more")
                    .wait_for_text("Review once more", 3000)
                    .press_key("Enter")
                    .wait_for_text("≡ queued › Review once more", 5000)
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .press_key("Enter")
                    .wait_for_text("≡ review request — publish after this turn", 5000)
                    .capture_labeled(
                        "mixed_queue_submission_order",
                        "Queued chat appears above the later review-request action",
                    )
                    .wait_for_text(QUEUED_REVIEW_REQUEST_TURN_ANSWER, 30000)
                    .wait_for_text(QUEUED_REVIEW_FOLLOW_UP_ANSWER, 10000)
                    .capture_labeled(
                        "mixed_queue_chat_execution_order",
                        "Active chat completes before the queued chat message",
                    )
                    .wait_for_text("[Review Request] Created PR", 15000)
                    .capture_labeled(
                        "mixed_queue_execution_order",
                        "Queued chat completes before the later review-request action",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_chat_row = queued_frame
                    .find_text("queued › Review once more")
                    .first()
                    .expect("missing queued chat message")
                    .rect
                    .row;
                let queued_publish_row = queued_frame
                    .find_text("review request — publish after this turn")
                    .first()
                    .expect("missing queued review-request action")
                    .rect
                    .row;
                assert!(queued_chat_row < queued_publish_row);

                let chat_execution_frame = common::frame_from_capture(&report.captures[1]);
                let active_answer_row = chat_execution_frame
                    .find_text(QUEUED_REVIEW_REQUEST_TURN_ANSWER)
                    .first()
                    .expect("missing active turn answer")
                    .rect
                    .row;
                let queued_answer_row = chat_execution_frame
                    .find_text(QUEUED_REVIEW_FOLLOW_UP_ANSWER)
                    .first()
                    .expect("missing queued chat answer")
                    .rect
                    .row;
                assert!(active_answer_row < queued_answer_row);

                let queued_answer_row = frame
                    .find_text(QUEUED_REVIEW_FOLLOW_UP_ANSWER)
                    .first()
                    .expect("missing queued chat answer")
                    .rect
                    .row;
                let review_request_row = frame
                    .find_text("[Review Request] Created PR")
                    .first()
                    .expect("missing created review request")
                    .rect
                    .row;
                assert!(queued_answer_row < review_request_row);
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

/// Verify post-turn auto-push progress renders below every durable notice that
/// preceded it — the earlier sync result and the completed turn's commit
/// notice — preserving workflow chronology in session output.
#[test]
fn test_session_output_chronology() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_output_chronology")
        .with_git()
        .with_terminal_size(120, 30)
        .setup(seed_published_session_output_chronology)
        .zola(
            "Chronological session output",
            "Follow sync, commit, and auto-push progress in execution order.",
            44,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("Continue after the sync")
                    .press_key("Enter")
                    .wait_for_text("Got it. What would you like me to do?", 30000)
                    .wait_for_text("Auto-pushing", 10000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "session_output_chronology",
                        "Earlier sync and commit results remain above later auto-push progress",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let view_text = frame.text_in_region(&full);
                // Row lookup goes through the rendered lines instead of
                // `find_text` because unpainted terminal cells drop the spaces
                // inside multi-word notices.
                let notice_row =
                    |needle: &str| view_text.lines().position(|line| line.contains(needle));
                for needle in [
                    "[Sync] Successfully synced",
                    "[Commit] No changes to commit.",
                    "Auto-pushing published branch",
                ] {
                    assert!(
                        notice_row(needle).is_some(),
                        "missing `{needle}` in frame:\n{view_text}"
                    );
                }

                let sync_row = notice_row("[Sync] Successfully synced").expect("sync result row");
                let commit_row =
                    notice_row("[Commit] No changes to commit.").expect("turn commit notice row");
                let auto_push_row =
                    notice_row("Auto-pushing published branch").expect("auto-push status row");

                assert!(sync_row < commit_row);
                assert!(commit_row < auto_push_row);
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

/// Verify that `Esc` leaves a clarification question active, then answering
/// one question, leaving with `q`, and reopening resumes at the next
/// unanswered question instead of restarting from the first.
#[test]
fn session_question_resume_after_leaving_to_list() -> E2eResult {
    // Arrange
    let agentty_root = Arc::new(Mutex::new(None::<PathBuf>));
    let setup_agentty_root = Arc::clone(&agentty_root);

    FeatureTest::new("session_question_resume")
        .with_git()
        .setup(move |env| {
            setup_agentty_root
                .lock()
                .expect("agentty root capture should remain available")
                .replace(env.agentty_root.clone());

            seed_question_refresh_project(env)
        })
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Other session", 5000)
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text(QUESTION_REFRESH_INITIAL_PROMPT)
                    .wait_for_text(QUESTION_REFRESH_INITIAL_PROMPT, 3000)
                    .press_key("Enter")
                    .wait_for_text("Question 1/2", 30000)
                    .wait_for_text(QUESTION_REFRESH_TITLE, 30000)
                    .wait_for_text("Tab: focus", 5000)
                    .capture_labeled(
                        "answer_focused",
                        "Title refresh completes while question mode remains active",
                    )
                    .press_key("Escape")
                    .wait_for_text(
                        "Tab: focus | Enter: send | q: sessions | Ctrl+C: end turn",
                        5000,
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
                    .press_key("Enter")
                    .wait_for_text(QUESTION_REFRESH_FINAL_ANSWER, 30000)
                    .capture_labeled(
                        "title_preserved",
                        "Clarification submission preserves title and initial prompt",
                    )
            },
            move |frame, report| {
                // Assert
                assert_question_refresh_result(frame, report);
                let agentty_root = agentty_root
                    .lock()
                    .expect("agentty root capture should remain available")
                    .clone()
                    .expect("feature setup should capture the agentty root");
                let (persisted_title, persisted_prompt) =
                    load_question_refresh_metadata(&agentty_root)
                        .expect("question metadata should remain persisted");

                assert_eq!(persisted_title.as_deref(), Some(QUESTION_REFRESH_TITLE));
                assert_eq!(persisted_prompt, QUESTION_REFRESH_INITIAL_PROMPT);
            },
        )?;

    Ok(())
}

/// Asserts question progress and refreshed-title output for the
/// question-refresh feature journey.
fn assert_question_refresh_result(frame: &TerminalFrame, report: &ProofReport) {
    let answer_focused_frame = common::frame_from_capture(&report.captures[0]);
    let answer_focused_full =
        Region::full(answer_focused_frame.cols(), answer_focused_frame.rows());
    assertion::assert_text_in_region(
        &answer_focused_frame,
        "Tab: focus | Enter: send | q: sessions | Ctrl+C: end turn",
        &answer_focused_full,
    );
    assertion::assert_text_in_region(
        &answer_focused_frame,
        QUESTION_REFRESH_TITLE,
        &answer_focused_full,
    );

    let chat_focused_frame = common::frame_from_capture(&report.captures[1]);
    let chat_focused_full = Region::full(chat_focused_frame.cols(), chat_focused_frame.rows());
    assertion::assert_text_in_region(
        &chat_focused_frame,
        "Tab: focus | j/k: scroll | q: sessions",
        &chat_focused_full,
    );
    assertion::assert_not_visible(&chat_focused_frame, "d: diff");
    assertion::assert_not_visible(&chat_focused_frame, "Ctrl+C");
    assertion::assert_text_in_region(&chat_focused_frame, "Question 1/2", &chat_focused_full);
    assertion::assert_text_in_region(&chat_focused_frame, FIRST_QUESTION_TEXT, &chat_focused_full);

    let resumed_frame = common::frame_from_capture(&report.captures[2]);
    let resumed_full = Region::full(resumed_frame.cols(), resumed_frame.rows());
    assertion::assert_text_in_region(&resumed_frame, "Question 2/2", &resumed_full);
    assertion::assert_text_in_region(&resumed_frame, SECOND_QUESTION_TEXT, &resumed_full);
    assertion::assert_not_visible(&resumed_frame, FIRST_QUESTION_TEXT);

    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(frame, QUESTION_REFRESH_TITLE, &full);
    assertion::assert_text_in_region(frame, QUESTION_REFRESH_FINAL_ANSWER, &full);
}

/// Loads the newly created question session's title and prompt from the
/// feature-test database.
fn load_question_refresh_metadata(
    agentty_root: &Path,
) -> Result<(Option<String>, String), Box<dyn std::error::Error>> {
    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = Database::open(&agentty_root.join(DB_DIR).join(DB_FILE))
            .await
            .map_err(|error| std::io::Error::other(format!("feature database: {error}")))?;
        let selected_session = database
            .sessions()
            .load_session(QUESTION_REFRESH_SELECTED_SESSION_ID)
            .await
            .map_err(|error| std::io::Error::other(format!("selected session: {error}")))?
            .ok_or_else(|| std::io::Error::other("selected session does not exist"))?;
        let project_id = selected_session
            .project_id
            .ok_or_else(|| std::io::Error::other("selected session has no project"))?;
        let question_session_id = database
            .sessions()
            .load_sessions_for_project(project_id)
            .await
            .map_err(|error| std::io::Error::other(format!("project sessions: {error}")))?
            .into_iter()
            .find(|session| session.id != QUESTION_REFRESH_SELECTED_SESSION_ID)
            .map(|session| session.id)
            .ok_or_else(|| std::io::Error::other("new question session is not listed"))?;
        let question_session = database
            .sessions()
            .load_session(&question_session_id)
            .await
            .map_err(|error| std::io::Error::other(format!("question session: {error}")))?
            .ok_or_else(|| std::io::Error::other("question session does not exist"))?;

        Result::<_, Box<dyn std::error::Error>>::Ok((
            question_session.title,
            question_session.prompt,
        ))
    })
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
        let query = sqlx::query!(
            r"
INSERT INTO setting (name, value) VALUES ('ActiveTab', 'Sessions')
ON CONFLICT(name) DO UPDATE SET value = excluded.value
"
        );
        connection.execute(query).await?;
        connection.close().await?;

        Result::<(), Box<dyn std::error::Error>>::Ok(())
    })?;

    Ok(())
}

/// Verify that `Tab` moves focus from the prompt composer to the chat
/// transcript, and that `q` returns to the sessions list while preserving the
/// typed draft for reopening.
#[test]
fn session_prompt_chat_focus_toggle() -> E2eResult {
    // Arrange
    FeatureTest::new("session_prompt_chat_focus")
        .with_git()
        .setup(seed_sessions_startup_tab)
        .zola(
            "Read the chat while composing",
            "Press Tab to read the chat, then return to Sessions and reopen the composer without \
             losing the typed draft.",
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
                    .press_key("q")
                    .wait_for_text("new session", 5000)
                    .viewing_pause_ms(1200)
                    .capture_labeled(
                        "sessions_list",
                        "Q returns from the focused chat to the sessions list",
                    )
                    .press_key("Enter")
                    .wait_for_text("Enter: send", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "restored_composer",
                        "Reopening the session restores the typed draft",
                    )
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
                assertion::assert_text_in_region(
                    &chat_focused_frame,
                    "q: sessions",
                    &chat_focused_full,
                );
                assertion::assert_text_in_region(
                    &chat_focused_frame,
                    "d: diff",
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
                assertion::assert_not_visible(frame, "q: sessions");

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
            "Choose a regular, draft, orchestrator, or stacked session from the creation selector.",
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
                assertion::assert_text_in_region(&selector_frame, "Orchestrator", &selector_full);
                assertion::assert_text_in_region(
                    &selector_frame,
                    "[Preview] Plan workers",
                    &selector_full,
                );
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

/// Verify the orchestrator proposes a durable plan for approval, fans out
/// children, reports live status, and submits a final roll-up.
#[test]
fn session_orchestration_runs_approved_parallel_wave() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_orchestration")
        .with_git()
        .setup(install_orchestration_claude_stub)
        .zola(
            "Parallel orchestration",
            "Approve an independent plan, watch workers run, and review the roll-up.",
            35,
        )
        .run(build_orchestration_scenario, |frame, report| {
            // Assert
            let picker_frame = common::frame_from_capture(&report.captures[0]);
            let picker_full = Region::full(picker_frame.cols(), picker_frame.rows());
            assertion::assert_text_in_region(&picker_frame, "Orchestrator", &picker_full);
            assertion::assert_text_in_region(&picker_frame, "[Preview] Plan workers", &picker_full);

            let approval_frame = common::frame_from_capture(&report.captures[1]);
            let approval_full = Region::full(approval_frame.cols(), approval_frame.rows());
            assertion::assert_text_in_region(
                &approval_frame,
                "Phase: AwaitingApproval",
                &approval_full,
            );
            assertion::assert_text_in_region(
                &approval_frame,
                "a approve  Enter discuss/revise",
                &approval_full,
            );

            let status_frame = common::frame_from_capture(&report.captures[2]);
            let status_full = Region::full(status_frame.cols(), status_frame.rows());
            assertion::assert_text_in_region(&status_frame, "Phase: Running", &status_full);
            assertion::assert_text_in_region(
                &status_frame,
                "Protocol worker [protocol]: running",
                &status_full,
            );
            assertion::assert_text_in_region(
                &status_frame,
                "UI worker [ui]: running",
                &status_full,
            );
            assertion::assert_match_count(&status_frame, "Phase: Running", 1);
            let protocol_status = status_frame.find_text("Protocol worker [protocol]: running");
            let ui_status = status_frame.find_text("UI worker [ui]: running");
            assert_ne!(protocol_status[0].rect.row, ui_status[0].rect.row);

            let list_frame = common::frame_from_capture(&report.captures[3]);
            assert_running_orchestration_session_list(&list_frame);
            assert_orchestration_rollup_and_references(frame, report);
        })?;

    Ok(())
}

fn build_orchestration_scenario(scenario: Scenario) -> Scenario {
    scenario
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .press_key("a")
        .wait_for_text("Orchestrator", 5000)
        .capture_labeled("orchestrator_picker", "Choose an orchestrator session")
        .press_key("Down")
        .press_key("Down")
        .press_key("Enter")
        .wait_for_text("Tab: focus | Enter: send", 5000)
        .write_text("Build protocol and UI in parallel")
        .press_key("Enter")
        .wait_for_text("Phase: AwaitingApproval", 30000)
        .capture_labeled(
            "plan_approval",
            "Review the independent plan before fan-out",
        )
        .press_key("a")
        .wait_for_text("Phase: Running", 10000)
        .wait_for_text("Protocol worker [protocol]: running", 10000)
        .wait_for_text("UI worker [ui]: running", 10000)
        .capture_labeled("live_status", "Monitor workers on the campaign board")
        .press_key("q")
        .wait_for_text("Phase: Running", 10000)
        .capture_labeled(
            "orchestration_sessions",
            "Workers stay grouped with their controller",
        )
        .wait_for_text("Phase: AwaitingIntegration", 30000)
        .press_key("j")
        .wait_for_stable_frame(300, 5000)
        .press_key("Enter")
        .wait_for_text("Rolled up both worker results.", 5000)
        .capture_labeled(
            "orchestration_rollup",
            "Review worker summaries and merge order",
        )
        .press_key("a")
        .wait_for_text("Integration Approach", 5000)
        .capture_labeled(
            "integration_approach",
            "Choose local merges or review requests",
        )
        .press_key("Escape")
        .wait_for_stable_frame(300, 5000)
        .press_key("Enter")
        .wait_for_text("Tab: focus | Enter: send", 5000)
        .write_text("Implement the protocol review suggestions")
        .press_key("Enter")
        .wait_for_text("Protocol worker [protocol]: continuing", 30000)
        .capture_labeled(
            "orchestration_continuation",
            "Continue a completed worker from orchestrator chat",
        )
        .wait_for_text("Phase: AwaitingIntegration", 30000)
        .capture_labeled(
            "orchestration_reverification",
            "Review the continued worker after reverification",
        )
        .press_key("Enter")
        .wait_for_text("Tab: focus | Enter: send", 5000)
        .write_text("Continue protocol beyond its expected areas")
        .press_key("Enter")
        .wait_for_text("Protocol worker [protocol]: continuing", 30000)
        .capture_labeled(
            "orchestration_reference_areas",
            "Continue work beyond its planning references",
        )
        .wait_for_text("Phase: AwaitingIntegration", 30000)
}

fn assert_orchestration_rollup_and_references(frame: &TerminalFrame, report: &ProofReport) {
    let rollup_frame = common::frame_from_capture(&report.captures[4]);
    let rollup_full = Region::full(rollup_frame.cols(), rollup_frame.rows());
    assertion::assert_text_in_region(&rollup_frame, "Phase: AwaitingIntegration", &rollup_full);
    assertion::assert_text_in_region(
        &rollup_frame,
        "Rolled up both worker results.",
        &rollup_full,
    );
    assertion::assert_text_in_region(
        &rollup_frame,
        "Protocol worker [protocol]: awaiting integration",
        &rollup_full,
    );
    assertion::assert_text_in_region(
        &rollup_frame,
        "within expected areas; verified",
        &rollup_full,
    );
    assertion::assert_not_visible(&rollup_frame, "d: diff");

    let approach_frame = common::frame_from_capture(&report.captures[5]);
    let approach_full = Region::full(approach_frame.cols(), approach_frame.rows());
    assertion::assert_text_in_region(&approach_frame, "Integration Approach", &approach_full);
    assertion::assert_text_in_region(&approach_frame, "Local merges", &approach_full);
    assertion::assert_text_in_region(&approach_frame, "Review requests", &approach_full);

    let continuation_frame = common::frame_from_capture(&report.captures[6]);
    let continuation_full = Region::full(continuation_frame.cols(), continuation_frame.rows());
    assertion::assert_text_in_region(
        &continuation_frame,
        "Protocol worker [protocol]: continuing",
        &continuation_full,
    );

    let reverification_frame = common::frame_from_capture(&report.captures[7]);
    let reverification_full =
        Region::full(reverification_frame.cols(), reverification_frame.rows());
    assertion::assert_text_in_region(
        &reverification_frame,
        "Phase: AwaitingIntegration",
        &reverification_full,
    );
    assertion::assert_text_in_region(
        &reverification_frame,
        "Protocol worker [protocol]: awaiting integration",
        &reverification_full,
    );

    let reference_frame = common::frame_from_capture(&report.captures[8]);
    let reference_full = Region::full(reference_frame.cols(), reference_frame.rows());
    assertion::assert_text_in_region(
        &reference_frame,
        "Protocol worker [protocol]: continuing",
        &reference_full,
    );

    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(frame, "Phase: AwaitingIntegration", &full);
    assertion::assert_not_visible(frame, "Question 1/1");
}

/// Verifies that multiline campaign progress preserves the title column in
/// the grouped Sessions table.
fn assert_running_orchestration_session_list(frame: &TerminalFrame) {
    let full = Region::full(frame.cols(), frame.rows());

    assertion::assert_text_in_region(frame, "Phase: Running", &full);
    assertion::assert_text_in_region(frame, "ACTIVE", &full);
    assertion::assert_match_count(frame, "[XS]", 3);
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
/// while direct slash entry opens the same command menu available after
/// entering the reply composer.
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
                        "Parent review session with reply, commands, and sync available",
                    )
                    .press_key("/")
                    .wait_for_text("Slash Command", 3000)
                    .capture_labeled(
                        "parent_slash_commands",
                        "Direct slash entry opens commands for a stacked parent",
                    )
            },
            |frame, report| {
                let parent_frame = common::frame_from_capture(&report.captures[0]);
                let parent_full = Region::full(parent_frame.cols(), parent_frame.rows());
                assertion::assert_text_in_region(
                    &parent_frame,
                    "Parent stack review",
                    &parent_full,
                );
                assertion::assert_text_in_region(&parent_frame, "Enter: reply", &parent_full);
                assertion::assert_text_in_region(&parent_frame, "/: commands menu", &parent_full);
                assertion::assert_text_in_region(
                    &parent_frame,
                    "m: add to merge queue",
                    &parent_full,
                );
                assertion::assert_text_in_region(&parent_frame, "r: sync", &parent_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Parent stack review", &full);
                assertion::assert_text_in_region(frame, "Slash Command", &full);
                assertion::assert_text_in_region(frame, "/model", &full);
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

/// Verify that the prompt `/model` picker exposes the current Gemini models
/// when the Gemini CLI is locally available.
#[test]
fn gemini_model_picker_lists_current_models() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("gemini_model_picker_lists_current_models")
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
                    .wait_for_text("gemini-3.7-flash", 3000)
                    .capture_labeled(
                        "gemini_model_picker",
                        "Gemini model picker lists current models",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gemini-3.7-flash", &full);
                assertion::assert_text_in_region(frame, "gemini-3.5-flash-lite", &full);
            },
        )?;

    Ok(())
}

/// Verify that the prompt `/model` picker exposes the current Claude models
/// when the Claude CLI is locally available.
#[test]
fn claude_model_picker_lists_current_models() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("claude_model_picker_lists_current_models")
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
                    .write_text("model")
                    .wait_for_text("Slash Command", 3000)
                    .press_key("Enter")
                    .wait_for_text("/model Agent", 3000)
                    .press_key("Down")
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("claude-opus-5", 3000)
                    .capture_labeled(
                        "claude_model_picker",
                        "Claude model picker lists current models",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "claude-fable-5", &full);
                assertion::assert_text_in_region(frame, "claude-opus-5", &full);
                assertion::assert_text_in_region(frame, "claude-sonnet-5", &full);
                assertion::assert_text_in_region(frame, "claude-haiku-4-5-20251001", &full);
            },
        )?;

    Ok(())
}

/// Seeds one still-active review session whose persisted model id has been
/// retired in favor of `gemini-3.5-flash-lite`.
fn seed_active_session_with_retired_model(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("retired-model-0001", "gemini-3.5-flash", "main", "Review")
            .with_title("Retired model"),
    )?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("retired-"))?;

    Ok(())
}

/// Verify that a still-active session stored on a retired model id is
/// switched automatically to the replacement model.
#[test]
fn retired_model_session_switches_to_replacement() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("retired_model_session_switches_to_replacement")
        .with_git()
        .setup(seed_active_session_with_retired_model)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("gemini-3.5-flash-lite", 5000)
                    .capture_labeled(
                        "retired_model_replacement",
                        "Active session switched to the replacement model",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Retired model", &full);
                assertion::assert_text_in_region(frame, "gemini-3.5-flash-lite", &full);
                assertion::assert_match_count(frame, "gemini-3.5-flash [medium]", 0);
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
                    .press_key("Enter")
                    .wait_for_text("gemini-3.7-flash", 3000)
                    .capture_labeled(
                        "antigravity_model_picker",
                        "Antigravity model picker includes Gemini models",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gemini-3.1-pro-preview", &full);
                assertion::assert_text_in_region(frame, "gemini-3.7-flash", &full);
                assertion::assert_text_in_region(frame, "gemini-3.5-flash-lite", &full);
            },
        )?;

    Ok(())
}

/// Verify outdated Antigravity installations are excluded from the provider
/// picker while supported fallback providers remain selectable.
#[test]
fn antigravity_model_picker_excludes_outdated_cli() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("antigravity_model_picker_excludes_outdated_cli")
        .with_git()
        .setup(seed_outdated_antigravity_cli_stub)
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
                    .capture_labeled(
                        "supported_agent_picker",
                        "Provider picker excludes outdated Antigravity",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Codex CLI", &full);
                assertion::assert_not_visible(frame, "Antigravity CLI");
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

/// Verify that an unmaterialized stacked draft resolves `@` suggestions from
/// its parent worktree.
#[test]
fn stacked_session_at_lookup() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_session_at_lookup")
        .with_git()
        .setup(seed_stacked_at_lookup_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Stacked lookup child", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: add draft", 3000)
                    .press_key("Enter")
                    .wait_for_text("Enter: stage draft", 3000)
                    .write_text("@parent_lookup")
                    .wait_for_text("parent_lookup_target.txt", 5000)
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "parent_lookup_target.txt", &full);
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
/// rightmost column and returning to the list clears the chat page.
#[test]
fn session_output_scrollbar_is_visible() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_output_scrollbar")
        .with_git()
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
                    .compose(&common::return_to_session_list())
                    .capture_labeled(
                        "session_list_after_scrollbar",
                        "Session list after leaving scrolled session output",
                    )
            },
            |_frame, report| {
                assert_eq!(report.captures.len(), 3);

                let top_frame = common::frame_from_capture(&report.captures[0]);
                let bottom_frame = common::frame_from_capture(&report.captures[1]);
                let list_frame = common::frame_from_capture(&report.captures[2]);
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
                assert!(
                    list_frame
                        .text_in_region(&Region::full(list_frame.cols(), list_frame.rows()))
                        .chars()
                        .all(|character| character != '█'),
                    "expected no stale scrollbar thumb after returning to the session list"
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
                    .wait_for_text("c: continue", 5000)
                    .wait_for_text("Press 'c' to continue in a new session.", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "done_session_actions",
                        "Linked done session offers continuation without review comments",
                    )
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
                let done_session_frame = common::frame_from_capture(&report.captures[0]);
                let done_session_full =
                    Region::full(done_session_frame.cols(), done_session_frame.rows());
                assertion::assert_text_in_region(
                    &done_session_frame,
                    "c: continue",
                    &done_session_full,
                );
                assertion::assert_not_visible(&done_session_frame, "comments");

                let confirmation_frame = common::frame_from_capture(&report.captures[1]);
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

/// Verify that pressing `c` in a canceled session opens a confirmation and,
/// after acceptance, stages its saved context in a new draft composer.
#[test]
fn canceled_session_continue_opens_seeded_prompt() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("canceled_session_continue")
        .with_git()
        .setup(seed_canceled_session_for_continuation)
        .zola(
            "Continue canceled session",
            "Continue a canceled session in a new draft with its saved summary staged as context.",
            46,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("c: continue", 5000)
                    .press_key("c")
                    .wait_for_text("Confirm Continue", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "continue_confirmation",
                        "Continuation confirmation for the canceled session",
                    )
                    .press_key("y")
                    .wait_for_text("Resume the remaining work.", 15000)
                    .wait_for_stable_frame(500, 15000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "canceled_session_continue",
                        "New draft composer with canceled-session context staged",
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
                assertion::assert_text_in_region(frame, "Status: Canceled", &full);
                assertion::assert_text_in_region(frame, "Previous session summary:", &full);
                assertion::assert_text_in_region(frame, "Resume the remaining work.", &full);
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

/// Verify a review-request notice created after focused review completion is
/// rendered below that review instead of being regrouped above it.
#[test]
fn review_request_notice_follows_completed_review() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_timeline_order")
        .with_git()
        .with_terminal_size(120, 40)
        .setup(seed_review_request_timeline)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("f")
                    .wait_for_text(RESOLVED_DECISION_REVIEW_TEXT, 30000)
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 3000)
                    .press_key("Enter")
                    .wait_for_text(REVIEW_REQUEST_TIMELINE_NOTICE_TEXT, 10000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "review_request_timeline_order",
                        "Review-request result follows the earlier focused review",
                    )
            },
            |frame, _report| {
                let review_finding = frame
                    .find_text(RESOLVED_DECISION_REVIEW_TEXT)
                    .into_iter()
                    .next()
                    .expect("completed focused review should render");
                let review_request_notice = frame
                    .find_text(REVIEW_REQUEST_TIMELINE_NOTICE_TEXT)
                    .into_iter()
                    .next()
                    .expect("review-request notice should render");

                assert!(
                    review_finding.rect.row < review_request_notice.rect.row,
                    "review-request notice should follow the earlier focused review"
                );
            },
        )?;

    Ok(())
}

/// Verify that linked review requests refresh in the background without a
/// manual review-request sync shortcut and disable local merge queueing.
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
                assertion::assert_not_visible(frame, "m: add to merge queue");
            },
        )?;

    Ok(())
}

/// Verify that a remote merge stays read-only `Merged` until the user syncs
/// main, then moves to `Done` without waiting for slow worktree cleanup.
#[test]
fn test_merged_review_request_waits_for_manual_sync() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("merged_review_request_manual_sync")
        .with_git()
        .setup(seed_slow_merged_review_request_status)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Merged", 10_000)
                    .capture_labeled(
                        "merged_status",
                        "Merged session waits in Active for manual main sync",
                    )
                    .compose(&common::open_selected_session_view())
                    .press_key("?")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "merged_read_only_actions",
                        "Merged session exposes only read-only review actions",
                    )
                    .press_key("?")
                    .press_key("d")
                    .wait_for_text("main.rs", 5000)
                    .press_key("j")
                    .wait_for_text("Enter: open", 5000)
                    .press_key("Enter")
                    .wait_for_text("Esc/Left: files", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "merged_diff_read_only",
                        "Merged diff hides inline comment actions",
                    )
                    .press_key("q")
                    .press_key("q")
                    .press_key("s")
                    .wait_for_text("Sync complete", 10_000)
                    .press_key("Enter")
                    .wait_for_text("Done", 10_000)
                    .capture_labeled(
                        "merged_done_after_sync",
                        "Manual main sync archives the merged session",
                    )
            },
            |frame, report| {
                assert_eq!(report.captures.len(), 4);
                let merged_frame = common::frame_from_capture(&report.captures[0]);
                let merged_full = Region::full(merged_frame.cols(), merged_frame.rows());
                assertion::assert_text_in_region(&merged_frame, "ACTIVE —— 1", &merged_full);
                assertion::assert_text_in_region(&merged_frame, "Merged", &merged_full);

                let read_only_frame = common::frame_from_capture(&report.captures[1]);
                let read_only_full = Region::full(read_only_frame.cols(), read_only_frame.rows());
                assertion::assert_text_in_region(&read_only_frame, "Show diff", &read_only_full);
                let read_only_text = read_only_frame.text_in_region(&read_only_full);
                for mutating_action in ["Reply", "Open commands menu", "Add to merge queue", "Sync"]
                {
                    assert!(
                        !read_only_text.contains(mutating_action),
                        "Merged help must hide `{mutating_action}`"
                    );
                }

                let diff_frame = common::frame_from_capture(&report.captures[2]);
                let diff_full = Region::full(diff_frame.cols(), diff_frame.rows());
                let diff_text = diff_frame.text_in_region(&diff_full);
                assertion::assert_text_in_region(&diff_frame, "Esc/Left: files", &diff_full);
                assert!(!diff_text.contains("Enter: comment"));
                assert!(!diff_text.contains("s: submit comments"));

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Done", &full);
            },
        )?;

    Ok(())
}

/// Verify one successful manual main sync archives both reviews in a fully
/// merged stack, including the child that still targets the parent branch.
#[test]
fn merged_stacked_reviews_complete_together_after_manual_sync() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("merged_stacked_reviews_complete_together_after_manual_sync")
        .with_git()
        .setup(seed_merged_stacked_review_requests)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Merged stack child", 5000)
                    .press_key("s")
                    .wait_for_text("Sync complete", 10_000)
                    .press_key("Enter")
                    .wait_for_text("Done", 10_000)
                    .capture_labeled(
                        "merged_stack_done",
                        "Manual main sync archives both merged stack sessions",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let session_list_text = frame.text_in_region(&full);

                assertion::assert_text_in_region(frame, "Merged stack parent", &full);
                assertion::assert_text_in_region(frame, "Merged stack child", &full);
                assert!(
                    session_list_text.matches("Done").count() >= 2,
                    "expected both merged stack rows to be Done:\n{session_list_text}"
                );
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
        .wait_for_text("Merged", 10_000)
        .press_key("s")
        .wait_for_text("Sync complete", 10_000)
        .press_key("Enter")
        .wait_for_text("Done", 10_000)
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
                    .wait_for_text("j/k: select file", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("diff_preview", "Diff preview after pressing d")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());

                assert_diff_file_tree_change_totals(frame);
                assertion::assert_text_in_region(frame, "println!(\"review\")", &full);
                assertion::assert_text_in_region(frame, "j/k: select file", &full);
            },
        )?;

    Ok(())
}

/// Verify that Diff mode collapses uninterrupted folder chains so the Files
/// sidebar shows more changed paths at once.
#[test]
fn test_compact_diff_tree() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("compact_diff_tree")
        .with_git()
        .setup(seed_review_session_with_compact_diff_tree)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("app/session/", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "compact_diff_tree",
                        "Diff tree with a compact single-child folder chain",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let file_tree = Region::new(0, 0, frame.cols() / 5, frame.rows());

                assertion::assert_text_in_region(frame, "app/session/", &full);
                assertion::assert_text_in_region(frame, "handler.rs", &full);
                assertion::assert_text_in_region(frame, "src/a", &file_tree);
                assertion::assert_text_in_region(frame, "han", &file_tree);
                let root_match = frame
                    .find_text_in_region("src/a", &file_tree)
                    .into_iter()
                    .next()
                    .expect("compact tree should render its root path");
                let nested_match = frame
                    .find_text_in_region("han", &file_tree)
                    .into_iter()
                    .next()
                    .expect("compact tree should render its nested path");
                assert_eq!(root_match.rect.col, 2);
                assert_eq!(nested_match.rect.col, 4);
            },
        )?;

    Ok(())
}

/// Verify that a known-clean session neither advertises nor opens Diff mode.
#[test]
fn test_clean_session_hides_diff_action() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("clean_session_hides_diff_action")
        .with_git()
        .setup(seed_clean_review_ready_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("d")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "clean_session_view",
                        "Clean session remains in chat after pressing d",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());

                assertion::assert_text_in_region(frame, "Review-ready session shortcuts", &full);
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "d: diff");
                assertion::assert_not_visible(frame, "Loading diff...");
            },
        )?;

    Ok(())
}

/// Verify opening a clean writable worktree makes subsequent edits inspectable.
#[test]
fn test_worktree_open_reenables_diff_action() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("worktree_open_reenables_diff_action")
        .env("TMUX", "/tmp/tmux-agentty-test/default,1,0")
        .with_git()
        .setup(seed_clean_review_session_with_worktree_edit)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("Enter: reply", 5000)
                    .capture_labeled("clean_session", "Clean session hides the diff action")
                    .press_key("o")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("d")
                    .wait_for_text("external worktree edit", 5000)
                    .capture_labeled(
                        "external_edit_diff",
                        "Diff opened after editing through the writable worktree",
                    )
            },
            |frame, report| {
                let clean_frame = common::frame_from_capture(&report.captures[0]);
                assertion::assert_not_visible(&clean_frame, "d: diff");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "external worktree edit", &full);
                assertion::assert_text_in_region(frame, "q/Esc: back", &full);
            },
        )?;

    Ok(())
}

/// Verify that a slow full diff leaves redraw and input handling responsive.
#[test]
fn test_slow_diff_loading_remains_cancelable() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("slow_diff_loading_remains_cancelable")
        .with_git()
        .setup(seed_slow_review_diff)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("Loading diff...", 2000)
                    .capture_labeled(
                        "diff_loading",
                        "Slow Git diff shows a responsive loading page",
                    )
                    .press_key("q")
                    .wait_for_text("Review-ready session shortcuts", 2000)
                    .capture_labeled(
                        "diff_loading_canceled",
                        "Cancel returns before the slow Git diff completes",
                    )
            },
            |frame, report| {
                let loading_frame = common::frame_from_capture(&report.captures[0]);
                let loading_full = Region::full(loading_frame.cols(), loading_frame.rows());
                assertion::assert_text_in_region(&loading_frame, "Loading diff...", &loading_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Review-ready session shortcuts", &full);
                assertion::assert_not_visible(frame, "Loading diff...");
            },
        )?;

    Ok(())
}

/// Verify that the right-hand patch scrolls while Files remains focused, then
/// `Enter` moves focus into changed-line navigation.
#[test]
fn test_diff_changed_line_navigation() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("diff_changed_line_navigation")
        .with_git()
        .setup(seed_scrollable_diff_session)
        .run(
            |scenario| {
                let scenario = scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("main.rs", 5000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000);
                let scenario = (0..70).fold(scenario, |scenario, _| scenario.press_key("Down"));
                let scenario = scenario
                    .wait_for_text("changed line 70", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "diff_file_scroll",
                        "Selected file scrolled without leaving Files focus",
                    )
                    .press_key("Enter")
                    .wait_for_text("Esc/Left: files", 5000);
                let scenario = (0..70).fold(scenario, |scenario, _| scenario.press_key("Down"));

                scenario
                    .wait_for_text("changed line 70", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "diff_changed_line_navigation",
                        "Changed-line cursor scrolled through the selected file",
                    )
            },
            |frame, report| {
                let file_focus_frame = common::frame_from_capture(&report.captures[0]);
                let file_focus_full =
                    Region::full(file_focus_frame.cols(), file_focus_frame.rows());
                let full = Region::full(frame.cols(), frame.rows());

                assertion::assert_text_in_region(
                    &file_focus_frame,
                    "changed line 70",
                    &file_focus_full,
                );
                assertion::assert_text_in_region(
                    &file_focus_frame,
                    "j/k: select file",
                    &file_focus_full,
                );
                assertion::assert_text_in_region(frame, "changed line 70", &full);
                assertion::assert_text_in_region(frame, "Esc/Left: files", &full);
                assertion::assert_text_in_region(frame, "j/k: select line", &full);
            },
        )?;

    Ok(())
}

/// Verify that multiple changed-line comments stay inline until one batch is
/// submitted as the next session turn.
#[test]
fn test_diff_line_comments() -> E2eResult {
    // Arrange — Ctrl+M emits Enter's carriage return consistently in PTY and VHS.
    const ENTER_KEY: &str = "Ctrl+m";

    // Act, Assert
    FeatureTest::new("diff_line_comments")
        .with_git()
        .zola(
            "Comment on changed lines",
            "Write multiple inline diff comments and submit them together in the next turn.",
            47,
        )
        .setup(|env| {
            seed_review_ready_session(env)?;
            seed_linked_review_worktree_with_diff(env)?;
            seed_line_comment_codex_stub(env)?;
            seed_line_comment_review_stub(env)?;
            seed_sessions_startup_tab(env)
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("main.rs", 5000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .wait_for_text("Enter: open", 5000)
                    .press_key(ENTER_KEY)
                    .wait_for_text("Enter: comment", 5000)
                    .press_key(ENTER_KEY)
                    .wait_for_text("comment: |", 5000)
                    .write_text("Explain the entry point.")
                    .wait_for_text("Explain the entry point.|", 3000)
                    .press_key(ENTER_KEY)
                    .press_key("j")
                    .press_key(ENTER_KEY)
                    .write_text("Why print review?")
                    .wait_for_text("Why print review?|", 3000)
                    .press_key(ENTER_KEY)
                    .wait_for_text("comment: Why print review?", 3000)
                    .wait_for_stable_frame(1000, 5000)
                    .capture_labeled(
                        "inline_line_comments",
                        "Multiple comments remain visible inside the diff",
                    )
                    .viewing_pause_ms(1500)
                    .press_key("s")
                    .wait_for_text("Line comments:", 5000)
                    .wait_for_text("src/main.rs:1 [new]: Explain the entry point.", 5000)
                    .wait_for_text("src/main.rs:2 [new]: Why print review?", 5000)
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .wait_for_text("Line comment received.", 5000)
                    .wait_for_text("Enter: reply", 5000)
                    .wait_for_text("[Commit] No changes to commit.", 5000)
                    .wait_for_text("No review findings.", 5000)
                    .wait_for_stable_frame(1000, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "line_comment_submitted",
                        "Line comment submitted in the next session turn",
                    )
            },
            |frame, report| {
                let diff_frame = common::frame_from_capture(&report.captures[0]);
                let diff_full = Region::full(diff_frame.cols(), diff_frame.rows());
                assertion::assert_text_in_region(
                    &diff_frame,
                    "comment: Explain the entry point.",
                    &diff_full,
                );
                assertion::assert_text_in_region(
                    &diff_frame,
                    "comment: Why print review?",
                    &diff_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Line comment received.", &full);
            },
        )?;

    Ok(())
}

/// Verify that `p` toggles a changed markdown file between raw diff and a
/// rendered markdown/mermaid preview.
#[test]
fn test_markdown_diff_preview() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("markdown_diff_preview")
        .with_git()
        .zola(
            "Rendered markdown diff preview",
            "Preview changed markdown and Mermaid diagrams directly from the diff view.",
            46,
        )
        .setup(seed_markdown_diff_preview)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("# Rendered Markdown Preview", 5000)
                    .press_key("j")
                    .press_key("p")
                    .wait_for_text("Preview — docs/日本.md", 5000)
                    .wait_for_text("Rendered Markdown Preview", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "markdown_preview",
                        "Rendered markdown and Mermaid diagram in the diff view",
                    )
                    .press_key("p")
                    .wait_for_text("# Rendered Markdown Preview", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("raw_diff", "Raw diff restored after toggling preview off")
            },
            |frame, report| {
                let preview_frame = common::frame_from_capture(&report.captures[0]);
                let preview_full = Region::full(preview_frame.cols(), preview_frame.rows());
                assertion::assert_text_in_region(
                    &preview_frame,
                    "Preview — docs/日本.md",
                    &preview_full,
                );
                assertion::assert_text_in_region(
                    &preview_frame,
                    "Rendered Markdown Preview",
                    &preview_full,
                );
                assertion::assert_text_in_region(&preview_frame, "Source", &preview_full);
                assertion::assert_text_in_region(&preview_frame, "Preview", &preview_full);
                let preview_text = preview_frame.text_in_region(&preview_full);
                assert!(preview_text.contains('┌'));
                assert!(preview_text.contains('▼'));
                assertion::assert_not_visible(&preview_frame, "graph TD");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "# Rendered Markdown Preview", &full);
                assertion::assert_text_in_region(frame, "p: preview", &full);
            },
        )?;

    Ok(())
}

/// Verify linked review comments share one workspace with changed files and
/// the selected thread's current diff context.
#[test]
fn test_session_review_comments() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_review_comments")
        .with_git()
        .with_terminal_size(160, 60)
        .zola(
            "Unified diff review comments",
            "Browse changed files and linked review comments in one diff workspace.",
            44,
        )
        .setup(|env| {
            seed_review_ready_session_with_review_request(env)?;
            seed_sessions_startup_tab(env)
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("c: comments", 5000)
                    .press_key("c")
                    .wait_for_text("Please explain why this review output is needed.", 5000)
                    .wait_for_text("Use stdout context.", 5000)
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
                    .press_key("j")
                    .wait_for_text("Original code context unavailable.", 5000)
                    .wait_for_text("a: address", 5000)
                    .press_key("a")
                    .wait_for_text("[A]", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "outdated_review_comment",
                        "Outdated comment without misleading current diff context",
                    )
                    .press_key("f")
                    .wait_for_text("c: comments", 5000)
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, report| {
                let inline_frame = common::frame_from_capture(&report.captures[0]);
                assert_inline_review_comment(&inline_frame);

                let file_frame = common::frame_from_capture(&report.captures[1]);
                let file_full = Region::full(file_frame.cols(), file_frame.rows());
                assertion::assert_text_in_region(&file_frame, "file  ·  1 comments", &file_full);
                assertion::assert_text_in_region(
                    &file_frame,
                    "This file-level comment is not attached to a code line.",
                    &file_full,
                );
                assertion::assert_text_in_region(
                    &file_frame,
                    "Please review the whole file.",
                    &file_full,
                );
                assertion::assert_not_visible(&file_frame, "println!(\"review\")");

                let outdated_frame = common::frame_from_capture(&report.captures[2]);
                assert_outdated_review_comment(&outdated_frame);

                let files_focus_region = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Files", &files_focus_region);
                assertion::assert_not_visible(frame, "a: address");
                assertion::assert_not_visible(frame, "d: deny");
                assertion::assert_not_visible(frame, "Enter: submit");
            },
        )?;

    Ok(())
}

/// Verify that `Esc` returns review-comment focus to Files without leaving
/// Diff mode.
#[test]
fn test_review_comments_escape_focuses_files() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_comments_escape_focuses_files")
        .with_git()
        .with_terminal_size(160, 60)
        .setup(|env| {
            seed_review_ready_session_with_review_request(env)?;
            seed_sessions_startup_tab(env)
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("c: comments", 5000)
                    .press_key("c")
                    .wait_for_text("a: address", 5000)
                    .wait_for_text("q: back", 5000)
                    .wait_for_text("f/Esc: files", 5000)
                    .press_key("Esc")
                    .wait_for_text("c: comments", 5000)
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());

                assertion::assert_text_in_region(frame, "Files", &full);
                assertion::assert_text_in_region(frame, "c: comments", &full);
                assertion::assert_not_visible(frame, "a: address");
            },
        )?;

    Ok(())
}

/// Verifies grouped row placement and inline code-context rendering.
fn assert_inline_review_comment(frame: &TerminalFrame) {
    let full = Region::full(frame.cols(), frame.rows());
    let page_text = frame.text_in_region(&full);

    assertion::assert_text_in_region(frame, "Comments (6)", &full);
    assertion::assert_text_in_region(frame, "Files", &full);
    assertion::assert_text_in_region(frame, "Unresolved", &full);
    assertion::assert_text_in_region(frame, "Outdated", &full);
    assertion::assert_text_in_region(frame, "Resolved", &full);
    assertion::assert_text_in_region(frame, "Standalone", &full);
    assertion::assert_text_in_region(frame, "src/main.rs:1-2", &full);
    assertion::assert_text_in_region(frame, "Conversation", &full);
    assertion::assert_text_in_region(
        frame,
        "Please explain why this review output is needed.",
        &full,
    );
    assertion::assert_text_in_region(frame, "Use stdout context.", &full);
    assertion::assert_not_visible(frame, "hidden reviewer note");
    assertion::assert_not_visible(frame, "<strong>");
    assertion::assert_text_in_region(frame, "Code context", &full);
    assertion::assert_text_in_region(frame, "println!(\"review\")", &full);
    assert!(
        matches!(
            (
                page_text.find("Code context"),
                page_text.find("Conversation"),
                page_text.find("Outdated"),
                page_text.find("old.rs:2"),
                page_text.find("Resolved"),
                page_text.find("ro.rs:4"),
            ),
            (
                Some(code_context_index),
                Some(conversation_index),
                Some(outdated_group_index),
                Some(outdated_thread_index),
                Some(resolved_group_index),
                Some(resolved_outdated_thread_index),
            ) if code_context_index < conversation_index
                && outdated_group_index < outdated_thread_index
                && outdated_thread_index < resolved_group_index
                && resolved_group_index < resolved_outdated_thread_index
        ),
        "unexpected review comment layout:\n{page_text}"
    );
}

/// Verifies that an outdated thread stays actionable without stale context.
fn assert_outdated_review_comment(frame: &TerminalFrame) {
    let full = Region::full(frame.cols(), frame.rows());

    assertion::assert_text_in_region(frame, "unresolved  ·  outdated", &full);
    assertion::assert_text_in_region(frame, "Original code context unavailable.", &full);
    assertion::assert_text_in_region(frame, "This comment refers to an earlier diff.", &full);
    assertion::assert_text_in_region(frame, "[A]", &full);
    assertion::assert_not_visible(frame, "println!(\"review\")");
    assertion::assert_text_in_region(frame, "a: address", &full);
    assertion::assert_text_in_region(frame, "d: den", &full);
    assertion::assert_text_in_region(frame, "j/k: select comment", &full);
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
                    .wait_for_text("j/k: select file", 5000)
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
                assert_diff_file_tree_change_totals(&diff_frame);
                assertion::assert_text_in_region(&diff_frame, "println!(\"review\")", &diff_full);
                assertion::assert_text_in_region(&diff_frame, "j/k: select file", &diff_full);

                // Leaving the diff restores the composer with the draft intact.
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "follow up draft", &full);
            },
        )?;

    Ok(())
}

/// Verifies the compact Files panel keeps both per-row totals and the expected
/// left-aligned hierarchy.
fn assert_diff_file_tree_change_totals(frame: &TerminalFrame) {
    let file_tree = Region::new(0, 0, frame.cols() / 5, frame.rows());

    assertion::assert_text_in_region(frame, "src/", &file_tree);
    assertion::assert_text_in_region(frame, "mai", &file_tree);
    let root_matches = frame.find_text_in_region("src/", &file_tree);
    let file_matches = frame.find_text_in_region("mai", &file_tree);
    let root_match = &root_matches[0];
    let file_match = &file_matches[0];
    assert_eq!(root_match.rect.col, 2);
    assert_eq!(file_match.rect.col, 4);

    let text = frame.text_in_region(&file_tree);
    assert_eq!(
        text.matches("+1/-0").count(),
        2,
        "expected change totals on both file-tree rows:\n{text}"
    );
}

/// Verify binary-only changes retain the chat-focus diff hint and open in the
/// diff preview even though their added/deleted line totals are zero.
#[test]
fn binary_diff_preview_opens_from_prompt_chat_focus() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("binary_diff_preview_from_prompt")
        .with_git()
        .setup(seed_binary_diff_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .press_key("Tab")
                    .wait_for_text("d: diff", 5000)
                    .capture_labeled(
                        "binary_diff_chat_focus",
                        "Binary-only diff remains available from chat focus",
                    )
                    .press_key("d")
                    .wait_for_text("asset.bin", 5000)
                    .wait_for_text("Binary files", 5000)
            },
            |frame, report| {
                let focused_frame = common::frame_from_capture(&report.captures[0]);
                let focused_full = Region::full(focused_frame.cols(), focused_frame.rows());
                assertion::assert_text_in_region(&focused_frame, "d: diff", &focused_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "asset.bin", &full);
                assertion::assert_text_in_region(frame, "Binary files", &full);
            },
        )?;

    Ok(())
}

/// Verify actionable review threads can be marked to address or deny and
/// submitted to the active session agent as one batch.
#[test]
fn session_review_comment_agent_resolution() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_review_comment_agent_resolution")
        .with_git()
        .with_terminal_size(160, 60)
        .zola(
            "Batch review comment actions",
            "Mark linked review comments to address or deny, then submit one agent batch.",
            45,
        )
        .setup(seed_review_comment_agent_resolution)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("c: comments", 5000)
                    .press_key("c")
                    .wait_for_text("a: address", 5000)
                    .press_key("a")
                    .wait_for_text("[A]", 5000)
                    .press_key("j")
                    .press_key("d")
                    .wait_for_text("[D]", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_comment_batch",
                        "Comments marked to address and deny before batch submission",
                    )
                    .press_key("Enter")
                    .wait_for_text("Resolving 2 review comments...", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "agent_review_comment_resolution",
                        "Address and deny batch submitted to the session agent",
                    )
                    .wait_for_text("Processed the selected review threads.", 15000)
                    .wait_for_text("Enter: reply", 15000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .press_key("Up")
                    .wait_for_text(REVIEW_HISTORY_PROMPT_TEXT, 5000)
                    .capture_labeled(
                        "review_comment_prompt_history",
                        "Composer history retains only user-authored prompts",
                    )
            },
            |frame, report| {
                let selection_frame = common::frame_from_capture(&report.captures[0]);
                let selection_full = Region::full(selection_frame.cols(), selection_frame.rows());
                assertion::assert_text_in_region(&selection_frame, "[A]", &selection_full);
                assertion::assert_text_in_region(&selection_frame, "[D]", &selection_full);
                let loader_frame = common::frame_from_capture(&report.captures[1]);
                let loader_full = Region::full(loader_frame.cols(), loader_frame.rows());
                assertion::assert_text_in_region(
                    &loader_frame,
                    "Resolving 2 review comments...",
                    &loader_full,
                );
                let full = Region::full(frame.cols(), frame.rows());

                assertion::assert_text_in_region(frame, REVIEW_HISTORY_PROMPT_TEXT, &full);
                assertion::assert_not_visible(
                    frame,
                    "Process the following selected forge review comments",
                );
                assertion::assert_not_visible(frame, "Thread ID: thread-inline");
                assertion::assert_not_visible(frame, "Requested action: Address");
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

/// Verify cyclic flowcharts in session output render as Unicode diagrams
/// instead of falling back to the raw Mermaid fenced block.
#[test]
fn session_view_cyclic_mermaid_output() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_cyclic_mermaid_output")
        .with_terminal_size(100, 60)
        .setup(seed_session_with_cyclic_mermaid_output)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Orchestrator controller", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "session_cyclic_mermaid_output",
                        "Cyclic Mermaid flowchart rendered in session chat",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Orchestrator controller", &full);
                assertion::assert_text_in_region(frame, "Typed command response", &full);
                assertion::assert_text_in_region(frame, "Session events", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Session events ───▶ Orchestrator controller",
                    &full,
                );
                assertion::assert_text_in_region(
                    frame,
                    "Orchestrator controller ───▶ Agent model",
                    &full,
                );
                assertion::assert_not_visible(frame, "flowchart LR");
            },
        )?;

    Ok(())
}

/// Verify an over-wide left-to-right flowchart uses the compact top-down
/// terminal layout instead of falling back to raw Mermaid source.
#[test]
fn session_view_compact_mermaid_output() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_compact_mermaid_output")
        .with_terminal_size(100, 40)
        .setup(seed_session_with_compact_mermaid_output)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("g")
                    .wait_for_text("Qwen complete", 5000)
                    .capture_labeled(
                        "session_compact_mermaid_output_top",
                        "Top of compacted over-wide Mermaid flow",
                    )
                    .write_text("G")
                    .wait_for_text("Grafana on port 3000", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_compact_mermaid_output_bottom",
                        "Bottom of compacted over-wide Mermaid flow",
                    )
            },
            |_frame, report| {
                assert_eq!(report.captures.len(), 2);
                let top_frame = common::frame_from_capture(&report.captures[0]);
                let bottom_frame = common::frame_from_capture(&report.captures[1]);
                let top_region = Region::full(top_frame.cols(), top_frame.rows());
                let bottom_region = Region::full(bottom_frame.cols(), bottom_frame.rows());

                assertion::assert_text_in_region(&top_frame, "Qwen complete", &top_region);
                assertion::assert_text_in_region(
                    &top_frame,
                    "Tracing spans and events",
                    &top_region,
                );
                assertion::assert_text_in_region(
                    &bottom_frame,
                    "Grafana on port 3000",
                    &bottom_region,
                );
                assertion::assert_text_in_region(&bottom_frame, "▼", &bottom_region);
                assertion::assert_not_visible(&top_frame, "flowchart LR");
                assertion::assert_not_visible(&bottom_frame, "flowchart LR");
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
        .with_terminal_size(100, 40)
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
                let summary_header = frame
                    .find_text("Change Summary")
                    .into_iter()
                    .next()
                    .expect("change summary header should render");
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

                assert!(summary_header.rect.row < impact_header.rect.row);
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

/// Verify a persisted focused review remains available after users switch
/// away from its owning project and back.
#[test]
fn focused_review_survives_project_switching() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("focused_review_survives_project_switching")
        .with_git()
        .setup(seed_cross_project_focused_review)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Project: alpha-project", 5000)
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Project: test-project", 5000)
                    .wait_for_text("Review-ready session shortcuts", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Persisted focused review finding.", 5000)
                    .capture_labeled(
                        "restored_review",
                        "Focused review restored after project switching",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Persisted focused review finding.", &full);
                assertion::assert_text_in_region(frame, "Suggestions", &full);
                assertion::assert_not_visible(frame, "Reviewing changes with");
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

/// Verify a blank Codex completion fallback cannot replace an earlier final
/// focused-review item.
#[test]
fn focused_review_ignores_blank_completed_fallback() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("focused_review_ignores_blank_completed_fallback")
        .with_git()
        .setup(seed_codex_review_with_blank_completed_fallback)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .press_key("f")
                    .wait_for_text("Final focused review result.", 30000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "final_review",
                        "Focused review preserves the nonblank final answer",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Final focused review result.", &full);
                assertion::assert_text_in_region(frame, "Suggestions", &full);
                assertion::assert_not_visible(frame, "I will inspect the current code.");
                assertion::assert_not_visible(frame, "Reviewing changes with");
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
        .setup(seed_dirty_fork_source_session)
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
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .press_key("Tab")
                    .wait_for_text("j/k: scroll", 5000)
                    .capture_labeled(
                        "forked_session_chat_focus",
                        "Clean fork hides the diff preview shortcut",
                    )
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

                let forked_chat_frame = common::frame_from_capture(&report.captures[3]);
                let forked_chat_full =
                    Region::full(forked_chat_frame.cols(), forked_chat_frame.rows());
                assertion::assert_text_in_region(
                    &forked_chat_frame,
                    "Tab: focus | j/k: scroll | q: sessions",
                    &forked_chat_full,
                );
                assertion::assert_not_visible(&forked_chat_frame, "d: diff");

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

/// Verify that moving up from the first slash-command option wraps selection
/// to the final visible option.
#[test]
fn slash_command_selection_wraps_from_first_to_last() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("slash_command_selection_wraps")
        .with_git()
        .setup(seed_review_ready_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .press_key("/")
                    .wait_for_text("Slash Command", 3000)
                    .press_key("Up")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "slash_selection_wrapped",
                        "Up wraps slash selection from the first option to the last",
                    )
            },
            |frame, _report| {
                let slash_menu_title = frame
                    .find_text("Slash Command")
                    .into_iter()
                    .next()
                    .expect("slash-command menu title should render");
                let slash_menu_left_col = (0..=slash_menu_title.rect.col)
                    .rev()
                    .find(|column| frame.cell_text(slash_menu_title.rect.row, *column) == "╭")
                    .expect("slash-command menu should have a left border");
                let option_rows = (slash_menu_title.rect.row + 1..frame.rows())
                    .take_while(|row| frame.cell_text(*row, slash_menu_left_col) == "│")
                    .collect::<Vec<_>>();
                let last_option_row = option_rows
                    .last()
                    .copied()
                    .expect("slash-command menu should render at least one option");

                assert_eq!(
                    frame.cell_text(last_option_row, slash_menu_left_col + 1),
                    ">",
                    "expected the final visible slash command to be selected after wrapping up"
                );
            },
        )?;

    Ok(())
}

/// Verify `/speed` exposes normal and fast modes, reflects the selection, and
/// drops both fast mode and its speed display when `/model` switches to a
/// provider without a speed control.
#[test]
fn session_speed_mode_selection() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_speed_mode_selection")
        .with_git()
        .with_terminal_size(180, 24)
        .setup(seed_review_ready_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("/")
                    .write_text("speed")
                    .wait_for_text("/speed", 3000)
                    .press_key("Enter")
                    .wait_for_text("/speed Mode", 3000)
                    .capture_labeled(
                        "speed_mode_picker",
                        "Speed picker offers normal and fast modes",
                    )
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("· Fast", 5000)
                    .wait_for_text("Reasoning: high  Speed: Fast", 5000)
                    .capture_labeled(
                        "fast_mode_selected",
                        "Session header and composer reflect fast mode",
                    )
                    .press_key("/")
                    .write_text("model")
                    .wait_for_text("/model", 3000)
                    .press_key("Enter")
                    .wait_for_text("/model Agent", 3000)
                    .press_key("Enter")
                    .wait_for_text("gemini-3.1-pro-preview", 3000)
                    .press_key("Enter")
                    .wait_for_text(
                        "Model: gemini-3.1-pro-preview  Reasoning: high  Tokens:",
                        5000,
                    )
                    .capture_labeled(
                        "incompatible_model_normalizes_speed",
                        "Switching to Gemini drops fast mode and its speed display",
                    )
            },
            |frame, report| {
                let picker_frame = common::frame_from_capture(&report.captures[0]);
                let picker_full = Region::full(picker_frame.cols(), picker_frame.rows());
                assertion::assert_text_in_region(&picker_frame, "Normal", &picker_full);
                assertion::assert_text_in_region(&picker_frame, "Fast", &picker_full);

                let fast_frame = common::frame_from_capture(&report.captures[1]);
                let fast_full = Region::full(fast_frame.cols(), fast_frame.rows());
                assertion::assert_text_in_region(&fast_frame, "· Fast", &fast_full);
                assertion::assert_text_in_region(
                    &fast_frame,
                    "Reasoning: high  Speed: Fast",
                    &fast_full,
                );

                // Gemini has no speed control, so the header runs straight from
                // reasoning to tokens and the composer drops its speed status.
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Model: gemini-3.1-pro-preview  Reasoning: high  Tokens:",
                    &full,
                );
                assertion::assert_not_visible(frame, "· Fast");
            },
        )?;

    Ok(())
}

/// Verify `/personality` lists worktree-local agent definitions and persists
/// the selected profile with visible transcript feedback.
#[test]
fn test_session_personality() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_personality")
        .with_git()
        .setup(seed_session_personality)
        .zola(
            "Session personality",
            "Choose a worktree-local agent personality for a session.",
            41,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .press_key("/")
                    .write_text("personality")
                    .wait_for_text("Slash Command", 3000)
                    .wait_for_text("List: .agents/agents/.", 3000)
                    .capture_labeled(
                        "personality_slash_command",
                        "Personality source directory appears in the slash-command menu",
                    )
                    .press_key("Enter")
                    .wait_for_text("None (default)", 3000)
                    .wait_for_text("Code Reviewer", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "personality_picker",
                        "Worktree personalities appear in the session picker",
                    )
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("Personality set to", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "personality_selected",
                        "Selected personality is confirmed in the transcript",
                    )
            },
            |frame, report| {
                let command_frame = common::frame_from_capture(&report.captures[0]);
                let command_full = Region::full(command_frame.cols(), command_frame.rows());
                assertion::assert_text_in_region(
                    &command_frame,
                    "List: .agents/agents/.",
                    &command_full,
                );

                let picker_frame = common::frame_from_capture(&report.captures[1]);
                let picker_full = Region::full(picker_frame.cols(), picker_frame.rows());
                assertion::assert_text_in_region(&picker_frame, "None (default)", &picker_full);
                assertion::assert_text_in_region(&picker_frame, "Code Reviewer", &picker_full);
                assertion::assert_text_in_region(
                    &picker_frame,
                    "Reviews code carefully",
                    &picker_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Personality set to Code Reviewer.", &full);
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

/// Verify that pressing `Backspace` after deleting all prompt text leaves the
/// empty prompt open.
#[test]
fn prompt_backspace_on_empty_input() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("prompt_empty_backspace").with_git().run(
        |scenario| {
            scenario
                .compose(&common::wait_for_agentty_startup())
                .compose(&common::switch_to_tab("Sessions"))
                .press_key("a")
                .press_key("Enter")
                .wait_for_stable_frame(300, 5000)
                .write_text("bug")
                .wait_for_text("bug", 3000)
                .press_key("Backspace")
                .press_key("Backspace")
                .press_key("Backspace")
                .press_key("Backspace")
                .wait_for_text("Type your message", 3000)
                .capture_labeled("empty_prompt", "Empty prompt after one extra Backspace")
        },
        |frame, _report| {
            let full = Region::full(frame.cols(), frame.rows());
            assertion::assert_text_in_region(frame, "Type your message", &full);
            assertion::assert_text_in_region(frame, "Enter: send", &full);
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
