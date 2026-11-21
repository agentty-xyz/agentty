//! Direction-neutral layered graph layout for terminal diagrams.
//!
//! Orders nodes within their layers to reduce edge crossings, assigns
//! coordinates that align connected nodes, and routes every edge through its
//! own border port and a routing track between two layers. Every coordinate is
//! measured along the cross axis: columns for a top-down diagram and rows for
//! a left-right one. All sweep counts are fixed, so layout cost stays bounded
//! by the graph size.

use std::cmp::Ordering;

const ALIGNED_WEIGHT: usize = 4;
const CHAIN_WEIGHT: usize = 8;
const FREE_WEIGHT: usize = 1;
const HIDDEN_WEIGHT: usize = 1024;
const ORDERING_ROUNDS: usize = 4;
const PORT_SPACING: usize = 2;
const POSITION_ROUNDS: usize = 4;

/// One edge between two adjacent layers.
pub(crate) struct LayerEdge {
    /// Node the edge leaves, in the earlier layer.
    pub(crate) from_index: usize,
    /// Whether the edge is drawn and therefore needs ports and a track.
    /// Unrouted edges still pull their endpoints together.
    pub(crate) is_routed: bool,
    /// Node the edge enters, in the next layer.
    pub(crate) to_index: usize,
}

/// Layered graph measured along the cross axis.
pub(crate) struct LayeredGraph {
    /// Edges, each joining a node to one in the next layer.
    pub(crate) edges: Vec<LayerEdge>,
    /// Minimum empty cells between neighboring nodes of one layer.
    pub(crate) gap: usize,
    /// Layer of every node.
    pub(crate) node_layers: Vec<usize>,
    /// Extent of every node. A size of one marks a hidden routing node, which
    /// keeps the long edge passing through it straight.
    pub(crate) node_sizes: Vec<usize>,
}

impl LayeredGraph {
    /// Orders and places the nodes within the extent of the widest packed
    /// layer, then routes every routed edge.
    pub(crate) fn arrange(&self) -> LayeredLayout {
        let links = NodeLinks::new(self);
        let layers = self.ordered_layers(&links);
        let (positions, extent) = self.aligned_positions(&layers, &links);
        let (source_ports, target_ports) = self.edge_ports(&layers, &positions);
        let (tracks, track_counts) = self.edge_tracks(&source_ports, &target_ports);
        let routes = self
            .edges
            .iter()
            .enumerate()
            .map(|(edge_index, edge)| {
                edge.is_routed.then(|| EdgeRoute {
                    source_port: source_ports[edge_index],
                    target_port: target_ports[edge_index],
                    track: tracks[edge_index],
                })
            })
            .collect();

        LayeredLayout {
            extent,
            positions,
            routes,
            track_counts,
        }
    }

    /// Orders the nodes of every layer, keeping the order with the fewest
    /// edge crossings seen across a fixed number of sweeps.
    ///
    /// The seed is a depth-first walk from the first layer, which keeps
    /// declaration order and places hidden routing nodes beside their
    /// endpoints. A seed without crossings is returned unchanged.
    fn ordered_layers(&self, links: &NodeLinks) -> Vec<Vec<usize>> {
        let mut layers = self.seeded_layers(links);
        let mut crossing_count = self.crossing_count(&layers);
        let mut candidate = layers.clone();

        for _ in 0..ORDERING_ROUNDS {
            if crossing_count == 0 {
                break;
            }

            for is_downward in [true, false] {
                self.sort_layers_by_median(&mut candidate, links, is_downward);
                self.swap_crossing_neighbors(&mut candidate, links);

                let candidate_crossing_count = self.crossing_count(&candidate);
                if candidate_crossing_count < crossing_count {
                    crossing_count = candidate_crossing_count;
                    layers.clone_from(&candidate);
                }
            }
        }

        layers
    }

