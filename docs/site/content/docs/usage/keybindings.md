+++
title = "Keybindings"
description = "Keyboard shortcuts across lists, session view, diff mode, prompt input, and question input."
weight = 2
+++

<a id="usage-keybindings-introduction"></a> This page lists keyboard shortcuts for each
Agentty view.

For session states and transition behavior, see [Workflow](@/docs/usage/workflow.md).

<!-- more -->

## Shared Text Editing

Prompt, question, publish-branch, and launch-configuration inputs share the same basic
editing shortcuts. Context-specific actions such as prompt submission, slash commands,
`@` completion, and question-option navigation run before these common fallbacks.

| Key                                      | Action                              |
| ---------------------------------------- | ----------------------------------- |
| `Left` / `Right`                         | Move one character                  |
| `Option+Left` / `Shift+Left` / `Alt+B`   | Move to previous word               |
| `Option+Right` / `Shift+Right` / `Alt+F` | Move to next word                   |
| `Home` / `End`                           | Move to start / end of input        |
| `Ctrl+A` / `Ctrl+E`                      | Move to start / end of current line |
| `Backspace` / `Delete`                   | Delete backward / forward           |
| `Option+Backspace` / `Shift+Backspace`   | Delete previous word                |
| `Ctrl+W`                                 | Delete previous word                |
| `Cmd+Backspace` / `Ctrl+U`               | Delete current line                 |
| `Ctrl+K`                                 | Delete to end of current line       |
| `Ctrl+Z`                                 | Undo                                |
| `Ctrl+Y` / `Ctrl+Shift+Z`                | Redo                                |
| paste                                    | Insert text at the cursor           |

On macOS, undo still uses `Ctrl+Z`, not `Cmd+Z`. Terminal applications such as Ghostty
may consume `Cmd+Z` before Agentty or a surrounding `tmux` session receives it.

Multiline prompt and question inputs also share vertical cursor movement and newline
insertion. Single-line publish and launch-configuration inputs keep only the first line
of pasted text.

## Session List

| Key                 | Action                                               |
| ------------------- | ---------------------------------------------------- |
| `q`                 | Quit                                                 |
| `a`                 | Check hooks, then open the session creation selector |
| `s`                 | Sync active project branch                           |
| `c`                 | Cancel selected session after confirmation           |
| `Enter`             | Open session                                         |
| `j` / `k`           | Navigate sessions                                    |
| `p`                 | Open project switcher popup                          |
| `Tab` / `Shift+Tab` | Switch to next / previous tab                        |
| `?`                 | Help                                                 |

If pre-commit configuration exists without an executable hook, `a` first opens a
warning. Press `Enter` to continue to the `Regular`, `Draft`, `Orchestrator`, or
`Stacked` selector, or `Esc` / `q` to cancel. `Orchestrator` and an available `Stacked`
option are marked `[Preview]`.

In the `a` selector, `Stacked` is enabled only when the selected session is a root
session with an active branch. `c` appears only for cancelable rows: running sessions,
review-ready sessions, unstarted draft sessions, and draft orchestrators. Canceling a
running orchestrator opens a confirmation that names its running-child count and
cascades to those children.

<a id="usage-session-list-project-switcher"></a> The `p` popup lists registered projects
in most-recently-opened order with the active project marked by a `* ` prefix. Each row
shows `▶ N` for projects with running sessions and stays blank otherwise. Use `j` / `k`
to move, `Enter` to switch the active project without leaving the Sessions view, and
`Esc` or `q` to close.

## Project List

| Key                 | Action                        |
| ------------------- | ----------------------------- |
| `q`                 | Quit                          |
| `s`                 | Sync active project branch    |
| `Enter`             | Select active project         |
| `j` / `k`           | Navigate projects             |
| `Tab` / `Shift+Tab` | Switch to next / previous tab |
| `?`                 | Help                          |

<a id="usage-project-list-active-highlight"></a> The currently active project is
highlighted in the table with a `* ` prefix and accented row text.

## Settings

