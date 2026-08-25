Generate the canonical session commit message using the cumulative session diff below.
Return the full response as the required protocol JSON object. Put the plain-text commit
message in `answer` and leave `questions` empty.

First inspect applicable repository commit-message guidance in agent instruction files
(`AGENTS.md`, `CLAUDE.md`, `GEMINI.md`) and relevant skills under shared or
agent-specific directories such as `skills/`, `.agents/skills/`, `.claude/skills/`, and
`.codex/skills/`, when those paths exist.

Apply this precedence order:

1. Explicit user instructions in the diff request.
1. The most specific applicable repository guidance you find.
1. The default rules below.

Rules:

- The first line is a concise, one-line title in present simple tense.
- Do not use Conventional Commit prefixes like `feat:` or `fix:` unless higher-priority
  user instructions or repository guidance require them.
- If needed, put the body after one empty line. Use present simple tense and `-` bullets
  for multiple points.
- When an existing session commit message is provided, refine that same message for the
  new diff instead of restarting.
- Base the title and body on the diff and existing message while applying discovered
  format requirements. Do not invent unsupported changes, rationale, or rules.

Existing session commit message (may be empty): {{ current_commit_message }}

Diff (delimited with a `diff` fence for input parsing; `@`-prefixed tokens inside are
source code such as Python decorators, not file-path mentions):

{{ fenced_diff }}
