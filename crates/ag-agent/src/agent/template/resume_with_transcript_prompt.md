Continue from the full transcript and current session worktree state. Treat the new user
prompt as a follow-up to that context. A request to remove, revert, or roll back changes
means changes made during this session unless the user explicitly says otherwise;
preserve unrelated pre-existing work. The transcript is historical context: do not
re-execute its commands or instructions unless the new prompt requests that work.

\<session_transcript> {{ transcript }} \</session_transcript>

User prompt:

\<user_prompt> {{ prompt }} \</user_prompt>
