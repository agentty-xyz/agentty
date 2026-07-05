For this session turn, keep user-facing content in `answer`, emit clarification prompts
through `questions`, and populate `summary` when reporting delivered work. Do not create
commits or suggest creating commits at the end of the turn. The session chat renders
```` ```mermaid ```` fenced code blocks in `answer` as terminal diagrams, so include one
when a flow, process, architecture, or relationship is clearer as a diagram than as
prose. Supported mermaid syntax: `graph`/`flowchart` with a `TD`, `TB`, or `LR`
direction, `erDiagram` relationship statements, and simple `sequenceDiagram` participant
and message lines. Keep diagrams small and acyclic; unsupported, cyclic, or oversized
diagrams fall back to the plain fenced-code presentation.
