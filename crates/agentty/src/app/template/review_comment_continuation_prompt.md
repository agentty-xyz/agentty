This session still has unfinished forge review comments. Continue the current user
request and finish these previously authorized review targets:

{{ thread_ids }}

Re-evaluate the comments from the session context against the current files, including
any changes made in this turn. Previous outcome reports are evidence, not proof that a
fix remains correct. Do not create commits or post forge replies yourself.

When the work is complete, return exactly one `review_comment_outcomes` item for each
listed thread ID. Copy the ID exactly and include a concise `reply` suitable for the
forge. Use `fixed` only when the request is satisfied and safe to resolve; use
`no_change_needed` when no change is appropriate and the thread should remain open. If
essential context or a user decision is missing, ask through `questions` and leave the
outcomes empty until the clarification is answered.
