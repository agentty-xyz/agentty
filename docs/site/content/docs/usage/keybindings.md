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

| Key                 | Action                                                    |
| ------------------- | --------------------------------------------------------- |
| `q`                 | Quit                                                      |
| `a`                 | Open creation selector (`Regular`, `Draft`, or `Stacked`) |
| `s`                 | Sync active project branch                                |
| `c`                 | Cancel selected session after confirmation                |
| `Enter`             | Open session                                              |
| `j` / `k`           | Navigate sessions                                         |
| `p`                 | Open project switcher popup                               |
| `Tab` / `Shift+Tab` | Switch to next / previous tab                             |
| `?`                 | Help                                                      |

In the `a` selector, `Stacked` is enabled only when the selected session is a root
session with an active branch. `c` appears only for cancelable rows: running sessions,
review-ready sessions, and unstarted draft sessions.

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
<tr><td><code>Enter</code></td><td>Open selector dropdown or <code>Launch Configurations</code> browser; select highlighted option; edit or save a launch configuration</td></tr>
<tr><td><code>Esc</code></td><td>Close selector dropdown or <code>Launch Configurations</code> browser; cancel launch-configuration add/edit input</td></tr>
<tr><td><code>a</code></td><td>Add an entry in the <code>Launch Configurations</code> browser</td></tr>
<tr><td><code>e</code></td><td>Edit the selected entry in the <code>Launch Configurations</code> browser</td></tr>
<tr><td><code>d</code></td><td>Delete the selected entry in the <code>Launch Configurations</code> browser</td></tr>
<tr><td><code>J</code> / <code>K</code></td><td>Move the selected <code>Launch Configurations</code> entry down or up</td></tr>
<tr><td><code>Left</code> / <code>Right</code> / <code>Home</code> / <code>End</code></td><td>Move cursor while adding or editing one <code>Launch Configurations</code> entry</td></tr>
<tr><td><code>Backspace</code> / <code>Delete</code></td><td>Delete characters while adding or editing one <code>Launch Configurations</code> entry</td></tr>
<tr><td><code>Tab</code> / <code>Shift+Tab</code></td><td>Switch to next / previous tab</td></tr>
<tr><td><code>?</code></td><td>Help</td></tr>
</tbody>
</table>

<a id="usage-settings-options"></a> The page is split into `Global settings` for the
app-wide `Theme` row (`Agentty Default`, `Agentty Green`, or `Dark Horizon`) and
`'<project>' settings` for the reasoning level, model defaults, commit coauthor toggle,
and `Launch Configurations` rows described in [Workflow](@/docs/usage/workflow.md).
Selector rows open dropdowns; use `j` / `k` to move through the dropdown and `Enter` to
save the highlighted value. The `Launch Configurations` row opens a list browser where
each command is added, edited, deleted, or reordered as its own entry.

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

## Issues

| Key                 | Action                                                           |
| ------------------- | ---------------------------------------------------------------- |
| `q`                 | Quit                                                             |
| `j` / `k`           | Navigate assigned issues                                         |
| `Enter`             | Open base details for the selected issue                         |
| `s`                 | Refresh open GitHub issues assigned to you in the active project |
| `Tab` / `Shift+Tab` | Switch to next / previous tab                                    |
| `?`                 | Help                                                             |

On the read-only detail page, use `j` / `k` or `Up` / `Down` to scroll, `Ctrl+d` /
`Ctrl+u` to move by half pages, `g` / `G` to jump to the top or bottom, and `q` / `Esc`
to return to the issue list. Issue comments are not loaded in this iteration.

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
- **InProgress** sessions use `Enter` to queue a follow-up message and keep `r`
  available to queue session sync behind the running turn. Each `Ctrl+c` retracts the
  newest queued message; when the queue is empty, `Ctrl+c` stops the active turn.
- Root **Review** and **AgentReview** sessions offer `F` to fork the current branch and
  copied transcript history into a new independent session; stacked children hide `F`.
- **Draft** sessions use `Enter` to add a staged message and `s` to start the staged
  session. They hide `o` and `r` until launch and let `Ctrl+V`, `Ctrl+Shift+V`, or
  `Alt+V` open the composer with an image paste; stacked drafts also hide `m` and show
  `s` only when the parent is review-ready and the stack is idle.
- **Question** sessions hide `r` until they return to review-ready state.
- Review-ready stacked parents with a materialized child keep `Enter`, `m`, and `r`
  while the stack is idle, but hide `/` until the child is terminal or no longer linked.
- **Done** sessions offer `c` to start a continuation draft (confirmation popup).
- **Canceled**, **Queued**, **Rebasing**, and **Merging** sessions are read-only (`q`,
  scroll, help).

`o` runs the configured `Launch Configurations` entry, or opens a selector popup when
several are configured. Run Agentty inside `tmux` when you rely on
`Launch Configurations`, because those commands are dispatched into tmux windows.
Publish (`p`), sync (`r`), and stacked behavior are described in
[Workflow](@/docs/usage/workflow.md).

## Publish Popup

