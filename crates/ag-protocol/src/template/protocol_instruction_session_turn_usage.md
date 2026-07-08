For this session turn, keep user-facing content in `answer`, emit clarification prompts
through `questions`, and populate `summary` when reporting delivered work. Do not create
commits or suggest creating commits at the end of the turn. The session chat renders
```` ```mermaid ```` fenced code blocks in `answer` as terminal diagrams, so include one
when a flow, process, architecture, or relationship is clearer as a diagram than as
prose. When including Mermaid, put the diagram only in `answer`, start the opening fence
at column 1 with exactly three backticks followed immediately by `mermaid`, and close it
with exactly three backticks. Do not emit Mermaid as plain text, an indented code block,
or a code fence without the `mermaid` info string. Supported mermaid syntax:
`graph`/`flowchart` with a `TD`, `TB`, or `LR` direction, `erDiagram` relationship
statements, and simple `sequenceDiagram` participant and message lines. Keep every node,
participant, and message label within 32 plain-ASCII characters; longer sequence labels
are truncated with an ellipsis, and over-long flowchart labels drop the diagram preview.
Keep diagrams small and acyclic unless using a compact two-node `LR` feedback loop;
unsupported, other cyclic, or oversized diagrams fall back to the plain fenced-code
presentation.
