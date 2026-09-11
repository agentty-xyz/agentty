use std::fmt::Write;

use ratatui::style::Color;

use crate::markdown::{code_block_style, render_markdown, render_markdown_with_settings};
use crate::mermaid;
use crate::style::TextRenderSettings;

#[test]
fn test_render_markdown_renders_mermaid_block_as_diagram() {
    // Arrange
    let input = "```mermaid {theme=default}\ngraph TD\n    A[Start] --> B[Finish]\n```";

    // Act
    let lines = render_markdown(input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Start"));
    assert!(text.contains("Finish"));
    assert!(text.contains("┌"));
    assert!(text.contains("▼"));
    assert!(!text.contains("graph TD"));
    assert!(!text.contains("```"));
}

#[test]
fn test_render_markdown_stacks_over_wide_left_right_mermaid_block() {
    // Arrange
    let input = concat!(
        "```mermaid\n",
        "flowchart LR\n",
        "    Q[Qwen complete] --> T[Tracing spans and events]\n",
        "    Q --> M[OTel metrics API]\n",
        "    T --> S[Trace and log providers]\n",
        "    M --> P[Meter provider]\n",
        "    S --> O[OTLP HTTP protobuf]\n",
        "    P --> O\n",
        "    O --> C[Collector on port 4318]\n",
        "    C --> B[Telemetry backends]\n",
        "    B --> G[Grafana on port 3000]\n",
        "```",
    );

    // Act
    let lines = render_markdown(input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Qwen complete"));
    assert!(text.contains("Tracing spans and events"));
    assert!(text.contains("Grafana on port 3000"));
    assert!(text.contains('▼'));
    assert!(!text.contains("flowchart LR"));
}

#[test]
fn test_render_markdown_keeps_code_fallback_for_mermaid_prefix_language() {
    // Arrange
    let input = "```mermaid-diagram\ngraph TD\n    A[Start] --> B[Finish]\n```";

    // Act
    let lines = render_markdown(input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("graph TD"));
    assert!(text.contains("A[Start] --> B[Finish]"));
    assert!(!text.contains("▼"));
    assert_eq!(lines[0].spans[0].style, code_block_style());
}

#[test]
fn test_render_markdown_accepts_mermaid_fence_with_tab_separator() {
    // Arrange
    let input = "```mermaid\t{theme=default}\ngraph TD\n    A[Start] --> B[Finish]\n```";

    // Act
    let lines = render_markdown(input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Start"));
    assert!(text.contains("Finish"));
    assert!(text.contains("▼"));
    assert!(!text.contains("graph TD"));
    assert!(!text.contains("```"));
}

#[test]
fn test_render_markdown_mermaid_uses_injected_palette() {
    // Arrange
    let input = "```mermaid\ngraph TD\n    A[Start] --> B[Finish]\n```";
    let settings = TextRenderSettings {
        cache_version: 7,
        palette: crate::TextPalette {
            text: Color::Red,
            ..crate::TextPalette::default()
        },
    };

    // Act
    let lines = render_markdown_with_settings(input, 80, settings);

    // Assert
    let start_span = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.contains("Start"))
        .expect("start label should render");
    assert_eq!(start_span.style.fg, Some(Color::Red));
}

#[test]
fn test_render_markdown_renders_feedback_mermaid_block_as_diagram() {
    // Arrange
    let input = concat!(
        "```mermaid\n",
        "flowchart LR\n",
        "    A[\"App\"] -- \"commands:<br/>prompt · interrupt · permission answer\" --> \
         H[\"ag-harness\"]\n",
        "    H -- \"typed events:<br/>deltas · tool calls · diffs · usage\" --> A\n",
        "```",
    );

    // Act
    let lines = render_markdown(input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("App"));
    assert!(text.contains("ag-harness"));
    assert!(text.contains("commands:"));
    assert!(text.contains("typed events:"));
    assert!(text.contains("◀"));
    assert!(!text.contains("flowchart LR"));
    assert!(!text.contains("<br/>"));
}

#[test]
fn test_render_markdown_renders_multi_node_feedback_mermaid_block_as_diagram() {
    // Arrange
    let input = concat!(
        "```mermaid\n",
        "flowchart LR\n",
        "    U[User and TUI] --> C[Orchestrator controller]\n",
        "    M[Agent model] --> P[Typed command response]\n",
        "    P --> C\n",
        "    C --> S[ag-session service]\n",
        "    S --> A[Agentty host adapter]\n",
        "    A --> W[Session workers]\n",
        "    W --> E[Session events]\n",
        "    E --> C\n",
        "    C --> M\n",
        "```",
    );

    // Act
    let lines = render_markdown(input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Orchestrator controller"));
    assert!(text.contains("Session events"));
    assert!(text.contains("Session events ───▶ Orchestrator controller"));
    assert!(text.contains("Orchestrator controller ───▶ Agent model"));
    assert!(!text.contains("flowchart LR"));
}

#[test]
fn test_render_markdown_keeps_code_fallback_for_unsupported_mermaid() {
    // Arrange
    let input = "```mermaid\nclassDiagram\n    A <|-- B\n```";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines[0].to_string(), "classDiagram");
    assert_eq!(lines[0].spans[0].style, code_block_style());
}

#[test]
fn test_render_markdown_renders_sequence_mermaid_block_as_diagram() {
    // Arrange
    let input = concat!(
        "```mermaid\n",
        "sequenceDiagram\n",
        "    participant User\n",
        "    participant Agentty\n",
        "    User->>Agentty: Start session\n",
        "```\n",
    );

    // Act
    let lines = render_markdown(input, 120);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("User"));
    assert!(text.contains("Agentty"));
    assert!(text.contains("Start session"));
    assert!(!text.contains("sequenceDiagram"));
}

#[test]
fn test_render_markdown_keeps_code_fallback_for_unclosed_mermaid_fence() {
    // Arrange
    let input = "```mermaid\ngraph TD\n    A[Start] --> B[Finish]";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines[0].to_string(), "graph TD");
    assert_eq!(lines[0].spans[0].style, code_block_style());
}

#[test]
fn test_render_markdown_keeps_code_fallback_for_line_limited_mermaid_source() {
    // Arrange
    let mut input = String::from("```mermaid\ngraph TD");
    for node_index in 0..mermaid::MAX_SOURCE_LINE_COUNT {
        write!(&mut input, "\n    N{node_index}").expect("writing to String should succeed");
    }
    input.push_str("\n```");

    // Act
    let lines = render_markdown(&input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("graph TD"));
    assert!(!text.contains("┌"));
}

#[test]
fn test_render_markdown_keeps_code_fallback_for_byte_limited_mermaid_source() {
    // Arrange
    let label = "x".repeat(mermaid::MAX_SOURCE_BYTE_COUNT);
    let input = format!("```mermaid\ngraph TD\n    A[{label}]\n```");

    // Act
    let lines = render_markdown(&input, 80);

    // Assert
    assert_eq!(lines[0].to_string(), "graph TD");
    assert_eq!(lines[0].spans[0].style, code_block_style());
}
