//! A real petgraph-fork experiment using the existing lance-graph evaluator.
//!
//! Runs ONE directed graph hop over borrowed SoA edge lanes, without constructing
//! a petgraph Graph, implementing graph traits, or interpreting an alternate IR.
//! The petgraph Graph/Bfs path exists solely as a differential oracle in tests.
//!
//! This does not solve iterative Gremlin repeat/BFS: Mask-RISC's
//! ScatterOrU32 terminal is a demanded sink, not a licensed intermediate
//! population. That is precisely the later lance-graph fold-barrier question.

#![forbid(unsafe_code)]

#[cfg(feature = "lance")]
pub mod lance {
    use lance_graph_mask_risc::{
        execute_into, words_for, Foreign, ForeignPlane, LaneRef, MaskOp, Operand,
        Out, Planes, Program, Scratch, Terminal,
    };

    /// Edges are not a second graph object: these are borrowed SoA columns.
    /// Row IDs are local positions, not canonical thought GUIDs.
    #[derive(Clone, Copy)]
    pub struct EdgeColumns<'a> {
        pub sources: &'a [u32],
        pub targets: &'a [u32],
    }

    /// A selected witness version over one stable row order.
    #[derive(Clone, Copy)]
    pub struct Snapshot<'a> {
        pub witness_version: u64,
        pub node_rows: usize,
        /// LSB-first, one bit per node. Scope authorization is already applied.
        pub visible_nodes: &'a [u64],
        /// LSB-first, one bit per edge, selected at this witness version.
        pub live_edges: &'a [u64],
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum HopError {
        StaleWitness,
        InvalidShape,
        InvalidSeed,
        InvisibleSeed,
        Executor(String),
    }

    fn contains(mask: &[u64], row: usize) -> bool {
        mask.get(row / 64)
            .is_some_and(|word| (word & (1u64 << (row % 64))) != 0)
    }

    fn valid_tail(mask: &[u64], rows: usize) -> bool {
        if mask.len() != words_for(rows) {
            return false;
        }
        if rows == 0 || rows % 64 == 0 {
            return true;
        }
        mask.last()
            .is_some_and(|word| (word >> (rows % 64)) == 0)
    }

    /// Execute one scoped, witness-pinned outgoing adjacency fold.
    ///
    /// The only query evaluator is Mask-RISC: an edge-presence-gated Gather
    /// from the source frontier, a visibility-gated Gather through target IDs,
    /// and ScatterOrU32 into the *demanded* one-hop result plane.
    ///
    /// An implementation of repeat/iterative BFS must add an explicit
    /// continuation contract upstream rather than quietly reusing the result
    /// as an undocumented intermediate.
    pub fn one_hop(
        edges: EdgeColumns<'_>,
        snapshot: Snapshot<'_>,
        seed: usize,
        expected_witness: u64,
    ) -> Result<Vec<u64>, HopError> {
        if snapshot.witness_version != expected_witness {
            return Err(HopError::StaleWitness);
        }
        if snapshot.node_rows > u32::MAX as usize
            || edges.sources.len() != edges.targets.len()
            || !valid_tail(snapshot.visible_nodes, snapshot.node_rows)
            || !valid_tail(snapshot.live_edges, edges.sources.len())
            || edges.sources.iter().chain(edges.targets.iter()).any(
                |&id| id as usize >= snapshot.node_rows
            )
        {
            return Err(HopError::InvalidShape);
        }
        if seed >= snapshot.node_rows {
            return Err(HopError::InvalidSeed);
        }
        if !contains(snapshot.visible_nodes, seed) {
            return Err(HopError::InvisibleSeed);
        }

        let mut frontier = vec![0u64; words_for(snapshot.node_rows)];
        frontier[seed / 64] |= 1u64 << (seed % 64);
        let mut targets = vec![0u64; words_for(snapshot.node_rows)];

        if edges.sources.is_empty() {
            return Ok(targets);
        }

        let planes_masks = [snapshot.live_edges];
        let lanes = [
            LaneRef::U32(edges.sources),
            LaneRef::U32(edges.targets),
        ];
        let planes = Planes {
            n_rows: edges.sources.len(),
            masks: &planes_masks,
            lanes: &lanes,
        };
        let foreign_planes = [
            ForeignPlane { words: &frontier, rows: snapshot.node_rows },
            ForeignPlane { words: snapshot.visible_nodes, rows: snapshot.node_rows },
        ];
        let foreign = Foreign { planes: &foreign_planes, lanes: &[] };
        let program = Program::new(
            vec![
                MaskOp::Gather {
                    lane: 0,
                    foreign: 0,
                    under: Some(Operand::Plane(0)),
                    dst: 0,
                },
                MaskOp::Gather {
                    lane: 1,
                    foreign: 1,
                    under: Some(Operand::Scratch(0)),
                    dst: 1,
                },
            ],
            Terminal::ScatterOrU32 {
                mask: Operand::Scratch(1),
                lane: 1,
                out_rows: snapshot.node_rows as u32,
            },
        );
        let mut scratch = Scratch::for_program(&program, edges.sources.len())
            .map_err(|error| HopError::Executor(format!("{error:?}")))?;
        execute_into(
            &program, &planes, &foreign, &mut scratch, Out::Mask(&mut targets),
        )
        .map_err(|error| HopError::Executor(format!("{error:?}")))?;

        Ok(targets)
    }
}

