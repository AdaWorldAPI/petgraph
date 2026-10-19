# Petgraph fork: direct lance-graph fold experiment

This is **code in the petgraph fork**, not a proposed `GraphFoldView` adapter.
It is deliberately an isolated Cargo workspace so stock petgraph and its
no_std `petgraph-core` build cannot acquire lance-graph as a dependency.

Run:

```bash
cargo test --manifest-path experiments/lance-fold-hop/Cargo.toml --features lance
```

The optional dependency is pinned to the specific `lance-graph` commit that introduces
`lance-graph-mask-risc::OutgoingHop`. The petgraph-side lowering has now been
replaced with a small wrapper over that upstream kernel.
The upstream kernel directly passes borrowed SoA edge columns and resident
snapshot masks into `lance-graph-mask-risc`:

1. `Gather(source, frontier)` gated by edge presence;
2. `Gather(target, visible)` gated by the surviving edges;
3. `ScatterOrU32(target)` to the explicitly requested one-hop node mask.

The oracle is petgraph's real `Bfs` advancing one step over a separately
constructed graph **in tests only**. The runtime fold path does not create
a petgraph `Graph`, neighbor vectors, an adjacency list, or a graph-trait
adapter. Seven differential/security tests cover multi-seed frontiers, multiple mask words, duplicate
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

Build with the fork's pinned **Rust/Cargo 1.99.0** toolchain; its optional
experiment declares `rust-version = "1.99"`. The upstream petgraph workspace
still advertises **MSRV 1.85**, since no upstream-compatible API needs a bump.
Do not run blanket `cargo upgrade`: the fork has no tracked root Cargo.lock,
and this experiment deliberately pins the lance-graph commit. Dependency
updates must be reviewed separately from a toolchain upgrade.

This change has **not** been compiled or benchmarked by the authoring agent;
CI and local Rust toolchains are required for validation.
