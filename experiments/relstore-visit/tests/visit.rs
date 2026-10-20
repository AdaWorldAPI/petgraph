//! The borrowed pattern-CSR view against an independent petgraph oracle.
//!
//! The oracle is a `DiGraph` built straight from the facts, never from the
//! prepared layout. `DiGraph` yields a node's neighbours in reverse insertion
//! order, so each source's edges are inserted in reverse `(src, fact id)`
//! order: then petgraph's own `Bfs`/`Dfs` on the oracle and on the view must
//! visit nodes in exactly the same order.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::{Bfs, Dfs, EdgeRef, IntoEdgeReferences, IntoEdges, IntoNeighbors};
use petgraph_relstore_visit::{Fact, PatternCsr, Refusal, RelationStore, RelationVersion};

// ---------------------------------------------------------------- allocations

struct Counting;

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.with(|c| c.set(c.get() + 1));
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocs<T>(f: impl FnOnce() -> T) -> (usize, T) {
    let before = ALLOCS.with(Cell::get);
    let v = f();
    (ALLOCS.with(Cell::get) - before, v)
}

// ------------------------------------------------------------------ fixtures

const V1: RelationVersion = RelationVersion {
    relation: 7,
    revision: 1,
};

/// Directed edges, two parallel edges, a self-loop, an isolated node (5) and
/// an edge whose reverse is absent (3 -> 4).
fn small() -> (u32, Vec<Fact>) {
    let f = |id, src, dst| Fact { id, src, dst };
    (
        6,
        vec![
            f(40, 0, 1),
            f(10, 0, 1), // parallel to fact 40
            f(30, 0, 2),
            f(20, 1, 3),
            f(50, 2, 2), // self-loop
            f(60, 3, 4),
            f(70, 2, 0),
        ],
    )
}

/// `m` facts over `n` nodes from an LCG: parallels and self-loops included.
fn random(n: u32, m: usize, seed: u64) -> Vec<Fact> {
    let mut s = seed;
    let mut next = || {
        s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (s >> 33) as u32
    };
    (0..m)
        .map(|i| {
            let src = next() % n;
            // Skew: a third of the edges land in the first 16 nodes.
            let dst = if next() % 3 == 0 {
                next() % 16.min(n)
            } else {
                next() % n
            };
            Fact {
                id: 1_000_000 + (i as u64) * 7919 % 1_000_003,
                src,
                dst,
            }
        })
        .collect()
}

fn oracle(n: u32, facts: &[Fact]) -> DiGraph<(), u64> {
    let mut g = DiGraph::with_capacity(n as usize, facts.len());
    for _ in 0..n {
        g.add_node(());
    }
    let mut sorted = facts.to_vec();
    sorted.sort_by_key(|f| (f.src, std::cmp::Reverse(f.id)));
    for f in sorted {
        g.add_edge(
            NodeIndex::new(f.src as usize),
            NodeIndex::new(f.dst as usize),
            f.id,
        );
    }
    g
}

fn bfs_view(v: PatternCsr<'_>, s: u32) -> Vec<u32> {
    let mut b = Bfs::new(v, s);
    let mut out = Vec::new();
    while let Some(x) = b.next(v) {
        out.push(x);
    }
    out
}

fn dfs_view(v: PatternCsr<'_>, s: u32) -> Vec<u32> {
    let mut d = Dfs::new(v, s);
    let mut out = Vec::new();
    while let Some(x) = d.next(v) {
        out.push(x);
    }
    out
}

fn bfs_oracle(g: &DiGraph<(), u64>, s: u32) -> Vec<u32> {
    let mut b = Bfs::new(g, NodeIndex::new(s as usize));
    let mut out = Vec::new();
    while let Some(x) = b.next(g) {
        out.push(x.index() as u32);
    }
    out
}

fn dfs_oracle(g: &DiGraph<(), u64>, s: u32) -> Vec<u32> {
    let mut d = Dfs::new(g, NodeIndex::new(s as usize));
    let mut out = Vec::new();
    while let Some(x) = d.next(g) {
        out.push(x.index() as u32);
    }
    out
}

