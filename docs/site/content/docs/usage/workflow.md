+++
title = "Workflow"
description = "Interface layout, session lifecycle, slash commands, and data location."
weight = 1
+++

<a id="usage-workflow-introduction"></a> Create a session, review its changes, then
merge locally or publish a review request. See
[Keybindings](@/docs/usage/keybindings.md) for shortcuts by view.

<!-- more -->

## Interface Layout

<a id="usage-interface-layout"></a> Use `Tab` and `Shift+Tab` to move between tabs.
Agentty restores the last tab on startup.

- **Projects**: Choose a repository; view activity, usage, and installed agent CLIs.
- **Sessions**: Create and manage work in merge-queue, active, and archive groups. Press
  `p` to switch projects. Each row shows a short model name, such as `opus-5.5`, and its
  reasoning level.
- **Settings**: Configure appearance, model defaults, and launch commands.

Press `?`, shown as `?: help` in the top bar, to list the shortcuts available on the
current page. With `NO_COLOR`, selections use bold reverse video.

Session chat shows `[size]`, the title, and added and removed line totals, plus any
linked review request. Scroll history with `j` / `k` or the arrow keys; `g` jumps to the
oldest output and `G` returns to the newest and follows updates. The composer border
shows `agent/model [reasoning]`, response style, speed when supported, permissions, and
timers such as `Timer: 2m 0s · Started 4m ago`. The footer shows the active directory,
branch, and ahead/behind counts.

Archiving a session while you type discards unsent drafts, partial answers, and attached
images. Up to eight session turns run at once; others wait for capacity.

### Projects and Discovery

Agentty opens the repository you launch it from. Otherwise, choose or add one in
**Projects**. Unavailable saved projects stay in your history and show an error when
opened. Older projects saved as repository subdirectories remain selectable with their
sessions and settings.

On first launch, a popup offers once to scan your home directory for repositories.
**No** is the default, and Agentty remembers your choice. Scanning may trigger
operating-system folder-access prompts.

In **Projects**, press `a` to add a repository root by path or `d` to discover
repositories in a folder. Paths can be absolute, relative to the current project, or
start with `~`. Scans search up to five levels and 200 repositories, skipping hidden
folders, Agentty worktrees, and directory symlinks. Select results with `Space`, then
press `Enter` to save them.

### Resource Usage

The list tab header shows `Processes`, `CPU`, and `RAM` (resident memory) for Agentty
and its descendant processes, including session agents, refreshed about every two
seconds. CPU can exceed `100%`, and shared memory may be counted more than once.
Detached processes are excluded.

### Project Sync

New worktrees start from the local base branch. Press `s` from a list tab to pull remote
changes first. Sync runs in the background; until it finishes, creating or starting
sessions, merging, and rebasing against that project's base checkout must be retried.
Progress appears in the top status bar.

A red `[merge conflict]` label means a session's committed branch conflicts with its
base.

For a linked GitHub pull request, a green `[ready]` label means GitHub reports the open,
non-draft request as cleanly mergeable under the repository's rules. `[ready (bypass)]`
appears when you can bypass branch protections, every required check has passed, and
either only a required approval is missing or a draft has no conflicts and no unresolved
review comments. Labels refresh every minute.

## Session Lifecycle

<a id="usage-session-lifecycle"></a>

| Status          | Meaning                                                  |
| --------------- | -------------------------------------------------------- |
| **Draft**       | Prompt staging or workspace setup; work has not started. |
| **InProgress**  | Agent is working.                                        |
| **Review**      | Changes are ready to inspect.                            |
| **AgentReview** | Focused review is running.                               |
| **Watching**    | Review-ready; linked review request is watched.          |
| **Question**    | Agent is waiting for clarification.                      |
| **Queued**      | Waiting to merge.                                        |
| **Rebasing**    | Session branch is syncing.                               |
| **Merging**     | Changes are merging into the base branch.                |
| **Merged**      | Merged remotely; waiting for manual local target sync.   |
| **Done**        | Completed; worktree removed.                             |
| **Canceled**    | Canceled; worktree removed.                              |

New sessions open the composer while their workspace is prepared; submit right away and
the prompt waits for setup. If setup fails, press `s` in session view to retry. Prompts
and images whose turns never began survive restart; review the transcript before
retrying a turn interrupted during dispatch. Startup restores interrupted rebases to
**Review**; storage or Git cleanup failures must be resolved before startup finishes.

