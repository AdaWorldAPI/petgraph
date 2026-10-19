# Petgraph fork: direct lance-graph fold experiment

This is **code in the petgraph fork**, not a proposed `GraphFoldView` adapter.
It is deliberately an isolated Cargo workspace so stock petgraph and its
no_std `petgraph-core` build cannot acquire lance-graph as a dependency.

Run:

```bash
cargo test --manifest-path experiments/lance-fold-hop/Cargo.toml --features lance
```

The optional dependency is pinned to a specific `lance-graph` git revision.
The standalone probe directly passes borrowed SoA edge columns and resident
snapshot masks to `lance-graph-mask-risc`:

1. `Gather(source, frontier)` gated by edge presence;
2. `Gather(target, visible)` gated by the surviving edges;
3. `ScatterOrU32(target)` to the explicitly requested one-hop node mask.

The oracle is petgraph's real `Bfs` advancing one step over a separately
constructed graph **in tests only**. The runtime fold path does not create
a petgraph `Graph`, neighbor vectors, an adjacency list, or a graph-trait
adapter. Six differential/security tests cover multiple mask words, duplicate
edges, invisible nodes, absent edges, distinct witness versions, unauthorized
seeds, invalid tails, and stale witnesses.

**Important limit:** this demonstrates exactly one demanded fold result.
`ScatterOrU32` is not a general-purpose intermediate under the substrate's
no-materialized-population doctrine. Iterative BFS and Gremlin `repeat`
need a reviewed phase-handoff / continuation primitive in lance-graph before
this experiment can be promoted to a multi-hop executor.

Next: run CI and use the observed gap to specify an upstream masked-frontier
continuation primitive. Do not introduce a second interpreter or speculative
`lance-graph-core` yet. Once Gremlin and petgraph both consume the same
primitive, extract the smaller core if it actually reduces complexity.

This branch has **not** been compiled or benchmarked by the authoring agent;
CI and local Rust toolchains are required for validation.