fn check_all_starts(n: u32, facts: &[Fact]) {
    let mut store = RelationStore::new();
    let rel = store.prepare(V1, n, facts).unwrap();
    let v = rel.view(V1).unwrap();
    let g = oracle(n, facts);
    for s in 0..n {
        assert_eq!(bfs_view(v, s), bfs_oracle(&g, s), "bfs from {s}");
        assert_eq!(dfs_view(v, s), dfs_oracle(&g, s), "dfs from {s}");
        let mut got: Vec<(u32, u64)> = v.edges(s).map(|e| (e.target(), e.id())).collect();
        let mut want: Vec<(u32, u64)> = g
            .edges(NodeIndex::new(s as usize))
            .map(|e| (e.target().index() as u32, *e.weight()))
            .collect();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want, "out-edges of {s}");
    }
    assert_eq!(store.prepares(), 1);
}

// --------------------------------------------------------------------- tests

#[test]
fn small_relation_traverses_like_the_digraph_oracle() {
    let (n, facts) = small();
    check_all_starts(n, &facts);

    let mut store = RelationStore::new();
    let rel = store.prepare(V1, n, &facts).unwrap();
    let v = rel.view(V1).unwrap();
    // Parallel edges are both kept; a source's edges are in fact-id order
    // (10 -> 1, 30 -> 2, 40 -> 1), not grouped by target.
    assert_eq!(v.neighbour_slice(0), &[1, 2, 1]);
    let ids: Vec<u64> = v.edges(0).map(|e| e.id()).collect();
    assert_eq!(ids, vec![10, 30, 40]);
    // The self-loop is an edge, and BFS visits its node once.
    assert!(v.neighbors(2).any(|x| x == 2));
    assert_eq!(bfs_view(v, 2), vec![2, 0, 1, 3, 4]);
    // Direction holds: 4 reaches nothing, and the isolated node only itself.
    assert_eq!(bfs_view(v, 4), vec![4]);
    assert_eq!(bfs_view(v, 5), vec![5]);
    assert_eq!(dfs_view(v, 5), vec![5]);
    assert_eq!(v.edge_references().count(), facts.len());
}

#[test]
fn empty_relations_traverse_to_the_start_only() {
    let mut store = RelationStore::new();
    let rel = store.prepare(V1, 4, &[]).unwrap();
    let v = rel.view(V1).unwrap();
    for s in 0..4 {
        assert_eq!(bfs_view(v, s), vec![s]);
        assert_eq!(dfs_view(v, s), vec![s]);
        assert_eq!(v.neighbors(s).count(), 0);
    }
    assert_eq!(v.edge_references().count(), 0);
    // No nodes at all is a valid, empty relation.
    let rel0 = store.prepare(V1, 0, &[]).unwrap();
    let v0 = rel0.view(V1).unwrap();
    assert_eq!(v0.n_nodes(), 0);
    assert_eq!(v0.edge_references().count(), 0);
}

#[test]
fn random_relations_traverse_like_the_digraph_oracle() {
    for (n, m, seed) in [(64, 300, 1), (500, 3000, 2), (1000, 800, 3)] {
        let facts = random(n, m, seed);
        // Not degenerate: parallels and self-loops both occur.
        let selfs = facts.iter().filter(|f| f.src == f.dst).count();
        let mut pairs: Vec<(u32, u32)> = facts.iter().map(|f| (f.src, f.dst)).collect();
        pairs.sort_unstable();
        let parallels = pairs.windows(2).filter(|w| w[0] == w[1]).count();
        assert!(
            selfs > 0 && parallels > 0,
            "n {n}: selfs {selfs} parallels {parallels}"
        );
        check_all_starts(n, &facts);
    }
}

#[test]
fn an_out_of_domain_endpoint_is_refused_never_dropped() {
    let (n, mut facts) = small();
    facts.push(Fact {
        id: 99,
        src: 1,
        dst: n,
    });
    let mut store = RelationStore::new();
    assert_eq!(
        store.prepare(V1, n, &facts).unwrap_err(),
        Refusal::OutOfDomain {
            fact: 99,
            src: 1,
            dst: n,
            n_nodes: n
        }
    );
    facts.last_mut().unwrap().src = n + 3;
    facts.last_mut().unwrap().dst = 0;
    assert!(matches!(
        store.prepare(V1, n, &facts),
        Err(Refusal::OutOfDomain { src, .. }) if src == n + 3
    ));
    // Silence twin: the last in-domain node is accepted.
    facts.last_mut().unwrap().src = n - 1;
    assert!(store.prepare(V1, n, &facts).is_ok());
    assert_eq!(store.prepares(), 1, "refused prepares are not counted");
}