    /// Returns the layers in depth-first visiting order.
    fn seeded_layers(&self, links: &NodeLinks) -> Vec<Vec<usize>> {
        let layer_count = self.node_layers.iter().max().map_or(0, |layer| layer + 1);
        let mut layers = vec![Vec::new(); layer_count];
        let mut is_visited = vec![false; self.node_layers.len()];
        let mut pending: Vec<usize> = (0..self.node_layers.len())
            .rev()
            .filter(|node_index| self.node_layers[*node_index] == 0)
            .collect();

        while let Some(node_index) = pending.pop() {
            if is_visited[node_index] {
                continue;
            }

            is_visited[node_index] = true;
            layers[self.node_layers[node_index]].push(node_index);
            pending.extend(links.targets[node_index].iter().rev());
        }

        layers
    }

    /// Counts the edge pairs that cross between two adjacent layers.
    fn crossing_count(&self, layers: &[Vec<usize>]) -> usize {
        let ranks = self.node_ranks(layers);
        let mut crossing_count = 0;

        for (edge_index, edge) in self.edges.iter().enumerate() {
            for other in &self.edges[..edge_index] {
                if self.node_layers[edge.from_index] != self.node_layers[other.from_index] {
                    continue;
                }

                let source_order = ranks[edge.from_index].cmp(&ranks[other.from_index]);
                let target_order = ranks[edge.to_index].cmp(&ranks[other.to_index]);
                if source_order != Ordering::Equal
                    && target_order != Ordering::Equal
                    && source_order != target_order
                {
                    crossing_count += 1;
                }
            }
        }

        crossing_count
    }

    /// Returns the index of every node within its layer.
    fn node_ranks(&self, layers: &[Vec<usize>]) -> Vec<usize> {
        let mut ranks = vec![0; self.node_layers.len()];
        for members in layers {
            for (rank, node_index) in members.iter().enumerate() {
                ranks[*node_index] = rank;
            }
        }

        ranks
    }

    /// Sorts every layer by the median rank of its neighbors in the layer
    /// visited just before it. Nodes without such neighbors keep their rank.
    fn sort_layers_by_median(
        &self,
        layers: &mut [Vec<usize>],
        links: &NodeLinks,
        is_downward: bool,
    ) {
        let layer_count = layers.len();
        for step in 1..layer_count {
            let layer_index = if is_downward {
                step
            } else {
                layer_count - 1 - step
            };
            let ranks = self.node_ranks(layers);
            let mut keyed_members: Vec<(usize, usize)> = layers[layer_index]
                .iter()
                .map(|node_index| {
                    let neighbors = if is_downward {
                        &links.sources[*node_index]
                    } else {
                        &links.targets[*node_index]
                    };
                    let mut neighbor_ranks: Vec<usize> =
                        neighbors.iter().map(|neighbor| ranks[*neighbor]).collect();
                    neighbor_ranks.sort_unstable();
                    let median_rank = match neighbor_ranks.len() {
                        0 => ranks[*node_index] * 2,
                        count => neighbor_ranks[(count - 1) / 2] + neighbor_ranks[count / 2],
                    };

                    (median_rank, *node_index)
                })
                .collect();
            keyed_members.sort_by_key(|(median_rank, _)| *median_rank);
            layers[layer_index] = keyed_members
                .into_iter()
                .map(|(_, node_index)| node_index)
                .collect();
        }
    }

    /// Swaps neighboring nodes of a layer when that removes edge crossings
    /// between their own edges.
    fn swap_crossing_neighbors(&self, layers: &mut [Vec<usize>], links: &NodeLinks) {
        for layer_index in 0..layers.len() {
            let ranks = self.node_ranks(layers);
            let members = &mut layers[layer_index];
            for right in 1..members.len() {
                let crossing_count =
                    links.crossing_count(&ranks, members[right - 1], members[right]);
                let swapped_crossing_count =
                    links.crossing_count(&ranks, members[right], members[right - 1]);
                if swapped_crossing_count < crossing_count {
                    members.swap(right - 1, right);
                }
            }
        }
    }

