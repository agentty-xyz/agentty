use std::collections::{HashMap, VecDeque};

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ui::style;

const CONNECT_UP: u8 = 1;
const CONNECT_DOWN: u8 = 2;
const CONNECT_LEFT: u8 = 4;
const CONNECT_RIGHT: u8 = 8;
const LAYER_GAP_COLUMNS: usize = 3;
const LAYER_GAP_ROWS: usize = 1;
const MAX_DUMMY_NODE_COUNT: usize = 32;
const MAX_EDGE_COUNT: usize = 24;
const MAX_LABEL_WIDTH: usize = 32;
const MAX_NODE_COUNT: usize = 16;
pub(crate) const MAX_SOURCE_BYTE_COUNT: usize = 16 * 1024;
pub(crate) const MAX_SOURCE_LINE_COUNT: usize = 128;
const NODE_BOX_HEIGHT: usize = 3;
const SEQUENCE_GAP_COLUMNS: usize = MAX_LABEL_WIDTH + 2;

/// Rendered mermaid diagram rows plus the widest row width in cells.
pub struct MermaidDiagram {
    /// Styled diagram rows ready for terminal painting.
    pub lines: Vec<Line<'static>>,
    /// Display width of the widest diagram row.
    pub width: usize,
}

/// Renders one ```` ```mermaid ```` source block into Unicode box-drawing
/// lines.
///
/// Supports `graph`/`flowchart` headers with `TD`, `TB`, and `LR` directions,
/// node statements, and edge chains, `erDiagram` headers with crow's-foot
/// relationship statements, and simple `sequenceDiagram` participant/message
/// statements.
/// Returns `None` for unsupported diagram types, unsupported cycles, or
/// layouts so callers can keep the plain code-block presentation.
pub fn render_mermaid(source: &str) -> Option<MermaidDiagram> {
    if !is_source_within_bounds(source) {
        return None;
    }

    if let Some(sequence_diagram) = parse_sequence_diagram(source) {
        return Some(draw_sequence_diagram(&sequence_diagram));
    }

    let graph = parse_graph(source)?;
    if let Some(diagram) = draw_left_right_feedback_graph(&graph) {
        return Some(diagram);
    }

    let graph = expand_long_edges(graph)?;
    let layout = layout_layers(&graph)?;

    let diagram = match graph.direction {
        FlowDirection::TopDown => draw_top_down(&graph, &layout),
        FlowDirection::LeftRight => draw_left_right(&graph, &layout),
    };

    Some(diagram)
}

/// Returns whether a source block stays within the preview parsing budget.
fn is_source_within_bounds(source: &str) -> bool {
    source.len() <= MAX_SOURCE_BYTE_COUNT
        && source.split('\n').take(MAX_SOURCE_LINE_COUNT + 1).count() <= MAX_SOURCE_LINE_COUNT
}

/// Flow direction taken from the diagram header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FlowDirection {
    LeftRight,
    TopDown,
}

/// Node outline shape derived from the mermaid label delimiters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NodeShape {
    Rectangle,
    Rounded,
}

/// One declared diagram node.
struct MermaidNode {
    is_hidden: bool,
    label: String,
    shape: NodeShape,
}

/// One directed or undirected link between two nodes.
struct MermaidEdge {
    from_index: usize,
    has_arrow: bool,
    label: Option<String>,
    source_marker: Option<char>,
    target_marker: Option<char>,
    to_index: usize,
}

/// Parsed mermaid diagram ready for layered layout.
struct MermaidGraph {
    direction: FlowDirection,
    edges: Vec<MermaidEdge>,
    nodes: Vec<MermaidNode>,
}

/// One participant in a simple sequence diagram.
struct SequenceParticipant {
    label: String,
}

/// One message row in a simple sequence diagram.
struct SequenceMessage {
    from_index: usize,
    label: String,
    to_index: usize,
}

/// Parsed sequence diagram ready for terminal drawing.
struct SequenceDiagram {
    messages: Vec<SequenceMessage>,
    participants: Vec<SequenceParticipant>,
}

/// Parses a simple `sequenceDiagram` block into participants and messages.
fn parse_sequence_diagram(source: &str) -> Option<SequenceDiagram> {
    let mut lines = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("%%"));

    if lines.next()? != "sequenceDiagram" {
        return None;
    }

    let mut participant_indexes: HashMap<String, usize> = HashMap::new();
    let mut participants = Vec::new();
    let mut messages = Vec::new();

    for line in lines {
        if let Some(participant_text) = line.strip_prefix("participant ") {
            parse_sequence_participant(
                participant_text,
                &mut participant_indexes,
                &mut participants,
            )?;

            continue;
        }

        parse_sequence_message(
            line,
            &mut participant_indexes,
            &mut participants,
            &mut messages,
        )?;
    }

    if participants.is_empty() || messages.is_empty() {
        return None;
    }

    Some(SequenceDiagram {
        messages,
        participants,
    })
}

/// Parses one sequence participant declaration.
fn parse_sequence_participant(
    participant_text: &str,
    participant_indexes: &mut HashMap<String, usize>,
    participants: &mut Vec<SequenceParticipant>,
) -> Option<()> {
    let (identifier, label) = if let Some((identifier, label)) = participant_text.split_once(" as ")
    {
        (identifier.trim(), label.trim().trim_matches('"').trim())
    } else {
        let identifier = participant_text.trim();

        (identifier, identifier)
    };

    sequence_participant_index(identifier, label, participant_indexes, participants)?;

    Some(())
}

/// Parses one sequence message line such as `User->>Agentty: Start`.
fn parse_sequence_message(
    line: &str,
    participant_indexes: &mut HashMap<String, usize>,
    participants: &mut Vec<SequenceParticipant>,
    messages: &mut Vec<SequenceMessage>,
) -> Option<()> {
    if messages.len() >= MAX_EDGE_COUNT {
        return None;
    }

    let (link_text, label_text) = line.split_once(':')?;
    let (from_identifier, to_identifier) = split_sequence_link(link_text)?;
    let label = label_text.trim().trim_matches('"').trim().to_string();
    if !is_renderable_label(&label) {
        return None;
    }

    let from_index = sequence_participant_index(
        from_identifier,
        from_identifier,
        participant_indexes,
        participants,
    )?;
    let to_index = sequence_participant_index(
        to_identifier,
        to_identifier,
        participant_indexes,
        participants,
    )?;

    messages.push(SequenceMessage {
        from_index,
        label,
        to_index,
    });

    Some(())
}

/// Splits the supported sequence message operators into source and target IDs.
fn split_sequence_link(link_text: &str) -> Option<(&str, &str)> {
    for operator in ["-->>", "->>", "-->", "->"] {
        let Some((from_identifier, to_identifier)) = link_text.split_once(operator) else {
            continue;
        };

        return Some((from_identifier.trim(), to_identifier.trim()));
    }

    None
}

/// Returns the dense participant index, inserting a participant on first use.
fn sequence_participant_index(
    identifier: &str,
    label: &str,
    participant_indexes: &mut HashMap<String, usize>,
    participants: &mut Vec<SequenceParticipant>,
) -> Option<usize> {
    if !is_renderable_identifier(identifier) || !is_renderable_label(label) {
        return None;
    }

    if let Some(existing_index) = participant_indexes.get(identifier) {
        return Some(*existing_index);
    }

    if participants.len() >= MAX_NODE_COUNT {
        return None;
    }

    participants.push(SequenceParticipant {
        label: label.to_string(),
    });
    participant_indexes.insert(identifier.to_string(), participants.len() - 1);

    Some(participants.len() - 1)
}

/// Parses mermaid source into a bounded diagram graph.
fn parse_graph(source: &str) -> Option<MermaidGraph> {
    let mut lines = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("%%"))
        .peekable();

    if *lines.peek()? == "erDiagram" {
        lines.next();

        return parse_er_graph(lines);
    }

    parse_flow_graph(lines)
}