<table>
<thead>
<tr><th>Key</th><th>Action</th></tr>
</thead>
<tbody>
<tr><td><code>q</code></td><td>Quit; closes an open selector dropdown or <code>Launch Configurations</code> browser first</td></tr>
<tr><td><code>s</code></td><td>Sync active project branch</td></tr>
<tr><td><code>j</code> / <code>k</code></td><td>Navigate settings; move inside an open selector dropdown or <code>Launch Configurations</code> browser</td></tr>
<tr><td><code>Enter</code></td><td>Open selector dropdown or <code>Launch Configurations</code> browser; continue from model to reasoning; save the highlighted value; edit or save a launch configuration</td></tr>
<tr><td><code>Esc</code></td><td>Close selector dropdown or <code>Launch Configurations</code> browser; cancel launch-configuration add/edit input</td></tr>
<tr><td><code>a</code></td><td>Add an entry in the <code>Launch Configurations</code> browser</td></tr>
<tr><td><code>e</code></td><td>Edit the selected entry in the <code>Launch Configurations</code> browser</td></tr>
<tr><td><code>d</code></td><td>Delete the selected entry in the <code>Launch Configurations</code> browser</td></tr>
<tr><td><code>J</code> / <code>K</code></td><td>Move the selected <code>Launch Configurations</code> entry down or up</td></tr>
<tr><td>shared text-editing keys</td><td>Edit, move by character or word, paste, undo, or redo while adding or editing one <code>Launch Configurations</code> entry</td></tr>
<tr><td><code>Tab</code> / <code>Shift+Tab</code></td><td>Switch to next / previous tab</td></tr>
<tr><td><code>?</code></td><td>Help</td></tr>
</tbody>
</table>

<a id="usage-settings-options"></a> The page is split into `Global settings` for the
app-wide `Theme` row (`Agentty Default`, `Agentty Green`, or `Dark Horizon`) and
`'<project>' settings` for Smart, Fast, and Review `agent/model [reasoning]` defaults,
the commit coauthor toggle, and `Launch Configurations` rows described in
[Workflow](@/docs/usage/workflow.md). Selector rows open dropdowns; use `j` / `k` to
move through the dropdown. For a role default, press `Enter` after choosing the model,
then choose and save its reasoning level with `Enter`. Other selectors save directly.
The `Launch Configurations` row opens a list browser where each command is added,
edited, deleted, or reordered as its own entry.

## Inbox

| Key                 | Action                                 |
| ------------------- | -------------------------------------- |
| `q`                 | Quit                                   |
| `j` / `k`           | Navigate reviews                       |
| `Enter`             | Open review details                    |
| `s`                 | Refresh PRs/MRs requesting your review |
| `Tab` / `Shift+Tab` | Switch to next / previous tab          |
| `?`                 | Help                                   |

On the read-only detail page, use `j` / `k` or `Up` / `Down` to scroll, `Ctrl+d` /
`Ctrl+u` to move by half pages, `g` / `G` to jump to the top or bottom, and `q` / `Esc`
to return to the review list.

## Session View

<a id="usage-session-view-actions"></a> Available actions depend on the session state.
The full set in **Review** state, subject to session and forge availability:

| Key                 | Action                                              |
| ------------------- | --------------------------------------------------- |
| `q`                 | Back to list                                        |
| `Enter`             | Compose a reply                                     |
| `/`                 | Open composer with `/` prefilled                    |
| `o`                 | Run a launch configuration in the worktree          |
| `p`                 | Publish branch and create or refresh review request |
| `c`                 | Show linked review-request comments                 |
| `d`                 | Show diff                                           |
| `f`                 | Append or regenerate focused review output          |
| `F`                 | Fork session with copied transcript history         |
| `m`                 | Add to merge queue after confirmation               |
| `r`                 | Sync session branch                                 |
| `j` / `k`           | Scroll output                                       |
| `g` / `G`           | Scroll to top / bottom                              |
| `Ctrl+d` / `Ctrl+u` | Half page down / up                                 |
| `?`                 | Help                                                |

State-specific differences:

