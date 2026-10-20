//! D-RST-3: a borrowed, version-pinned pattern-CSR view over a prepared
//! relationship layout, traversed by petgraph's own `Bfs`/`Dfs` unchanged.
//!
//! Three pieces, each with one owner:
//!
//! - [`PreparedRelation`] owns the buffers: `u32` row offsets, `u32`
//!   neighbour slots, `u64` stable fact ids and the `u32` source lane a
//!   compiled scan reads. It stands in for a layout the relationship store
//!   prepares; it is built once and counted ([`RelationStore::prepares`]).
//! - [`PatternCsr`] borrows those buffers and implements petgraph's visit
//!   traits. It owns nothing, copies nothing, and refuses a version that is
//!   not the one the buffers were prepared from.
//! - Nothing here evaluates a query. The view only answers "the neighbours
//!   of `u`", which petgraph's existing algorithms compose.
//!
//! A pattern CSR carries structure only. BlasGraph's `CsrStorage` keeps a
//! 2 KiB `BitVec` per entry; this view has no value slot at all, and
//! [`RelationStore::prepare_from_blasgraph_parts`] takes the two index arrays
//! only, so a value buffer cannot be passed to it.
//!
//! The edge id is the stable fact id, never the physical row. A row offset is
//! reported separately by [`FactEdge::row`] and changes when the layout is
//! rebuilt; the fact id does not.

use std::fmt;

use fixedbitset::FixedBitSet;
use petgraph::visit::{
    Data, EdgeRef, GraphBase, GraphRef, IntoEdgeReferences, IntoEdges, IntoNeighbors,
    NodeCompactIndexable, NodeCount, NodeIndexable, Visitable,
};

/// The version a buffer was prepared from: which relation, at which revision.
/// Stands in for the store's `RelationVersion` (D-RST plan §2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RelationVersion {
    pub relation: u32,
    pub revision: u64,
}

/// One relationship fact as the store holds it: a stable id and its endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fact {
    pub id: u64,
    pub src: u32,
    pub dst: u32,
}