/// Parses `erDiagram` statements into a top-down entity-relationship graph.
///
/// Supports entity declarations and crow's-foot relationship statements.
/// Entity attribute blocks are consumed but omitted from the preview, which
/// keeps entity boxes, relationship lines, labels, and cardinality markers.
fn parse_er_graph<'source>(lines: impl Iterator<Item = &'source str>) -> Option<MermaidGraph> {
    let mut node_indexes: HashMap<String, usize> = HashMap::new();
    let mut nodes: Vec<MermaidNode> = Vec::new();
    let mut edges: Vec<MermaidEdge> = Vec::new();
    let mut in_attribute_block = false;

    for line in lines {
        if in_attribute_block {
            in_attribute_block = line != "}";

            continue;
        }

        let (statement, opens_attribute_block) = match line.strip_suffix('{') {
            Some(before_brace) => (before_brace.trim(), true),
            None => (line, false),
        };

        if !statement.contains(char::is_whitespace) {
            er_entity_index(statement, &mut node_indexes, &mut nodes)?;
            in_attribute_block = opens_attribute_block;

            continue;
        }

        if opens_attribute_block {
            return None;
        }

        parse_er_relationship(statement, &mut node_indexes, &mut nodes, &mut edges)?;
    }

    bounded_graph(FlowDirection::TopDown, nodes, edges)
}

/// Parses one `A ||--o{ B : label` crow's-foot relationship statement.
fn parse_er_relationship(
    statement: &str,
    node_indexes: &mut HashMap<String, usize>,
    nodes: &mut Vec<MermaidNode>,
    edges: &mut Vec<MermaidEdge>,
) -> Option<()> {
    let (link_text, label_text) = match statement.split_once(':') {
        Some((link_text, label_text)) => (link_text, Some(label_text)),
        None => (statement, None),
    };

    let mut tokens = link_text.split_whitespace();
    let from_index = er_entity_index(tokens.next()?, node_indexes, nodes)?;
    let operator = tokens.next()?;
    let to_index = er_entity_index(tokens.next()?, node_indexes, nodes)?;
    if tokens.next().is_some() {
        return None;
    }

    let source_marker = er_cardinality_marker(operator.get(..2)?)?;
    let connector = operator.get(2..4)?;
    if connector != "--" && connector != ".." {
        return None;
    }
    let target_marker = er_cardinality_marker(operator.get(4..)?)?;

    let label = label_text.and_then(renderable_edge_label);

    if edges.len() >= MAX_EDGE_COUNT {
        return None;
    }

    edges.push(MermaidEdge {
        from_index,
        has_arrow: false,
        label,
        source_marker: Some(source_marker),
        target_marker: Some(target_marker),
        to_index,
    });

    Some(())
}

/// Returns the dense node index for one ER entity, inserting it on first use.
fn er_entity_index(
    identifier: &str,
    node_indexes: &mut HashMap<String, usize>,
    nodes: &mut Vec<MermaidNode>,
) -> Option<usize> {
    if !is_renderable_identifier(identifier) || !is_renderable_label(identifier) {
        return None;
    }

    if let Some(existing_index) = node_indexes.get(identifier) {
        return Some(*existing_index);
    }

    if nodes.len() >= MAX_NODE_COUNT {
        return None;
    }

    nodes.push(MermaidNode {
        is_hidden: false,
        label: identifier.to_string(),
        shape: NodeShape::Rectangle,
    });
    node_indexes.insert(identifier.to_string(), nodes.len() - 1);

    Some(nodes.len() - 1)
}

/// Maps one crow's-foot cardinality token to its compact marker glyph.
///
/// `1` marks exactly one, `?` zero or one, `*` zero or more, and `+` one or
/// more, matching UML-style multiplicity shorthand.
fn er_cardinality_marker(token: &str) -> Option<char> {
    match token {
        "||" => Some('1'),
        "|o" | "o|" => Some('?'),
        "}o" | "o{" => Some('*'),
        "}|" | "|{" => Some('+'),
        _ => None,
    }
}

/// Parses `graph`/`flowchart` statements into a bounded flowchart graph.
fn parse_flow_graph<'source>(lines: impl Iterator<Item = &'source str>) -> Option<MermaidGraph> {
    let mut direction: Option<FlowDirection> = None;
    let mut node_indexes: HashMap<String, usize> = HashMap::new();
    let mut nodes: Vec<MermaidNode> = Vec::new();
    let mut edges: Vec<MermaidEdge> = Vec::new();

    for line in lines {
        let mut statements = line.split(';');
        if direction.is_none() {
            direction = Some(parse_direction_header(statements.next()?.trim())?);
        }

        for statement in statements {
            let statement = statement.trim();
            if statement.is_empty() {
                continue;
            }

            parse_statement(statement, &mut node_indexes, &mut nodes, &mut edges)?;
        }
    }

    bounded_graph(direction?, nodes, edges)
}

/// Builds the graph when node and edge counts stay within preview bounds and
/// every final node label is renderable.
///
/// Bare identifiers become node labels without upfront validation, so the
/// label bounds are enforced here on the final labels before layout sizes the
/// canvas from their widths.
fn bounded_graph(
    direction: FlowDirection,
    nodes: Vec<MermaidNode>,
    edges: Vec<MermaidEdge>,
) -> Option<MermaidGraph> {
    if nodes.is_empty() || nodes.len() > MAX_NODE_COUNT || edges.len() > MAX_EDGE_COUNT {
        return None;
    }
    if !nodes.iter().all(|node| is_renderable_label(&node.label)) {
        return None;
    }

    Some(MermaidGraph {
        direction,
        edges,
        nodes,
    })
}

/// Splits long graph edges into adjacent-layer segments through hidden nodes.
fn expand_long_edges(mut graph: MermaidGraph) -> Option<MermaidGraph> {
    let node_layers = assign_node_layers(&graph)?;
    let mut edges = Vec::with_capacity(graph.edges.len());
    let mut dummy_node_count = 0;

    for edge in graph.edges {
        let from_layer = node_layers[edge.from_index];
        let to_layer = node_layers[edge.to_index];
        if to_layer <= from_layer {
            return None;
        }

        if to_layer == from_layer + 1 {
            edges.push(edge);

            continue;
        }

        let mut from_index = edge.from_index;
        let mut source_marker = edge.source_marker;
        let mut label = edge.label;
        for _ in from_layer + 1..to_layer {
            if dummy_node_count >= MAX_DUMMY_NODE_COUNT {
                return None;
            }

            let dummy_index = graph.nodes.len();
            graph.nodes.push(MermaidNode {
                is_hidden: true,
                label: String::new(),
                shape: NodeShape::Rectangle,
            });
            dummy_node_count += 1;

            edges.push(MermaidEdge {
                from_index,
                has_arrow: false,
                label: label.take(),
                source_marker: source_marker.take(),
                target_marker: None,
                to_index: dummy_index,
            });
            from_index = dummy_index;
        }

        edges.push(MermaidEdge {
            from_index,
            has_arrow: edge.has_arrow,
            label,
            source_marker,
            target_marker: edge.target_marker,
            to_index: edge.to_index,
        });
    }

    graph.edges = edges;

    Some(graph)
}

/// Parses the `graph`/`flowchart` header line into a supported direction.
fn parse_direction_header(line: &str) -> Option<FlowDirection> {
    let mut tokens = line.split_whitespace();
    let keyword = tokens.next()?;
    if keyword != "graph" && keyword != "flowchart" {
        return None;
    }

    let direction = match tokens.next()? {
        "TD" | "TB" => FlowDirection::TopDown,
        "LR" => FlowDirection::LeftRight,
        _ => return None,
    };
    if tokens.next().is_some() {
        return None;
    }

    Some(direction)
}

/// Parses one statement: a node declaration or an edge chain.
fn parse_statement(
    statement: &str,
    node_indexes: &mut HashMap<String, usize>,
    nodes: &mut Vec<MermaidNode>,
    edges: &mut Vec<MermaidEdge>,
) -> Option<()> {
    let mut cursor = StatementCursor { rest: statement };
    let mut from_index = cursor.parse_node(node_indexes, nodes)?;

    loop {
        cursor.skip_whitespace();
        if cursor.rest.is_empty() {
            return Some(());
        }

        let (has_arrow, label) = cursor.parse_edge_operator()?;
        let to_index = cursor.parse_node(node_indexes, nodes)?;
        if edges.len() >= MAX_EDGE_COUNT {
            return None;
        }

        edges.push(MermaidEdge {
            from_index,
            has_arrow,
            label,
            source_marker: None,
            target_marker: None,
            to_index,
        });
        from_index = to_index;
    }
}

/// Successful outcome of parsing an optional node-shape suffix.
enum NodeShapeParse {
    Bare,
    Labeled(NodeShape, String),
}

/// Incremental parser over one mermaid statement.
struct StatementCursor<'a> {
    rest: &'a str,
}

