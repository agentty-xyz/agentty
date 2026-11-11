use std::fmt::Write;

use super::support::diagram_text;
use crate::mermaid::{
    Canvas, MAX_EDGE_COUNT, MAX_NODE_COUNT, draw_sequence_message_label,
    is_ignorable_sequence_statement, render_mermaid, render_mermaid_for_width,
};

#[test]
fn test_render_mermaid_rejects_invalid_and_oversized_sequence_diagrams() {
    // Arrange
    let mut too_many_participants = String::from("sequenceDiagram");
    for participant_index in 0..=MAX_NODE_COUNT {
        write!(
            &mut too_many_participants,
            "\n    participant P{participant_index}"
        )
        .expect("writing to String should succeed");
    }
    let mut too_many_messages = String::from("sequenceDiagram");
    for _ in 0..=MAX_EDGE_COUNT {
        too_many_messages.push_str("\n    A->>B: ping");
    }
    let sources = [
        "sequenceDiagram\n    participant A".to_string(),
        "sequenceDiagram\n    A: missing operator".to_string(),
        "sequenceDiagram\n    participant A.B\n    A.B->>C: dotted".to_string(),
        too_many_participants,
        too_many_messages,
    ];

    for source in sources {
        // Act
        let diagram = render_mermaid(&source);

        // Assert
        assert!(diagram.is_none(), "{source:?}");
    }
}

#[test]
fn test_is_ignorable_sequence_statement_skips_blank_statements() {
    // Arrange, Act, Assert
    assert!(is_ignorable_sequence_statement(""));
    assert!(is_ignorable_sequence_statement("Note over A: hi"));
    assert!(!is_ignorable_sequence_statement("A->>B: hi"));
}

#[test]
fn test_draw_sequence_message_label_skips_label_wider_than_run() {
    // Arrange
    let mut canvas = Canvas::new(6, 2);

    // Act
    draw_sequence_message_label(&mut canvas, 0, 3, 1, "long");
    let diagram = canvas.into_diagram();

    // Assert
    assert_eq!(diagram.width, 0);
}

#[test]
fn test_render_mermaid_draws_sequence_diagram() {
    // Arrange
    let source = concat!(
        "sequenceDiagram\n",
        "    participant User\n",
        "    participant Agentty\n",
        "    participant Agent\n",
        "    User->>Agentty: Start new session\n",
        "    Agentty->>Agent: Send prompt\n",
        "    Agent-->>Agentty: Stream result\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("sequence diagram should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("User"));
    assert!(text.contains("Agentty"));
    assert!(text.contains("Start new session"));
    assert!(text.contains('▶'));
    assert!(!text.contains("sequenceDiagram"));
}

#[test]
fn test_render_mermaid_truncates_long_sequence_labels() {
    // Arrange
    let source = concat!(
        "sequenceDiagram\n",
        "    participant A as agentty (client)\n",
        "    participant S as ag-harness (service)\n",
        "    A->>S: connect (WebSocket, JSON-RPC)\n",
        "    S-->>A: events seq 1..40 (deltas, diffs, usage)\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("long labels should truncate, not reject");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("agentty (client)"));
    assert!(text.contains("connect (WebSocket, JSON-RPC)"));
    assert!(text.contains("events seq 1..40 (deltas, diffs…"));
    assert!(!text.contains("diffs, usage)"));
}

#[test]
fn test_render_mermaid_draws_sequence_self_message() {
    // Arrange
    let source = concat!(
        "sequenceDiagram\n",
        "    participant A as agentty (client)\n",
        "    participant S as ag-harness (service)\n",
        "    A->>S: disconnect (app closes)\n",
        "    S->>S: session keeps running, events journaled\n",
        "    S-->>A: replay 41..n, then live events\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("self message should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("session keeps running, events j…"));
    assert!(text.contains('┐'));
    assert!(text.contains('┘'));
    assert!(text.contains('◀'));
}

#[test]
fn test_render_mermaid_skips_sequence_notes_blocks_and_activations() {
    // Arrange
    let source = concat!(
        "sequenceDiagram\n",
        "    autonumber\n",
        "    actor User\n",
        "    User->>+Agentty: Start\n",
        "    activate Agentty\n",
        "    Note over Agentty: thinking\n",
        "    alt success\n",
        "    Agentty-->>-User: Done\n",
        "    else failure\n",
        "    Agentty--xUser: Abort\n",
        "    end\n",
        "    deactivate Agentty\n",
        "    Agentty-)User: Async ping",
    );

    // Act
    let diagram = render_mermaid(source).expect("tolerant sequence should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("User"));
    assert!(text.contains("Agentty"));
    assert!(text.contains("Start"));
    assert!(text.contains("Done"));
    assert!(text.contains("Abort"));
    assert!(text.contains("Async ping"));
    assert!(!text.contains("thinking"));
    assert!(!text.contains("success"));
}

#[test]
fn test_render_mermaid_skips_sequence_critical_option_branches() {
    // Arrange
    let source = concat!(
        "sequenceDiagram\n",
        "    participant Agentty\n",
        "    participant Forge\n",
        "    critical Open review request\n",
        "    Agentty->>Forge: Push branch\n",
        "    option Network timeout\n",
        "    Agentty->>Agentty: Retry push\n",
        "    option Auth rejected\n",
        "    Agentty->>Agentty: Report failure\n",
        "    end\n",
        "    Forge-->>Agentty: Review URL",
    );

    // Act
    let diagram = render_mermaid(source).expect("critical block should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Agentty"));
    assert!(text.contains("Forge"));
    assert!(text.contains("Push branch"));
    assert!(text.contains("Retry push"));
    assert!(text.contains("Report failure"));
    assert!(text.contains("Review URL"));
    assert!(!text.contains("Network timeout"));
    assert!(!text.contains("Auth rejected"));
}

#[test]
fn test_render_mermaid_narrows_sequence_gap_for_short_labels() {
    // Arrange
    let source = concat!(
        "sequenceDiagram\n",
        "    participant User\n",
        "    participant Agentty\n",
        "    participant Git\n",
        "    User->>Agentty: Start\n",
        "    Agentty->>Git: Commit\n",
        "    Git-->>Agentty: Ok\n",
        "    Agentty-->>User: Done",
    );

    // Act
    let diagram = render_mermaid(source).expect("sequence should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(diagram.width <= 50);
    assert!(text.contains("Commit"));
}

#[test]
fn test_sequence_preview_rejects_narrow_width_and_reuses_parse_when_widened() {
    // Arrange
    let source = "sequenceDiagram\nAlice->>Bob: Hello";
    let original = render_mermaid_for_width(source, 80).expect("sequence preview");

    // Act
    let too_narrow = render_mermaid_for_width(source, original.width - 1);
    let widened = render_mermaid_for_width(source, original.width).expect("restored preview");

    // Assert
    assert!(too_narrow.is_none());
    assert_eq!(widened.lines, original.lines);
    assert_eq!(widened.width, original.width);
}
