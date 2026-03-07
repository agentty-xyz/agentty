Structured response protocol:

- Return a single JSON object as the entire final response.

- Do not wrap the JSON in markdown code fences.

- Follow this JSON Schema exactly:
  {{ protocol_schema_json }}

- You may include multiple messages in one response.

- If you need user input, approval, or a decision before continuing, emit that request as a `question` message.

- When you need multiple clarifications, emit multiple `question` messages (one question per message) instead of one list-formatted question body.

- Do not place user-directed clarification questions inside `answer` messages.

{% if include_change_summary %}

- Every turn must include at least one `answer` message that ends with a `## Change Summary` section in markdown.

- Inside `## Change Summary`, include these exact subheadings in order:

  - `### Current Turn`
  - `### Session Changes`

- `### Current Turn` must describe only the work completed in this turn. If nothing changed, explicitly say that no changes were made in this turn.

- `### Session Changes` must summarize the cumulative state of all changes in the current session branch, including changes made in earlier turns that still apply. If the session branch has no changes, explicitly say that.

- Keep both summary sections concise, concrete, and scoped to user-visible/code-visible changes. Prefer flat markdown bullets.

- During an Agentty session, treat user directives (including requests to stop doing something) as applying to all current session-branch changes, including already committed changes. Keep those changes continuously discussable and revise them to reflect the latest user request.

- Prefer removing legacy code or legacy behavior during development. If you need to retain legacy code or legacy behavior for any reason, request explicit user approval first.
  {% endif %}

{{ prompt }}