impl<'source> StatementCursor<'source> {
    /// Parses one node reference and returns its dense node index.
    fn parse_node(
        &mut self,
        node_indexes: &mut HashMap<String, usize>,
        nodes: &mut Vec<MermaidNode>,
    ) -> Option<usize> {
        self.skip_whitespace();
        let identifier_length = self
            .rest
            .chars()
            .take_while(|character| character.is_alphanumeric() || *character == '_')
            .map(char::len_utf8)
            .sum::<usize>();
        if identifier_length == 0 {
            return None;
        }

        let (identifier, remaining) = self.rest.split_at(identifier_length);
        self.rest = remaining;
        let shape_parse = self.parse_node_shape()?;

        let node_index = if let Some(existing_index) = node_indexes.get(identifier) {
            *existing_index
        } else {
            if nodes.len() >= MAX_NODE_COUNT {
                return None;
            }

            nodes.push(MermaidNode {
                is_hidden: false,
                label: identifier.to_string(),
                shape: NodeShape::Rectangle,
            });
            node_indexes.insert(identifier.to_string(), nodes.len() - 1);

            nodes.len() - 1
        };

        if let NodeShapeParse::Labeled(shape, label) = shape_parse {
            if !is_renderable_label(&label) {
                return None;
            }

            nodes[node_index].label = label;
            nodes[node_index].shape = shape;
        }

        Some(node_index)
    }

    /// Parses an optional bracketed node label, returning `None` only for an
    /// unterminated shape delimiter.
    fn parse_node_shape(&mut self) -> Option<NodeShapeParse> {
        let delimiters: [(&str, &str, NodeShape); 4] = [
            ("((", "))", NodeShape::Rounded),
            ("(", ")", NodeShape::Rounded),
            ("[", "]", NodeShape::Rectangle),
            ("{", "}", NodeShape::Rectangle),
        ];

        for (open_delimiter, close_delimiter, shape) in delimiters {
            let Some(after_open) = self.rest.strip_prefix(open_delimiter) else {
                continue;
            };
            let close_index = after_open.find(close_delimiter)?;
            let label = after_open[..close_index]
                .trim()
                .trim_matches('"')
                .trim()
                .to_string();
            self.rest = &after_open[close_index + close_delimiter.len()..];

            return Some(NodeShapeParse::Labeled(shape, label));
        }

        Some(NodeShapeParse::Bare)
    }

    /// Parses one edge operator, returning arrow presence and optional label.
    fn parse_edge_operator(&mut self) -> Option<(bool, Option<String>)> {
        self.skip_whitespace();

        if let Some(after_dashes) = self.rest.strip_prefix("-- ") {
            return self.parse_inline_label_operator(after_dashes);
        }

        let operators: [(&str, bool); 6] = [
            ("--->", true),
            ("-->", true),
            ("-.->", true),
            ("==>", true),
            ("---", false),
            ("--", false),
        ];
        for (operator, has_arrow) in operators {
            let Some(after_operator) = self.rest.strip_prefix(operator) else {
                continue;
            };
            self.rest = after_operator;

            let Some(after_pipe) = self.rest.strip_prefix('|') else {
                return Some((has_arrow, None));
            };
            let close_index = after_pipe.find('|')?;
            let label = renderable_edge_label(&after_pipe[..close_index]);
            self.rest = &after_pipe[close_index + 1..];

            return Some((has_arrow, label));
        }

        None
    }

    /// Parses the `A -- label --> B` inline edge-label form.
    fn parse_inline_label_operator(
        &mut self,
        after_dashes: &'source str,
    ) -> Option<(bool, Option<String>)> {
        let (label_text, has_arrow, after_operator) =
            if let Some(arrow_index) = after_dashes.find("-->") {
                (
                    &after_dashes[..arrow_index],
                    true,
                    &after_dashes[arrow_index + 3..],
                )
            } else {
                let line_index = after_dashes.find("---")?;

                (
                    &after_dashes[..line_index],
                    false,
                    &after_dashes[line_index + 3..],
                )
            };
        self.rest = after_operator;
        let label = renderable_edge_label(label_text);

        Some((has_arrow, label))
    }

    /// Advances the cursor past leading whitespace.
    fn skip_whitespace(&mut self) {
        self.rest = self.rest.trim_start();
    }
}

/// Normalizes an optional edge label to the single-line subset that the
/// terminal preview can paint.
fn renderable_edge_label(label_text: &str) -> Option<String> {
    let label = first_mermaid_label_line(label_text)
        .trim()
        .trim_matches('"')
        .trim();
    if !is_renderable_label(label) {
        return None;
    }

    Some(label.to_string())
}

/// Returns the first line from Mermaid's common HTML line-break label syntax.
fn first_mermaid_label_line(label_text: &str) -> &str {
    for delimiter in ["<br/>", "<br />", "<br>"] {
        if let Some((first_line, _)) = label_text.split_once(delimiter) {
            return first_line;
        }
    }

    label_text
}

/// Returns whether a label stays within bounds and uses single-cell glyphs.
///
/// The canvas assigns one character per cell, so labels with zero-width or
/// double-width characters would break box alignment and force a fallback.
fn is_renderable_label(label: &str) -> bool {
    !label.is_empty()
        && UnicodeWidthStr::width(label) <= MAX_LABEL_WIDTH
        && label
            .chars()
            .all(|character| UnicodeWidthChar::width(character) == Some(1))
}

/// Returns whether an identifier uses the preview's supported token syntax.
fn is_renderable_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_' || character == '-')
}

/// Node-to-layer assignment produced by longest-path layering.
struct GraphLayout {
    layer_members: Vec<Vec<usize>>,
    node_layers: Vec<usize>,
}

/// Draws a compact two-node left-right feedback graph.
///
/// The layered graph renderer intentionally rejects cycles. This preview keeps
/// the common `A --> B` / `B --> A` shape useful in chat by placing both nodes
/// once and rendering the reciprocal arrows underneath them.
fn draw_left_right_feedback_graph(graph: &MermaidGraph) -> Option<MermaidDiagram> {
    if !is_left_right_feedback_graph(graph) {
        return None;
    }

    let node_widths = left_right_node_widths(graph);
    let label_width = graph
        .edges
        .iter()
        .filter_map(|edge| edge.label.as_deref())
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(0);
    let gap_width = label_width.max(LAYER_GAP_COLUMNS * 2 + 4);
    let right_column = node_widths[0] + gap_width;
    let canvas_width = right_column + node_widths[1];
    let canvas_height = NODE_BOX_HEIGHT + graph.edges.len() * 2;
    let left_center = node_widths[0] / 2;
    let right_center = right_column + node_widths[1] / 2;
    let mut canvas = Canvas::new(canvas_width, canvas_height);

    draw_node_box(&mut canvas, &graph.nodes[0], 0, 0, node_widths[0]);
    draw_node_box(
        &mut canvas,
        &graph.nodes[1],
        right_column,
        0,
        node_widths[1],
    );
    for (edge_index, edge) in graph.edges.iter().enumerate() {
        let label_row = NODE_BOX_HEIGHT + edge_index * 2;
        let arrow_row = label_row + 1;
        draw_feedback_edge_label(&mut canvas, edge, left_center, right_center, label_row);
        draw_feedback_edge_arrow(&mut canvas, edge, left_center, right_center, arrow_row);
    }

    Some(canvas.into_diagram())
}

/// Returns whether a graph is the two-node reciprocal shape.
fn is_left_right_feedback_graph(graph: &MermaidGraph) -> bool {
    if !matches!(graph.direction, FlowDirection::LeftRight)
        || graph.nodes.len() != 2
        || graph.edges.len() != 2
    {
        return false;
    }

    graph.edges.iter().all(|edge| {
        (edge.from_index == 0 && edge.to_index == 1) || (edge.from_index == 1 && edge.to_index == 0)
    }) && graph
        .edges
        .iter()
        .any(|edge| edge.from_index == 0 && edge.to_index == 1)
        && graph
            .edges
            .iter()
            .any(|edge| edge.from_index == 1 && edge.to_index == 0)
}

/// Writes one feedback edge label between the node centers when it fits.
fn draw_feedback_edge_label(
    canvas: &mut Canvas,
    edge: &MermaidEdge,
    left_center: usize,
    right_center: usize,
    row: usize,
) {
    let Some(label) = &edge.label else {
        return;
    };

    let label_width = UnicodeWidthStr::width(label.as_str());
    let run_width = right_center.saturating_sub(left_center + 1);
    if label_width > run_width {
        return;
    }

    let start_column = left_center + 1 + (run_width - label_width) / 2;
    canvas.try_write_label(start_column, row, label);
}