    /// Assigns the start coordinate of every node and returns the extent the
    /// nodes occupy.
    ///
    /// The nodes stay within the widest layer packed with the full gap
    /// between all of its nodes. Every layer starts packed and centered. A
    /// fixed number of sweeps then pulls each node toward the median of its
    /// anchor nodes while keeping the layer order and the gaps. Unused
    /// leading cells are dropped at the end.
    fn aligned_positions(&self, layers: &[Vec<usize>], links: &NodeLinks) -> (Vec<usize>, usize) {
        let mut packed_offsets = vec![0; self.node_sizes.len()];
        let packed_extents: Vec<usize> = layers
            .iter()
            .map(|members| {
                let mut cursor = 0;
                for (rank, node_index) in members.iter().enumerate() {
                    if rank > 0 {
                        cursor += self.node_gap(members[rank - 1], *node_index);
                    }
                    packed_offsets[*node_index] = cursor;
                    cursor += self.node_sizes[*node_index];
                }

                cursor
            })
            .collect();
        let extent = layers
            .iter()
            .map(|members| {
                members
                    .iter()
                    .map(|node_index| self.node_sizes[*node_index])
                    .sum::<usize>()
                    + self.gap * members.len().saturating_sub(1)
            })
            .max()
            .unwrap_or(0);
        let mut positions: Vec<usize> = packed_offsets
            .iter()
            .zip(&self.node_layers)
            .map(|(packed_offset, layer)| (extent - packed_extents[*layer]) / 2 + packed_offset)
            .collect();

        for _ in 0..POSITION_ROUNDS {
            for is_downward in [true, false] {
                for step in 0..layers.len() {
                    let layer_index = if is_downward {
                        step
                    } else {
                        layers.len() - 1 - step
                    };
                    self.align_layer(
                        &layers[layer_index],
                        &packed_offsets,
                        extent,
                        links,
                        is_downward,
                        &mut positions,
                    );
                }
            }
        }

        let first_position = positions.iter().copied().min().unwrap_or(0);
        for position in &mut positions {
            *position -= first_position;
        }
        let occupied_extent = positions
            .iter()
            .zip(&self.node_sizes)
            .map(|(position, size)| position + size)
            .max()
            .unwrap_or(0);

        (positions, occupied_extent)
    }

    /// Returns the empty cells kept between two neighboring nodes of a layer.
    ///
    /// Two hidden routing nodes need a single cell, so parallel long edges
    /// run side by side.
    fn node_gap(&self, left: usize, right: usize) -> usize {
        if self.node_sizes[left] == 1 && self.node_sizes[right] == 1 {
            return self.gap.min(1);
        }

        self.gap
    }

    /// Moves the nodes of one layer toward their alignment targets.
    ///
    /// Each node asks for a shift from its packed coordinate in
    /// `packed_offsets`. Neighbors whose requests would overlap merge into
    /// one block that takes their weighted mean, so nodes pulled to the same
    /// place settle around it evenly. The shift is biased by `extent` to
    /// stay unsigned and limited to the room the packed layer leaves within
    /// the extent.
    fn align_layer(
        &self,
        members: &[usize],
        packed_offsets: &[usize],
        extent: usize,
        links: &NodeLinks,
        is_downward: bool,
        positions: &mut [usize],
    ) {
        let mut blocks: Vec<AlignmentBlock> = Vec::new();
        for node_index in members {
            let (target_center, weight) =
                self.alignment_target(*node_index, links, is_downward, positions);
            let center_offset =
                packed_offsets[*node_index] + (self.node_sizes[*node_index] - 1) / 2;
            let mut block = AlignmentBlock {
                node_count: 1,
                weight,
                weighted_shift: weight * (target_center + extent - center_offset),
            };
            while let Some(previous) = blocks.pop_if(|previous| previous.is_after(&block)) {
                block.node_count += previous.node_count;
                block.weight += previous.weight;
                block.weighted_shift += previous.weighted_shift;
            }
            blocks.push(block);
        }

        let packed_extent = members
            .last()
            .map_or(0, |last| packed_offsets[*last] + self.node_sizes[*last]);
        let mut remaining_members = members.iter();
        for block in blocks {
            let shift = block.shift().clamp(extent, 2 * extent - packed_extent) - extent;
            for node_index in remaining_members.by_ref().take(block.node_count) {
                positions[*node_index] = packed_offsets[*node_index] + shift;
            }
        }
    }