#[test]
fn a_view_is_refused_for_any_other_version() {
    let (n, facts) = small();
    let mut store = RelationStore::new();
    let rel = store.prepare(V1, n, &facts).unwrap();
    for asked in [
        RelationVersion {
            relation: 7,
            revision: 2,
        },
        RelationVersion {
            relation: 8,
            revision: 1,
        },
    ] {
        assert_eq!(
            rel.view(asked).unwrap_err(),
            Refusal::Stale {
                prepared: V1,
                asked
            }
        );
    }
    assert_eq!(rel.view(V1).unwrap().version(), V1);
}

#[test]
fn duplicate_fact_ids_are_refused() {
    let (n, mut facts) = small();
    facts.push(Fact {
        id: 30,
        src: 4,
        dst: 5,
    });
    assert_eq!(
        RelationStore::new().prepare(V1, n, &facts).unwrap_err(),
        Refusal::DuplicateFact { fact: 30 }
    );
}

/// The edge id is the fact id. Rows are physical and move when the layout is
/// rebuilt; fact ids and their endpoints do not.
#[test]
fn fact_ids_survive_reordering_and_compaction_rows_do_not() {
    let n = 200;
    let facts = random(n, 1500, 9);
    let mut store = RelationStore::new();
    let a = store.prepare(V1, n, &facts).unwrap();

    // Same facts, reversed arrival order: the identical layout.
    let mut rev = facts.clone();
    rev.reverse();
    let b = store.prepare(V1, n, &rev).unwrap();
    assert_eq!(a.offsets(), b.offsets());
    assert_eq!(a.dst_lane(), b.dst_lane());
    assert_eq!(a.fact_lane(), b.fact_lane());

    // Compaction: drop every third fact and rebuild at a new revision.
    let v2 = RelationVersion {
        relation: 7,
        revision: 2,
    };
    let kept: Vec<Fact> = facts.iter().copied().step_by(3).collect();
    let c = store.prepare(v2, n, &kept).unwrap();
    let endpoints = |rel: &petgraph_relstore_visit::PreparedRelation, ver| {
        let v = rel.view(ver).unwrap();
        v.edge_references()
            .map(|e| (e.id(), (e.source(), e.target(), e.row())))
            .collect::<std::collections::HashMap<u64, (u32, u32, u32)>>()
    };
    let before = endpoints(&a, V1);
    let after = endpoints(&c, v2);
    let mut moved = 0;
    for f in &kept {
        let (s0, t0, r0) = before[&f.id];
        let (s1, t1, r1) = after[&f.id];
        assert_eq!((s0, t0), (s1, t1), "fact {} changed endpoints", f.id);
        assert_eq!((s1, t1), (f.src, f.dst));
        moved += usize::from(r0 != r1);
    }
    // Anti-vacuity: the compaction really moved rows.
    assert!(
        moved * 2 > kept.len(),
        "only {moved} of {} rows moved",
        kept.len()
    );
    assert_eq!(store.prepares(), 3);
}

/// The view borrows the prepared buffers: same start pointers, neighbour
/// slices inside the prepared `dst` buffer, and no allocation to build a view
/// or to walk every neighbour list.
#[test]
fn the_view_borrows_and_allocates_nothing() {
    let n = 500;
    let facts = random(n, 3000, 5);
    let mut store = RelationStore::new();
    let rel = store.prepare(V1, n, &facts).unwrap();

    let (a_view, v) = allocs(|| rel.view(V1).unwrap());
    assert_eq!(a_view, 0, "constructing a view allocated");
    let (off, dst, fact) = v.buffer_ptrs();
    assert_eq!(off, rel.offsets().as_ptr());
    assert_eq!(dst, rel.dst_lane().as_ptr());
    assert_eq!(fact, rel.fact_lane().as_ptr());

    let range = rel.dst_lane().as_ptr_range();
    let mut nonempty = 0;
    for u in 0..n {
        let s = v.neighbour_slice(u);
        if !s.is_empty() {
            nonempty += 1;
            assert!(range.contains(&s.as_ptr()), "neighbours of {u} not in dst");
        }
    }
    assert!(nonempty > n / 2);

    let (a_walk, sum) = allocs(|| {
        let mut sum = 0u64;
        for u in 0..n {
            for x in v.neighbors(u) {
                sum += u64::from(x);
            }
            for e in v.edges(u) {
                sum ^= e.id();
            }
        }
        sum
    });
    assert_eq!(a_walk, 0, "walking neighbour lists allocated");
    assert_ne!(sum, 0);

    // A traversal allocates only petgraph's own state (visit map + queue),
    // and never anything per edge.
    let (a_bfs, order) = allocs(|| bfs_view(v, 0));
    assert!(order.len() > 10);
    assert!(
        a_bfs < 40,
        "bfs allocated {a_bfs} times for {} nodes",
        order.len()
    );
    assert_eq!(store.prepares(), 1);
}