#[cfg(all(test, feature = "lance"))]
mod tests {
    use super::lance::{one_hop, EdgeColumns, HopError, Snapshot};
    use lance_graph_mask_risc::words_for;
    use petgraph::{graph::{DiGraph, NodeIndex}, visit::Bfs};
    use std::collections::BTreeSet;

    fn mask(rows: usize, present: &[usize]) -> Vec<u64> {
        let mut out = vec![0u64; words_for(rows)];
        for &i in present {
            out[i / 64] |= 1u64 << (i % 64);
        }
        out
    }

    fn ids(bits: &[u64], rows: usize) -> BTreeSet<usize> {
        (0..rows).filter(|&i| (bits[i / 64] >> (i % 64)) & 1 != 0).collect()
    }

    /// This is the oracle, not the execution path. An ordinary Graph is
    /// constructed ONLY inside the tests.
    fn petgraph_first_bfs_layer(
        node_count: usize,
        edges: &[(usize, usize)],
        live: &[u64],
        visible: &[u64],
        seed: usize,
    ) -> BTreeSet<usize> {
        let mut graph = DiGraph::<u128, ()>::new();
        for i in 0..node_count {
            graph.add_node(0xA000_0000_0000_0000u128 + i as u128);
        }
        for (edge_idx, &(src, dst)) in edges.iter().enumerate() {
            if ((live[edge_idx / 64] >> (edge_idx % 64)) & 1) != 0
                && ((visible[src / 64] >> (src % 64)) & 1) != 0
                && ((visible[dst / 64] >> (dst % 64)) & 1) != 0
            {
                graph.add_edge(NodeIndex::new(src), NodeIndex::new(dst), ());
            }
        }
        let mut bfs = Bfs::new(&graph, NodeIndex::new(seed));
        assert_eq!(bfs.next(&graph), Some(NodeIndex::new(seed)));
        bfs.stack.iter().map(|id| id.index()).collect()
    }

    fn compare(
        nodes: usize,
        edge_pairs: &[(usize, usize)],
        visible: &[u64],
        live: &[u64],
        witness: u64,
        seed: usize,
    ) -> BTreeSet<usize> {
        let sources: Vec<u32> = edge_pairs.iter().map(|e| e.0 as u32).collect();
        let targets: Vec<u32> = edge_pairs.iter().map(|e| e.1 as u32).collect();
        let got = one_hop(
            EdgeColumns { sources: &sources, targets: &targets },
            Snapshot {
                witness_version: witness,
                node_rows: nodes,
                visible_nodes: visible,
                live_edges: live,
            },
            seed, witness,
        ).expect("Mask-RISC hop must execute");
        let actual = ids(&got, nodes);
        let expected = petgraph_first_bfs_layer(nodes, edge_pairs, live, visible, seed);
        assert_eq!(actual, expected, "Mask-RISC and petgraph BFS first layer diverged");
        actual
    }