    /// Returns the center one node wants and how strongly it wants it.
    ///
    /// A node follows the side that holds more of its edges. With equal
    /// sides it follows a one-to-one chain edge, and otherwise the layer the
    /// sweep placed just before it. A node without edges keeps its place.
    fn alignment_target(
        &self,
        node_index: usize,
        links: &NodeLinks,
        is_downward: bool,
        positions: &[usize],
    ) -> (usize, usize) {
        let sources = &links.sources[node_index];
        let targets = &links.targets[node_index];
        let is_source_chain = sources.len() == 1 && links.targets[sources[0]].len() == 1;
        let is_target_chain = targets.len() == 1 && links.sources[targets[0]].len() == 1;
        let follows_sources = match sources.len().cmp(&targets.len()) {
            Ordering::Greater => true,
            Ordering::Less => false,
            Ordering::Equal if is_source_chain == is_target_chain => is_downward,
            Ordering::Equal => is_source_chain,
        };
        let (anchors, is_chain) = if follows_sources {
            (sources, is_source_chain)
        } else {
            (targets, is_target_chain)
        };

        let mut anchor_centers: Vec<usize> = anchors
            .iter()
            .map(|anchor| self.node_center(*anchor, positions))
            .collect();
        anchor_centers.sort_unstable();
        let anchor_count = anchor_centers.len();
        if anchor_count == 0 {
            return (self.node_center(node_index, positions), FREE_WEIGHT);
        }

        let target_center =
            anchor_centers[(anchor_count - 1) / 2].midpoint(anchor_centers[anchor_count / 2]);
        let weight = if self.node_sizes[node_index] == 1 {
            HIDDEN_WEIGHT
        } else if is_chain {
            CHAIN_WEIGHT
        } else {
            ALIGNED_WEIGHT
        };

        (target_center, weight)
    }

    /// Returns the center coordinate of one placed node.
    fn node_center(&self, node_index: usize, positions: &[usize]) -> usize {
        positions[node_index] + (self.node_sizes[node_index] - 1) / 2
    }

