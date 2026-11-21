+++
title = "Keybindings"
description = "Keyboard shortcuts across lists, session view, diff mode, prompt input, and question input."
weight = 2
+++

<a id="usage-keybindings-introduction"></a> This page lists keyboard shortcuts for each
Agentty view. In the app, press `?`, shown as `?: help` in the top bar, to list the
shortcuts available on the current page; close it with `Esc` or `q`, and scroll long
lists with `j` / `k`.

For session states and behavior, see [Workflow](@/docs/usage/workflow.md).

<!-- more -->

## Session List

| Key                 | Action                                               |
| ------------------- | ---------------------------------------------------- |
| `q`                 | Quit after confirmation                              |
| `a`                 | Check hooks, then open the session creation selector |
| `s`                 | Sync active project branch                           |
| `c`                 | Cancel selected session after confirmation           |
| `Enter`             | Open session, or load more / show less archive rows  |
| `j` / `k`           | Navigate sessions                                    |
| `p`                 | Open project switcher popup                          |
| `Tab` / `Shift+Tab` | Switch to next / previous tab                        |
| `?`                 | Help                                                 |

The archive shows the 10 most recently updated sessions; `Load more...` adds 10 more and
`Show less...` returns to the first 10. The archive heading counts all archived sessions
in the project. Switching projects resets the archive window.