- **AgentReview** keeps the review shortcuts, including `r`. Pressing `r` starts session
  sync immediately and cancels the pending focused review so stale review output cannot
  appear after the rebase begins.
- **InProgress** sessions use `Enter` to queue a follow-up message, keep `r` available
  to queue session sync, and keep `p` available to queue review-request creation behind
  the running turn. Each `Ctrl+c` retracts the newest queued message; when the queue is
  empty, `Ctrl+c` stops the active turn.
- **Rebasing** sessions use `Enter` to queue a follow-up message behind the active
  session sync. Slash commands and branch actions remain unavailable until sync
  finishes.
- Root **Review** and **AgentReview** sessions offer `F` to fork the current branch and
  copied transcript history into a new independent session; stacked children hide `F`.
- **Draft** sessions use `Enter` to add a staged message and `s` to start the staged
  session. They hide `o` and `r` until launch and let `Ctrl+V`, `Ctrl+Shift+V`, or
  `Alt+V` open the composer with an image paste; stacked drafts also hide `m` and show
  `s` only when the parent is review-ready and the stack is idle.
- **Question** sessions hide `r` until they return to review-ready state.
- **Orchestrator** sessions use a campaign board above chat. On a parked plan, `a`
  approves the plan; after verification, `a` opens a choice between local merges and
  review requests. Worker parallelism comes from the global **Orchestrator Parallelism**
  setting. Read-only research waves also use that cap and can start without approval
  through **Auto-approve Research**. Controllers hide branch actions: `d`, `o`, `p`,
  `F`, `m`, and `r`.
- Managed orchestration workers restrict direct Agentty actions. `d` opens their diff,
  `D` confirms a one-way detach into a regular user-owned session, and a worker in
  **Review** exposes `o` to open its materialized worktree. The confirmation warns that
  the shell has normal write access and edits can invalidate orchestration verification.
  Reply, slash-command, publish, fork, merge, sync, cancel, linked review-comment, and
  direct question-answer actions stay hidden; `Ctrl+c` is ignored while the worker
  remains managed. After a managed merge removes the worktree, `d` reads the immutable
  diff archived during integration.
- Temporary research children expose their transcript and `d` evidence while active, but
  hide `D` and `o`: their worktree is always reclaimed after report capture and cannot
  be transferred into a user-owned session. After cleanup, `d` reads the archived
  observed diff so unexpected writes remain inspectable.
- Review-ready stacked parents with a materialized child keep `Enter`, `/`, `m`, and `r`
  while the stack is idle.
- Sessions with a linked pull request or merge request use `c` to open its comments page
  and hide `m`; merge the linked request through its forge instead of Agentty's local
  merge queue.
- **Merged** sessions remain in Active and expose only read-only navigation, linked
  review comments, and `d` for the diff until list-mode `s` successfully syncs their
  local target branch.
- **Done** and **Canceled** sessions offer `c` to start a continuation draft
  (confirmation popup). Done-session drafts use the merged commit hash when available;
  canceled-session drafts use the saved summary, transcript, or original prompt. Both
  terminal states hide linked review comments so continuation remains unambiguous.
- **Queued** and **Merging** sessions are otherwise read-only (`q`, scroll, help).
  Linked review requests remain available from other session states with `c`.

`o` runs the configured `Launch Configurations` entry, or opens a selector popup when
several are configured. Run Agentty inside `tmux` when you rely on
`Launch Configurations`, because those commands are dispatched into tmux windows.
Publish (`p`), sync (`r`), and stacked behavior are described in
[Workflow](@/docs/usage/workflow.md).

## Review Comments

The review-comments page uses the same split layout as diff view. The left panel lists
unresolved, outdated, and resolved threads plus standalone review-request comments in
separate groups. The Outdated group contains unresolved threads with stale anchors;
resolved threads stay in the Resolved group even when their anchors are outdated. The
right panel shows the selected comment's author, resolution state, outdated-anchor
metadata when applicable, current diff context for its attached line or range, and then
the conversation. Comment bodies render Markdown and common embedded HTML without
showing HTML comments. Outdated threads explicitly report that their original context is
unavailable instead of mapping the stale anchor onto the current diff. File-level
comments similarly show that they have no attached code line instead of highlighting an
arbitrary diff row. Current inline snippets use the same gutters and added/removed line
colors as diff view.