/// Draws one horizontal feedback arrow between the node centers.
fn draw_feedback_edge_arrow(
    canvas: &mut Canvas,
    edge: &MermaidEdge,
    left_center: usize,
    right_center: usize,
    row: usize,
) {
    for column in left_center..=right_center {
        canvas.merge_connector(column, row, CONNECT_LEFT | CONNECT_RIGHT);
    }

    if !edge.has_arrow {
        return;
    }

    if edge.to_index == 1 {
        canvas.put_arrow(right_center, row, '▶');
    } else {
        canvas.put_arrow(left_center, row, '◀');
    }
}

/// Draws a simple sequence diagram with participant boxes and message arrows.
fn draw_sequence_diagram(diagram: &SequenceDiagram) -> MermaidDiagram {
    let participant_widths: Vec<usize> = diagram
        .participants
        .iter()
        .map(|participant| UnicodeWidthStr::width(participant.label.as_str()) + 4)
        .collect();
    let mut participant_columns = Vec::with_capacity(diagram.participants.len());
    let mut next_column = 0;
    for participant_width in &participant_widths {
        participant_columns.push(next_column);
        next_column += *participant_width + SEQUENCE_GAP_COLUMNS;
    }
    let canvas_width = next_column.saturating_sub(SEQUENCE_GAP_COLUMNS).max(1);
    let canvas_height = NODE_BOX_HEIGHT + 1 + diagram.messages.len() * 2;
    let mut canvas = Canvas::new(canvas_width, canvas_height);

    let lifeline_columns: Vec<usize> = participant_columns
        .iter()
        .zip(&participant_widths)
        .map(|(left_column, width)| left_column + width / 2)
        .collect();

    for column in &lifeline_columns {
        for row in NODE_BOX_HEIGHT..canvas_height {
            canvas.merge_connector(*column, row, CONNECT_UP | CONNECT_DOWN);
        }
    }

    for (participant_index, participant) in diagram.participants.iter().enumerate() {
        draw_node_box(
            &mut canvas,
            &MermaidNode {
                is_hidden: false,
                label: participant.label.clone(),
                shape: NodeShape::Rectangle,
            },
            participant_columns[participant_index],
            0,
            participant_widths[participant_index],
        );
    }

    for (message_index, message) in diagram.messages.iter().enumerate() {
        let row = NODE_BOX_HEIGHT + 1 + message_index * 2;
        draw_sequence_message(&mut canvas, &lifeline_columns, message, row);
    }

    canvas.into_diagram()
}

/// Draws one sequence message as a horizontal arrow between lifelines.
fn draw_sequence_message(
    canvas: &mut Canvas,
    lifeline_columns: &[usize],
    message: &SequenceMessage,
    row: usize,
) {
    let source_column = lifeline_columns[message.from_index];
    let target_column = lifeline_columns[message.to_index];
    let left_column = source_column.min(target_column);
    let right_column = source_column.max(target_column);

    for column in left_column..=right_column {
        canvas.merge_connector(column, row, CONNECT_LEFT | CONNECT_RIGHT);
    }

    if target_column >= source_column {
        canvas.put_arrow(target_column, row, '▶');
    } else {
        canvas.put_arrow(target_column, row, '◀');
    }

    draw_sequence_message_label(canvas, left_column, right_column, row, &message.label);
}

/// Writes a sequence message label above its arrow run when it fits.
fn draw_sequence_message_label(
    canvas: &mut Canvas,
    left_column: usize,
    right_column: usize,
    arrow_row: usize,
    label: &str,
) {
    let label_width = UnicodeWidthStr::width(label);
    let run_width = right_column.saturating_sub(left_column + 1);
    if label_width > run_width {
        return;
    }

    let start_column = left_column + 1 + (run_width - label_width) / 2;
    let label_row = arrow_row.saturating_sub(1);
    for (character_index, character) in label.chars().enumerate() {
        canvas.put_label(start_column + character_index, label_row, character);
    }
}

/// Assigns nodes to layers via longest-path topological layering.
///
/// Returns `None` for cyclic graphs or for edges that still skip layers after
/// long-edge expansion.
fn layout_layers(graph: &MermaidGraph) -> Option<GraphLayout> {
    let node_layers = assign_node_layers(graph)?;

    for edge in &graph.edges {
        if node_layers[edge.to_index] != node_layers[edge.from_index] + 1 {
            return None;
        }
    }

    let layer_count = node_layers
        .iter()
        .max()
        .map_or(0, |max_layer| max_layer + 1);
    let mut layer_members: Vec<Vec<usize>> = vec![Vec::new(); layer_count];
    for (node_index, layer) in node_layers.iter().enumerate() {
        layer_members[*layer].push(node_index);
    }

    Some(GraphLayout {
        layer_members,
        node_layers,
    })
}

/// Assigns every node to a topological layer using longest-path layering.
fn assign_node_layers(graph: &MermaidGraph) -> Option<Vec<usize>> {
    let node_count = graph.nodes.len();
    let mut indegrees = vec![0_usize; node_count];
    let mut outgoing: Vec<Vec<usize>> = vec![Vec::new(); node_count];

    for edge in &graph.edges {
        if edge.from_index == edge.to_index {
            return None;
        }

        indegrees[edge.to_index] += 1;
        outgoing[edge.from_index].push(edge.to_index);
    }

    let mut node_layers = vec![0_usize; node_count];
    let mut ready: VecDeque<usize> = (0..node_count)
        .filter(|node_index| indegrees[*node_index] == 0)
        .collect();
    let mut processed_count = 0;

    while let Some(node_index) = ready.pop_front() {
        processed_count += 1;
        for target_index in &outgoing[node_index] {
            node_layers[*target_index] =
                node_layers[*target_index].max(node_layers[node_index] + 1);
            indegrees[*target_index] -= 1;
            if indegrees[*target_index] == 0 {
                ready.push_back(*target_index);
            }
        }
    }

    if processed_count != node_count {
        return None;
    }

    Some(node_layers)
}

/// Routed geometry for one top-down edge.
struct TopDownEdgePath {
    arrow_row: usize,
    has_arrow: bool,
    label: Option<String>,
    region_top: usize,
    source_column: usize,
    source_marker: Option<char>,
    target_column: usize,
    target_marker: Option<char>,
    track_row: usize,
}

/// Draws a top-down diagram: layers as rows, edges as vertical elbows.
fn draw_top_down(graph: &MermaidGraph, layout: &GraphLayout) -> MermaidDiagram {
    let box_widths = top_down_node_widths(graph);
    let layer_widths = top_down_layer_widths(layout, &box_widths);
    let canvas_width = layer_widths.iter().copied().max().unwrap_or(1);

    let region_edges = graph_region_edges(graph, layout);
    let (layer_top_rows, canvas_height) = top_down_layer_top_rows(layout, &region_edges);
    let mut canvas = Canvas::new(canvas_width, canvas_height);
    let box_columns = draw_top_down_nodes(
        &mut canvas,
        graph,
        layout,
        &box_widths,
        &layer_widths,
        &layer_top_rows,
        canvas_width,
    );
    let edge_paths = top_down_edge_paths(
        graph,
        &region_edges,
        &layer_top_rows,
        &box_columns,
        &box_widths,
    );

    draw_top_down_edge_paths(&mut canvas, &edge_paths);

    canvas.into_diagram()
}

/// Returns the rendered width of each top-down graph node.
fn top_down_node_widths(graph: &MermaidGraph) -> Vec<usize> {
    graph.nodes.iter().map(top_down_node_width).collect()
}

/// Returns the width reserved for one top-down graph node.
fn top_down_node_width(node: &MermaidNode) -> usize {
    if node.is_hidden {
        return 1;
    }

    UnicodeWidthStr::width(node.label.as_str()) + 4
}

/// Returns the rendered width of each top-down graph layer.
fn top_down_layer_widths(layout: &GraphLayout, box_widths: &[usize]) -> Vec<usize> {
    layout
        .layer_members
        .iter()
        .map(|members| {
            members
                .iter()
                .map(|node_index| box_widths[*node_index])
                .sum::<usize>()
                + LAYER_GAP_COLUMNS * members.len().saturating_sub(1)
        })
        .collect()
}

/// Groups graph edges by the routing region after their source layer.
fn graph_region_edges<'graph>(
    graph: &'graph MermaidGraph,
    layout: &GraphLayout,
) -> Vec<Vec<&'graph MermaidEdge>> {
    let mut region_edges = vec![Vec::new(); layout.layer_members.len().saturating_sub(1)];
    for edge in &graph.edges {
        region_edges[layout.node_layers[edge.from_index]].push(edge);
    }

    region_edges
}