/// Why a layout or a view was refused. Every refusal names what it saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// An endpoint outside `0..n_nodes`. Never dropped silently.
    OutOfDomain {
        fact: u64,
        src: u32,
        dst: u32,
        n_nodes: u32,
    },
    /// Two facts with the same id.
    DuplicateFact { fact: u64 },
    /// The view was asked for a version the buffers were not prepared from.
    Stale {
        prepared: RelationVersion,
        asked: RelationVersion,
    },
    /// More edges or nodes than a `u32` slot can address.
    TooLarge { what: &'static str, len: usize },
    /// BlasGraph parts that do not describe a square pattern CSR.
    MalformedCsr { reason: &'static str },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Refusal {}

/// The prepared layout: CSR offsets and slots sorted by `(src, fact id)`,
/// with the source lane and the fact ids at the same row coordinates.
#[derive(Debug)]
pub struct PreparedRelation {
    version: RelationVersion,
    n_nodes: u32,
    offsets: Vec<u32>,
    src: Vec<u32>,
    dst: Vec<u32>,
    fact: Vec<u64>,
}

impl PreparedRelation {
    pub fn version(&self) -> RelationVersion {
        self.version
    }

    pub fn n_nodes(&self) -> u32 {
        self.n_nodes
    }

    pub fn n_edges(&self) -> usize {
        self.dst.len()
    }

    /// The buffer a compiled scan reads for the source column.
    pub fn src_lane(&self) -> &[u32] {
        &self.src
    }

    /// The neighbour slots. The view and a compiled scan both read this.
    pub fn dst_lane(&self) -> &[u32] {
        &self.dst
    }

    pub fn fact_lane(&self) -> &[u64] {
        &self.fact
    }

    pub fn offsets(&self) -> &[u32] {
        &self.offsets
    }

    /// Bytes the layout holds (the four buffers' lengths, not capacities).
    pub fn bytes(&self) -> usize {
        4 * (self.offsets.len() + self.src.len() + self.dst.len()) + 8 * self.fact.len()
    }

    /// A view pinned to `asked`. Refused when the buffers came from another
    /// version, so a caller holding an old pin cannot read a new layout.
    pub fn view(&self, asked: RelationVersion) -> Result<PatternCsr<'_>, Refusal> {
        if asked != self.version {
            return Err(Refusal::Stale {
                prepared: self.version,
                asked,
            });
        }
        Ok(PatternCsr {
            version: self.version,
            offsets: &self.offsets,
            dst: &self.dst,
            fact: &self.fact,
        })
    }
}

/// Stands in for the relationship store's layout preparation. The counter is
/// how a test proves that a consumer did not rebuild the adjacency.
#[derive(Debug, Default)]
pub struct RelationStore {
    prepares: usize,
}

impl RelationStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many layouts this store has prepared.
    pub fn prepares(&self) -> usize {
        self.prepares
    }

    /// Prepare the pattern CSR for `facts` over the node domain `0..n_nodes`.
    ///
    /// Rows are ordered by `(src, fact id)`, so the layout depends on the
    /// facts and not on the order they arrived in: a compaction that reorders
    /// the input rebuilds the same layout, and every fact keeps its id.
    pub fn prepare(
        &mut self,
        version: RelationVersion,
        n_nodes: u32,
        facts: &[Fact],
    ) -> Result<PreparedRelation, Refusal> {
        if facts.len() >= u32::MAX as usize {
            return Err(Refusal::TooLarge {
                what: "edges",
                len: facts.len(),
            });
        }
        for f in facts {
            if f.src >= n_nodes || f.dst >= n_nodes {
                return Err(Refusal::OutOfDomain {
                    fact: f.id,
                    src: f.src,
                    dst: f.dst,
                    n_nodes,
                });
            }
        }
        let mut order: Vec<u32> = (0..facts.len() as u32).collect();
        order.sort_unstable_by_key(|&i| {
            let f = facts[i as usize];
            (f.src, f.id)
        });
        let mut ids: Vec<u64> = facts.iter().map(|f| f.id).collect();
        ids.sort_unstable();
        if let Some(w) = ids.windows(2).find(|w| w[0] == w[1]) {
            return Err(Refusal::DuplicateFact { fact: w[0] });
        }

        let mut offsets = vec![0u32; n_nodes as usize + 1];
        for f in facts {
            offsets[f.src as usize + 1] += 1;
        }
        for i in 0..n_nodes as usize {
            offsets[i + 1] += offsets[i];
        }
        let mut src = Vec::with_capacity(facts.len());
        let mut dst = Vec::with_capacity(facts.len());
        let mut fact = Vec::with_capacity(facts.len());
        for &i in &order {
            let f = facts[i as usize];
            src.push(f.src);
            dst.push(f.dst);
            fact.push(f.id);
        }
        self.prepares += 1;
        Ok(PreparedRelation {
            version,
            n_nodes,
            offsets,
            src,
            dst,
            fact,
        })
    }

    /// Prepare from BlasGraph `CsrStorage`'s index arrays, in its own
    /// vocabulary (`nrows`, `ncols`, `row_ptrs`, `col_indices`).
    ///
    /// The values array is not a parameter: a pattern CSR has no use for the
    /// 2 KiB `BitVec`s, and taking them would invite copying them. BlasGraph
    /// has no stable fact ids, so the store supplies them, one per entry.
    /// The arrays are `usize` there and `u32` here, so this is a preparation
    /// (counted), not a borrow.
    pub fn prepare_from_blasgraph_parts(
        &mut self,
        version: RelationVersion,
        nrows: usize,
        ncols: usize,
        row_ptrs: &[usize],
        col_indices: &[usize],
        fact_ids: &[u64],
    ) -> Result<PreparedRelation, Refusal> {
        if nrows != ncols {
            return Err(Refusal::MalformedCsr {
                reason: "a traversal relation needs one node domain (nrows == ncols)",
            });
        }
        if nrows >= u32::MAX as usize {
            return Err(Refusal::TooLarge {
                what: "nodes",
                len: nrows,
            });
        }
        if row_ptrs.len() != nrows + 1 {
            return Err(Refusal::MalformedCsr {
                reason: "row_ptrs.len() != nrows + 1",
            });
        }
        if fact_ids.len() != col_indices.len() {
            return Err(Refusal::MalformedCsr {
                reason: "one fact id per entry",
            });
        }
        if row_ptrs[0] != 0
            || row_ptrs[nrows] != col_indices.len()
            || row_ptrs.windows(2).any(|w| w[0] > w[1])
        {
            return Err(Refusal::MalformedCsr {
                reason: "row_ptrs must start at 0, be non-decreasing and end at nnz",
            });
        }
        let mut facts = Vec::with_capacity(col_indices.len());
        for r in 0..nrows {
            for k in row_ptrs[r]..row_ptrs[r + 1] {
                let c = col_indices[k];
                facts.push(Fact {
                    id: fact_ids[k],
                    src: r as u32,
                    dst: u32::try_from(c).unwrap_or(u32::MAX),
                });
            }
        }
        self.prepare(version, nrows as u32, &facts)
    }
}

/// A borrowed pattern CSR, pinned to one relationship version.
///
/// `Copy`, so it is its own `GraphRef`: `Bfs::new(view, start)` works on it
/// directly. Holds three slices and a version; constructing one allocates
/// nothing.
#[derive(Clone, Copy, Debug)]
pub struct PatternCsr<'a> {
    version: RelationVersion,
    offsets: &'a [u32],
    dst: &'a [u32],
    fact: &'a [u64],
}

impl<'a> PatternCsr<'a> {
    pub fn version(&self) -> RelationVersion {
        self.version
    }

    pub fn n_nodes(&self) -> u32 {
        (self.offsets.len() - 1) as u32
    }

    pub fn n_edges(&self) -> usize {
        self.dst.len()
    }

