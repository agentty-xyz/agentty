+++
title = "Keybindings"
description = "Keyboard shortcuts across lists, session view, diff mode, prompt input, and question input."
weight = 2
+++

<a id="usage-keybindings-introduction"></a> This page lists keyboard shortcuts for each
Agentty view.

For session states and transition behavior, see [Workflow](@/docs/usage/workflow.md).

<!-- more -->

## Session List

| Key | Action | |-----|--------| | `q` | Quit | | `a` | Open session creation selector
(`Regular`, `Draft`, `Stacked`) | | `s` | Sync active project branch | | `c` | Cancel
the selected session (confirmation popup) | | `Enter` | Open session | | `j` / `k` |
Navigate sessions | | `Tab` | Switch tab | | `?` | Help |

In the `a` selector, `Stacked` is enabled only when the selected session is a root
session with an active branch. `c` appears only for cancelable rows: running sessions,
review-ready sessions, and unstarted draft sessions.

## Project List

| Key | Action | |-----|--------| | `q` | Quit | | `Enter` | Select active project | |
`j` / `k` | Navigate projects | | `Tab` | Switch tab | | `?` | Help |

<a id="usage-project-list-active-highlight"></a> The currently active project is
highlighted in the table with a `* ` prefix and accented row text.

## Settings

<table>
<thead>
<tr><th>Key</th><th>Action</th></tr>
</thead>
<tbody>
<tr><td><code>q</code></td><td>Quit; closes an open selector dropdown first</td></tr>
<tr><td><code>j</code> / <code>k</code></td><td>Navigate settings; move inside an open selector dropdown</td></tr>
<tr><td><code>Enter</code></td><td>Open selector dropdown, select highlighted option, or finish text edit</td></tr>
<tr><td><code>Esc</code></td><td>Close selector dropdown or finish text edit</td></tr>
<tr><td><code>Alt+Enter</code> or <code>Shift+Enter</code></td><td>Add newline while editing <code>Open Commands</code></td></tr>
<tr><td><code>Up</code> / <code>Down</code> / <code>Left</code> / <code>Right</code></td><td>Move cursor while editing <code>Open Commands</code></td></tr>
<tr><td><code>Tab</code></td><td>Switch tab</td></tr>
<tr><td><code>?</code></td><td>Help</td></tr>
</tbody>
</table>

<a id="usage-settings-options"></a> The page is split into `Global settings` for the
app-wide `Theme` row (`Agentty Default`, `Agentty Green`, or `Dark Horizon`) and
`'<project>' settings` for the reasoning level, model defaults, commit coauthor toggle,
and `Open Commands` rows described in [Workflow](@/docs/usage/workflow.md). Selector
rows open dropdowns; use `j` / `k` to move through the dropdown and `Enter` to save the
highlighted value.

## Logs

| Key | Action | |-----|--------| | `q` | Quit | | `j` / `k` | Scroll logs | | `g` |
Jump to oldest retained log entries | | `G` | Jump to newest log entries | | `Tab` |
Switch tab | | `?` | Help |

## Review

| Key | Action | |-----|--------| | `q` | Quit | | `j` / `k` | Navigate reviews | |
`Enter` | Open review details | | `s` | Refresh PRs/MRs requesting your review | | `Tab`
| Switch tab | | `?` | Help |

On the read-only detail page, use `j` / `k` or `Up` / `Down` to scroll, `Ctrl+d` /
`Ctrl+u` to move by half pages, `g` / `G` to jump to the top or bottom, and `q` / `Esc`
to return to the review list.

## Session View

<a id="usage-session-view-actions"></a> Available actions depend on the session state.
The full set in **Review** state:

| Key | Action | |-----|--------| | `q` | Back to list | | `Enter` | Compose the first
prompt, a reply, or a queued message during a running turn | | `/` | Open the composer
with `/` prefilled for slash commands | | `s` | Start a staged draft session | | `o` |
Open worktree in tmux when the session worktree exists | | `p` | Publish session branch
and create or refresh forge review request | | `d` | Show diff | | `f` | Append focused
review output (regenerate if already present) | | `F` | Fork into a new session with the
current transcript history | | `m` | Add to merge queue (confirmation popup) | | `r` |
Sync session branch | | `j` / `k` | Scroll output | | `g` / `G` | Scroll to top / bottom
| | `Ctrl+d` / `Ctrl+u` | Half page down / up | | `Ctrl+c` | Retract the newest queued
message, or stop the running turn when the queue is empty | | `?` | Help |

State-specific differences:

- **AgentReview** keeps the review shortcuts, including `r`. Pressing `r` starts session
  sync immediately and cancels the pending focused review so stale review output cannot
  appear after the rebase begins.
- Root **Review** and **AgentReview** sessions offer `F` to fork the current branch and
  copied transcript history into a new independent session; stacked children hide `F`.
- **Draft** sessions hide `o` until the worktree exists; stacked drafts hide `m` and `r`
  and show `s` only when the parent is review-ready and the stack is idle.
- Stacked parents with a materialized child keep `Enter` and `r` while the stack is
  idle, but hide `/` and `m` until the child is terminal or no longer linked.
