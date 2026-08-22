---
status: accepted
date: 2026-08-13
decision-makers: Graph Storage design review
---

# ADR-0004: Graph analytics runs in-process in Rust, replacing NetworkX without numeric parity

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A. Python analytics sidecar with NetworkX](#a-python-analytics-sidecar-with-networkx)
  - [B. All metrics in SQL](#b-all-metrics-in-sql)
  - [C. In-process Rust analytics with explicit determinism contracts](#c-in-process-rust-analytics-with-explicit-determinism-contracts)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-graph-storage-adr-analytics-in-rust`

## Context and Problem Statement

The prototype computed whole-graph metrics — degree, PageRank, betweenness centrality, Louvain communities, connected components — with NetworkX, loading the full graph into a Python in-memory structure. NetworkX has no Rust equivalent with identical algorithms, RNG behavior, and tie-breaking. The gear must decide how to deliver graph analytics in Rust: which library strategy, which parity promises, and where computation runs.

## Decision Drivers

- `cpt-cf-graph-storage-fr-graph-metrics` (degree, PageRank, components) and `cpt-cf-graph-storage-fr-graph-analytics-extended` (betweenness, communities) define the required algorithm set with different priorities.
- `cpt-cf-graph-storage-nfr-analytics-memory` bounds analytics to a topology-only in-memory projection under a configurable node ceiling.
- NetworkX-exact outputs are unattainable for sampled betweenness (RNG-dependent sampling) and Louvain communities (RNG- and tie-break-dependent); any design promising parity would be dishonest.
- The prototype's consumers depend on stable community ordering (size, then smallest member) for stable UI coloring — an ordering convention, not a numeric parity requirement.
- Degree and components are trivially computable; PageRank is a short, well-specified iteration; Brandes betweenness (exact and sampled) is well documented; Louvain has Rust implementations but with different tie-breaking.
- Metrics are cached by graph revision (`cpt-cf-graph-storage-fr-metrics-cache`), so computation cost is paid once per graph change, not per read.

## Considered Options

- A. Keep a Python analytics sidecar running NetworkX
- B. Compute all metrics in SQL inside PostgreSQL
- C. In-process Rust analytics: petgraph-based topology projection with algorithm implementations selected per metric (library where suitable, in-house where the algorithm is small), explicit determinism contracts, no NetworkX parity promise

## Decision Outcome

Chosen option: "C. In-process Rust analytics with explicit determinism contracts", because every required algorithm is implementable or available in Rust at the required scale, the topology-only projection fits the platform's bounded-memory posture, and honest per-algorithm guarantees replace an unattainable parity promise. Per metric:

- **Degree** (total/in/out): computed from the edge topology (or SQL aggregation when cheaper).
- **Connected components**: union-find over the undirected topology.
- **PageRank**: power iteration over the directed topology with fixed damping, tolerance, and iteration cap — deterministic.
- **Betweenness centrality**: Brandes exact below a node threshold; above it, sampled Brandes over the canonicalized topology with a seeded, documented sampling scheme — deterministic for a fixed graph and configuration, not comparable to NetworkX's sampling.
- **Community detection**: Louvain-family algorithm with seeded initialization over the canonicalized topology; results stabilized by the prototype's ordering convention (communities sorted by size, then smallest member key) — stable across recomputation of the same graph, not identical to NetworkX partitions.

**Canonical input ordering** underpins every seeded guarantee: a seed alone does not make an algorithm repeatable when database row order, hash-map iteration, or adjacency layout varies between runs. The topology projection is therefore canonicalized before any seeded algorithm executes — nodes ordered by node key; edges by (type, source key, target key, discriminator); adjacency lists sorted by neighbor key; and every algorithmic tie-break defined on node keys. Determinism comes from ordered inputs plus the seed, never from incidental iteration order.

Each metric carries a **normative contract** covering the semantics that change outputs: edge multiplicity and self-loop treatment, direction handling, PageRank damping, dangling-node redistribution, convergence tolerance and iteration cap, Brandes normalization and endpoint inclusion, sampling rule above the exact threshold, and Louvain graph construction, weighting, and resolution. That contract carries an immutable `algorithm_contract_version`, which is part of the cache identity — lookup, single-flight coordination, publication, persisted rows, result provenance, and metric annotations all include it. The version is bumped whenever an output-affecting semantic, default, sampling rule, or implementation contract changes, and every version is covered by golden fixtures; an old cached result can therefore never be served under new semantics.

All metrics support edge-type exclusion, load only node keys and typed edges (never payloads), and are cached by graph revision, parameters, and contract version.

### Consequences

- The determinism contract of each metric (exact, seeded-deterministic, or ordering-stable) must be documented in the API schema so consumers do not assume cross-implementation comparability.
- Migrating data from the prototype means community assignments and sampled betweenness values will change once; consumers of the prototype must be told explicitly.
- The analytics component owns the node-ceiling enforcement and refuses oversized graphs with a clear error rather than degrading the whole gear.
- Choosing or replacing a Louvain crate (or implementing Leiden later) is contained inside the analytics component behind its own interface; the API contract names guarantees, not libraries.
- Analytics runs on the gear's runtime; long computations must be cooperatively cancellable and must not starve request handling.

### Confirmation

- Golden tests on fixed small graphs assert exact values for degree, components, and PageRank (within tolerance) and assert stability for betweenness and communities — including runs with deliberately shuffled input row order, which must produce identical outputs (canonical ordering, not luck, carries the determinism).
- Profiling tests verify the topology-only memory footprint and ceiling enforcement (`cpt-cf-graph-storage-nfr-analytics-memory`).
- API documentation review confirms each metric's determinism contract is stated.

## Pros and Cons of the Options

### A. Python analytics sidecar with NetworkX

Keep NetworkX behind an internal service boundary; the gear calls it for metrics.

- Good, because outputs match the prototype exactly.
- Good, because NetworkX's algorithm breadth stays available for future metrics.
- Bad, because it adds a second runtime, image, and deployment unit to every installation for five metrics.
- Bad, because the full graph crosses a process boundary on every recomputation.
- Bad, because platform posture (single Rust runtime, bounded memory, unified observability) breaks at the sidecar.

### B. All metrics in SQL

Express PageRank, betweenness, and communities as iterative SQL over the edge table.

- Good, because no topology ever leaves the database.
- Good, because degree in SQL is genuinely the cheapest option (and is retained where cheaper).
- Bad, because iterative algorithms in recursive SQL are notoriously hard to write, bound, and debug — betweenness and Louvain in SQL are research projects, not deliverables.
- Bad, because long-running analytical SQL competes with interactive search and traversal on the same database.

### C. In-process Rust analytics with explicit determinism contracts

Topology projection into petgraph-style structures; per-metric implementation choice; honest guarantees.

- Good, because it needs no new runtime or service and fits bounded-memory rules.
- Good, because revision-keyed caching amortizes computation to once per graph change.
- Good, because per-metric determinism contracts are truthful and testable.
- Neutral, because some algorithms (sampled Brandes, Louvain seeding) are in-house or crate-dependent code the team must own and review.
- Bad, because numeric continuity with the prototype breaks once at migration for RNG-dependent metrics.
- Bad, because future exotic metrics require Rust engineering rather than a NetworkX one-liner.

## More Information

The prototype's metric surface, its `exclude_edge_types` option (added because repository-membership edges dominated centrality), and its community-ordering convention are carried forward as behavioral requirements. The one-time result discontinuity at migration is the accepted cost of leaving NetworkX.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

- `cpt-cf-graph-storage-fr-graph-metrics` — algorithm and determinism choices for the core metric set
- `cpt-cf-graph-storage-fr-graph-analytics-extended` — betweenness and community strategy, parity waiver
- `cpt-cf-graph-storage-fr-metrics-cache` — revision-keyed caching amortizes in-process computation
- `cpt-cf-graph-storage-nfr-analytics-memory` — topology-only projection and node ceiling