    /// Assigns the source and target port of every routed edge.
    ///
    /// The edges of one node border are ordered by the position of their
    /// other end, and each border starts with evenly spread ports. The wider
    /// end of every edge then moves toward the port of the narrower end,
    /// which stays centered when both can meet. Target ports then follow the
    /// source ports and source ports follow the target ports, so an edge
    /// runs straight whenever both borders reach a common coordinate.
    fn edge_ports(&self, layers: &[Vec<usize>], positions: &[usize]) -> (Vec<usize>, Vec<usize>) {
        let ranks = self.node_ranks(layers);
        let mut incoming = vec![Vec::new(); self.node_sizes.len()];
        let mut outgoing = vec![Vec::new(); self.node_sizes.len()];
        for (edge_index, edge) in self.edges.iter().enumerate() {
            if edge.is_routed {
                incoming[edge.to_index].push(edge_index);
                outgoing[edge.from_index].push(edge_index);
            }
        }
        for edge_indexes in &mut incoming {
            edge_indexes.sort_by_key(|edge_index| ranks[self.edges[*edge_index].from_index]);
        }
        for edge_indexes in &mut outgoing {
            edge_indexes.sort_by_key(|edge_index| ranks[self.edges[*edge_index].to_index]);
        }

        let mut source_ports = vec![0; self.edges.len()];
        let mut target_ports = vec![0; self.edges.len()];
        self.assign_side_ports(&outgoing, positions, None, &mut source_ports);
        self.assign_side_ports(&incoming, positions, None, &mut target_ports);

        let mut desired_source_ports = source_ports.clone();
        let mut desired_target_ports = target_ports.clone();
        for (edge_index, edge) in self.edges.iter().enumerate() {
            if self.node_sizes[edge.to_index] < self.node_sizes[edge.from_index] {
                desired_source_ports[edge_index] = target_ports[edge_index];
            } else {
                desired_target_ports[edge_index] = source_ports[edge_index];
            }
        }
        self.assign_side_ports(
            &outgoing,
            positions,
            Some(&desired_source_ports),
            &mut source_ports,
        );
        self.assign_side_ports(
            &incoming,
            positions,
            Some(&desired_target_ports),
            &mut target_ports,
        );
        self.assign_side_ports(&incoming, positions, Some(&source_ports), &mut target_ports);
        self.assign_side_ports(&outgoing, positions, Some(&target_ports), &mut source_ports);
        self.separate_swapped_ports(positions, &source_ports, &mut target_ports);

        (source_ports, target_ports)
    }

    /// Moves a target port one cell aside when two bending edges of one
    /// region swap coordinates.
    ///
    /// Each of the two edges leaves the coordinate the other enters, so one
    /// of those coordinates would carry both edges on top of each other.
    /// With a port moved to a free neighboring cell the edges cross in one
    /// cell instead. The port stays when its border has no free cell.
    fn separate_swapped_ports(
        &self,
        positions: &[usize],
        source_ports: &[usize],
        target_ports: &mut [usize],
    ) {
        for (edge_index, edge) in self.edges.iter().enumerate() {
            let source_port = source_ports[edge_index];
            let target_port = target_ports[edge_index];
            let is_swapped = source_port != target_port
                && self.edges[..edge_index]
                    .iter()
                    .enumerate()
                    .any(|(other_index, other)| {
                        self.node_layers[other.from_index] == self.node_layers[edge.from_index]
                            && source_ports[other_index] == target_port
                            && target_ports[other_index] == source_port
                    });
            if !is_swapped {
                continue;
            }

            let side = PortSide::new(positions[edge.to_index], self.node_sizes[edge.to_index], 0);
            let free_port = [target_port + 1, target_port.saturating_sub(1)]
                .into_iter()
                .find(|port| {
                    (side.first..=side.last).contains(port)
                        && !self.edges.iter().enumerate().any(|(other_index, other)| {
                            other.is_routed
                                && other.to_index == edge.to_index
                                && target_ports[other_index] == *port
                        })
                });
            if let Some(free_port) = free_port {
                target_ports[edge_index] = free_port;
            }
        }
    }

    /// Assigns the ports of one border of every node, moving each port to
    /// the coordinate its edge asks for when `desired_ports` is present.
    fn assign_side_ports(
        &self,
        side_edges: &[Vec<usize>],
        positions: &[usize],
        desired_ports: Option<&[usize]>,
        ports: &mut [usize],
    ) {
        for (node_index, edge_indexes) in side_edges.iter().enumerate() {
            let side = PortSide::new(
                positions[node_index],
                self.node_sizes[node_index],
                edge_indexes.len(),
            );
            let side_ports = match desired_ports {
                Some(desired_ports) => side.snapped_ports(
                    &edge_indexes
                        .iter()
                        .map(|edge_index| desired_ports[*edge_index])
                        .collect::<Vec<_>>(),
                ),
                None => side.default_ports(),
            };
            for (edge_index, port) in edge_indexes.iter().zip(side_ports) {
                ports[*edge_index] = port;
            }
        }
    }