### Typical Transitions

```mermaid
flowchart TD
  Draft --> InProgress
  InProgress --> Question
  Question --> InProgress
  InProgress --> Review
  Review --> AgentReview
  AgentReview --> Review
  Review --> Watching
  Watching --> Review
  Watching --> InProgress
  Watching --> Rebasing
  Review --> Rebasing
  Rebasing --> Review
  Review --> Queued
  Queued --> Merging
  Merging --> Done
  Review --> Merged
  Merged -->|Manual target sync| Done
  Review --> Canceled
```

### Active Turns and the Message Queue

Press `Enter` during **InProgress** or **Rebasing** to queue a message. Messages,
session sync (`r`), and publishing (`p`) run in submission order after the active work
and are listed below the status. Repeated `r` presses queue one sync.

During **InProgress**, `Ctrl+C` removes the newest queued message; with none left, it
stops the turn and cancels queued branch actions. Rebases cannot be interrupted. Queued
chat survives project switching but not an Agentty restart.

While work runs, `/` offers only `/mode`, `/model`, `/reasoning`, `/style`, and `/fast`.
`/model` and `/fast` changes wait until the active work and queue finish and appear as
the last queued action; choosing the current value cancels the change. Pending changes
are lost on restart.

While composing, `Tab` focuses chat for scrolling; from there, `d` previews changes and
`q` returns to the list with the draft kept. `Shift+Tab` cycles permission modes.

