# Fork-first experiment: petgraph on lance-graph folds

**Status:** executable design contract, not yet a wired runtime. Fork: AdaWorldAPI/petgraph (petgraph 0.8.3, edition 2024, petgraph-core). This document intentionally starts in petgraph, not with a generic lance-graph adapter.

## Direction

1. Add an opt-in lance-graph-backed **algorithm execution path inside this fork**.
2. Run petgraph traversal and graph-algorithm tests against that path. Reuse the existing public petgraph API wherever possible; no parallel public graph facade.
3. Implement missing shared fold/frontier operations **inside lance-graph** when a real algorithm demands them. Gremlin traversal lowering and petgraph algorithms should meet at the same operations, not invoke one another.
4. Only after two or more consumers prove a primitive necessary, extract a minimal `lance-graph-core` from lance-graph. Do not move packages speculatively.

## Concrete first vertical slice

**Breadth-first reachability, scoped to one snapshot.**

- Fixture: directed graph with stable canonical node identities, multiple witness versions, and visibility/presence masks.
- Baseline: existing petgraph BFS on ordinary `Graph`.
- Fork path: a feature-gated lance-graph backend that advances a borrowed frontier via existing Mask-RISC/Quack/R2IL fold operations; use the canonical SoA adjacency/presence layout.
- Compare visited identities, edge direction, hop bounds, and scope isolation. Order is either pinned or compared as a set; never silently assume hash iteration order.
- Prove a private bridge cannot relay a public reachability path and an unauthorized seed is rejected before execution.
- Trace whether this needs a new primitive in lance-graph. If so, implement it there, with a differential test against the petgraph fixture.
- Bench allocations and copies; avoid materializing an entire second graph.

## Then grow the algorithm surface

- Gremlin `out/in/both`, `has`, `repeat` and petgraph BFS/DFS share a masked frontier and direction/edge-label selection.
- Dijkstra needs weighted scheduling; don't pretend an ordinary Boolean frontier suffices.
- SCC, centrality and PageRank need iterative state and convergence semantics; add only after BFS parity.
- ACT-R production eligibility and NAL evidence may use the same masks but are **not** equivalent to graph reachability.

## Dependency direction and build isolation

The petgraph fork may depend on lance-graph behind a non-default feature, but lance-graph must **not** depend on this fork along the same build path. Avoid a circular Cargo graph. Keep the fork's default tests and no_std surfaces intact; a standalone optional experiment crate in this repository is acceptable if petgraph's core build cannot tolerate the dependency. Pin exact git revisions for reproducibility.

Inspect current petgraph trait bounds and actual lance-graph fold signatures before changing them. Prefer a narrow algorithm backend inside the fork over an exported `GraphFoldView` adapter trait. Do not require petgraph users to adopt a second graph type to run the experiment.

## What would justify lance-graph-core?

An extracted primitive must:
- have at least two real users (e.g. petgraph + Gremlin);
- express borrowed, version-scoped, authorization-gated frontier expansion or a similarly reusable operation;
- own no duplicate state, evaluator or serialization layer;
- preserve 128-bit identity, version-as-witness and 64-thought same-order semantics;
- be independently testable and reduce dependency surface rather than merely rearrange crates.

**Stop rule:** if the first petgraph experiment requires materializing a duplicate graph, stop and fix the underlying lance-graph execution surface rather than accepting the copy as architecture.

## Next implementation PR

Add a feature-gated integration experiment to this fork for one bounded BFS fixture. Start with real API inventory and differential tests; do not claim the lance-graph path is operational until it compiles and executes.