/// Returns top row coordinates for top-down layers and the canvas height.
fn top_down_layer_top_rows(
    layout: &GraphLayout,
    region_edges: &[Vec<&MermaidEdge>],
) -> (Vec<usize>, usize) {
    let layer_count = layout.layer_members.len();
    let mut layer_top_rows = Vec::with_capacity(layer_count);
    let mut next_row = 0;
    for region_edges_after_layer in region_edges.iter().take(layer_count.saturating_sub(1)) {
        layer_top_rows.push(next_row);
        next_row += NODE_BOX_HEIGHT;
        next_row += region_edges_after_layer.len() + 2;
    }
    if layer_count > 0 {
        layer_top_rows.push(next_row);
        next_row += NODE_BOX_HEIGHT;
    }

    (layer_top_rows, next_row)
}

/// Draws top-down nodes and returns the left column of each node box.
fn draw_top_down_nodes(
    canvas: &mut Canvas,
    graph: &MermaidGraph,
    layout: &GraphLayout,
    box_widths: &[usize],
    layer_widths: &[usize],
    layer_top_rows: &[usize],
    canvas_width: usize,
) -> Vec<usize> {
    let mut box_columns = vec![0_usize; graph.nodes.len()];
    for (layer_index, members) in layout.layer_members.iter().enumerate() {
        let mut cursor_column = (canvas_width - layer_widths[layer_index]) / 2;
        for node_index in members {
            box_columns[*node_index] = cursor_column;
            if graph.nodes[*node_index].is_hidden {
                draw_top_down_hidden_node(canvas, cursor_column, layer_top_rows[layer_index]);
            } else {
                draw_node_box(
                    canvas,
                    &graph.nodes[*node_index],
                    cursor_column,
                    layer_top_rows[layer_index],
                    box_widths[*node_index],
                );
            }
            cursor_column += box_widths[*node_index] + LAYER_GAP_COLUMNS;
        }
    }

    box_columns
}

/// Builds routed top-down edge geometry for every graph edge.
fn top_down_edge_paths(
    graph: &MermaidGraph,
    region_edges: &[Vec<&MermaidEdge>],
    layer_top_rows: &[usize],
    box_columns: &[usize],
    box_widths: &[usize],
) -> Vec<TopDownEdgePath> {
    let mut edge_paths = Vec::with_capacity(graph.edges.len());
    for (layer_index, edges) in region_edges.iter().enumerate() {
        let region_top = layer_top_rows[layer_index] + NODE_BOX_HEIGHT;
        for (track_index, edge) in edges.iter().enumerate() {
            edge_paths.push(TopDownEdgePath {
                arrow_row: region_top + edges.len() + 1,
                has_arrow: edge.has_arrow,
                label: edge.label.clone(),
                region_top,
                source_column: box_columns[edge.from_index] + box_widths[edge.from_index] / 2,
                source_marker: edge.source_marker,
                target_column: box_columns[edge.to_index] + box_widths[edge.to_index] / 2,
                target_marker: edge.target_marker,
                track_row: region_top + 1 + track_index,
            });
        }
    }

    edge_paths
}

/// Draws routed top-down edge connectors, labels, and markers.
fn draw_top_down_edge_paths(canvas: &mut Canvas, edge_paths: &[TopDownEdgePath]) {
    for edge_path in edge_paths {
        draw_top_down_edge_connectors(canvas, edge_path);
    }
    for edge_path in edge_paths {
        draw_top_down_edge_label(canvas, edge_path);
    }
    for edge_path in edge_paths {
        canvas.try_put_marker(
            edge_path.source_column,
            edge_path.region_top,
            edge_path.source_marker,
        );
        canvas.try_put_marker(
            edge_path.target_column,
            edge_path.arrow_row,
            edge_path.target_marker,
        );
    }
}

/// Draws the through-connector for a hidden top-down routing node.
fn draw_top_down_hidden_node(canvas: &mut Canvas, column: usize, top_row: usize) {
    for row in top_row..top_row + NODE_BOX_HEIGHT {
        canvas.merge_connector(column, row, CONNECT_UP | CONNECT_DOWN);
    }
}

/// Draws the connector cells for one top-down edge.
fn draw_top_down_edge_connectors(canvas: &mut Canvas, edge_path: &TopDownEdgePath) {
    for row in edge_path.region_top..edge_path.track_row {
        canvas.merge_connector(edge_path.source_column, row, CONNECT_UP | CONNECT_DOWN);
    }

    if edge_path.source_column == edge_path.target_column {
        for row in edge_path.track_row..edge_path.arrow_row {
            canvas.merge_connector(edge_path.source_column, row, CONNECT_UP | CONNECT_DOWN);
        }
    } else {
        let left_column = edge_path.source_column.min(edge_path.target_column);
        let right_column = edge_path.source_column.max(edge_path.target_column);
        let (source_mask, target_mask) = if edge_path.target_column > edge_path.source_column {
            (CONNECT_UP | CONNECT_RIGHT, CONNECT_DOWN | CONNECT_LEFT)
        } else {
            (CONNECT_UP | CONNECT_LEFT, CONNECT_DOWN | CONNECT_RIGHT)
        };

        canvas.merge_connector(edge_path.source_column, edge_path.track_row, source_mask);
        canvas.merge_connector(edge_path.target_column, edge_path.track_row, target_mask);
        for column in left_column + 1..right_column {
            canvas.merge_connector(column, edge_path.track_row, CONNECT_LEFT | CONNECT_RIGHT);
        }
        for row in edge_path.track_row + 1..edge_path.arrow_row {
            canvas.merge_connector(edge_path.target_column, row, CONNECT_UP | CONNECT_DOWN);
        }
    }

    if edge_path.has_arrow {
        canvas.put_arrow(edge_path.target_column, edge_path.arrow_row, '▼');
    } else {
        canvas.merge_connector(
            edge_path.target_column,
            edge_path.arrow_row,
            CONNECT_UP | CONNECT_DOWN,
        );
    }
}

/// Writes one top-down edge label onto its horizontal track when it fits.
fn draw_top_down_edge_label(canvas: &mut Canvas, edge_path: &TopDownEdgePath) {
    let Some(label) = &edge_path.label else {
        return;
    };
    let label_width = UnicodeWidthStr::width(label.as_str());

    if edge_path.source_column == edge_path.target_column {
        canvas.try_write_label(edge_path.source_column + 2, edge_path.track_row, label);

        return;
    }

    let left_column = edge_path.source_column.min(edge_path.target_column);
    let right_column = edge_path.source_column.max(edge_path.target_column);
    let run_width = right_column.saturating_sub(left_column + 1);
    if label_width > run_width {
        return;
    }

    let start_column = left_column + 1 + (run_width - label_width) / 2;
    canvas.try_write_label(start_column, edge_path.track_row, label);
}

/// Routed geometry for one left-right edge.
struct LeftRightEdgePath {
    arrow_column: usize,
    has_arrow: bool,
    label: Option<String>,
    region_left: usize,
    source_marker: Option<char>,
    source_row: usize,
    target_marker: Option<char>,
    target_row: usize,
    track_column: usize,
}

/// Draws a left-right diagram: layers as columns, edges as horizontal elbows.
fn draw_left_right(graph: &MermaidGraph, layout: &GraphLayout) -> MermaidDiagram {
    let node_widths = left_right_node_widths(graph);
    let layer_widths = left_right_layer_widths(layout, &node_widths);
    let layer_heights = left_right_layer_heights(layout);
    let canvas_height = layer_heights.iter().copied().max().unwrap_or(1);

    let region_edges = graph_region_edges(graph, layout);
    let (layer_left_columns, canvas_width) =
        left_right_layer_left_columns(layout, &region_edges, &layer_widths);
    let mut canvas = Canvas::new(canvas_width, canvas_height);
    let box_rows = draw_left_right_nodes(
        &mut canvas,
        graph,
        layout,
        &layer_widths,
        &layer_heights,
        &layer_left_columns,
        canvas_height,
    );
    let edge_paths = left_right_edge_paths(
        graph,
        &region_edges,
        &layer_left_columns,
        &layer_widths,
        &box_rows,
    );

    draw_left_right_edge_paths(&mut canvas, &edge_paths);

    canvas.into_diagram()
}

/// Returns the rendered width of each left-right graph node.
fn left_right_node_widths(graph: &MermaidGraph) -> Vec<usize> {
    graph.nodes.iter().map(left_right_node_width).collect()
}

