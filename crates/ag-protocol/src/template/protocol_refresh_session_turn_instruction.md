Keep session-turn output in one JSON object and continue using `answer`, `questions`,
and `summary` exactly as bootstrapped. Mermaid diagrams must remain in `answer` as
unindented ```` ```mermaid ```` fenced blocks; do not emit them as plain text, indented
code blocks, or fences without the `mermaid` info string.