| Key           | Action                                                  |
| ------------- | ------------------------------------------------------- |
| `q` / `Esc`   | Return to session view                                  |
| `j` / `k`     | Select previous/next comment                            |
| `Up` / `Down` | Scroll selected comment info                            |
| `a`           | Toggle the selected actionable thread as address        |
| `d`           | Toggle the selected actionable thread as deny           |
| `Enter`       | Submit all marked address and deny actions to the agent |

Actionable rows start with `[ ]`, then show `[A]` for address or `[D]` for deny.
Pressing the same action again clears that row; pressing the other action replaces it.
`a`, `d`, and `Enter` appear only while the session can accept a turn, and `Enter`
appears only after at least one row is marked. Unresolved outdated threads remain
actionable through their forge thread ID, although their original code context is no
longer available. Resolved threads and standalone comments are read-only.

## Publish Popup

| Key                      | Action                                             |
| ------------------------ | -------------------------------------------------- |
| `Enter`                  | Publish typed or default target in the background  |
| `Esc`                    | Cancel and return to session view                  |
| shared text-editing keys | Edit, paste, move, delete, undo, or redo           |
| text keys                | Edit remote branch name, including the character q |

## Launch Configuration Selector

| Key         | Action                                 |
| ----------- | -------------------------------------- |
| `j` / `k`   | Move selection                         |
| `Enter`     | Open worktree and run selected command |
| `Esc` / `q` | Cancel and return to session view      |

## Diff Mode

Pressing `d` from session view opens diff mode with the right panel showing the git
diff.

| Key           | Action                      |
| ------------- | --------------------------- |
| `q` / `Esc`   | Back to session             |
| `j` / `k`     | Select file                 |
| `p`           | Toggle markdown preview     |
| `Up` / `Down` | Scroll selected right panel |
| `?`           | Help                        |

<a id="usage-diff-totals"></a> The diff panel title includes aggregate `+added` and
`-removed` line totals plus the selected file or folder's added/removed counts.

On a selected `.md` file, `p` replaces the raw patch with the rendered post-change
worktree file. Headings, lists, tables, code blocks, and supported Mermaid diagrams use
the same renderer as session output. Preview stays enabled while navigating: another
markdown file loads automatically, while folders and other file types continue showing
their normal diff. Deleted, binary, oversized, and unreadable markdown files show a
short availability notice. Press `p` again to return to raw diff lines.

## Prompt Input

| Key                                 | Action                              |
| ----------------------------------- | ----------------------------------- |
| `Enter`                             | Send prompt or stage it in a draft  |
| `Alt+Enter` / `Shift+Enter`         | Insert newline                      |
| `Ctrl+J` / `Ctrl+M`                 | Insert newline (terminal fallback)  |
| `Ctrl+V` / `Ctrl+Shift+V` / `Alt+V` | Paste image as `[Image #n]`         |
| `Cmd+Left` / `Cmd+Right`            | Move to start / end of current line |
| `Option+Left` / `Option+Right`      | Move to previous / next word        |
| `Option+Backspace`                  | Delete previous word                |
| `Cmd+Backspace`                     | Delete current line                 |
| `Ctrl+Z`                            | Undo                                |
| `Ctrl+Y` / `Ctrl+Shift+Z`           | Redo                                |
| `Esc`                               | Cancel                              |
| `Tab`                               | Focus chat output for scrolling     |
| `@`                                 | Open file picker                    |
| `/`                                 | Open slash commands                 |
| `j` / `k` / `Up` / `Down`           | Navigate and wrap slash menu        |