In Diff mode, add file, line, or range comments and press `s` to submit them with the
draft and images. Leaving Diff mode with unsent comments asks whether to submit them;
declined comments are kept until the next turn starts. Read-only sessions cannot submit
comments. See [Diff Mode](@/docs/usage/keybindings.md#diff-mode).

### Continuing Long Sessions

Follow-ups keep the active goal and accepted decisions unless you cancel or replace
them; a status question does not cancel work. Long sessions send opening and recent
context with access to the full saved history. Agents reuse successful checks while
their inputs are unchanged and still run repository-required checks.

### Focused Review

Agentty reviews a changed diff automatically when a session enters **Review**; unchanged
diffs and stopped turns are skipped. Press `f` to show the review, request one, or bring
a running review's progress into view. Results stay visible until the next prompt, and a
conflict-free sync keeps them.

Reviews read the diff, conversation, files, and history and may browse, but recommend
checks instead of running them. Each finding is verified against source; findings whose
cited code does not match the diff are marked unanchored. Coverage lists processed and
unfinished files; a processed file is not guaranteed defect-free.

Each attempt has a 128-call budget and a 30-minute deadline. A `Partial` result keeps
its findings and lists unfinished checks; an empty suggestion list does not mean the
review completed. Press `f` and confirm regeneration to resume a partial review for
unchanged inputs, including after restart. Completed reviews, changed inputs, and
accepted syncs start fresh.

Reviews apply criteria for the changed file types and test code. Add project criteria in
`.agentty/review-rules.json` in the session worktree:

```json
{
  "rules": [
    {
      "path_prefix": "src/api/",
      "extensions": ["rs"],
      "instructions": "Check authorization before accessing tenant data."
    }
  ]
}
```

Both filters must match when present; omit one to match everything. Use relative path
prefixes and extensions without dots; renames match old and new paths. Rules add to the
standard review policy, and an invalid file stops the review with an error. The file is
limited to 64 KiB and the selected criteria to 8,000 bytes; shorten instructions or
narrow the review if they exceed it. Regenerate a review to apply changed criteria.

Use `/apply` to have the agent verify suggestions and apply those that remain valid.
[Permission modes](@/docs/usage/workflow.md#slash-commands) can automate this for up to
three iterations.

### Session Output Markdown

Chat renders headings, lists, quotes, code blocks, and tables, keeping indentation when
lines wrap. Tabs use four-column stops.

<a id="usage-session-mermaid"></a> Mermaid code blocks render simple flowcharts (`TD`,
`TB`, and `LR`), entity-relationship diagrams, and sequence diagrams; labels over 32
characters are truncated. Unsupported or oversized diagrams stay readable as code.
Markdown file previews use the same renderer.

### Forking a Review Session

Press `F` in a root **Review** or **AgentReview** session to fork it after confirmation.
The fork gets a new worktree from the source commit and a copy of the saved
conversation; uncommitted changes are not copied. Publishing, review state, usage, and
timing start fresh. Stacked children cannot be forked.

### Commit and Merge Behavior

After each file-changing turn, Agentty creates or updates one session commit. The
project's Fast model writes its message from the cumulative changes, honoring the
coauthor setting; with `--experimental-harness`, that can be a
[harness model](@/docs/agents/backends.md#backends-harness-fast-model). Reverting all
changes removes the empty commit. If message generation fails, auto-commit stops and the
worktree stays intact.

<a id="usage-title-refinement"></a> The Fast model also refines session titles from the
overall goal. Draft titles update as messages are staged, committed work uses the commit
title, and failed refinement keeps the provisional title.

A Git index lock is retried for five seconds, then reported as `[Commit Error]`. Agentty
never removes the lock; wait for active Git operations to finish, and let only the
repository owner remove a confirmed stale lock.

If the project has pre-commit configuration but no installed hook, session creation
warns you: continue with `Enter` or cancel with `Esc` / `q`, and install the hook with
`prek install` or `pre-commit install`. Commits without the hook show
`[Commit Warning]`; failing hooks stop commits.

For an unlinked session, `m` queues a local squash merge using the session commit
message. The target checkout must be clean. A failed rebase or merge returns the session
to **Review**. Linked review requests merge through their forge; see
[Review Request Sync](@/docs/usage/workflow.md#review-request-sync).

Session sync (`r`) rebases onto the local base for unpublished sessions, or onto the
fetched remote base for published ones. The session agent resolves conflicts; Agentty
stages the repairs, runs the installed pre-commit hook, and continues. After three
failed hook repairs, the rebase aborts with `[Sync Error]` and nothing is pushed. A
conflict clears the focused review.

`[Main Checkout Warning]` means tracked files in the main checkout changed during a turn
and remain dirty. Inspect them before continuing.

### Continuing a Terminal Session

Press `c` on **Done** or **Canceled** to confirm a new continuation draft. It uses the
merged commit or saved conversation as context and leaves the original session
unchanged.

## Session Types

<a id="usage-draft-stacked"></a> Press `a` on **Sessions** and choose a type.
Unavailable types are shown muted.

- **Regular**: The first `Enter` starts work.
- **Draft**: Each `Enter` stages a message; `s` starts them together and creates the
  worktree from the current base branch.
- **Stacked**: A draft based on a parent session, up to five levels below a root.
- **Append to stack** `[Preview]`: Moves an independent review-ready session below an
  eligible parent and syncs it onto that branch. Sessions with children or linked review
  requests cannot be moved.
- **Harness** `[Preview]`: A regular session run by Agentty's built-in agent loop,
  offered only with `--experimental-harness`; see
  [Harness Sessions](@/docs/agents/backends.md#backends-harness).

Start stacked drafts from parent to child; each needs a review-ready parent and an idle
stack. Parents can receive replies and sync while their children are idle. Completing a
parent turn or syncing the parent rebases review-ready descendants. A parent merge
retargets children to its base and keeps their changes; failed child syncs show
`[Sync Error]`. Canceling a parent cancels its unfinished descendants.

The orchestrator session type is retired. Former controllers and their children remain
independent sessions with transcripts and workspaces intact; pending campaign work is
stopped, and former controllers and research sessions stay read-only.

## Branch Publish Flow

<a id="usage-review-request-flow"></a> Press `p` to publish a GitHub pull request or
GitLab merge request. During a turn or rebase, publishing waits in the queue.

1. Keep the default branch name or enter one that does not exist remotely. The name is
   fixed after publishing.
1. Agentty pushes with a force-with-lease check, then creates or refreshes an open
   review request. Stacked children target their parent's branch.
1. The request URL appears in chat. Later completed turns push to the same branch
   automatically when no chat or sync is queued. Retry failed pushes with `p`.

Review-request titles stay stable unless the primary objective changes. Descriptions
follow repository PR/MR instructions, skills, and templates, or else use `Why?`,
`What?`, and `Impact?` sections. They compare the published branch with the request's
current target branch and exclude uncommitted edits. Updates preserve existing content,
notes, and checklists, and the coauthor setting adds one Agentty attribution footer.
Generation failures are reported for retry. Agentty checks for concurrent remote edits
before updating, but a later edit can still race.

<a id="usage-review-request-prerequisites"></a> Publishing requires Git credentials and
an authenticated forge CLI: `gh` for GitHub or `glab` for GitLab. See
[Forge Authentication](@/docs/usage/forge-authentication.md).

### Addressing Review Comments

Open linked comments with `c`, select actionable threads with `Space`, and press `Enter`
to have the agent address them in one turn. Resolved threads and standalone comments are
read-only; outdated unresolved threads stay actionable. While the session is busy, the
batch joins the queue; while clarification is pending, it waits for your answers. Only
one review-comment batch or watched follow-up can wait at a time.

After a successful commit and push, Agentty replies to each selected thread and resolves
those reported as fixed. Threads needing no change get an explanation and stay open; a
reviewer follow-up makes them actionable again. Review targets survive clarification and
restart.

`[Review Comments Warning]` reports failures, and failed or incomplete batches stay
pending for the next turn. Later pushes retry saved replies while the fix commit is
still the branch tip, without duplicating posted replies; if the branch has changed,
continue the session to re-evaluate them. Stopping a review turn also stops its push and
thread updates, which remain retryable.

## Review Request Sync

<a id="usage-review-request-sync"></a> Published review-ready sessions refresh forge
status in the background:

| Indicator | Meaning                             |
| --------- | ----------------------------------- |
| `↑`       | Branch published; no request found. |
| `⊙ <id>`  | Request is open.                    |
| `✓ <id>`  | Request was merged.                 |
| `✗ <id>`  | Request was closed.                 |

A remote merge moves the session to read-only **Merged**, keeping its transcript and
diff in Active. Sync its local target branch with `s` from the list to move it to
**Done**, remove the worktree, and retarget stacked children; if that fails, follow the
warning and retry. Syncing a parent's target also completes merged children. Closing an
unmerged request cancels its session.

### Watching a Review Request

<a id="usage-review-request-watch"></a> When you publish a branch that is not pushed
yet, Agentty asks whether to watch the review request, with `Yes` preselected: `Enter`
or `y` watches, `n` publishes without watching, and `Esc` cancels publishing. Press `W`
in session view to start or stop watching later.

A watched session shows **Watching** instead of **Review** and keeps every review
action. About once a minute, Agentty checks the request and starts at most one
follow-up:

1. **Merge conflicts**: Syncs the branch as `r` does, unless the request now targets a
   different branch than the session.
1. **Review comments**: Addresses unresolved threads awaiting a response from you or a
   trusted author, as in
   [Addressing Review Comments](@/docs/usage/workflow.md#addressing-review-comments).
   Once all checks finish, failed checks join the same turn.
1. **Failing checks**: Once all checks finish, starts a turn to fix the failures. If
   other work is running, it waits in the queue as `fix N failing checks: <names>`.

Trusted authors are GitHub owners, members, and collaborators, GitLab project members
with at least the Developer role, and review bots listed in `.agentty/review-watch.json`
in the project folder:

```json
{ "trusted_bots": ["greptile-apps"] }
```

On GitHub, a listed login matches only a bot account; on GitLab, it matches the
username. A trusted bot's own failed check is not fixed separately, because its findings
arrive as comments. Without the file, no bot is trusted.

Each conflict or failed check is tried once per pushed commit, and each thread once per
new reviewer comment. If a fix turn pushes nothing, Agentty reruns the failed GitHub
Actions or GitLab pipeline jobs once; checks that still fail, or cannot be rerun, get
one `Leaving N failing checks to you` notice. `[Review Watch]` notices record each
follow-up and any failure, which is retried on the next check. Watching pauses during
turns, sync, and focused review, ends when you press `W` or the request is merged or
closed, and runs only while Agentty is open with the session's project active.

## Clarification Interaction Loop

<a id="usage-clarification-loop"></a> In **Question**, answer each question; Agentty
sends all answers together as one follow-up turn.

<a id="usage-question-options"></a> Choose an option with `j` / `k` or the arrow keys
and `Enter`, or move past the options to type a free-text answer; a blank answer means
`no answer`. Free-text answers support `@` file lookup and
[pasted images](@/docs/usage/workflow.md#usage-prompt-extras). Choosing a predefined
option discards images pasted for that question.

`Ctrl+C` from the answer input ends the turn without replying; for graph questions it
only closes the panel. `q` outside free-text input returns to the list, and reopening
resumes the unanswered questions. See
[Question Input](@/docs/usage/keybindings.md#question-input-free-text).

## Prompt Input Extras

<a id="usage-prompt-extras"></a> Paste a clipboard image with `Ctrl+V`, `Ctrl+Shift+V`,
or `Alt+V`; in a draft session, these also open the composer. Each image appears as
`[Image #n]`; typing that text does not attach a file. You can keep typing while
`Pasting image...` is shown, and `Enter` waits until the paste finishes. On Wayland,
image paste uses `wl-paste` when available; unsupported clipboards show an inline error.

`Up` / `Down` browse prompt history, keeping attached images. Type `@` to look up
repository files; Rust and Python files show their module documentation summary.
Unstarted stacked drafts look up files in the nearest available ancestor worktree. See
[Prompt Input](@/docs/usage/keybindings.md#prompt-input) for editing keys.

During a turn, the output shows live tool and skill counts with succeeded, failed,
running, and interrupted totals, plus the latest successful, running, and interrupted
calls with shortened command previews. Completed responses keep a usage footer: `Tools`
lists invocation counts and `Skills` lists explicit Claude skill invocations. Codex,
Claude, and Antigravity report this activity. Provider failures show a short error and
captured output where available.

## Session Sizes

<a id="usage-session-size"></a> Sizes reflect changed lines and refresh after each turn:

| Size    | Changed lines |
| ------- | ------------- |
| **XS**  | 0–10          |
| **S**   | 11–30         |
| **M**   | 31–80         |
| **L**   | 81–200        |
| **XL**  | 201–500       |
| **XXL** | 501+          |

## Slash Commands

<a id="usage-slash-commands"></a> Type `/` in the composer to open a fuzzy-matching
command picker. From session view, `/` opens a fresh composer, replacing any saved
draft.

| Command      | Action                                                       |
| ------------ | ------------------------------------------------------------ |
| `/apply`     | Verify and apply valid focused-review suggestions.           |
| `/graph`     | Request a graph proposal; needs `--experimental-graph`.      |
| `/mode`      | Choose permissions and review automation.                    |
| `/model`     | Choose a backend and model; local or paid models in Harness. |
| `/reasoning` | Set reasoning effort for this session.                       |
| `/style`     | Choose concise, balanced, or detailed answers.               |
| `/fast`      | Toggle normal or fast responses for Claude or Codex.         |

`/apply` requires a completed focused review. `/mode` selects:

- **Auto Edit**: Standard editing permissions. Codex and Claude have full command
  access, so commands are not confined to the worktree.
- **Auto Edit + Auto Address Comments**: The same permissions, plus automatic
  verification and application of focused-review suggestions, for up to three turns. A
  new prompt or mode selection resets that limit.
- **Read Only**: Prevents filesystem writes during chat turns.

`Shift+Tab` cycles these modes while composing. See
[Agents & Models](@/docs/agents/backends.md#switching-models) for speed costs and model
compatibility.

`/style` applies to later turns: **Concise** keeps essential results, caveats, and
verification; **Balanced** adds useful context; **Detailed** explains decisions and
trade-offs. Explicit prompt instructions take precedence, and style never changes
permissions or required output.

## Agent Definitions

Generated graph proposals and the Task API choose agents from a catalog of bundled
`planner` and `implementer` definitions plus repository definitions in
`.agents/agents/<id>/agent.md`. A repository definition with a bundled ID overrides it.
Global definitions are not loaded.

Definitions use simple `key: value` frontmatter followed by Markdown instructions:

```markdown
---
id: planner
name: Project Planner
description: Plans changes for this project
enabled: true
permission-mode: read-only
---

Inspect the project and propose a concrete plan, including validation.
```

`id` defaults to the directory name; `name`, `description`, and the instruction body are
required. `enabled: false` hides an agent, including a bundled one. `permission-mode`
accepts `inherit` (default) or `read-only` and cannot expand session permissions.
Definitions inherit the session's provider and model (`model: inherit` is accepted).
Other model overrides, tool lists, hooks, MCP configuration, and Claude-specific fields
are unsupported and produce a diagnostic, so convert Claude YAML subagents to this
subset. Proposals capture the definitions they use, so later edits affect only new
proposals.

## Settings Scope

Move the selection with `j` / `k`, then press `Enter` to open or edit a setting.

<a id="usage-settings-scope"></a> **Global settings** control the theme and the
[subagent limit](@/docs/agents/backends.md#subagent-limits), which applies after
restart. **Project settings** include model, reasoning, speed, and style defaults,
commit trailers, and launch configurations. Defaults affect new sessions; use slash
commands to change an existing one. See
[Backend Defaults](@/docs/agents/backends.md#selecting-a-backend) for the Smart, Fast,
and Review roles.

`Launch Configurations` is a command list: add with `a`, edit with `e` or `Enter`,
delete with `d`, and reorder with `J` / `K`. When Agentty runs in `tmux`, session-view
`o` runs a configured command in the worktree, or opens a selector if there are several.

## Generated Graph Proposals

Start Agentty with `--experimental-graph` to enable `/graph`. In a prepared idle
session, select `/graph`, describe the task, and press `Enter`; `Esc` disarms it and
keeps your draft. The composer title shows `Graph`, and pasted images reach generation
and every proposed node. Generation reads the workspace; no node runs until you approve.

Chat and questions stay in the left pane. The right pane, at most 35% of the width,
shows each node with its status (Waiting, Working, Needs your input, Done, Failed, or
Canceled) and `│` / `▼` connectors from its dependencies. When the graph is taller than
the pane, a one-row-per-node summary prioritizes nodes needing attention and counts
hidden nodes. Proposals are restored when you reopen the session or restart Agentty.

While a proposal is generated or a reply is being decided, a **Review** session shows
**InProgress**. A message sent meanwhile starts at once. `Ctrl+C` cancels generation or
drops an undecided reply.

Reply in chat to act on a pending proposal. The session model decides what your reply
means:

- Clear approval, such as "run it", approves the displayed version and starts it.
- A clear request to discard, such as "cancel", cancels the proposal.
- Anything else, including answers and requested changes, generates a new version within
  the generation limit.

When unsure, it revises. Wait for each decision before replying again. Images in a
revision reply carry over to every later version; a reply with images always revises, so
send a text approval afterward. At the generation limit, it approves without the images
and says so.

Questions from a pending proposal or running node open in the same panel as
[clarification questions](@/docs/usage/workflow.md#usage-clarification-loop), and
answers go to the exact version or node that asked. `Ctrl+C` closes the panel; reopen
the session to show it again, or reply in chat. A paused graph continues once you resume
it from `w`.

Approval does not accept later outputs. When a node waits for another node's output to
be accepted, the producer shows **Awaiting your approval**. Your next chat reply accepts
it, as with "start implementing", or sends revision guidance; replies with images always
revise. Outputs already consumed by a started dependent, or from nodes without a task
conversation, can only be accepted.

Duplicate approvals do not launch again. Pending proposals and undecided replies survive
a restart or project switch, though a recovered reply loses its images; stopping the
session abandons an undecided reply. Approved graphs keep the limits fixed at approval
and resume after restart with their remaining provider calls.

## Execution Graph Inspector

Press `w` in session view to inspect the latest proposal or saved execution graphs,
including branches and joins. It shows the proposal version, model, captured agent
instructions, objectives, access, dependencies, output gates, limits, execution state,
blocked reasons, questions, and output versions. Opening it never starts work. See
[Keybindings](@/docs/usage/keybindings.md#execution-graph-inspector) for navigation.

Available controls appear below the graph. `a` accepts the displayed output, `e` sends
revision guidance to that node's task conversation, `t` retries a failed node, `s`
resumes paused dispatch, and `x` cancels the whole graph permanently after you type
`cancel`. Answer node questions in session chat. Retry and answers record intent; they
do not resume a stopped graph. A rejected reply keeps its draft: press `Ctrl+r` to
refresh and resubmit, or `Esc` to discard it.

Provider-call limits are fixed when a graph first launches, and resuming continues from
the calls that remain; a node whose budget is spent stays blocked. Nothing replays
automatically on restart. Focused-review graphs can be inspected and canceled here;
regenerate them with `f`.

Programmatic integrations can track outcomes through the
[Task API](@/docs/core-components/durable-components/task.md) and query, answer, retry,
resume, or cancel graphs through `SessionService`; see
[Execution Graphs](@/docs/architecture/execution-graphs.md).

## Telemetry

<a id="telemetry"></a> Agentty has two independent telemetry channels:

| Channel        | Default  | Destination                      | Contents                              | Control               |
| -------------- | -------- | -------------------------------- | ------------------------------------- | --------------------- |
| PostHog events | Enabled  | Agentty PostHog project          | The usage events below                | `--disable-telemetry` |
| OTLP export    | Disabled | Your `--otlp-endpoint` collector | Session traces, metrics, usage events | `--otlp-endpoint URL` |

PostHog telemetry sends events to the Agentty PostHog project at
`https://us.i.posthog.com`. Release builds bundle its public, write-only project token;
debug builds, including tests, send nothing. Pass `--disable-telemetry` to turn it off
for a launch:

```bash
agentty --disable-telemetry
```

`--disable-telemetry` does not affect OTLP export; see
[Local Usage Events](@/docs/usage/workflow.md#local-usage-events) and
[Session Traces](@/docs/usage/workflow.md#session-traces).

Every event includes `app_source` (`cli`), `app_version`, and `install_method` (`npm`,
`sh`, `cargo`, or `unknown`).

| Event                    | When sent                                                      | Additional properties                            |
| ------------------------ | -------------------------------------------------------------- | ------------------------------------------------ |
| `agentty_launch`         | After the database opens                                       | `ci`, `interactive`                              |
| `agentty_failure`        | When startup after the database opens or the terminal UI fails | `failure_category`, `stage`, `ci`, `interactive` |
| `agentty_session_start`  | When new session metadata is reserved                          | `session_type`, `agent`, `model`                 |
| `agentty_turn_start`     | When a first or follow-up message begins execution             | `agent`, `model`, `permission_mode`              |
| `agentty_turn_finish`    | When a turn and its post-processing complete successfully      | `agent`, `model`, `permission_mode`              |
| `agentty_turn_interrupt` | When an executing turn is stopped by the user                  | `agent`, `model`, `permission_mode`              |

- `ci` is `true` when `CI`, `GITHUB_ACTIONS`, `TF_BUILD`, `JENKINS_URL`, or
  `CODEBUILD_BUILD_ID` is set to a value other than empty, `0`, or `false`.
- `interactive` is `true` when standard input and output are both terminals.
- `failure_category` is `session`, `database`, `git`, `one_shot`, `no_agent`,
  `terminal`, or `workflow`; `stage` is `startup` or `runtime`. Failures before the
  database opens send nothing.
- `session_type` is `regular`, `draft`, `stacked`, or `fork`.
- `agent` and `model` identify the selected provider and model; any local model is sent
  as `openai-compatible`, never by name.
- `permission_mode` is `auto_edit`, `auto_edit_address_comments`, or `read_only`.

Each event carries a random installation ID, created on the first launch with telemetry
or `--otlp-endpoint` enabled and stored in Agentty's database, and sets
`$process_person_profile=false`. PostHog receives the request's IP address and derives
an approximate location from it. Agentty does not send error messages, stack traces,
project paths, session IDs, or prompts. The install method is inferred from the npm
package path, the shell install receipt, or Cargo install metadata, and describes
launches, not installations.

### Local Usage Events

When `--otlp-endpoint` ends in `/v1/traces`, Agentty also exports each event above as an
OTLP log record to the sibling `/v1/logs` URL, keeping any path prefix and query; for
example, `http://localhost:4318/v1/traces` sends events to
`http://localhost:4318/v1/logs`. This mirror is independent of PostHog, so it also runs
in debug builds and with `--disable-telemetry`.

Records use the event name as their body, `INFO` severity, and the PostHog properties,
plus `service.name`, `service.version`, a per-launch `service.instance.id`, and the
installation ID as `user.id`. Collectors without a logs pipeline reject them. For
authenticated endpoints, set `OTEL_EXPORTER_OTLP_LOGS_HEADERS`, falling back to
`OTEL_EXPORTER_OTLP_HEADERS`.

With a collector that forwards logs to Loki, count turns by model in Grafana with:

```logql
sum by (agent, model) (count_over_time({service_name="agentty"} |= "agentty_turn_start" [$__auto]))
```

### Session Traces

Session traces are separate from PostHog analytics and disabled by default. Pass a
complete OTLP HTTP/protobuf traces endpoint to enable them, including in debug builds:

```sh
agentty --otlp-endpoint http://localhost:4318/v1/traces
```

Agentty uses the URL as given, and endpoint environment variables cannot enable tracing.
For authenticated endpoints, set `OTEL_EXPORTER_OTLP_TRACES_HEADERS`, falling back to
`OTEL_EXPORTER_OTLP_HEADERS`.

When the traces URL ends in `/v1/traces`, Agentty also exports content-free Harness
request metrics, such as `gen_ai.client.operation.duration` and
`gen_ai.client.token.usage`, to the sibling `/v1/metrics` URL, authenticated with
`OTEL_EXPORTER_OTLP_METRICS_HEADERS` or `OTEL_EXPORTER_OTLP_HEADERS`.

Each turn has its own trace, correlated by session and operation identifiers. Spans
cover queue and capacity waits (`agentty.queue.wait_ms` on the root span), workspace and
context preparation, agent attempts, response validation, persistence, and
post-processing. Utility calls record their `agentty.purpose`, such as `commit message`.
Provider operations appear beneath `agent.attempt`:

| Provider    | Operations exposed                                              |
| ----------- | --------------------------------------------------------------- |
| Codex       | Tools, responses, reasoning, compaction, subagents              |
| Claude      | Tools, subagents                                                |
| Antigravity | Tools, responses, reasoning, compaction, checkpoints, subagents |
| Harness     | Turns, model requests with token usage, tools                   |

Provider operations depend on the CLI version and are not necessarily individual model
requests. `agentty.timing.source` is `lifecycle` for paired events, `provider` for
intervals reconstructed from a reported duration, or `completion` for observations
without a start. Harness `invoke_agent`, `chat <model>`, and `execute_tool <tool>` spans
nest beneath `agent.run` and time each model request exactly.

The trace resource carries the installation ID as `user.id`. By default, traces contain
execution identifiers, provider and model selections, token counts, outcomes, tool
names, call identifiers, executable basenames, exit codes, and output byte counts, but
no command arguments, tool output, or project paths. Tool spans repeat the run's
`agentty.harness`, `agentty.session.id`, `agentty.purpose`, `agentty.run.id`, and
`gen_ai.request.model` attributes for grouping.

Separate traces cover startup (`app.startup`), each background remote-status pass
(`sync.refresh`), manual project sync (`project.sync_main`), and foreground cycles that
take at least 50 ms between waking and drawing (`ui.slow_cycle`), with per-phase
durations to help diagnose unresponsive frames.

Add `--otlp-capture-content` to also export tool commands, arguments, and results as
previews of at most 4 KiB per attribute, keeping their beginning and end. Oversized
structured values use `agentty.tool.input.preview` and `agentty.tool.output.preview`
instead of `gen_ai.tool.call.arguments` and `gen_ai.tool.call.result`. Values with
common credential markers or URL credentials become `[REDACTED]`, but other secrets and
project paths may still appear, so enable this only for a receiver you trust. Prompts,
transcripts, reasoning, and subagent prompts are never exported.

Content previews cover Codex command, MCP, and dynamic-tool items and Claude tool
messages; Antigravity exports tool identities only. Codex command executions and file
changes report `gen_ai.tool.name` as `command_execution` and `file_change`; patches are
never captured. Export is asynchronous and best effort, so a full queue or unavailable
receiver may lose spans. Agentty prints a diagnostic summary after restoring the
terminal and waits at most three seconds for the final flush.

## Auto-Update

<a id="usage-auto-update"></a> Agentty checks the npm registry at startup and hourly.
Global `npm`, Bun, and `pnpm` installs update through the original package manager when
exactly one manager's installation matches the running binary; the status bar shows
progress and asks you to restart. Otherwise, or if the update fails, rerun your original
installation command; Cargo, shell, local, and `npx` installs always update manually.
See [Installation](@/docs/getting-started/installation.md) for update commands.

```bash
agentty --no-update
```

This disables automatic installation but keeps update checks and hints. See
[Backend Selection](@/docs/agents/backends.md#selecting-a-backend) for agent CLI
refresh. Use `agentty --help` for launch options and `agentty --version` for the
installed version.

## Data Location

<a id="usage-data-location"></a> Agentty keeps its database, logs, and worktrees in its
data root, normally `~/.agentty/`. Worktrees are removed when sessions finish, are
canceled, or are deleted. Set `AGENTTY_ROOT` to use another root; each root allows one
running Agentty instance. Session state, including staged draft attachments, stays in
the data root, so a project's `.agentty/` folder holds only configuration you can
commit.