| Key                               | Action                                 |
| --------------------------------- | -------------------------------------- |
| `Enter`                           | Publish typed or default branch target |
| `Esc` / `q`                       | Cancel and return to session view      |
| `Left` / `Right` / `Home` / `End` | Move cursor                            |
| `Up` / `Down`                     | Move cursor across wrapped lines       |
| `Backspace` / `Delete`            | Delete character                       |
| text keys                         | Edit remote branch name                |

## Launch Configuration Selector

| Key         | Action                                 |
| ----------- | -------------------------------------- |
| `j` / `k`   | Move selection                         |
| `Enter`     | Open worktree and run selected command |
| `Esc` / `q` | Cancel and return to session view      |

## Diff Mode

Pressing `d` from session view opens diff mode with the right panel showing the git
diff. Within diff mode, press `c` to toggle the right panel between the annotated git
diff and the cached review-request comments.

| Key           | Action                                   |
| ------------- | ---------------------------------------- |
| `q` / `Esc`   | Back to session                          |
| `j` / `k`     | Select file                              |
| `Up` / `Down` | Scroll selected panel                    |
| `c`           | Toggle right panel between diff/comments |
| `?`           | Help                                     |

<a id="usage-diff-totals"></a> The diff panel title includes aggregate `+added` and
`-removed` line totals plus the selected file or folder's added/removed counts, and
cached review-request line comments render inline below matching diff lines. See
[Workflow](@/docs/usage/workflow.md) for the comments panel behavior.

## Prompt Input

| Key                                 | Action                               |
| ----------------------------------- | ------------------------------------ |
| `Enter`                             | Submit prompt or stage it in a draft |
| `Alt+Enter` / `Shift+Enter`         | Insert newline                       |
| `Ctrl+J` / `Ctrl+M`                 | Insert newline (terminal fallback)   |
| `Ctrl+V` / `Ctrl+Shift+V` / `Alt+V` | Paste image as `[Image #n]`          |
| `Cmd+Left` / `Cmd+Right`            | Move to start / end of current line  |
| `Option+Left` / `Option+Right`      | Move to previous / next word         |
| `Option+Backspace`                  | Delete previous word                 |
| `Cmd+Backspace`                     | Delete current line                  |
| `Esc`                               | Cancel                               |
| `Tab`                               | Focus chat output for scrolling      |
| `@`                                 | Open file picker                     |
| `/`                                 | Open slash commands                  |

While the chat output is focused, `j` / `k` / `Up` / `Down` scroll the transcript, `g` /
`G` jump to the top or bottom, `Ctrl+D` / `Ctrl+U` scroll by half a page, and `Tab`
returns focus to the composer. The typed draft is preserved: keys pressed in chat focus
never edit it, and `Ctrl+C` still cancels the composer.

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
| `Enter`                   | Choose highlighted option       |
| `Tab`                     | Focus chat output for scrolling |
| `q`                       | Return to sessions list         |
| `Ctrl+C`                  | End turn without answering      |

## Question Input — Free Text

After moving above or below the predefined option list, or when no predefined options
exist:

| Key                              | Action                                 |
| -------------------------------- | -------------------------------------- |
| `Enter`                          | Submit response; blank means no answer |
| `Alt+Enter` / `Shift+Enter`      | Insert newline                         |
| `Ctrl+J` / `Ctrl+M`              | Insert newline (terminal fallback)     |
| `Ctrl+C`                         | End turn without answering             |
| `Esc`                            | End turn when answer text is empty     |
| `Left` / `Right` / `Up` / `Down` | Move cursor                            |
| `Backspace` / `Delete`           | Delete character                       |
| `Home` / `End`                   | Move to start / end                    |
| `Cmd+Left` / `Cmd+Right`         | Move to start / end of current line    |
| `Option+Left` / `Option+Right`   | Move to previous / next word           |
| `Option+Backspace` / `Ctrl+W`    | Delete previous word                   |
| `Cmd+Backspace`                  | Delete current line                    |
| `Ctrl+K`                         | Delete to end of current line          |
| `Ctrl+D`                         | Delete character forward               |
| `Tab`                            | Focus chat output for scrolling        |

In free-text mode every other printable character — including `q` — is inserted into the
answer. To leave without answering, press `Tab` to focus the chat output and then `q`,
or press `Ctrl+C` from any focus to end the turn.

## Question Input — Chat Scroll

When chat output is focused (press `Tab` to switch):

| Key                       | Action                            |
| ------------------------- | --------------------------------- |
| `j` / `k` / `Up` / `Down` | Scroll chat output                |
| `g` / `G`                 | Scroll to top / bottom            |
| `Ctrl+d` / `Ctrl+u`       | Half page down / up               |
| `d`                       | Open current-session diff preview |
| `Enter` / `Esc` / `Tab`   | Return focus to answer input      |
| `q`                       | Return to sessions list           |
| `Ctrl+C`                  | End turn without answering        |

<a id="usage-question-input-submit-flow"></a> After the last question is answered,
Agentty sends one follow-up message with each question and its response, then returns to
session view. Pressing `q` (outside free-text input) returns to the sessions list while
leaving the session in **Question** state; answers already submitted and the current
free-text draft are kept, so reopening the session resumes at the next unanswered
question.