While the chat output is focused, the `d` diff-preview hint is hidden only when the
latest successful refresh found an empty diff against the session's base branch. The
shortcut remains available for text, binary, metadata-only, and diagnostic diff output.
`j` / `k` / `Up` / `Down` scroll the transcript, `g` / `G` jump to the top or bottom,
`Ctrl+D` / `Ctrl+U` scroll by half a page, `Tab` returns focus to the composer, and `q`
returns to the sessions list. The typed draft is preserved when leaving with `q`:
reopening the session restores the composer with input focus. Other keys pressed in chat
focus never edit the draft, and `Ctrl+C` is ignored. Leaving the diff preview also
returns to the composer with the draft intact.

Prompt input keeps regular text paste on terminal `Event::Paste`. The dedicated image
paste shortcuts insert highlighted `[Image #n]` tokens directly in the composer and send
the referenced local image for Codex, Gemini, Antigravity, and Claude session models.
Clipboard image capture uses Agentty's host clipboard backend, with Wayland reads using
`wl-paste` when it is available. Missing or unsupported clipboard backends report an
inline paste error. Codex preserves the multimodal ordering at transport level, while
Antigravity and Claude rewrite the placeholders to local image paths before streaming
the prompt.

Agentty requests enhanced keyboard reporting from supporting terminals and `tmux` panes
so remote sessions can distinguish `Shift+Enter` from plain `Enter`. Image paste and `@`
lookup behavior are described in [Workflow](@/docs/usage/workflow.md).

## Question Input — Option Selection

When predefined options are shown:

| Key                       | Action                          |
| ------------------------- | ------------------------------- |
| `j` / `k` / `Up` / `Down` | Navigate options                |
| `Enter`                   | Send highlighted option         |
| `Tab`                     | Focus chat output for scrolling |
| `q`                       | Return to sessions list         |
| `Ctrl+C`                  | End turn without answering      |

## Question Input — Free Text

After moving above or below the predefined option list, or when no predefined options
exist:

| Key                              | Action                               |
| -------------------------------- | ------------------------------------ |
| `Enter`                          | Send response; blank means no answer |
| `Alt+Enter` / `Shift+Enter`      | Insert newline                       |
| `Ctrl+J` / `Ctrl+M`              | Insert newline (terminal fallback)   |
| `Ctrl+C`                         | End turn without answering           |
| `Left` / `Right` / `Up` / `Down` | Move cursor                          |
| `Backspace` / `Delete`           | Delete character                     |
| `Home` / `End`                   | Move to start / end                  |
| `Cmd+Left` / `Cmd+Right`         | Move to start / end of current line  |
| `Option+Left` / `Option+Right`   | Move to previous / next word         |
| `Option+Backspace` / `Ctrl+W`    | Delete previous word                 |
| `Cmd+Backspace`                  | Delete current line                  |
| `Ctrl+K`                         | Delete to end of current line        |
| `Ctrl+D`                         | Delete character forward             |
| `Ctrl+Z`                         | Undo                                 |
| `Ctrl+Y` / `Ctrl+Shift+Z`        | Redo                                 |
| `Tab`                            | Focus chat output for scrolling      |

In free-text mode every other printable character — including `q` — is inserted into the
answer. To leave without answering, press `Tab` to focus the chat output and then `q`,
or press `Ctrl+C` while the answer input is focused.

## Question Input — Chat Scroll

When chat output is focused (press `Tab` to switch):

| Key                       | Action                            |
| ------------------------- | --------------------------------- |
| `j` / `k` / `Up` / `Down` | Scroll chat output                |
| `g` / `G`                 | Scroll to top / bottom            |
| `Ctrl+d` / `Ctrl+u`       | Half page down / up               |
| `d`                       | Open available diff or diagnostic |
| `Tab`                     | Return focus to answer input      |
| `q`                       | Return to sessions list           |

<a id="usage-question-input-submit-flow"></a> After the last question is answered,
Agentty sends one follow-up message with each question and its response, then returns to
session view. Pressing `q` (outside free-text input) returns to the sessions list while
leaving the session in **Question** state; answers already submitted and the current
free-text draft are kept, so reopening the session resumes at the next unanswered
question.
