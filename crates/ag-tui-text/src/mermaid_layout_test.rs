//! Unit tests for the layered graph layout stages.

use super::{LayerEdge, LayeredGraph, NodeLinks, PortSide, TrackSegment};

/// Builds a graph of box nodes five cells wide with a three-cell gap, whose
/// edges are all routed.
fn box_graph(node_layers: &[usize], edges: &[(usize, usize)]) -> LayeredGraph {
    LayeredGraph {
        edges: edges
            .iter()
            .map(|(from_index, to_index)| LayerEdge {
                from_index: *from_index,
                is_routed: true,
                to_index: *to_index,
            })
            .collect(),
        gap: 3,
        node_layers: node_layers.to_vec(),
        node_sizes: vec![5; node_layers.len()],
    }
}

/// Builds a segment for one bending edge.
fn bend(edge_index: usize, source_port: usize, target_port: usize) -> TrackSegment {
    TrackSegment {
        bus_port: None,
        edge_indexes: vec![edge_index],
        source_ports: vec![source_port],
        target_ports: vec![target_port],
    }
}

#[test]
fn test_arrange_centers_parent_over_children_and_routes_separate_ports() {
    // Arrange
    let graph = box_graph(&[0, 1, 1], &[(0, 1), (0, 2)]);

    // Act
    let layout = graph.arrange();
    let routes: Vec<(usize, usize, Option<usize>)> = layout
        .routes
        .iter()
        .flatten()
        .map(|route| (route.source_port, route.target_port, route.track))
        .collect();

    // Assert
    assert_eq!(layout.extent, 13);
    assert_eq!(layout.positions, [4, 0, 8]);
    assert_eq!(routes, [(5, 2, Some(0)), (7, 10, Some(0))]);
    assert_eq!(layout.track_counts, [1]);
}

#[test]
fn test_arrange_skips_ports_and_tracks_of_unrouted_edges() {
    // Arrange
    let mut graph = box_graph(&[0, 1, 1], &[(0, 1), (0, 2)]);
    graph.edges[0].is_routed = false;

    // Act
    let layout = graph.arrange();

    // Assert
    assert!(layout.routes[0].is_none());
    let route = layout.routes[1].as_ref().expect("routed edge");
    assert_eq!((route.source_port, route.target_port), (6, 10));
    assert_eq!(layout.positions, [4, 0, 8]);
}

#[test]
fn test_arrange_reorders_layers_and_keeps_rank_of_nodes_without_neighbors() {
    // Arrange
    let graph = box_graph(&[0, 0, 1, 1, 2], &[(0, 2), (0, 3), (1, 2), (2, 4)]);

    // Act
    let layout = graph.arrange();

    // Assert
    assert!(layout.positions[3] < layout.positions[2]);
    assert!(layout.positions[0] < layout.positions[1]);
    assert_eq!(layout.positions[4], layout.positions[2]);
}

#[test]
fn test_swap_crossing_neighbors_swaps_when_crossings_drop() {
    // Arrange
    let graph = box_graph(&[0, 0, 1, 1], &[(0, 3), (1, 2)]);
    let links = NodeLinks::new(&graph);
    let mut layers = vec![vec![0, 1], vec![2, 3]];

    // Act
    graph.swap_crossing_neighbors(&mut layers, &links);

    // Assert
    assert_eq!(layers, [[1, 0], [2, 3]]);
    assert_eq!(graph.crossing_count(&layers), 0);
}

#[test]
fn test_port_side_spreads_ports_and_shares_center_when_too_short() {
    // Arrange
    let wide_side = PortSide::new(10, 8, 3);
    let short_side = PortSide::new(10, 5, 3);
    let hidden_side = PortSide::new(4, 1, 1);

    // Act
    let wide_ports = wide_side.default_ports();
    let short_ports = short_side.snapped_ports(&[11, 12, 13]);
    let hidden_ports = hidden_side.default_ports();

    // Assert
    assert_eq!(wide_ports, [11, 13, 15]);
    assert_eq!(short_ports, [12, 12, 12]);
    assert_eq!(hidden_ports, [4]);
}

#[test]
fn test_port_side_snaps_reachable_ports_and_keeps_order_and_spacing() {
    // Arrange
    let side = PortSide::new(0, 12, 3);

    // Act
    let reachable_ports = side.snapped_ports(&[2, 6, 9]);
    let crowded_ports = side.snapped_ports(&[9, 9, 30]);

    // Assert
    assert_eq!(reachable_ports, [2, 6, 9]);
    assert_eq!(crowded_ports, [6, 8, 10]);
}

#[test]
fn test_track_segments_group_edges_of_one_shared_port_into_a_bus() {
    // Arrange
    let edge_indexes = [0, 1, 2, 3, 4];
    let source_ports = [6, 6, 6, 20, 30];
    let target_ports = [2, 6, 10, 10, 34];

    // Act
    let segments = TrackSegment::grouped(&edge_indexes, &source_ports, &target_ports);
    let grouped_edges: Vec<&[usize]> = segments
        .iter()
        .map(|segment| segment.edge_indexes.as_slice())
        .collect();

    // Assert
    assert_eq!(grouped_edges, [&[0][..], &[2][..], &[3][..], &[4][..]]);
    assert!(segments[0].bus_port.is_some());
    assert!(segments[1].bus_port.is_none());
    assert!(segments[2].bus_port.is_some());
    assert!(segments[3].bus_port.is_none());
}

#[test]
fn test_assign_tracks_orders_nested_bends_and_shares_disjoint_tracks() {
    // Arrange
    let segments = vec![bend(0, 10, 2), bend(1, 12, 6), bend(2, 20, 30)];
    let mut tracks = vec![None; 3];

    // Act
    let track_count = TrackSegment::assign_tracks(segments, &mut tracks);

    // Assert
    assert_eq!(track_count, 2);
    assert_eq!(tracks, [Some(0), Some(1), Some(0)]);
}

#[test]
fn test_assign_tracks_resolves_conflicting_constraints_without_overlap() {
    // Arrange
    let crossing_segments = vec![bend(0, 5, 1), bend(1, 3, 7)];
    let overlapping_segments = vec![bend(0, 3, 9), bend(1, 9, 2)];
    let mut crossing_tracks = vec![None; 2];
    let mut overlapping_tracks = vec![None; 2];

    // Act
    let crossing_track_count = TrackSegment::assign_tracks(crossing_segments, &mut crossing_tracks);
    let overlapping_track_count =
        TrackSegment::assign_tracks(overlapping_segments, &mut overlapping_tracks);

    // Assert
    assert_eq!(crossing_track_count, 2);
    assert_eq!(crossing_tracks, [Some(0), Some(1)]);
    assert_eq!(overlapping_track_count, 2);
    assert_eq!(overlapping_tracks, [Some(1), Some(0)]);
}
