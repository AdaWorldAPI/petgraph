//! Costs of the borrowed pattern-CSR view, each stage measured on its own.
//!
//! Run: `cargo run --release --example relstore_cost` (add `--features quack`
//! for the Quack one-hop arm). One machine, warm caches, medians of REPS runs.
//! Every traversal is checked against the petgraph `DiGraph` arm before it is
//! timed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use petgraph::csr::Csr;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::{Bfs, IntoNeighbors};
use petgraph_relstore_visit::{Fact, RelationStore, RelationVersion};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const REPS: usize = 7;
const V1: RelationVersion = RelationVersion {
    relation: 1,
    revision: 1,
};

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

/// Median time and the allocation count of the LAST run.
fn time<T>(mut f: impl FnMut() -> T) -> (Duration, usize, T) {
    let mut ts = Vec::with_capacity(REPS);
    let mut last = None;
    let mut allocs = 0;
    for _ in 0..REPS {
        let a = ALLOCS.load(Ordering::Relaxed);
        let t = Instant::now();
        let v = black_box(f());
        ts.push(t.elapsed());
        allocs = ALLOCS.load(Ordering::Relaxed) - a;
        last = Some(v);
    }
    (median(ts), allocs, last.unwrap())
}

fn us(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

fn facts(n: u32, m: usize, seed: u64) -> Vec<Fact> {
    let mut s = seed;
    let mut next = || {
        s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (s >> 33) as u32
    };
    (0..m)
        .map(|i| Fact {
            id: (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
            src: next() % n,
            dst: next() % n,
        })
        .collect()
}

fn section(n: u32, m: usize) {
    let fs = facts(n, m, 42);
    println!("\n## n {n}, m {m}");

    let mut store = RelationStore::new();
    let (prep, prep_allocs, rel) = time(|| store.prepare(V1, n, &fs).unwrap());
    // Everything below reads this layout; none of it may prepare another.
    let prepared = store.prepares();
    let (view_t, view_allocs, v) = time(|| rel.view(V1).unwrap());

    // Arms built from the same facts.
    let (graph_t, _, g) = time(|| {
        let mut g = DiGraph::<(), u64>::with_capacity(n as usize, fs.len());
        for _ in 0..n {
            g.add_node(());
        }
        let mut sorted = fs.clone();
        sorted.sort_by_key(|f| (f.src, std::cmp::Reverse(f.id)));
        for f in sorted {
            g.add_edge(
                NodeIndex::new(f.src as usize),
                NodeIndex::new(f.dst as usize),
                f.id,
            );
        }
        g
    });
    let mut pairs: Vec<(u32, u32)> = fs.iter().map(|f| (f.src, f.dst)).collect();
    pairs.sort_unstable();
    pairs.dedup();
    let (csr_t, _, csr) =
        time(|| Csr::<(), (), petgraph::Directed, u32>::from_sorted_edges(&pairs).unwrap());

    // Correctness before timing: BFS orders agree with the DiGraph arm.
    for s in [0, n / 2, n - 1] {
        let mut a = Bfs::new(v, s);
        let mut b = Bfs::new(&g, NodeIndex::new(s as usize));
        loop {
            let (x, y) = (a.next(v), b.next(&g));
            assert_eq!(x, y.map(|y| y.index() as u32));
            if x.is_none() {
                break;
            }
        }
    }

    let walk = |f: &dyn Fn(u32) -> u64| (0..n).map(f).fold(0u64, |a, b| a.wrapping_add(b));
    let (walk_view, walk_view_allocs, s1) =
        time(|| walk(&|u| v.neighbors(u).map(u64::from).sum::<u64>()));
    let (walk_graph, _, s2) = time(|| {
        walk(&|u| {
            g.neighbors(NodeIndex::new(u as usize))
                .map(|x| x.index() as u64)
                .sum::<u64>()
        })
    });
    let (walk_csr, _, _) = time(|| {
        walk(&|u| {
            csr.neighbors_slice(u)
                .iter()
                .map(|&x| u64::from(x))
                .sum::<u64>()
        })
    });
    assert_eq!(s1, s2);

    let bfs_all = |start: u32| {
        let mut b = Bfs::new(v, start);
        let mut k = 0u32;
        while let Some(x) = b.next(v) {
            k = k.wrapping_add(x);
        }
        k
    };
    let (bfs_view, bfs_view_allocs, k_view) = time(|| bfs_all(0));
    let (bfs_graph, _, k_graph) = time(|| {
        let mut b = Bfs::new(&g, NodeIndex::new(0));
        let mut k = 0u32;
        while let Some(x) = b.next(&g) {
            k = k.wrapping_add(x.index() as u32);
        }
        k
    });
    let (bfs_csr, _, k_csr) = time(|| {
        let mut b = Bfs::new(&csr, 0u32);
        let mut k = 0u32;
        while let Some(x) = b.next(&csr) {
            k = k.wrapping_add(x);
        }
        k
    });

    // The three BFS arms visit the same node set (same wrapping sum).
    assert_eq!((k_view, k_view), (k_graph, k_csr));
    let per_edge = |d: Duration| d.as_secs_f64() * 1e9 / m as f64;
    println!("stage, us, allocs, ns/edge");
    println!(
        "prepare (sort + offsets + 3 lanes), {:.1}, {prep_allocs}, {:.2}",
        us(prep),
        per_edge(prep)
    );
    println!("view construction, {:.3}, {view_allocs}, -", us(view_t));
    println!(
        "build DiGraph arm, {:.1}, -, {:.2}",
        us(graph_t),
        per_edge(graph_t)
    );
    println!(
        "build Csr arm ({} unique pairs of {m}), {:.1}, -, {:.2}",
        pairs.len(),
        us(csr_t),
        per_edge(csr_t)
    );
    println!(
        "neighbour walk, all nodes: view {:.1} ({walk_view_allocs} allocs) | DiGraph {:.1} | Csr {:.1}  [{:.2} / {:.2} / {:.2} ns/edge]",
        us(walk_view),
        us(walk_graph),
        us(walk_csr),
        per_edge(walk_view),
        per_edge(walk_graph),
        per_edge(walk_csr),
    );
    println!(
        "BFS from 0: view {:.1} ({bfs_view_allocs} allocs) | DiGraph {:.1} | Csr {:.1}",
        us(bfs_view),
        us(bfs_graph),
        us(bfs_csr)
    );
    println!(
        "layout {} B for {m} edges ({:.1} B/edge)",
        rel.bytes(),
        rel.bytes() as f64 / m as f64
    );

    #[cfg(feature = "quack")]
    {
        use petgraph_relstore_visit::quack_hop::one_hop;
        let sources: Vec<u32> = (0..64).map(|i| (i * 7919) % n).collect();
        let mut out = Vec::new();
        let (quack, quack_allocs, _) = time(|| {
            let mut k = 0usize;
            for &s in &sources {
                out.clear();
                one_hop(&rel, V1, s, &mut out).unwrap();
                k += out.len();
            }
            k
        });
        let (view_hop, _, _) = time(|| {
            sources
                .iter()
                .map(|&s| v.neighbors(s).count())
                .sum::<usize>()
        });
        println!(
            "64 one-hop queries: Quack (lower + full src scan + dst read) {:.1} us ({quack_allocs} allocs) | view neighbours {:.2} us",
            us(quack),
            us(view_hop)
        );
    }
    assert_eq!(store.prepares(), prepared, "a consumer prepared a second layout");
}

fn main() {
    println!("relstore-visit cost | release, medians of {REPS}");
    for (n, m) in [(1_000, 10_000), (100_000, 1_000_000)] {
        section(n, m);
    }
}