/// Returns the width reserved for one left-right graph node.
fn left_right_node_width(node: &MermaidNode) -> usize {
    if node.is_hidden {
        return 1;
    }

    UnicodeWidthStr::width(node.label.as_str()) + 4
}

/// Returns the rendered width of each left-right graph layer.
fn left_right_layer_widths(layout: &GraphLayout, node_widths: &[usize]) -> Vec<usize> {
    layout
        .layer_members
        .iter()
        .map(|members| {
            members
                .iter()
                .map(|node_index| node_widths[*node_index])
                .max()
                .unwrap_or(1)
        })
        .collect()
}

/// Returns the rendered height of each left-right graph layer.
fn left_right_layer_heights(layout: &GraphLayout) -> Vec<usize> {
    layout
        .layer_members
        .iter()
        .map(|members| {
            members.len() * NODE_BOX_HEIGHT + LAYER_GAP_ROWS * members.len().saturating_sub(1)
        })
        .collect()
}

/// Returns left column coordinates for left-right layers and the canvas width.
fn left_right_layer_left_columns(
    layout: &GraphLayout,
    region_edges: &[Vec<&MermaidEdge>],
    layer_widths: &[usize],
) -> (Vec<usize>, usize) {
    let layer_count = layout.layer_members.len();
    let mut layer_left_columns = Vec::with_capacity(layer_count);
    let mut next_column = 0;
    for layer_index in 0..layer_count {
        layer_left_columns.push(next_column);
        next_column += layer_widths[layer_index];
        if layer_index + 1 < layer_count {
            next_column += region_edges[layer_index].len() + 2;
        }
    }

    (layer_left_columns, next_column)
}

/// Draws left-right nodes and returns the top row of each node box.
fn draw_left_right_nodes(
    canvas: &mut Canvas,
    graph: &MermaidGraph,
    layout: &GraphLayout,
    layer_widths: &[usize],
    layer_heights: &[usize],
    layer_left_columns: &[usize],
    canvas_height: usize,
) -> Vec<usize> {
    let mut box_rows = vec![0_usize; graph.nodes.len()];
    for (layer_index, members) in layout.layer_members.iter().enumerate() {
        let mut cursor_row = (canvas_height - layer_heights[layer_index]) / 2;
        for node_index in members {
            box_rows[*node_index] = cursor_row;
            if graph.nodes[*node_index].is_hidden {
                draw_left_right_hidden_node(
                    canvas,
                    layer_left_columns[layer_index],
                    cursor_row + 1,
                    layer_widths[layer_index],
                );
            } else {
                draw_node_box(
                    canvas,
                    &graph.nodes[*node_index],
                    layer_left_columns[layer_index],
                    cursor_row,
                    layer_widths[layer_index],
                );
            }
            cursor_row += NODE_BOX_HEIGHT + LAYER_GAP_ROWS;
        }
    }

    box_rows
}

/// Builds routed left-right edge geometry for every graph edge.
fn left_right_edge_paths(
    graph: &MermaidGraph,
    region_edges: &[Vec<&MermaidEdge>],
    layer_left_columns: &[usize],
    layer_widths: &[usize],
    box_rows: &[usize],
) -> Vec<LeftRightEdgePath> {
    let mut edge_paths = Vec::with_capacity(graph.edges.len());
    for (layer_index, edges) in region_edges.iter().enumerate() {
        let region_left = layer_left_columns[layer_index] + layer_widths[layer_index];
        for (track_index, edge) in edges.iter().enumerate() {
            edge_paths.push(LeftRightEdgePath {
                arrow_column: region_left + edges.len() + 1,
                has_arrow: edge.has_arrow,
                label: edge.label.clone(),
                region_left,
                source_marker: edge.source_marker,
                source_row: box_rows[edge.from_index] + 1,
                target_marker: edge.target_marker,
                target_row: box_rows[edge.to_index] + 1,
                track_column: region_left + 1 + track_index,
            });
        }
    }

    edge_paths
}

/// Draws routed left-right edge connectors, labels, and markers.
fn draw_left_right_edge_paths(canvas: &mut Canvas, edge_paths: &[LeftRightEdgePath]) {
    for edge_path in edge_paths {
        draw_left_right_edge_connectors(canvas, edge_path);
    }
    for edge_path in edge_paths {
        draw_left_right_edge_label(canvas, edge_path);
    }
    for edge_path in edge_paths {
        canvas.try_put_marker(
            edge_path.region_left,
            edge_path.source_row,
            edge_path.source_marker,
        );
        canvas.try_put_marker(
            edge_path.arrow_column,
            edge_path.target_row,
            edge_path.target_marker,
        );
    }
}

/// Draws the through-connector for a hidden left-right routing node.
fn draw_left_right_hidden_node(canvas: &mut Canvas, left_column: usize, row: usize, width: usize) {
    for column in left_column..left_column + width {
        canvas.merge_connector(column, row, CONNECT_LEFT | CONNECT_RIGHT);
    }
}

/// Draws the connector cells for one left-right edge.
fn draw_left_right_edge_connectors(canvas: &mut Canvas, edge_path: &LeftRightEdgePath) {
    for column in edge_path.region_left..edge_path.track_column {
        canvas.merge_connector(column, edge_path.source_row, CONNECT_LEFT | CONNECT_RIGHT);
    }

    if edge_path.source_row == edge_path.target_row {
        for column in edge_path.track_column..edge_path.arrow_column {
            canvas.merge_connector(column, edge_path.source_row, CONNECT_LEFT | CONNECT_RIGHT);
        }
    } else {
        let top_row = edge_path.source_row.min(edge_path.target_row);
        let bottom_row = edge_path.source_row.max(edge_path.target_row);
        let (source_mask, target_mask) = if edge_path.target_row > edge_path.source_row {
            (CONNECT_LEFT | CONNECT_DOWN, CONNECT_UP | CONNECT_RIGHT)
        } else {
            (CONNECT_LEFT | CONNECT_UP, CONNECT_DOWN | CONNECT_RIGHT)
        };

        canvas.merge_connector(edge_path.track_column, edge_path.source_row, source_mask);
        canvas.merge_connector(edge_path.track_column, edge_path.target_row, target_mask);
        for row in top_row + 1..bottom_row {
            canvas.merge_connector(edge_path.track_column, row, CONNECT_UP | CONNECT_DOWN);
        }
        for column in edge_path.track_column + 1..edge_path.arrow_column {
            canvas.merge_connector(column, edge_path.target_row, CONNECT_LEFT | CONNECT_RIGHT);
        }
    }

    if edge_path.has_arrow {
        canvas.put_arrow(edge_path.arrow_column, edge_path.target_row, '▶');
    } else {
        canvas.merge_connector(
            edge_path.arrow_column,
            edge_path.target_row,
            CONNECT_LEFT | CONNECT_RIGHT,
        );
    }
}

/// Writes one left-right edge label onto its entry run when it fits.
fn draw_left_right_edge_label(canvas: &mut Canvas, edge_path: &LeftRightEdgePath) {
    let Some(label) = &edge_path.label else {
        return;
    };
    let label_width = UnicodeWidthStr::width(label.as_str());
    let run_end = if edge_path.source_row == edge_path.target_row {
        edge_path.arrow_column
    } else {
        edge_path.track_column
    };
    let run_width = run_end.saturating_sub(edge_path.region_left);
    if label_width > run_width {
        return;
    }

    let start_column = edge_path.region_left + (run_width - label_width) / 2;
    canvas.try_write_label(start_column, edge_path.source_row, label);
}

/// Draws one node as a bordered box with a centered single-line label.
fn draw_node_box(
    canvas: &mut Canvas,
    node: &MermaidNode,
    left_column: usize,
    top_row: usize,
    box_width: usize,
) {
    let (top_left, top_right, bottom_left, bottom_right) = match node.shape {
        NodeShape::Rectangle => ('┌', '┐', '└', '┘'),
        NodeShape::Rounded => ('╭', '╮', '╰', '╯'),
    };
    let right_column = left_column + box_width - 1;
    let middle_row = top_row + 1;
    let bottom_row = top_row + 2;

    canvas.put_border(left_column, top_row, top_left);
    canvas.put_border(right_column, top_row, top_right);
    canvas.put_border(left_column, bottom_row, bottom_left);
    canvas.put_border(right_column, bottom_row, bottom_right);
    for column in left_column + 1..right_column {
        canvas.put_border(column, top_row, '─');
        canvas.put_border(column, bottom_row, '─');
    }
    canvas.put_border(left_column, middle_row, '│');
    canvas.put_border(right_column, middle_row, '│');

    let label_width = UnicodeWidthStr::width(node.label.as_str());
    let inner_width = box_width - 2;
    let label_column = left_column + 1 + (inner_width - label_width) / 2;
    for (character_index, character) in node.label.chars().enumerate() {
        canvas.put_label(label_column + character_index, middle_row, character);
    }
}