    /// The physical rows of `u`'s out-edges.
    pub fn rows(&self, u: u32) -> std::ops::Range<usize> {
        self.offsets[u as usize] as usize..self.offsets[u as usize + 1] as usize
    }

    /// The neighbour slots of `u`, as a sub-slice of the prepared buffer.
    pub fn neighbour_slice(&self, u: u32) -> &'a [u32] {
        &self.dst[self.rows(u)]
    }

    /// Start pointers of the borrowed buffers, for identity checks.
    pub fn buffer_ptrs(&self) -> (*const u32, *const u32, *const u64) {
        (self.offsets.as_ptr(), self.dst.as_ptr(), self.fact.as_ptr())
    }
}

/// One edge: its endpoints, its stable fact id, and the physical row it
/// occupies in this layout. Only the fact id survives a rebuild.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FactEdge<'a> {
    source: u32,
    target: u32,
    fact: &'a u64,
    row: u32,
}

impl FactEdge<'_> {
    /// The physical row. Changes when the layout is rebuilt.
    pub fn row(&self) -> u32 {
        self.row
    }
}

impl<'a> EdgeRef for FactEdge<'a> {
    type NodeId = u32;
    type EdgeId = u64;
    type Weight = u64;

    fn source(&self) -> u32 {
        self.source
    }

    fn target(&self) -> u32 {
        self.target
    }

    /// The weight of a pattern edge is its fact id: there is no other value.
    fn weight(&self) -> &u64 {
        self.fact
    }

    fn id(&self) -> u64 {
        *self.fact
    }
}

impl GraphBase for PatternCsr<'_> {
    type NodeId = u32;
    type EdgeId = u64;
}

/// The view is `Copy`, so it is its own graph reference.
impl GraphRef for PatternCsr<'_> {}

impl<'a> IntoNeighbors for PatternCsr<'a> {
    type Neighbors = std::iter::Copied<std::slice::Iter<'a, u32>>;

    fn neighbors(self, a: u32) -> Self::Neighbors {
        self.neighbour_slice(a).iter().copied()
    }
}

impl Visitable for PatternCsr<'_> {
    type Map = FixedBitSet;

    fn visit_map(&self) -> FixedBitSet {
        FixedBitSet::with_capacity(self.n_nodes() as usize)
    }

    fn reset_map(&self, map: &mut FixedBitSet) {
        map.clear();
        map.grow(self.n_nodes() as usize);
    }
}

impl NodeIndexable for PatternCsr<'_> {
    fn node_bound(&self) -> usize {
        self.n_nodes() as usize
    }

    fn to_index(&self, a: u32) -> usize {
        a as usize
    }

    fn from_index(&self, i: usize) -> u32 {
        i as u32
    }
}

impl NodeCount for PatternCsr<'_> {
    fn node_count(&self) -> usize {
        self.n_nodes() as usize
    }
}

impl NodeCompactIndexable for PatternCsr<'_> {}

impl Data for PatternCsr<'_> {
    type NodeWeight = ();
    type EdgeWeight = u64;
}

/// Every edge in row order.
#[derive(Clone, Debug)]
pub struct AllEdges<'a> {
    view: PatternCsr<'a>,
    source: u32,
    row: usize,
}

impl<'a> Iterator for AllEdges<'a> {
    type Item = FactEdge<'a>;

    fn next(&mut self) -> Option<FactEdge<'a>> {
        if self.row >= self.view.dst.len() {
            return None;
        }
        while self.view.offsets[self.source as usize + 1] as usize <= self.row {
            self.source += 1;
        }
        let row = self.row;
        self.row += 1;
        Some(FactEdge {
            source: self.source,
            target: self.view.dst[row],
            fact: &self.view.fact[row],
            row: row as u32,
        })
    }
}

/// The out-edges of one node.
#[derive(Clone, Debug)]
pub struct OutEdges<'a> {
    view: PatternCsr<'a>,
    source: u32,
    rows: std::ops::Range<usize>,
}

impl<'a> Iterator for OutEdges<'a> {
    type Item = FactEdge<'a>;

    fn next(&mut self) -> Option<FactEdge<'a>> {
        let row = self.rows.next()?;
        Some(FactEdge {
            source: self.source,
            target: self.view.dst[row],
            fact: &self.view.fact[row],
            row: row as u32,
        })
    }
}

impl<'a> IntoEdgeReferences for PatternCsr<'a> {
    type EdgeRef = FactEdge<'a>;
    type EdgeReferences = AllEdges<'a>;

    fn edge_references(self) -> AllEdges<'a> {
        AllEdges {
            view: self,
            source: 0,
            row: 0,
        }
    }
}

impl<'a> IntoEdges for PatternCsr<'a> {
    type Edges = OutEdges<'a>;

    fn edges(self, a: u32) -> OutEdges<'a> {
        OutEdges {
            view: self,
            source: a,
            rows: self.rows(a),
        }
    }
}

#[cfg(feature = "quack")]
pub mod quack_hop;
