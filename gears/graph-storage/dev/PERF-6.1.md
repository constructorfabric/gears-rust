# The § 6.1 numbers, and what they are evidence of

*(Working note. Not part of the published documentation set; the numbers it
records belong in the PR body and, if they hold on CI hardware, in DESIGN's
NFR allocation table.)*

## How they were produced

`graph-storage/tests/perf.rs`, an opt-in lane:

```
GEARS_GRAPH_PERF=1 GEARS_TEST_PG_GRAPH_IMAGE=pg19-pgvector-test:latest \
  cargo test --release -p cf-gears-graph-storage --test perf -- --nocapture --test-threads 1
```

It seeds one graph of the size the criteria name and asks it the four
retrieval questions of PRD § 9 criterion 3, then times a producer-sized
ingest separately. `GEARS_GRAPH_PERF_SCALE` runs a smaller graph; at any
scale but 1.0 the lane prints that its numbers bound nothing about the
reference graph and asserts no threshold, because a latency measured on a
tenth of the graph is not evidence about the graph.

**What is inside the measurement:** the store port and the engine port, with
the indexes, the scope predicates and the admission bounds a real request
carries. **What is outside:** the api-gateway, the PDP round trip, JSON
serialization, and query embedding -- the last excluded because
`nfr-search-latency` says so, the rest because they are the deployment's
budget and reporting them here would put someone else's milliseconds in this
gear's column.

**Release build.** A debug build measures the compiler's inlining decisions,
not the gear: the same four scenarios are 5-20x slower unoptimized, which is
the difference between passing and failing a 500 ms budget.

## The numbers

Run 2026-09-11, release build, PostgreSQL 19 + pgvector in a container on a
WSL2 developer machine (not the CI reference profile — see the caveat below).
Graph: 100 000 nodes, 500 000 edges, every node embedded by the deterministic
provider, warm indexes.

| scenario | criterion | measured | budget |
| --- | --- | --- | --- |
| hybrid narrowing (arm limit 50, query embedding excluded) | `nfr-search-latency` | p50 34.5 ms, **p95 41.2 ms**, max 41.7 ms | 500 ms |
| criteria table (filter on a declared payload path, page 50) | — | p50 3.5 ms, **p95 4.2 ms**, max 4.5 ms | — |
| bounded traversal, depth 3, edge-type filtered, 8 seeds | `nfr-traversal-latency` | p50 448 ms, **p95 511 ms**, max 541 ms | 1 s |
| depth-3 UI neighborhood, 1 000-node budget, hydrated | `nfr-traversal-latency` | p50 350 ms, **p95 396 ms**, max 409 ms | 1 s |
| 10 000 nodes + 20 000 edges, embedding excluded | `nfr-ingest-throughput` | **19.7 s** | 60 s |

All four retrieval scenarios and the ingest criterion pass. PRD § 9
criterion 3 is met on this hardware.

## Two things the run showed that no criterion asks about

**Edge ingest slows as the edge table grows.** Seeding the reference graph
took 810 s for 600 000 rows, and it was not linear: the first 50 000 edges
went in at roughly a thousand a second, the batches around the 450 000 mark
at under a hundred. The criterion's own batch is 20 000 edges on a fresh
tenant and lands in 19.7 s, so nothing published is at risk — but a producer
re-syncing a repository into an already-large graph is on the slow part of
that curve, and "10k + 20k in 60 s" says nothing about it. Worth a look
before anyone promises a bulk-import SLA: the likely candidates are the
per-batch endpoint resolution and the conflict handling on `edge_key`,
neither of which was written with a half-million-row table in mind.

**The traversal budget is mostly the third hop.** At depth 3 with a
1 000-node frontier the p95 is half the budget, and the seeded graph is
deliberately hub-heavy. A denser graph, or a frontier cap raised from 1 000,
would put it against the ceiling — which is what the cap is for, and which
is why `max_frontier` belongs in configuration rather than in a constant.

## Caveats worth carrying into the PR

- **Developer hardware, not the reference profile.** The criteria name a
  reference deployment configuration and the benchmark suite defines it; this
  ran on a laptop under WSL2 with the database in a container beside the test
  process. Treat the numbers as a floor on headroom, not as the certified
  result: they say the design is not off by an order of magnitude, which is
  what was in doubt.
- **The fake embedding provider.** Query embedding is excluded by the
  criterion, and ingest-side embedding is excluded from the throughput run,
  so the provider's own speed does not enter either number. It does mean the
  stored vectors are the fake's, which affects what HNSW has to traverse; a
  real model's vectors are not more expensive to search, but they are not
  these vectors either.
- **One arm at a time is not measured.** The hybrid number is the fused
  answer, which is the scenario. If a regression lands, the first question
  will be which arm, and the lane does not answer it yet.
