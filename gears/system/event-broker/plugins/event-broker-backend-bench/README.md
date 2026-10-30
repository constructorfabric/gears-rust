# Event-broker backend bench

Performance workloads for the storage backend contract (feature 0007, `event_broker_sdk::backend`), written once and run by every backend plugin against its own medium. The bench calls only what ingest and delivery call - `provision`, `append` under a fence, `read` by offset, `watch`, `resolve` - so the numbers are the backend's as the broker sees it, plus the bench's own client-side cost.

## Running

```bash
make bench-eb-memory                                  # memory backend, every scenario
make bench-eb-pg                                      # PostgreSQL in a testcontainers container (needs Docker)
EB_BENCH_POSTGRES_URL=postgres://u:p@host:5432/db make bench-eb-pg   # an existing server

make bench-eb-memory BENCH_ARGS="--quick"             # 5 s phases, 500k events per phase, 30 s soak: a smoke run
make bench-eb-pg BENCH_ARGS="--scenarios b2 --cells 'b2/batch-16p16c-*'"
```

The targets build with `cargo bench --no-run` (optimised), then run the plugin's `benches/backend.rs` inside a systemd user scope capped at `BENCH_MEMORY_MAX` (default `6G`, no swap), passing `BENCH_ARGS` through: a runaway run is killed instead of taking the machine with it. `MALLOC_ARENA_MAX=2` keeps glibc from spreading freed memory over per-thread arenas the next phase does not reuse. Override the wrapper with `BENCH_SCOPE=` (empty) where systemd user scopes are not available.

The memory backend keeps every event of a phase in the process, roughly 1.7-3.3 KiB of RSS per 512-byte event depending on the run, and glibc keeps freed memory in per-thread arenas that the next phase does not always reuse. A memory run at the default cap therefore does not fit in 6 GiB; use a lower `--max-events-per-phase` (the `--quick` default of 500 000 peaked at 1.9 GiB) or a larger `BENCH_MEMORY_MAX`. Against PostgreSQL the events live in the database and the bench process stays small. The markdown report goes to stdout, progress to stderr, and the JSON report to `target/eb-bench/<backend>.json`. The run exits non-zero if any cell failed its check.

The PostgreSQL bench is gated on the plugin's `integration` feature, like its other Docker-backed targets, and builds every instance with the plugin's default settings (only `url` and `schema` are set). Each run uses a fresh schema.

| Flag | Default | Meaning |
|---|---|---|
| `--scenarios` | `b1,b2,b3,b7` | Scenarios to run, comma separated |
| `--cells` | `*` | Comma-separated patterns over `<scenario>/<cell>`; `*` matches any run of characters, e.g. `b2/batch-*-read:async` |
| `--duration` | `20s` (`5s` quick) | Measured time of each phase |
| `--warmup` | `3s` (`1s` quick) | Unmeasured time before each phase's measurement |
| `--quick` | off | Short phases; flags given explicitly still win |
| `--payload-bytes` | `512` | Padding bytes in each event's payload |
| `--keys` | `10000` | Distinct partition keys writers draw from, grouped by the partition they hash to |
| `--out` | none (the make targets set it) | Where to write the JSON report |
| `--max-events-per-phase` | `2000000` (`500000` quick) | A phase ends at its duration or after this many events, whichever comes first; B3 prefills at most this many |
| `--soak-duration` | `10m` (`30s` quick) | Measured time of B7 |
| `--catchup-events` | `1000000` (`100000` quick) | Events B3 prefills |

## The model

**Topics.** Every phase builds a fresh backend instance and provisions a fresh topic with 16 partitions and a 24 h retention, so nothing a phase writes is removed under it and no phase sees another's events. Every instance is started before its phase. After it the bench stops the instance, waiting for its workers to end (5 s in good order, then at once), and drops the instance itself, so an in-process backend frees a phase's events before the next phase starts.

**Event cap.** A phase ends at `--duration` or once its writers have appended `--max-events-per-phase` events, whichever comes first. The cap bounds what an in-process backend holds (every event is kept: retention is not shortened to make room). Warmup may use a fifth of the budget and ends early once it has, so a fast backend always has a measured window. A capped phase is marked `(capped)` and its rates are taken over the shorter window, from the end of warmup to when the budget ran out; everything written is still checked.

**Writers** (`p`). Each writer holds its own fence key and a strictly increasing fence seq, and never has more than one append in flight: it sends the next append only after the previous one answered, as one ingest writer does. Each append carries 1 (`single`) or 100 (`batch`) events for one partition, as an ingest batch does: random keys among those that hash to it. A writer starts at its own partition and moves to the next one every append. A failed append is resent under the same fence up to 5 times; `Duplicate` to a resend counts as stored.

**Consumers** (`c`). Consumer `i` owns the partitions `p % c == i`: one consumer follows all 16, or each of 16 consumers owns one. Every consumer reads from the start and checks what it reads as it goes. Reads take at most 1 (`single`) or 1000 (`batch`) events.

