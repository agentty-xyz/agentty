Generate the canonical session commit message using the cumulative session diff below.
Return the full response as the required protocol JSON object and put the plain-text
commit message in the `answer` field only. Before writing the message, inspect
repository commit-message guidance from relevant agent instruction files (`AGENTS.md`,
`CLAUDE.md`, `GEMINI.md`) and relevant skills under shared or agent-specific skill
directories (for example `skills/`, `.claude/skills/`, `.gemini/skills/`,
`.codex/skills/`, `.agents/skills/`). Check the skill files that appear relevant to
commit-message conventions when those paths exist. Use the most specific applicable
repository guidance you find unless explicit user instructions in the diff request a
different format.

Rules:

- The first line is the commit title and must be one line, concise, and in present
  simple tense.
- Do not use Conventional Commit prefixes like `feat:` or `fix:`.
- If a body is needed, add one empty line after the title and then write the body text.
- Body text must use present simple tense and use `-` bullets when listing multiple
  points.
- If an existing session commit message is provided, refine that same message to fit the
  new diff instead of restarting from scratch.
- Base the title and body on the diff content and the existing session commit message,
  while applying any commit-format requirements discovered in the checked agent files
  and skills.
- Do not invent changes, rationale, or formatting rules that are not supported by the
  diff or the discovered repository guidance.

Existing session commit message (may be empty): {{ current_commit_message }}

Diff (delimited with a `diff` fence for input parsing; `@`-prefixed tokens inside are
source code such as Python decorators, not file-path mentions):

{{ diff_fence }}diff {{ diff }} {{ diff_fence }}