    /// Assigns a routing track to every routed edge that bends and returns
    /// the number of tracks each region between two layers uses.
    fn edge_tracks(
        &self,
        source_ports: &[usize],
        target_ports: &[usize],
    ) -> (Vec<Option<usize>>, Vec<usize>) {
        let region_count = self.node_layers.iter().max().copied().unwrap_or(0);
        let mut region_edges: Vec<Vec<usize>> = vec![Vec::new(); region_count];
        for (edge_index, edge) in self.edges.iter().enumerate() {
            if edge.is_routed {
                region_edges[self.node_layers[edge.from_index]].push(edge_index);
            }
        }

        let mut tracks = vec![None; self.edges.len()];
        let track_counts = region_edges
            .iter()
            .map(|edge_indexes| {
                let segments = TrackSegment::grouped(edge_indexes, source_ports, target_ports);

                TrackSegment::assign_tracks(segments, &mut tracks)
            })
            .collect();

        (tracks, track_counts)
    }
}

/// Result of laying out a [`LayeredGraph`].
pub(crate) struct LayeredLayout {
    /// Cross-axis extent that holds every node.
    pub(crate) extent: usize,
    /// Start coordinate of every node.
    pub(crate) positions: Vec<usize>,
    /// Route of every edge, absent for edges that are not routed.
    pub(crate) routes: Vec<Option<EdgeRoute>>,
    /// Number of routing tracks in the region after each layer but the last.
    pub(crate) track_counts: Vec<usize>,
}

/// Ports and routing track of one routed edge.
pub(crate) struct EdgeRoute {
    /// Coordinate where the edge leaves its source node.
    pub(crate) source_port: usize,
    /// Coordinate where the edge enters its target node.
    pub(crate) target_port: usize,
    /// Track the edge bends on, counted from the source layer. Absent when
    /// both ports share one coordinate and the edge runs straight.
    pub(crate) track: Option<usize>,
}

/// Neighbor nodes of every node, in edge declaration order.
struct NodeLinks {
    sources: Vec<Vec<usize>>,
    targets: Vec<Vec<usize>>,
}

impl NodeLinks {
    /// Collects the neighbors joined by every edge, routed or not.
    fn new(graph: &LayeredGraph) -> Self {
        let mut sources = vec![Vec::new(); graph.node_layers.len()];
        let mut targets = vec![Vec::new(); graph.node_layers.len()];
        for edge in &graph.edges {
            sources[edge.to_index].push(edge.from_index);
            targets[edge.from_index].push(edge.to_index);
        }

        Self { sources, targets }
    }

    /// Counts the crossings between the edges of two nodes of one layer when
    /// `left` is placed before `right`.
    fn crossing_count(&self, ranks: &[usize], left: usize, right: usize) -> usize {
        [
            (&self.sources[left], &self.sources[right]),
            (&self.targets[left], &self.targets[right]),
        ]
        .iter()
        .map(|(left_neighbors, right_neighbors)| {
            left_neighbors
                .iter()
                .map(|left_neighbor| {
                    right_neighbors
                        .iter()
                        .filter(|right_neighbor| ranks[**right_neighbor] < ranks[*left_neighbor])
                        .count()
                })
                .sum::<usize>()
        })
        .sum()
    }
}

/// Run of neighboring nodes in one layer that share one shift.
struct AlignmentBlock {
    node_count: usize,
    weight: usize,
    weighted_shift: usize,
}

impl AlignmentBlock {
    /// Returns whether this block asks for a larger shift than `next`, the
    /// block that follows it in the layer.
    fn is_after(&self, next: &Self) -> bool {
        self.weighted_shift * next.weight > next.weighted_shift * self.weight
    }

    /// Returns the weighted mean shift of the block, rounded to a cell.
    fn shift(&self) -> usize {
        (2 * self.weighted_shift + self.weight) / (2 * self.weight)
    }
}