- `read:sync` - polls `read(Wait::No)` round-robin over its partitions, sleeping 1 ms after a round that returned nothing.
- `read:async` - follows `watch()` (`support::poll_heads` when the backend answers `Watch::Poll`) and reads with `Wait::No` up to each head it sees.

**Push.** `push:sync` appends with `Flush::Sync`, `push:async` with `Flush::Async`. The memory and PostgreSQL plugins both answer `Heads` to either, so their `push:sync` and `push:async` cells are expected to match within noise.

**Phases.**

- `peak` - closed loop: every writer appends as fast as answers come back. Gives events/s and MiB/s.
- `latency` - open loop at 50 % of the rate the cell's peak sustained (written, or the lower of written and read when consumers follow). Sends are scheduled on a fixed grid per writer; a writer that falls behind sends at once and skips nothing, and every latency is measured from the **scheduled** send time, so time spent queued behind a slow append is counted (no coordinated omission).

**Correctness check**, at the end of every phase:

- consumers read every acked event, and in every partition each sequence is above the one before (gaps allowed, no repeat), up to the partition head (`resolve(Latest)`) once it covers every head an append reported;
- each writer's events of one key arrive in the order it wrote them;
- no append answered `Duplicate` on its first send (nothing is resent unless an append failed);
- no fence failed all its attempts.

A failed check marks the cell `FAIL` with the reasons and the run moves on.

## Scenarios

### B1 ingest append

Writers only: `{single,batch} x {1p,16p} x push:{sync,async}`, 8 cells, each a peak and a latency phase. Afterwards 16 readers sweep the topic back (`read:sync`, 1000 per read) for the check; the sweep is not timed.

Cell names: `single-1p-push:sync` ... `batch-16p-push:async`.

### B2 pipeline

Writers and consumers together, the full matrix: `{single,batch} x {1p1c,16p16c} x push:{sync,async} x read:{sync,async}`, 16 cells, each a peak and a latency phase. Consumers start before writers; once writers stop, consumers drain to the final heads (2 min at most) before the check.

Cell names: `single-1p1c-push:sync-read:sync` ... `batch-16p16c-push:async-read:async`.

### B3 catch-up

16 batching writers prefill `--catchup-events`, then `{1c,16c} x read:{1,1000}` consumers (`read:sync`) drain the topic from the start until they reach the heads or `--duration` passes. 4 cells: `1c-read:1`, `1c-read:1000`, `16c-read:1`, `16c-read:1000`. The rate is events read over the time to drain, or over `--duration` when capped (the check then verifies what was read, not that it is all).

### B7 soak

The `batch-16p16c-push:async-read:async` cell at 70 % of its peak for `--soak-duration`, or until `--max-events-per-phase` events: against a fast in-process backend the cap, not the duration, usually ends the soak. Its peak comes from B2 when B2 ran that cell in the same run; otherwise B7 measures it first (a `peak` row under B7). Latencies are bucketed per second of **read** time, so a stall shows in the second the stalled reads finish in, and each window reports p99 and max.

### Not yet built

B4 random history reads, B5 hot partition, B6 retries with expected `Duplicate`s, B8 many topics: see `openspec/changes/eb-backend-bench/tasks.md`.

## Reading the results

- **written ev/s** - events of appends that answered inside the measured window, per second. **read ev/s** - events consumers read inside the window. In a healthy pipeline they match; read below written means consumers fell behind.
- **MiB/s** - the headline rate (written; read for B3) times `--payload-bytes`. It counts the padding only, not the event envelope.
- **ack** - scheduled send to the append's answer, per event.
- **head** - scheduled send to the consumer seeing a head at or above the event's sequence; `read:async` only (`-` for `read:sync`). An event a read returned before its head arrived counts as seen when read.
- **read** - scheduled send to the consumer's read returning it.
- **read call** (B3) - how long each read that returned events took.
- Latency columns are `p50 / p99 / p99.9 / max` in milliseconds; the JSON also has p90 and the sample count. Latencies go into a constant-memory log-linear histogram (HDR-style: exact below 128 ns, then 64 buckets per power of two), so a percentile is the nearest-rank bucket's upper bound, at most about 1.6 % above the true value, and max is exact. In a `peak` row the send is never late, so latencies there are those of a saturated system.
- **errors** - append, read and watch errors. They are retried, so they do not fail a cell by themselves; what they cost shows in the check (a lost fence) and in latency.

Things the numbers include that a production deployment would not:

- The bench runs in the backend's process and on its runtime. Building events, parsing stamps and checking order cost CPU on the same cores; for the memory backend that client work is a large share of the total. Writers and consumers spend tokio's cooperative budget on every call, so a backend whose calls never suspend still yields now and then and cannot starve the other side.
- The memory backend keeps every topic of an instance behind one lock, which a writer and a consumer reading one event at a time contend for; that is the likely reason its B2 `single-1p1c` cells run an order of magnitude below its B1 `single-1p` rate.
- `read:sync` latency includes up to the 1 ms backoff and the timer tick it rounds to.
- Open-loop sends are made on time to within a scheduler yield: writers sleep to within 2 ms of a send and then yield until it is due, because the timer only fires on a 1 ms tick.