    #[test]
    fn masked_hop_matches_petgraph_bfs_first_layer_across_word_boundaries() {
        let edges = [(0, 1), (0, 65), (0, 129), (0, 129), (1, 66), (129, 128)];
        let visible = mask(130, &(0..130).collect::<Vec<_>>());
        let live = mask(edges.len(), &(0..edges.len()).collect::<Vec<_>>());
        assert_eq!(compare(130, &edges, &visible, &live, 7, 0),
            BTreeSet::from([1, 65, 129]));
    }

    #[test]
    fn private_bridge_cannot_be_returned_or_implicitly_traversed() {
        let edges = [(0, 65), (65, 129), (0, 1)];
        let visible = mask(130, &[0, 1, 129]); // 65 is private
        let live = mask(edges.len(), &[0, 1, 2]);
        assert_eq!(compare(130, &edges, &visible, &live, 9, 0),
            BTreeSet::from([1]));
    }

    #[test]
    fn same_identity_rows_have_distinct_edge_presence_by_witness() {
        let edges = [(0, 1), (0, 65), (0, 129)];
        let visible = mask(130, &[0, 1, 65, 129]);
        let first = mask(edges.len(), &[0, 1]);
        let second = mask(edges.len(), &[0, 2]);
        assert_eq!(compare(130, &edges, &visible, &first, 11, 0),
            BTreeSet::from([1, 65]));
        assert_eq!(compare(130, &edges, &visible, &second, 12, 0),
            BTreeSet::from([1, 129]));
    }

    #[test]
    fn stale_witness_and_hidden_seed_are_refused_before_execution() {
        let sources = [0u32];
        let targets = [1u32];
        let edges = EdgeColumns { sources: &sources, targets: &targets };
        let visible = mask(2, &[1]);
        let live = mask(1, &[0]);
        let snapshot = Snapshot {
            witness_version: 4, node_rows: 2, visible_nodes: &visible, live_edges: &live,
        };
        assert_eq!(one_hop(edges, snapshot, 0, 3), Err(HopError::StaleWitness));
        assert_eq!(one_hop(edges, snapshot, 0, 4), Err(HopError::InvisibleSeed));
    }

    #[test]
    fn absent_and_deleted_edges_are_not_promoted_to_graph_truth() {
        let edges = [(0, 1), (0, 2), (0, 3)];
        let visible = mask(4, &[0, 1, 2, 3]);
        let live = mask(3, &[0]);
        assert_eq!(compare(4, &edges, &visible, &live, 20, 0),
            BTreeSet::from([1]));
        let no_edges = mask(3, &[]);
        assert!(compare(4, &edges, &visible, &no_edges, 21, 0).is_empty());
    }

    #[test]
    fn invalid_mask_tail_and_unaddressable_edge_are_refused() {
        let sources = [0u32];
        let targets = [3u32]; // two node rows; invalid
        let vis = mask(2, &[0, 1]);
        let live = mask(1, &[0]);
        let snapshot = Snapshot {
            witness_version: 1, node_rows: 2, visible_nodes: &vis, live_edges: &live,
        };
        assert_eq!(one_hop(
            EdgeColumns { sources: &sources, targets: &targets },
            snapshot, 0, 1,
        ), Err(HopError::InvalidShape));
        let invalid_vis = [u64::MAX];
        let snapshot = Snapshot { visible_nodes: &invalid_vis, ..snapshot };
        assert_eq!(one_hop(
            EdgeColumns { sources: &sources, targets: &targets },
            snapshot, 0, 1,
        ), Err(HopError::InvalidShape));
    }
}
