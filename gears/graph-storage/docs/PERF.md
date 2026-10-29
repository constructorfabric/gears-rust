# Graph Storage — Performance Benchmark

How the gear's read latency is measured, how to run the measurement, and where a
run's results go. The baseline and every later run are in
[gears-rust#5517](https://github.com/constructorfabric/gears-rust/issues/5517).

The benchmark is the opt-in perf lane, `graph-storage/tests/perf.rs`. It answers
one question: how long the gear's own reads take on the reference graph of the
PRD's latency criteria, and how that changes when the database is further away.
It does not answer throughput under concurrency, larger graphs, many tenants or
the end-to-end REST path — see [What it does not measure](#what-it-does-not-measure).

## What is measured

**Graph.** 100 000 nodes and 500 000 edges in one tenant (`GEARS_GRAPH_PERF_SCALE`
scales both; thresholds are asserted only at 1.0). The generator is
deterministic, so two runs time the same graph; a tenth of the nodes are hubs,
because a traversal budget only matters where the graph is dense. Nodes are
embedded by the deterministic fake provider. The graph is seeded through
`ingest`, and the seeding's wall clock is printed — it is the cost of the lane,
not a criterion.

**Layer.** The store and engine ports, called directly under the same admission
bounds the service applies. REST, the PDP round trip and JSON serialization are
excluded: they belong to a deployment's budget.

**Scenarios**, all on the one seeded graph:

| Scenario | What it does | Criterion |
| --- | --- | --- |
| One round trip | `revision` read, one statement. A control: what the stand and the pool cost before the gear does any work; it also calibrates the added round-trip time | — |
| Hybrid search | lexical + vector arms, arm limit 50, query embedded outside the loop | `nfr-search-latency`: p95 ≤ 500 ms |
| Criteria table | filter on an indexed payload path, ordered, page of 50 | — |
| Typed traversal | depth 3 from 8 seeds over one edge type; frontier ≤ 1 000, edge budget 50 000 per hop | p95 ≤ 1 s |
| Neighbourhood | depth 3 from one node over all edge types, ≤ 1 000 nodes, then the nodes hydrated | `nfr-traversal-latency`: p95 ≤ 1 s |
| Neighbourhood + degrees | the same, with each reached node's degree, which ranks a hub's neighbours under the node budget | `nfr-traversal-latency`: p95 ≤ 1 s |

**Timing.** One warm-up call (the criteria say "warm indexes"), then 40 runs for
the controls and search, 30 for the hops. Nearest-rank p50 and p95.

**Repeats.** `GEARS_GRAPH_PERF_REPEAT=n` times every scenario n times on the same
graph, so a run reports its own spread. A laptop is a noisy host: report at
least 5.

**Added round-trip time.** `GEARS_GRAPH_PERF_RTT_MS=0,0.5,1,2` repeats the matrix
with that many milliseconds added to every packet the database container sends
(`netem`, applied from a `nicolaka/netshoot` container in the database
container's network namespace; Docker is the only host requirement). The p50
grows by about one added delay per sequential round trip, so the slope of p50
against the delay estimates how many round trips a request makes — the number
that decides latency once the database is not on the same host. The thresholds
are asserted only at `0`.

**Pool ping.** The lane connects with the pool's defaults, under which sqlx pings
the connection before every acquire, so every statement costs two round trips.
That is what a deployment gets unless its pool configuration sets
`test_before_acquire: false`. At +2 ms per round trip the ping costs +45% on
the neighbourhood and +80% on the criteria table (gears-rust#5517); next to the
database it costs 0–4%.

## Running it

From the repository root, on an otherwise idle machine. In Bash with
`pipefail`, so a failed run does not exit with `tee`'s success:

```bash
set -o pipefail
GEARS_GRAPH_PERF=1 \
GEARS_TEST_PG_GRAPH_IMAGE=pgvector/pgvector:0.8.7-pg18 \
GEARS_GRAPH_PERF_REPEAT=5 \
GEARS_GRAPH_PERF_RTT_MS=0,0.5,1,2 \
  cargo test --release -p cf-gears-graph-storage --test perf \
  -- --nocapture the_retrieval_scenarios 2>&1 | tee perf.log

python3 gears/graph-storage/scripts/perf_summary.py < perf.log
```

Seeding takes 13–18 minutes on a laptop; the matrix above adds a few minutes.
Without `GEARS_GRAPH_PERF_RTT_MS` (or with `0` alone) no container but the
database is started. A setting that does not parse or is out of range
(`GEARS_GRAPH_PERF_SCALE` 0.001–10, `_REPEAT` 1–20, `_RTT_MS` 0–100 each,
at least one) stops the run before it seeds, rather than falling back to a
default.

Before trusting a run:

- **Nothing else runs.** No build, no other test lane, no other database
  container under load. A run whose repeats disagree by more than ~10% was
  disturbed; run it again rather than take the median of a disturbed run.
- **The calibration holds.** The one-round-trip control at `+d` ms is about
  `0.5 + 2d` ms with the ping, `0.5 + d` without it. If not, the delay did not
  apply as asked.
- **Compare like with like.** Two runs are comparable on the same machine, the
  same image and the same pool settings. Absolute numbers differ between hosts
  and drift on one host over days; when a difference matters, run both commits
  back to back on the same stand.

## Reporting a run

Run it after a change to the store, the engine, the migrations or the pool
configuration, and before a release. Post a comment on
[gears-rust#5517](https://github.com/constructorfabric/gears-rust/issues/5517)
with:

1. the commit, and what changed since the previous run;
2. the stand: host, CPU, memory, database image, pool settings;
3. the seeding line and the output of `perf_summary.py`;
4. a sentence per scenario that moved more than the earlier run's min–max
   spread — a regression to explain, or an improvement to name.

## What it does not measure

Each is a question the lane cannot answer today and is tracked in
gears-rust#5517:

- throughput under concurrency — operations per second at a fixed p95 with
  several callers; every number here is one caller;
- a scale curve — the same scenarios at 0.1×, 1× and 10× the reference graph;
- a consumer-shaped query — resolve a node, one typed hop, filter and page the
  far nodes by a payload path;
- ingest throughput as a measured scenario (gears-rust#5040);
- many tenants in one database, a database on a separate host, and the request
  end to end through REST and the PDP.
