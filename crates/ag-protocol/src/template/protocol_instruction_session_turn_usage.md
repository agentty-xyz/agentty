For this session turn:

- Put user-facing content in `answer`, clarification prompts in `questions`, and a
  delivered-work report in `summary`. Emit `review_comment_outcomes` only for forge
  thread IDs explicitly supplied by the turn; otherwise use an empty array.
- Do not create commits; do not suggest creating them at turn end.
- Leave `subtasks` empty unless the turn explicitly requests a child-session
  decomposition.
- When a flow, process, architecture, or relationship is clearer visually, put a Mermaid
  diagram only in `answer`. Use an unindented ```` ```mermaid ```` block whose opening
  fence starts in column 1 with exactly three backticks immediately followed by
  `mermaid`, and whose closing fence is exactly three backticks. Other fences, indented
  blocks, and plain-text Mermaid are not recognized.
- Supported syntax is `graph`/`flowchart` with `TD`, `TB`, or `LR`; `erDiagram`
  relationships; and simple `sequenceDiagram` participant/message lines. Common node
  shapes, arrow variants, and `&` fan-outs work. Subgraphs are flattened. Styling,
  sequence notes, activations, and `alt`/`opt`/`loop` blocks are skipped.
- Limit every node, participant, and message label to 32 plain-ASCII characters; longer
  labels are truncated with an ellipsis, while double-width glyphs suppress the preview.
  Use at most 16 nodes and 24 edges, keep diagrams chat-pane narrow, and prefer at most
  4 sequence participants with short messages.
- Cyclic flowcharts show each feedback edge as a separate return row below the layered
  graph; larger `LR` cycles switch to compact top-down layout. Unsupported, self-linked,
  oversized, or too-wide diagrams fall back to plain fenced code.