/// One node border that edges attach to.
struct PortSide {
    center: usize,
    first: usize,
    last: usize,
    port_count: usize,
}

impl PortSide {
    /// Describes the border of a node placed at `position`.
    ///
    /// Box corners never hold a port, so only a hidden routing node offers
    /// its whole extent.
    fn new(position: usize, size: usize, port_count: usize) -> Self {
        let corner = usize::from(size >= 3);

        Self {
            center: position + (size - 1) / 2,
            first: position + corner,
            last: position + size - 1 - corner,
            port_count,
        }
    }

    /// Returns the ports spread evenly around the border center.
    fn default_ports(&self) -> Vec<usize> {
        if self.is_shared() {
            return vec![self.center; self.port_count];
        }

        let radius = (self.center - self.first).min(self.last - self.center);
        let first = self.center - radius;
        let width = 2 * radius + 1;

        (0..self.port_count)
            .map(|port_index| first + (2 * port_index + 1) * width / (2 * self.port_count))
            .collect()
    }

    /// Returns the ports moved to the coordinates in `desired_ports` where
    /// the border reaches them, keeping their order and spacing.
    fn snapped_ports(&self, desired_ports: &[usize]) -> Vec<usize> {
        let default_ports = self.default_ports();
        if self.is_shared() {
            return default_ports;
        }

        let mut ports = Vec::with_capacity(self.port_count);
        let mut lower = self.first;
        for (port_index, (default_port, desired_port)) in
            default_ports.iter().zip(desired_ports).enumerate()
        {
            let upper = self.last - PORT_SPACING * (self.port_count - 1 - port_index);
            let preferred_port = if (self.first..=self.last).contains(desired_port) {
                *desired_port
            } else {
                *default_port
            };
            let port = preferred_port.clamp(lower, upper);
            ports.push(port);
            lower = port + PORT_SPACING;
        }

        ports
    }

    /// Returns whether the border is too short to keep one free cell between
    /// its ports, so every edge shares the center port.
    fn is_shared(&self) -> bool {
        self.last - self.first + PORT_SPACING < PORT_SPACING * self.port_count
    }
}

/// Port that joins several bending edges into one bus.
#[derive(Clone, Copy, PartialEq)]
enum BusPort {
    Source(usize),
    Target(usize),
}

/// Bending edges of one region that run along one routing track together.
///
/// An edge whose ports differ bends on a track. Edges that fan out of one
/// shared source port, or into one shared target port, form a bus: they use
/// one segment and branch off it. An edge that shares both of its ports
/// stays alone, because a bus joined at both ends would connect unrelated
/// nodes.
struct TrackSegment {
    bus_port: Option<BusPort>,
    edge_indexes: Vec<usize>,
    source_ports: Vec<usize>,
    target_ports: Vec<usize>,
}

impl TrackSegment {
    /// Groups the bending edges among the routed edges of one region.
    fn grouped(
        edge_indexes: &[usize],
        source_ports: &[usize],
        target_ports: &[usize],
    ) -> Vec<Self> {
        let mut segments: Vec<Self> = Vec::new();

        for edge_index in edge_indexes {
            let source_port = source_ports[*edge_index];
            let target_port = target_ports[*edge_index];
            if source_port == target_port {
                continue;
            }

            let is_source_shared = edge_indexes
                .iter()
                .filter(|other| source_ports[**other] == source_port)
                .count()
                > 1;
            let is_target_shared = edge_indexes
                .iter()
                .filter(|other| target_ports[**other] == target_port)
                .count()
                > 1;
            let bus_port = match (is_source_shared, is_target_shared) {
                (true, false) => Some(BusPort::Source(source_port)),
                (false, true) => Some(BusPort::Target(target_port)),
                _ => None,
            };
            let bus = segments
                .iter_mut()
                .find(|segment| bus_port.is_some() && segment.bus_port == bus_port);
            if let Some(bus) = bus {
                bus.edge_indexes.push(*edge_index);
                bus.source_ports.push(source_port);
                bus.target_ports.push(target_port);
            } else {
                segments.push(Self {
                    bus_port,
                    edge_indexes: vec![*edge_index],
                    source_ports: vec![source_port],
                    target_ports: vec![target_port],
                });
            }
        }

        segments
    }