/// One drawable cell in the diagram canvas.
#[derive(Clone, Copy, PartialEq)]
enum CanvasCell {
    Arrow(char),
    Border(char),
    Connector(u8),
    Empty,
    Label(char),
}

/// Fixed-size character canvas that merges box-drawing connectors.
struct Canvas {
    cells: Vec<Vec<CanvasCell>>,
}

impl Canvas {
    /// Creates an empty canvas of the given cell dimensions.
    fn new(width: usize, height: usize) -> Self {
        Self {
            cells: vec![vec![CanvasCell::Empty; width]; height],
        }
    }

    /// Places one box-border glyph, overwriting any existing cell.
    fn put_border(&mut self, column: usize, row: usize, character: char) {
        self.put(column, row, CanvasCell::Border(character));
    }

    /// Places one label glyph, overwriting any existing cell.
    fn put_label(&mut self, column: usize, row: usize, character: char) {
        self.put(column, row, CanvasCell::Label(character));
    }

    /// Places one arrow-head glyph, overwriting any existing cell.
    fn put_arrow(&mut self, column: usize, row: usize, character: char) {
        self.put(column, row, CanvasCell::Arrow(character));
    }

    /// ORs one connector mask into a cell, leaving glyph cells untouched.
    fn merge_connector(&mut self, column: usize, row: usize, mask: u8) {
        let Some(cell) = self.cell_mut(column, row) else {
            return;
        };

        match cell {
            CanvasCell::Empty => *cell = CanvasCell::Connector(mask),
            CanvasCell::Connector(existing_mask) => *existing_mask |= mask,
            CanvasCell::Arrow(_) | CanvasCell::Border(_) | CanvasCell::Label(_) => {}
        }
    }

    /// Writes label text when every target cell is empty or a plain
    /// horizontal connector, keeping other edges' verticals intact.
    fn try_write_label(&mut self, start_column: usize, row: usize, label: &str) {
        let character_count = label.chars().count();
        let Some(row_cells) = self.cells.get(row) else {
            return;
        };
        if start_column + character_count > row_cells.len() {
            return;
        }

        let is_writable = row_cells[start_column..start_column + character_count].iter().all(|cell| {
            matches!(cell, CanvasCell::Empty)
                || matches!(cell, CanvasCell::Connector(mask) if *mask == CONNECT_LEFT | CONNECT_RIGHT)
        });
        if !is_writable {
            return;
        }

        for (character_index, character) in label.chars().enumerate() {
            self.put_label(start_column + character_index, row, character);
        }
    }

    /// Replaces one straight connector cell with a cardinality marker glyph.
    ///
    /// Junction cells and cells already holding glyphs are left untouched so
    /// shared edge tracks keep their box-drawing shape.
    fn try_put_marker(&mut self, column: usize, row: usize, marker: Option<char>) {
        let Some(marker) = marker else {
            return;
        };
        let Some(cell) = self.cell_mut(column, row) else {
            return;
        };

        let is_straight_connector = matches!(
            cell,
            CanvasCell::Connector(mask)
                if *mask == CONNECT_UP | CONNECT_DOWN || *mask == CONNECT_LEFT | CONNECT_RIGHT
        );
        if is_straight_connector {
            *cell = CanvasCell::Label(marker);
        }
    }

    /// Converts the canvas into styled lines plus the widest row width.
    fn into_diagram(self) -> MermaidDiagram {
        let mut lines = Vec::with_capacity(self.cells.len());
        let mut width = 0;

        for row_cells in &self.cells {
            let trimmed_length = row_cells
                .iter()
                .rposition(|cell| *cell != CanvasCell::Empty)
                .map_or(0, |last_index| last_index + 1);
            width = width.max(trimmed_length);

            let mut spans: Vec<Span<'static>> = Vec::new();
            for cell in &row_cells[..trimmed_length] {
                let (character, cell_style) = Self::cell_presentation(*cell);
                match spans.last_mut() {
                    Some(last_span) if last_span.style == cell_style => {
                        last_span.content.to_mut().push(character);
                    }
                    _ => spans.push(Span::styled(character.to_string(), cell_style)),
                }
            }

            lines.push(Line::from(spans));
        }

        MermaidDiagram { lines, width }
    }

    /// Returns the character and style used to paint one cell.
    fn cell_presentation(cell: CanvasCell) -> (char, Style) {
        match cell {
            CanvasCell::Empty => (' ', Style::default()),
            CanvasCell::Connector(mask) => (connector_character(mask), structure_style()),
            CanvasCell::Arrow(character) | CanvasCell::Border(character) => {
                (character, structure_style())
            }
            CanvasCell::Label(character) => (character, label_style()),
        }
    }

    /// Returns a mutable cell reference when the coordinates are in bounds.
    fn cell_mut(&mut self, column: usize, row: usize) -> Option<&mut CanvasCell> {
        self.cells.get_mut(row)?.get_mut(column)
    }

    /// Places one glyph cell, overwriting any existing content.
    fn put(&mut self, column: usize, row: usize, cell: CanvasCell) {
        if let Some(existing_cell) = self.cell_mut(column, row) {
            *existing_cell = cell;
        }
    }
}

/// Maps one connector direction mask to its box-drawing character.
fn connector_character(mask: u8) -> char {
    const UP: u8 = CONNECT_UP;
    const DOWN: u8 = CONNECT_DOWN;
    const LEFT: u8 = CONNECT_LEFT;
    const RIGHT: u8 = CONNECT_RIGHT;

    match mask {
        mask if mask == UP | DOWN | LEFT | RIGHT => '┼',
        mask if mask == UP | DOWN | LEFT => '┤',
        mask if mask == UP | DOWN | RIGHT => '├',
        mask if mask == UP | LEFT | RIGHT => '┴',
        mask if mask == DOWN | LEFT | RIGHT => '┬',
        mask if mask == UP | LEFT => '┘',
        mask if mask == UP | RIGHT => '└',
        mask if mask == DOWN | LEFT => '┐',
        mask if mask == DOWN | RIGHT => '┌',
        mask if mask & (LEFT | RIGHT) != 0 && mask & (UP | DOWN) == 0 => '─',
        _ => '│',
    }
}

/// Returns the style for diagram borders, connectors, and arrow heads.
fn structure_style() -> Style {
    Style::default().fg(style::palette::text_subtle())
}

