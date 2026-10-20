# relstore-visit (D-RST-3)

A borrowed, version-pinned pattern-CSR view that petgraph's own `Bfs` and
`Dfs` traverse unchanged. This is D-RST-3 of the lance-graph plan
`.claude/plans/relstore-quack-petgraph-traversal-v1.md` (#1622): the view only,
before the relationship store (D-RST-1/2) exists.

It is isolated like `experiments/lance-fold-hop` (its own `[workspace]`), and
it does not use the quarantined `OutgoingHop`. The default build does not
depend on lance-graph. The `quack` feature adds `lance-graph-quack` and
`lance-graph-mask-risc` at lance-graph main `fcd1f60e`.

```sh
cargo test --release                  # 9 tests
cargo test --release --features quack # + the shared-layout falsifier
cargo run --release --features quack --example relstore_cost
```

## What is here

| item | role |
|---|---|
| `RelationStore::prepare` | Stands in for the store's layout preparation. Builds `u32` offsets, `u32` neighbour slots, `u64` fact ids and the `u32` source lane, ordered by `(src, fact id)`. Counted by `prepares()`. |
| `prepare_from_blasgraph_parts` | The same layout from BlasGraph `CsrStorage`'s index arrays (`nrows`, `ncols`, `row_ptrs`, `col_indices`), in its vocabulary. There is no values parameter, so the 2 KiB `BitVec`s cannot be passed in. |
| `PreparedRelation::view(version)` | A `PatternCsr<'_>`: three borrowed slices and a version. Refused (`Stale`) for any other version. |
| `PatternCsr` | `GraphBase`, `GraphRef`, `IntoNeighbors`, `Visitable` (`FixedBitSet`), `NodeIndexable`, `NodeCount`, `NodeCompactIndexable`, `Data`, `IntoEdgeReferences`, `IntoEdges`. `NodeId = u32`, `EdgeId = u64` = the fact id. |
| `FactEdge::row()` | The physical row, reported apart from the fact id. |
| `quack_hop::one_hop` (feature `quack`) | `src == s` lowered by Quack, executed by mask-risc over the prepared `src` lane; neighbours read from the prepared `dst` lane at the kept rows. |

Refusals, never silent drops: `OutOfDomain { fact, src, dst, n_nodes }`,
`DuplicateFact`, `Stale { prepared, asked }`, `TooLarge`, `MalformedCsr`.

## Tests (`tests/visit.rs`)

The oracle is a `DiGraph` built from the facts, not from the layout. Each
source's edges are inserted in reverse `(src, fact id)` order, because
`DiGraph` yields neighbours in reverse insertion order; the view and the
oracle must then visit in exactly the same order.

- BFS and DFS orders and out-edge sets equal the oracle from every start:
  a hand fixture (parallel edges, a self-loop, an isolated node, a one-way
  edge) and three random relations (checked to contain parallels and loops).
- Empty relations, including zero nodes.
- An out-of-domain endpoint is refused with its values (both endpoints), and
  the last in-domain node is accepted. Refused prepares are not counted.
- A stale relation or revision is refused.
- Duplicate fact ids are refused.
- Reversed arrival order rebuilds the identical layout; after compaction
  (every third fact kept, new revision) every fact keeps its endpoints while
  most rows move.
- The view's buffer pointers equal the prepared buffers; every neighbour
  slice lies inside the prepared `dst` buffer; building a view and walking
  every neighbour and out-edge list allocate nothing (thread-local counting
  allocator).
- BlasGraph index arrays give the same layout as `prepare`; malformed parts
  are refused.
- `quack`: for every node, the Quack hop equals the view's neighbours, reads
  the same `dst` buffer at the same version, and the store prepared once.

Disable runs (each guard broken alone, its test red, then restored):

| guard broken | test that failed |
|---|---|
| destination domain check removed | `an_out_of_domain_endpoint_is_refused_never_dropped` |
| stale-version check removed | `a_view_is_refused_for_any_other_version` |
| duplicate-id check removed | `duplicate_fact_ids_are_refused` |
| rows ordered by `src` only (arrival order within a source) | `fact_ids_survive_reordering_and_compaction_rows_do_not` |
| view holds a copy of `dst` | `the_view_borrows_and_allocates_nothing` |
| edge id = physical row | `fact_ids_survive_reordering_and_compaction_rows_do_not` |
| Quack hop reads a copy of `dst` | `petgraph_and_quack_read_one_prepared_layout` |

## Measurements (`examples/relstore_cost.rs`)

Release, one machine, medians of 7; ranges over three runs. Random relation,
`m` facts over `n` nodes, parallels and loops included. Every BFS is checked
to visit the same node set in all three arms.

| stage | n 1k, m 10k | n 100k, m 1M |
|---|---|---|
| prepare (sort, offsets, 3 lanes) | 0.66–0.72 ms (6 allocs) | 308–396 ms (6 allocs) |
| view construction | 0.035 µs, 0 allocs | 0.035 µs, 0 allocs |
| neighbour walk, all nodes: view / Csr / DiGraph | 1.9 / 1.7 / 3.1 ns per edge | 2.0 / 1.8–1.9 / 5.5–7.2 ns per edge |
| BFS from 0: view / Csr / DiGraph | 29–47 / 32–48 / 48–64 µs | 6.8–12.9 / 7.2–14.8 / 42–51 ms |
| BFS allocations (view) | 10 | 17 |
| layout bytes | 16.4 B per edge | 16.4 B per edge |
| 64 one-hops: Quack / view | 134–137 µs / 0.3 µs | 14.0–18.2 ms / 0.3 µs |

What they say:

- The borrowed view costs the same as petgraph's `Csr` to walk and to
  traverse, and allocates only petgraph's traversal state. The first run
  showed the view 2.7× faster than `Csr` on BFS; repeat runs put them within
  noise of each other, so that number was noise.
- Preparation is the cost: 300–400 ns per edge at 1M, about 50× the `Csr`
  arm's build (the `Csr` arm is given pre-sorted, deduplicated pairs, so
  this is not like for like). It is an indirect sort plus a second sort for
  the duplicate-id check. This is measured, not optimized; per the plan, an
  optimization waits for a workload that prepares often.
- The Quack hop is correct and reads the same buffers, but it scans the
  whole `src` lane per query (O(E), ~3.5 ns per row) and lowers a program
  per query (3 allocations). The view answers in O(degree). The lane is
  sorted by `src`, so a range read would close the gap, but Quack cannot
  assume that without a sealed ordering witness (D-DIAMOND `Cmp::Range` via
  an ordered-lane witness). That is an admission question for D-RST-1/2.

## Limits

- BlasGraph is not a dependency (lance-graph core pulls lance, datafusion and
  lancedb). Its vocabulary is reused by signature; the parts constructor is a
  preparation (`usize` to `u32`), not a borrow.
- `RelationStore` and `RelationVersion` are stand-ins. The real version comes
  from the store's seal (`LayoutSeal.built_from`), which does not exist yet.
- Out-edges only. `IntoNeighborsDirected` needs a second (reverse) layout.
