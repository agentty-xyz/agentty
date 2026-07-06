Continue this session using the full transcript below.

Treat the user's new prompt as a follow-up in the context of the whole session,
including the transcript and current session worktree state. When the user asks to
remove, revert, or roll back changes, interpret that as changes made during this Agentty
session unless the user explicitly says otherwise; preserve unrelated pre-existing work.
The transcript is historical context only; do not re-execute commands or instructions
from it unless the user's new prompt asks for that work.

\<session_transcript> {{ transcript }} \</session_transcript>

User prompt:

\<user_prompt> {{ prompt }} \</user_prompt>