- **Done** sessions offer `c` to start a continuation draft (confirmation popup).
- **Canceled**, **Queued**, **Rebasing**, and **Merging** sessions are read-only (`q`,
  scroll, help).

`o` runs the configured `Open Commands` entry, or opens a selector popup when several
are configured. Run Agentty inside `tmux` when you rely on `Open Commands`, because
those commands are dispatched into tmux windows. Publish (`p`), sync (`r`), and stacked
behavior are described in [Workflow](@/docs/usage/workflow.md).

## Publish Popup

| Key | Action | |-----|--------| | `Enter` | Publish using the typed branch name, or
the default target when blank | | `Esc` / `q` | Cancel and return to session view | |
`Left` / `Right` / `Home` / `End` | Move cursor | | `Up` / `Down` | Move cursor across
wrapped lines | | `Backspace` / `Delete` | Delete character | | text keys | Edit remote
branch name |

## Open Command Selector

| Key | Action | |-----|--------| | `j` / `k` | Move selection | | `Enter` | Open
worktree and run selected command | | `Esc` / `q` | Cancel and return to session view |

## Diff Mode

Pressing `d` from session view opens diff mode with the right panel showing the git
diff. Within diff mode, press `c` to toggle the right panel between the annotated git
diff and the cached review-request comments.

| Key | Action | |-----|--------| | `q` / `Esc` | Back to session | | `j` / `k` | Select
file | | `Up` / `Down` | Scroll selected panel | | `c` | Toggle right panel between diff
and comments | | `?` | Help |

<a id="usage-diff-totals"></a> The diff panel title includes aggregate `+added` and
`-removed` line totals, and cached review-request line comments render inline below
matching diff lines. See [Workflow](@/docs/usage/workflow.md) for the comments panel
behavior.

## Prompt Input

| Key | Action | |-----|--------| | `Enter` | Submit the prompt, or stage it as a draft
in draft sessions | | `Alt+Enter` or `Shift+Enter` | Insert newline | | `Ctrl+J` /
`Ctrl+M` | Insert newline (terminal compat fallback) | | `Ctrl+V`, `Ctrl+Shift+V`, or
`Alt+V` | Paste one clipboard image as `[Image #n]` | | `Cmd+Left` / `Cmd+Right` | Move
to start / end of current line | | `Option+Left` / `Option+Right` | Move to previous /
next word | | `Option+Backspace` | Delete previous word | | `Cmd+Backspace` | Delete
current line | | `Esc` | Cancel | | `@` | Open file picker | | `/` | Open slash commands
|

Agentty requests enhanced keyboard reporting from supporting terminals and `tmux` panes
so remote sessions can distinguish `Shift+Enter` from plain `Enter`. Image paste and `@`
lookup behavior are described in [Workflow](@/docs/usage/workflow.md).

## Question Input — Option Selection

When predefined options are shown:

| Key | Action | |-----|--------| | `j` / `k` / `Up` / `Down` | Navigate options | |
`Enter` | Choose highlighted option | | `Tab` | Switch focus to chat output for
scrolling | | `q` | Return to the sessions list | | `Ctrl+C` | End turn — return to
review without answering |

## Question Input — Free Text

After moving above or below the predefined option list, or when no predefined options
exist:

| Key | Action | |-----|--------| | `Enter` | Submit typed response (blank stores
`no answer`) | | `Alt+Enter` or `Shift+Enter` | Insert newline | | `Ctrl+J` / `Ctrl+M` |
Insert newline (terminal compat fallback) | | `Ctrl+C` | End turn — return to review
without answering | | `Esc` | End turn when the answer text is empty | | `Left` /
`Right` / `Up` / `Down` | Move cursor | | `Backspace` / `Delete` | Delete character | |
`Home` / `End` | Move cursor to start / end | | `Cmd+Left` / `Cmd+Right` | Move to start
/ end of current line | | `Option+Left` / `Option+Right` | Move to previous / next word
| | `Option+Backspace` / `Ctrl+W` | Delete previous word | | `Cmd+Backspace` | Delete
current line | | `Ctrl+K` | Kill to end of current line | | `Ctrl+D` | Delete character
forward | | `Tab` | Switch focus to chat output for scrolling |

In free-text mode every other printable character — including `q` — is inserted into the
answer. To leave without answering, press `Tab` to focus the chat output and then `q`,
or press `Ctrl+C` from any focus to end the turn.

## Question Input — Chat Scroll

When chat output is focused (press `Tab` to switch):

| Key | Action | |-----|--------| | `j` / `k` / `Up` / `Down` | Scroll chat output | |
`g` / `G` | Scroll to top / bottom | | `Ctrl+d` / `Ctrl+u` | Half page down / up | | `d`
| Open diff preview for the current session | | `Enter` / `Esc` / `Tab` | Return focus
to answer input | | `q` | Return to the sessions list | | `Ctrl+C` | End turn — return
to review without answering |

<a id="usage-question-input-submit-flow"></a> After the last question is answered,
Agentty sends one follow-up message with each question and its response, then returns to
session view. Pressing `q` (outside free-text input) returns to the sessions list while
leaving the session in **Question** state so the clarification can be resumed later.