/// Returns the style for node and edge label text.
fn label_style() -> Style {
    Style::default().fg(style::palette::text())
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use super::*;

    fn diagram_text(diagram: &MermaidDiagram) -> String {
        diagram
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn test_render_mermaid_draws_top_down_chain() {
        // Arrange
        let source = "graph TD\n    A[Start] --> B[Finish]";

        // Act
        let diagram = render_mermaid(source).expect("chain should render");
        let text = diagram_text(&diagram);

        // Assert
        assert!(text.contains("Start"));
        assert!(text.contains("Finish"));
        assert!(text.contains('┌'));
        assert!(text.contains('▼'));
        assert!(diagram.width > 0);
    }

    #[test]
    fn test_render_mermaid_draws_branching_diamond() {
        // Arrange
        let source = "graph TD\n    A --> B\n    A --> C\n    B --> D\n    C --> D";

        // Act
        let diagram = render_mermaid(source).expect("diamond should render");
        let text = diagram_text(&diagram);

        // Assert
        assert!(text.contains('B'));
        assert!(text.contains('C'));
        assert_eq!(text.matches('▼').count(), 3);
    }

    #[test]
    fn test_render_mermaid_draws_top_down_long_edge() {
        // Arrange
        let source = concat!(
            "flowchart TD\n",
            "    A[User starts session] --> B{Choose action}\n",
            "    B -->|Ask agent| C[Send prompt]\n",
            "    B -->|Review changes| D[Open diff view]\n",
            "    C --> E[Agent works in worktree]\n",
            "    E --> F[Run checks]\n",
            "    F --> G[Report result]\n",
            "    D --> G\n",
        );

        // Act
        let diagram = render_mermaid(source).expect("long-edge flowchart should render");
        let text = diagram_text(&diagram);

        // Assert
        assert!(text.contains("User starts session"));
        assert!(text.contains("Open diff view"));
        assert!(text.contains("Report result"));
        assert!(text.contains('▼'));
    }

    #[test]
    fn test_render_mermaid_draws_left_right_direction() {
        // Arrange
        let source = "flowchart LR\n    A[In] --> B[Out]";

        // Act
        let diagram = render_mermaid(source).expect("LR chain should render");
        let text = diagram_text(&diagram);

        // Assert
        assert!(text.contains('▶'));
        let first_box_line = text
            .lines()
            .find(|line| line.contains("In"))
            .expect("label row");
        assert!(first_box_line.contains("Out"));
    }

    #[test]
    fn test_render_mermaid_draws_left_right_feedback_cycle() {
        // Arrange
        let source = concat!(
            "flowchart LR\n",
            "    A[\"App\"] -- \"commands:<br/>prompt · interrupt · permission answer\" --> \
             H[\"ag-harness\"]\n",
            "    H -- \"typed events:<br/>deltas · tool calls · diffs · usage\" --> A\n",
        );

        // Act
        let diagram = render_mermaid(source).expect("two-node feedback graph should render");
        let text = diagram_text(&diagram);

        // Assert
        assert!(text.contains("App"));
        assert!(text.contains("ag-harness"));
        assert!(text.contains("commands:"));
        assert!(text.contains("typed events:"));
        assert!(text.contains('▶'));
        assert!(text.contains('◀'));
        assert!(!text.contains("flowchart LR"));
        assert!(!text.contains("<br/>"));
    }

    #[test]
    fn test_render_mermaid_writes_edge_label_on_track() {
        // Arrange
        let source = "graph TD\n    A --> B\n    A -->|yes| C";

        // Act
        let diagram = render_mermaid(source).expect("labeled edge should render");
        let text = diagram_text(&diagram);

        // Assert
        assert!(text.contains("yes"));
    }

    #[test]
    fn test_render_mermaid_supports_rounded_and_chained_statements() {
        // Arrange
        let source = "graph TD; A(Begin) --> B{Choice}; B --> C((End))";

        // Act
        let diagram = render_mermaid(source).expect("chained statements should render");
        let text = diagram_text(&diagram);

        // Assert
        assert!(text.contains('╭'));
        assert!(text.contains("Begin"));
        assert!(text.contains("Choice"));
        assert!(text.contains("End"));
    }

    #[test]
    fn test_render_mermaid_draws_er_diagram_with_cardinality_markers() {
        // Arrange
        let source = concat!(
            "erDiagram\n",
            "    CUSTOMER ||--o{ ORDER : places\n",
            "    CUSTOMER ||--|| ACCOUNT : owns\n",
        );

        // Act
        let diagram = render_mermaid(source).expect("er diagram should render");
        let text = diagram_text(&diagram);

        // Assert
        assert!(text.contains("CUSTOMER"));
        assert!(text.contains("ORDER"));
        assert!(text.contains("ACCOUNT"));
        assert!(text.contains("places"));
        assert!(text.contains('1'));
        assert!(text.contains('*'));
        assert!(!text.contains('▼'));
    }

    #[test]
    fn test_render_mermaid_er_omits_attribute_blocks() {
        // Arrange
        let source = concat!(
            "erDiagram\n",
            "    CUSTOMER {\n",
            "        string name\n",
            "    }\n",
            "    CUSTOMER ||--o{ ORDER : places\n",
        );

        // Act
        let diagram = render_mermaid(source).expect("er diagram should render");
        let text = diagram_text(&diagram);

        // Assert
        assert!(text.contains("CUSTOMER"));
        assert!(text.contains("ORDER"));
        assert!(!text.contains("string"));
    }

    #[test]
    fn test_render_mermaid_er_supports_hyphenated_entities_and_bare_links() {
        // Arrange
        let source = concat!(
            "erDiagram\n",
            "    ORDER ||--|{ LINE-ITEM : contains\n",
            "    LINE-ITEM }o..o| DISCOUNT\n",
        );

        // Act
        let diagram = render_mermaid(source).expect("er diagram should render");
        let text = diagram_text(&diagram);

        // Assert
        assert!(text.contains("LINE-ITEM"));
        assert!(text.contains("DISCOUNT"));
        assert!(text.contains('+'));
        assert!(text.contains('?'));
    }

    #[test]
    fn test_render_mermaid_er_rejects_unknown_relationship_operators() {
        // Arrange & Act & Assert
        assert!(render_mermaid("erDiagram\n    A |x--o{ B : bad").is_none());
        assert!(render_mermaid("erDiagram\n    A ||==o{ B : bad").is_none());
        assert!(render_mermaid("erDiagram").is_none());
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
    fn test_render_mermaid_rejects_unsupported_diagram_types() {
        // Arrange & Act & Assert
        assert!(render_mermaid("graph RL\n    A --> B").is_none());
        assert!(render_mermaid("").is_none());
    }

    #[test]
    fn test_render_mermaid_rejects_source_over_preview_limits() {
        // Arrange
        let mut long_source = String::from("graph TD");
        for node_index in 0..MAX_SOURCE_LINE_COUNT {
            write!(&mut long_source, "\n    N{node_index}")
                .expect("writing to String should succeed");
        }

        let wide_source = format!("graph TD\n    A[{}]", "x".repeat(MAX_SOURCE_BYTE_COUNT));

        // Act & Assert
        assert!(render_mermaid(&long_source).is_none());
        assert!(render_mermaid(&wide_source).is_none());
    }

    #[test]
    fn test_render_mermaid_rejects_node_and_edge_over_preview_limits() {
        // Arrange
        let mut too_many_nodes = String::from("graph TD");
        for node_index in 0..=MAX_NODE_COUNT {
            write!(&mut too_many_nodes, "\n    N{node_index}")
                .expect("writing to String should succeed");
        }

        let mut too_many_edges = String::from("graph TD");
        for _ in 0..=MAX_EDGE_COUNT {
            too_many_edges.push_str("\n    A --> B");
        }

        // Act & Assert
        assert!(render_mermaid(&too_many_nodes).is_none());
        assert!(render_mermaid(&too_many_edges).is_none());
    }

    #[test]
    fn test_render_mermaid_rejects_unsupported_cycles() {
        // Arrange
        let cyclic = "graph TD\n    A --> B\n    B --> A";
        let three_node_cycle = "graph LR\n    A --> B\n    B --> C\n    C --> A";

        // Act & Assert
        assert!(render_mermaid(cyclic).is_none());
        assert!(render_mermaid(three_node_cycle).is_none());
    }

    #[test]
    fn test_render_mermaid_rejects_subgraph_statements() {
        // Arrange
        let source = "graph TD\n    subgraph Group\n    A --> B\n    end";

        // Act & Assert
        assert!(render_mermaid(source).is_none());
    }

    #[test]
    fn test_render_mermaid_rejects_wide_character_labels() {
        // Arrange
        let source = "graph TD\n    A[你好] --> B";

        // Act & Assert
        assert!(render_mermaid(source).is_none());
    }

    #[test]
    fn test_render_mermaid_rejects_unbounded_bare_identifiers() {
        // Arrange
        let long_identifier = "N".repeat(MAX_LABEL_WIDTH + 1);
        let long_bare = format!("graph TD\n    {long_identifier} --> B");
        let wide_bare = "graph TD\n    你好 --> B";

        // Act & Assert
        assert!(render_mermaid(&long_bare).is_none());
        assert!(render_mermaid(wide_bare).is_none());
    }

    #[test]
    fn test_render_mermaid_accepts_long_bare_identifier_with_short_label() {
        // Arrange
        let long_identifier = "N".repeat(MAX_LABEL_WIDTH + 1);
        let source = format!("graph TD\n    {long_identifier}[Short] --> B");

        // Act
        let diagram = render_mermaid(&source).expect("labeled node should render");
        let text = diagram_text(&diagram);

        // Assert
        assert!(text.contains("Short"));
        assert!(!text.contains(&long_identifier));
    }

    #[test]
    fn test_render_mermaid_skips_comments_and_inline_label_form() {
        // Arrange
        let source = "graph LR\n    %% comment line\n    A -- ok --> B";

        // Act
        let diagram = render_mermaid(source).expect("inline label form should render");
        let text = diagram_text(&diagram);

        // Assert
        assert!(text.contains('▶'));
        assert!(!text.contains("comment"));
    }
}