If configured hooks are missing, `a` warns first: `Enter` continues and `Esc` / `q`
cancels. See [Session Types](@/docs/usage/workflow.md#session-types) and
[Project Sync](@/docs/usage/workflow.md#project-sync).

<a id="usage-session-list-project-switcher"></a> The `p` popup lists projects in
most-recently-opened order, marks the active one with `* `, and shows `▶ N` for projects
with running sessions. Use `j` / `k` to move, `Enter` to switch projects, and `Esc` or
`q` to close.

## Project List

| Key                 | Action                        |
| ------------------- | ----------------------------- |
| `q`                 | Quit after confirmation       |
| `s`                 | Sync active project branch    |
| `a`                 | Add project by path           |
| `d`                 | Discover projects in a folder |
| `Enter`             | Select active project         |
| `j` / `k`           | Navigate projects             |
| `Tab` / `Shift+Tab` | Switch to next / previous tab |
| `?`                 | Help                          |

In path popups, `Enter` submits and `Esc` cancels. In discovery results, `j` / `k`
navigate, `Space` selects a repository, `Enter` saves the selection, and `Esc` discards
it. In the startup discovery popup, `y` scans your home directory and `n` / `Esc` skips.
See [Projects and Discovery](@/docs/usage/workflow.md#projects-and-discovery).

<a id="usage-project-list-active-highlight"></a> The active project has a `* ` prefix
and accented row text.

## Settings

| Key                 | Action                                            |
| ------------------- | ------------------------------------------------- |
| `q`                 | Close an editor or selector first; otherwise quit |
| `s`                 | Sync project                                      |
| `j` / `k`           | Navigate rows or options                          |
| `Enter`             | Open selection, advance a role picker, or save    |
| `Esc`               | Close selection or cancel an edit                 |
| `a`                 | Add a launch command                              |
| `e`                 | Edit selected launch command                      |
| `d`                 | Delete selected launch command                    |
| `J` / `K`           | Move launch command down / up                     |
| `Tab` / `Shift+Tab` | Next / previous tab                               |
| `?`                 | Help                                              |

<a id="usage-settings-options"></a> Role pickers ask for model, reasoning, and, for
Claude or Codex, speed. See [Settings Scope](@/docs/usage/workflow.md#settings-scope).

## Session View

<a id="usage-session-view-actions"></a> Available actions depend on the session state.
The full set in **Review**, subject to session and forge availability:

| Key                 | Action                                              |
| ------------------- | --------------------------------------------------- |
| `q`                 | Back to list                                        |
| `Enter`             | Compose a reply                                     |
| `/`                 | Open composer with `/` prefilled                    |
| `o`                 | Run a launch configuration in the worktree (`tmux`) |
| `p`                 | Publish branch and create or refresh review request |
| `c`                 | Show linked review-request comments                 |
| `d`                 | Show diff in any state, including live edits        |
| `w`                 | Inspect saved execution graphs                      |
| `f`                 | Show, generate, or regenerate focused review        |
| `F`                 | Fork session with copied transcript history         |
| `W`                 | Watch an open review request, or stop watching      |
| `m`                 | Add to merge queue after confirmation               |
| `r`                 | Sync session branch                                 |
| `j` / `k`           | Scroll output                                       |
| `g` / `G`           | Scroll to top / bottom                              |
| `Ctrl+d` / `Ctrl+u` | Half page down / up                                 |
| `?`                 | Help                                                |

State-specific differences:

- **AgentReview**: `r` cancels the pending review and starts sync.
- **Watching**: Same as **Review**; `W` stops watching. See
  [Watching a Review Request](@/docs/usage/workflow.md#usage-review-request-watch).
- **InProgress**: `Enter`, `r`, and `p` queue a message, sync, or publishing. `Ctrl+C`
  retracts the newest queued message, then stops the turn. `Ctrl+s` sends queued chat
  into the running turn when the harness supports it; see
  [Active Turns](@/docs/usage/workflow.md#active-turns-and-the-message-queue).
- **InProgress / Rebasing**: Slash commands offer only settings; see
  [Active Turns](@/docs/usage/workflow.md#active-turns-and-the-message-queue).
- **Rebasing**: `Enter` and `p` queue work; cancellation is unavailable.
- **Draft**: `Enter` stages, `s` starts, and image-paste keys open the composer.
- **Question**: Answer through the question panel; `r` is hidden.
- **Linked review request**: `c` opens comments; local `m` is unavailable.
- **Merged**: Read-only chat, comments, and diff until manual target sync completes.
- **Done / Canceled**: `c` confirms a continuation draft and linked comments are hidden;
  `d` shows the archived diff.
- **Queued / Merging**: Navigation, scrolling, diff, and help only.

## Execution Graph Inspector

Press `w` from session view. Actions appear only when the displayed snapshot supports
them; see [Workflow](@/docs/usage/workflow.md#execution-graph-inspector).

| Key                              | Action                                           |
| -------------------------------- | ------------------------------------------------ |
| `[` / `]`                        | Previous / next graph                            |
| `p`                              | Return to the pending proposal                   |
| `j` / `k`, `Down` / `Up`         | Select node                                      |
| `r`                              | Refresh graph state                              |
| `o`                              | Inspect next output                              |
| `J` / `K`, `PageDown` / `PageUp` | Scroll details                                   |
| `Home`                           | Scroll to top                                    |
| `a`                              | Accept the displayed output version              |
| `e`                              | Reply to or revise the selected task             |
| `t`                              | Retry selected failed node                       |
| `s`                              | Resume paused graph dispatch                     |
| `x`                              | Confirm cancellation of the whole graph          |
| `Enter`                          | Submit graph input                               |
| `Ctrl+r`                         | Refresh and reconcile a rejected draft           |
| `PageUp` / `PageDown` in input   | Scroll the editor notice                         |
| `Alt+Enter` / `Shift+Enter`      | Insert newline in graph input                    |
| `Esc`                            | Cancel graph input, or return to session view    |
| `q`                              | Return to session view while outside graph input |

## Review Comments in Diff Mode

Press `c` in session view to open linked review comments, grouped as unresolved,
outdated, resolved, and standalone. The right pane shows the selected conversation and
any available line context.

| Key           | Action                                   |
| ------------- | ---------------------------------------- |
| `q` / `Esc`   | Return to session view                   |
| `j` / `k`     | Select previous/next comment             |
| `f`           | Focus the Files section                  |
| `Up` / `Down` | Scroll selected comment info             |
| `Space`       | Toggle the selected actionable thread    |
| `Enter`       | Submit all selected threads to the agent |

`[ ]` marks an actionable thread and `[x]` a selected one. Submitting needs a
reply-capable session. See
[Addressing Review Comments](@/docs/usage/workflow.md#addressing-review-comments).

## Diff Mode

`d` opens Files. Focus a patch with `Enter` or `l`, comment on changed lines, then
submit all comments with `s`.

| Key                         | Action                                         |
| --------------------------- | ---------------------------------------------- |
| `q`                         | Back to session                                |
| `Esc`                       | Focus Files, or leave from Files               |
| `j` / `k`                   | Select a file, changed line, or inline comment |
| `Shift+j` / `Shift+k`       | Scroll selected file, or select a diff row     |
| `Shift+C`                   | Comment on the selected whole file             |
| `Shift+V`                   | Start visual changed-row selection             |
| `Alt+Enter` / `Shift+Enter` | Insert a comment newline                       |
| `Enter`                     | Focus a file, or edit/finish a comment         |
| `l`                         | Focus the selected file's changes              |
| `Up` / `Down`               | Scroll file/preview, or select a diff row      |
| `Left` / `h` / `f`          | Return to Files                                |
| `p`                         | Toggle markdown preview                        |
| `c`                         | Focus linked review comments                   |
| `s`                         | Submit all diff comments                       |
| `?`                         | Help                                           |

After `Shift+V`, move to extend the range and press `Enter` to comment or `Esc` to
cancel. Finish a comment with `Enter` or `Esc`; finishing with empty text deletes it. In
comments, `@` opens file lookup: arrows choose, `Tab` / `Enter` insert, and `Esc`
dismisses. Leaving with unsent comments opens a `Submit diff comments?` preview: `Yes`
submits, `No` leaves and keeps them, and `Esc` stays. Read-only diffs hide commenting.

<a id="usage-diff-totals"></a> The panel and file tree show added and removed line
totals, hidden when names need the space. `A`, `M`, `D`, and `R` mark added, modified,
deleted, and renamed files; binary files show `Binary`.

For Markdown files, `p` previews the complete post-change file and stays on while you
switch files; other files keep showing diffs.

## Shared Text Editing

Prompt, question, graph, publish-branch, and launch-configuration inputs share these
keys. Open completion menus and question options take precedence.

| Key                                      | Action                              |
| ---------------------------------------- | ----------------------------------- |
| `Left` / `Right`                         | Move one character                  |
| `Option+Left` / `Shift+Left` / `Alt+B`   | Move to previous word               |
| `Option+Right` / `Shift+Right` / `Alt+F` | Move to next word                   |
| `Home` / `End`                           | Move to start / end of input        |
| `Cmd+Left` / `Cmd+Right`                 | Move to start / end of current line |
| `Ctrl+A` / `Ctrl+E`                      | Move to start / end of current line |
| `Backspace` / `Delete`, `Ctrl+D`         | Delete backward / forward           |
| `Option+Backspace` / `Shift+Backspace`   | Delete previous word                |
| `Ctrl+W`                                 | Delete previous word                |
| `Cmd+Backspace` / `Ctrl+U`               | Delete current line                 |
| `Ctrl+K`                                 | Delete to end of current line       |
| `Ctrl+Z`                                 | Undo                                |
| `Ctrl+Y` / `Ctrl+Shift+Z`                | Redo                                |

On macOS, undo uses `Ctrl+Z`; terminals such as Ghostty may consume `Cmd+Z`. `Cmd`,
`Option`, and modified `Enter` shortcuts need the terminal to forward them. Otherwise
use `Ctrl+U`, `Ctrl+W`, or `Ctrl+J` for a newline. Single-line inputs keep only the
first line of pasted text.

## Publish Popup

| Key       | Action                                                |
| --------- | ----------------------------------------------------- |
| `Enter`   | Publish the typed or default branch in the background |
| `Esc`     | Cancel and return to session view                     |
| text keys | Edit the remote branch name, including `q`            |

## ChatGPT Sign-In Popup

Opens from the Harness `/model` list on a `chatgpt/` model that needs a sign-in, or when
sending a prompt on such a model. It shows the sign-in URL in case the browser does not
open.

| Key     | Action                                                       |
| ------- | ------------------------------------------------------------ |
| `Enter` | Open the browser sign-in, or close the finished result       |
| `Esc`   | Cancel, including a running sign-in, and return to the draft |

## Launch Configuration Selector

| Key         | Action                                 |
| ----------- | -------------------------------------- |
| `j` / `k`   | Move selection                         |
| `Enter`     | Open worktree and run selected command |
| `Esc` / `q` | Cancel and return to session view      |

## Prompt Input

Prompt input also supports
[shared text editing](@/docs/usage/keybindings.md#shared-text-editing).

| Key                                 | Action                                  |
| ----------------------------------- | --------------------------------------- |
| `Enter`                             | Send prompt or stage it in a draft      |
| `Alt+Enter` / `Shift+Enter`         | Insert newline                          |
| `Ctrl+J` / `Ctrl+M`                 | Insert newline (terminal fallback)      |
| `Up` / `Down`                       | Move between lines, then browse history |
| `Ctrl+V` / `Ctrl+Shift+V` / `Alt+V` | Paste image as `[Image #n]`             |
| `Esc` / `Ctrl+C`                    | Cancel                                  |
| `Tab`                               | Focus chat output for scrolling         |
| `Shift+Tab`                         | Cycle the session permission mode       |
| `@`                                 | Open file picker                        |
| `/`                                 | Open slash commands                     |
| `j` / `k` / `Up` / `Down`           | Navigate and wrap slash menu            |

`Shift+Tab` cycles `Auto Edit`, `Auto Edit + Auto Address Comments`, and `Read Only`.
With `--experimental-graph`, `/graph` arms the composer: `Enter` submits your task and
`Esc` disarms it, keeping the draft. See
[Generated Graph Proposals](@/docs/usage/workflow.md#generated-graph-proposals).

With chat focused, scroll with `j` / `k` or arrows, jump with `g` / `G`, or move half a
page with `Ctrl+D` / `Ctrl+U`. `d` previews changes, `Tab` returns to input, and `q`
returns to the list with the draft saved. Other keys, including `Ctrl+C`, are ignored.
See [Prompt Input Extras](@/docs/usage/workflow.md#prompt-input-extras) for images and
file lookup.

## Question Input — Option Selection

When predefined options are shown:

| Key                       | Action                          |
| ------------------------- | ------------------------------- |
| `j` / `k` / `Up` / `Down` | Navigate options                |
| `Enter`                   | Send highlighted option         |
| `Tab`                     | Focus chat output for scrolling |
| `q`                       | Return to sessions list         |
| `Ctrl+C`                  | End turn or close graph panel   |

## Question Input — Free Text

After moving past the option list, or when there are no options, the answer input
supports [shared text editing](@/docs/usage/keybindings.md#shared-text-editing) and:

| Key                                 | Action                               |
| ----------------------------------- | ------------------------------------ |
| `Enter`                             | Send response; blank means no answer |
| `Alt+Enter` / `Shift+Enter`         | Insert newline                       |
| `Ctrl+J` / `Ctrl+M`                 | Insert newline (terminal fallback)   |
| `Ctrl+V` / `Ctrl+Shift+V` / `Alt+V` | Paste image as `[Image #n]`          |
| `@`                                 | Look up files                        |
| `Ctrl+C`                            | End turn or close graph panel        |
| `Tab`                               | Focus chat output for scrolling      |

In file lookup, arrows select a match, `Tab` / `Enter` insert it without sending, and
`Esc` dismisses. Every other printable character, including `q`, goes into the answer;
to leave without answering, press `Tab` and then `q`, or press `Ctrl+C`.

## Question Input — Chat Scroll

When chat output is focused (press `Tab` to switch):

| Key                       | Action                       |
| ------------------------- | ---------------------------- |
| `j` / `k` / `Up` / `Down` | Scroll chat output           |
| `g` / `G`                 | Scroll to top / bottom       |
| `Ctrl+d` / `Ctrl+u`       | Half page down / up          |
| `d`                       | Open diff or diagnostic      |
| `Tab`                     | Return focus to answer input |
| `q`                       | Return to sessions list      |

<a id="usage-question-input-submit-flow"></a> After the last answer, Agentty sends one
follow-up with every question and response. Leaving with `q` keeps the session in
**Question** with submitted answers and the current draft, so reopening resumes at the
next unanswered question.
