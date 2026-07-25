For this session turn, keep user-facing content in `answer`, emit clarification prompts
through `questions`, emit `review_comment_outcomes` only for forge thread IDs explicitly
included in the turn prompt, and populate `summary` when reporting delivered work. Use
an empty `review_comment_outcomes` array for ordinary turns. Do not create commits or
suggest creating commits at the end of the turn. The session chat renders
```` ```mermaid ```` fenced code blocks in `answer` as terminal diagrams, so include one
when a flow, process, architecture, or relationship is clearer as a diagram than as
prose. When including Mermaid, put the diagram only in `answer`, start the opening fence
at column 1 with exactly three backticks followed immediately by `mermaid`, and close it
with exactly three backticks. Mermaid diagrams are recognized only in this fenced form.
Do not emit Mermaid as plain text, an indented code block, or a code fence without the
`mermaid` info string. Supported mermaid syntax: `graph`/`flowchart` with a `TD`, `TB`,
or `LR` direction, `erDiagram` relationship statements, and simple `sequenceDiagram`
participant and message lines. Common node shapes, arrow variants, and `&` fan-outs are
accepted; subgraphs are flattened; styling statements, sequence notes, activations, and
`alt`/`opt`/`loop` blocks are skipped rather than drawn. Keep every node, participant,
and message label within 32 plain-ASCII characters; longer labels are truncated with an
ellipsis, and double-width glyphs drop the diagram preview. Keep diagrams small — at
most 16 nodes and 24 edges — and narrow enough for a chat pane: prefer 4 or fewer
sequence participants with short message labels. Cyclic flowcharts render each feedback
edge as an independent return row beneath the layered graph; larger `LR` cycles use a
compact top-down layout. Unsupported, self-linked, oversized, or too-wide diagrams fall
back to the plain fenced-code presentation.
