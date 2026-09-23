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

Prompt, question, publish-branch, and launch-configuration inputs share these keys.
Completion menus and question options take precedence when open.

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

`a` opens the session-type selector, with a warning first if configured hooks are
missing. `Enter` continues past that warning; `Esc` / `q` cancels. New-session setup
runs in the background; `s` retries failed setup from session view.

For draft, stacked, and orchestrator eligibility, see
[Session Types](@/docs/usage/workflow.md#session-types). Project sync leaves navigation
available; see [Project Sync](@/docs/usage/workflow.md#project-sync).

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

Project sync follows the same rules as in the Sessions list.

<a id="usage-project-list-active-highlight"></a> The currently active project is
highlighted in the table with a `* ` prefix and accented row text.

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
Claude or Codex, speed. Launch commands use the shared text editor. See
[Settings Scope](@/docs/usage/workflow.md#settings-scope) for available settings.

## Session View

<a id="usage-session-view-actions"></a> Available actions depend on the session state.
The full set in **Review** state, subject to session and forge availability:

| Key                 | Action                                              |
| ------------------- | --------------------------------------------------- |
| `q`                 | Back to list                                        |
| `Enter`             | Compose a reply                                     |
| `/`                 | Open composer with `/` prefilled                    |
| `o`                 | Run a launch configuration in the worktree (`tmux`) |
| `p`                 | Publish branch and create or refresh review request |
| `c`                 | Show linked review-request comments                 |
| `d`                 | Show diff when the session has changes              |
| `f`                 | Append or regenerate focused review output          |
| `F`                 | Fork session with copied transcript history         |
| `m`                 | Add to merge queue after confirmation               |
| `r`                 | Sync session branch                                 |
| `j` / `k`           | Scroll output                                       |
| `g` / `G`           | Scroll to top / bottom                              |
| `Ctrl+d` / `Ctrl+u` | Half page down / up                                 |
| `?`                 | Help                                                |

State-specific differences:

- **AgentReview**: `r` cancels the pending review and starts sync.
- **InProgress**: `Enter`, `r`, and `p` queue messages, sync, and publishing. `Ctrl+C`
  retracts the newest queued message; with none left, it stops the turn.
- **Rebasing**: `Enter` and `p` queue work; cancellation and slash commands are
  unavailable.
- **Draft**: `Enter` stages, `s` starts, and image-paste keys open the composer. Stacked
  drafts require a review-ready parent and idle stack.
- **Question**: Answer through the question panel; `r` is hidden.
- **Orchestrator**: `a` approves plans or opens integration choices. Branch actions are
  hidden; cancel with `c` from the list.
- **Managed worker**: Inspect chat or `d`; `D` permanently detaches an implementation
  worker. In `tmux`, a review-ready worker offers `o`, with a warning that edits can
  invalidate verification. Other mutation actions and `Ctrl+C` are disabled.
- **Research child**: Inspect chat and `d`, including archived evidence after cleanup;
  no detach or worktree-open action.
- **Stacked child**: No `F`. Idle review-ready parents retain reply, slash, merge, and
  sync actions.
- **Linked review request**: `c` opens comments; local `m` is unavailable.
- **Merged**: Read-only chat, comments, and diff until manual target sync completes.
- **Done / Canceled**: `c` confirms a continuation draft; linked comments are hidden.
- **Queued / Merging**: Navigation, scrolling, and help remain available.

A known-empty diff hides `d`. `o` requires `tmux` and runs the configured launch command
or opens a command selector. See [Workflow](@/docs/usage/workflow.md) for lifecycle
details.

## Review Comments in Diff Mode

Use `c` to open linked review comments or `d` to open Files. Comments are grouped as
unresolved, outdated, resolved, and standalone. The right pane shows the selected
conversation and available line context; outdated or file-level comments explain why
line context is absent.

| Key           | Action                                   |
| ------------- | ---------------------------------------- |
| `q` / `Esc`   | Return to session view                   |
| `j` / `k`     | Select previous/next comment             |
| `f`           | Focus the Files section                  |
| `Up` / `Down` | Scroll selected comment info             |
| `Space`       | Toggle the selected actionable thread    |
| `Enter`       | Submit all selected threads to the agent |

`[ ]` marks an actionable thread; `[x]` marks a selection. Submission requires a
reply-capable session and at least one selected thread. Outdated unresolved threads
remain actionable; resolved threads and standalone comments are read-only. See
[Addressing Review Comments](@/docs/usage/workflow.md#addressing-review-comments) for
reply and resolution behavior.

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

`d` opens Files. Focus a patch with `Enter` or `l`, add feedback on changed lines, then
submit the batch with `s`. `Shift+V` starts a range selection; move to extend it and
press `Enter` to comment, or `Esc` to cancel. `Shift+C` opens whole-file feedback and
clears a range selection.

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

<a id="usage-diff-totals"></a> The panel and file tree show added/removed line totals.
File comments sit above the patch; inline comments retain their old/new line context.

Press `Enter` on a comment to edit it and `Enter` or `Esc` to finish. Finish with empty
text to delete it. `@` opens file lookup: arrows choose, `Tab` / `Enter` insert, and
`Esc` dismisses. Completed comments survive navigation. `s` combines them with existing
draft text and images; a new turn clears them. Retracting a queued batch preserves them.
Read-only diffs hide comment editing and submission.

For Markdown, `p` previews the complete post-change file and stays enabled across file
navigation. Other file types keep showing diffs; unavailable files show a notice. Press
`p` again to return to the patch.

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
| `Shift+Tab`                         | Cycle the session permission mode   |
| `@`                                 | Open file picker                    |
| `/`                                 | Open slash commands                 |
| `j` / `k` / `Up` / `Down`           | Navigate and wrap slash menu        |

Use `/mode` to select `Auto Edit`, `Auto Edit + Auto Address Comments`, or `Read Only`.
`Shift+Tab` cycles those modes in that order.

With chat focused, scroll using `j` / `k` or arrows, jump with `g` / `G`, or move half a
page with `Ctrl+D` / `Ctrl+U`. `d` previews available changes, `Tab` returns to input,
and `q` returns to the list with the draft saved. `Ctrl+C` is ignored in chat focus.

Text uses normal terminal paste. Image shortcuts attach an image as `[Image #n]`; see
[Prompt Input Extras](@/docs/usage/workflow.md#prompt-input-extras). If modified Enter
keys do not reach Agentty, use `Ctrl+J` or `Ctrl+M` for newlines.

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

Type `@` and a filename fragment to look up files. Arrows select a match; `Tab` /
`Enter` insert it without sending the answer. With no matches, those keys close the
lookup. `Esc` dismisses it; modified Enter still inserts a newline.

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