#[test]
fn blasgraph_index_arrays_prepare_the_same_layout() {
    // BlasGraph vocabulary: usize row_ptrs / col_indices, no values.
    let (n, facts) = small();
    let mut sorted = facts.clone();
    sorted.sort_by_key(|f| (f.src, f.id));
    let mut row_ptrs = vec![0usize; n as usize + 1];
    for f in &sorted {
        row_ptrs[f.src as usize + 1] += 1;
    }
    for i in 0..n as usize {
        row_ptrs[i + 1] += row_ptrs[i];
    }
    let col: Vec<usize> = sorted.iter().map(|f| f.dst as usize).collect();
    let ids: Vec<u64> = sorted.iter().map(|f| f.id).collect();

    let mut store = RelationStore::new();
    let a = store.prepare(V1, n, &facts).unwrap();
    let b = store
        .prepare_from_blasgraph_parts(V1, n as usize, n as usize, &row_ptrs, &col, &ids)
        .unwrap();
    assert_eq!(a.offsets(), b.offsets());
    assert_eq!(a.dst_lane(), b.dst_lane());
    assert_eq!(a.fact_lane(), b.fact_lane());

    let nu = n as usize;
    let mal = |r: Result<_, Refusal>| matches!(r, Err(Refusal::MalformedCsr { .. }));
    assert!(mal(store.prepare_from_blasgraph_parts(
        V1,
        nu,
        nu + 1,
        &row_ptrs,
        &col,
        &ids
    )));
    assert!(mal(store.prepare_from_blasgraph_parts(
        V1,
        nu,
        nu,
        &row_ptrs[1..],
        &col,
        &ids
    )));
    assert!(mal(store.prepare_from_blasgraph_parts(
        V1,
        nu,
        nu,
        &row_ptrs,
        &col,
        &ids[1..]
    )));
    let mut bad = row_ptrs.clone();
    bad.swap(1, 2);
    if bad.windows(2).any(|w| w[0] > w[1]) {
        assert!(mal(
            store.prepare_from_blasgraph_parts(V1, nu, nu, &bad, &col, &ids)
        ));
    }
    // A column past the domain is refused by the shared domain check.
    let mut wide = col.clone();
    wide[0] = nu;
    assert!(matches!(
        store.prepare_from_blasgraph_parts(V1, nu, nu, &row_ptrs, &wide, &ids),
        Err(Refusal::OutOfDomain { .. })
    ));
    assert_eq!(store.prepares(), 2);
}

/// The F2b-shaped falsifier: a petgraph iterator and a Quack-compiled hop
/// read the same prepared buffers at the same version, and neither rebuilds
/// the adjacency.
#[cfg(feature = "quack")]
#[test]
fn petgraph_and_quack_read_one_prepared_layout() {
    use petgraph_relstore_visit::quack_hop::{HopError, one_hop};

    let n = 300;
    let facts = random(n, 2000, 11);
    let mut store = RelationStore::new();
    let rel = store.prepare(V1, n, &facts).unwrap();
    let v = rel.view(V1).unwrap();

    let (mut empty, mut multi) = (0, 0);
    let mut got = Vec::new();
    for s in 0..n {
        got.clear();
        let rep = one_hop(&rel, V1, s, &mut got).unwrap();
        let want: Vec<u32> = v.neighbors(s).collect();
        assert_eq!(got, want, "one hop from {s}");
        assert_eq!(rep.version, v.version());
        assert_eq!(rep.dst_ptr, v.buffer_ptrs().1, "different dst buffer");
        assert_eq!(rep.src_ptr, rel.src_lane().as_ptr());
        assert_eq!(rep.kept_rows, want.len());
        empty += usize::from(want.is_empty());
        multi += usize::from(want.len() >= 2);
    }
    assert!(empty > 0 && multi > 0, "empty {empty} multi {multi}");
    assert_eq!(store.prepares(), 1, "a consumer rebuilt the adjacency");

    let v2 = RelationVersion {
        relation: 7,
        revision: 2,
    };
    assert!(matches!(
        one_hop(&rel, v2, 0, &mut got),
        Err(HopError::Refused(Refusal::Stale { .. }))
    ));
}