    /// Assigns a track to every segment of one region and returns the number
    /// of tracks used.
    ///
    /// Each pass fills one track with the segments that the fewest unplaced
    /// segments must precede, skipping any that would overlap a segment
    /// already on the track. Segments without such constraints therefore
    /// never cross, and segments with disjoint spans share a track. When the
    /// constraints conflict, a segment that would otherwise run on top of
    /// another one goes first, leaving a plain crossing.
    fn assign_tracks(mut segments: Vec<Self>, tracks: &mut [Option<usize>]) -> usize {
        segments.sort_by_key(Self::first);
        let mut track_count = 0;

        while !segments.is_empty() {
            let blocker_counts: Vec<(usize, usize)> = segments
                .iter()
                .map(|segment| {
                    (
                        segments
                            .iter()
                            .filter(|other| other.leaves_port_of(segment))
                            .count(),
                        segments
                            .iter()
                            .filter(|other| other.precedes(segment))
                            .count(),
                    )
                })
                .collect();
            let fewest_blocker_count = blocker_counts.iter().copied().min().unwrap_or((0, 0));
            let mut track_segments: Vec<Self> = Vec::new();
            let mut later_segments = Vec::new();

            for (segment, blocker_count) in segments.into_iter().zip(blocker_counts) {
                if blocker_count == fewest_blocker_count
                    && track_segments
                        .iter()
                        .all(|placed| placed.shares_track(&segment))
                {
                    for edge_index in &segment.edge_indexes {
                        tracks[*edge_index] = Some(track_count);
                    }
                    track_segments.push(segment);
                } else {
                    later_segments.push(segment);
                }
            }

            segments = later_segments;
            track_count += 1;
        }

        track_count
    }

    /// Returns whether this segment leaves a port coordinate that `other`
    /// enters, so the two would run on top of each other unless this
    /// segment uses a track nearer the source layer.
    fn leaves_port_of(&self, other: &Self) -> bool {
        self.source_ports
            .iter()
            .any(|port| other.target_ports.contains(port))
    }

    /// Returns whether this segment must use a track nearer the source
    /// layer than `other` to keep the two from crossing or overlapping.
    ///
    /// The run from a source port must end before the other track when that
    /// track passes the port, and the run into a target port must start
    /// after this track when this track passes the port. A port both
    /// segments use imposes no order.
    fn precedes(&self, other: &Self) -> bool {
        self.source_ports
            .iter()
            .any(|port| !other.source_ports.contains(port) && other.spans(*port))
            || other
                .target_ports
                .iter()
                .any(|port| !self.target_ports.contains(port) && self.spans(*port))
    }

    /// Returns whether both segments fit on one track.
    ///
    /// Their spans must stay one cell apart.
    fn shares_track(&self, other: &Self) -> bool {
        self.first().max(other.first()) > self.last().min(other.last()) + 1
    }

    /// Returns whether the segment covers `port`.
    fn spans(&self, port: usize) -> bool {
        (self.first()..=self.last()).contains(&port)
    }

    /// Returns the smallest port coordinate.
    fn first(&self) -> usize {
        self.ports().min().unwrap_or(0)
    }

    /// Returns the largest port coordinate.
    fn last(&self) -> usize {
        self.ports().max().unwrap_or(0)
    }

    /// Returns every port the segment connects.
    fn ports(&self) -> impl Iterator<Item = usize> {
        self.source_ports.iter().chain(&self.target_ports).copied()
    }
}

#[cfg(test)]
#[path = "mermaid_layout_test.rs"]
mod tests;
