# Long-haul run

Runs the PostgreSQL backend under steady load with bursts, for as long as you leave it, against a server you can look into. Every segment goes through its whole life - extended, sealed, expired, detached, dropped - while the run checks the events it reads and the storage invariants (`tests/sql/invariants.sql`), and you watch the same tables from `psql`.

## Start

```bash
cd gears/system/event-broker/plugins/postgres-event-broker-plugin
docker compose -f longhaul/compose.yaml up -d --wait     # PostgreSQL on 127.0.0.1:54329, database `events`

# from the repository root; EB_LONGHAUL_URL defaults to the compose server
make longhaul-eb-pg LONGHAUL_ARGS="--reset"
make longhaul-eb-pg LONGHAUL_ARGS="--reset --duration 30m --rate 5000"
```

The run writes to schema `evbk_longhaul` (`--schema` for another). It refuses a schema that exists unless `--reset`, which drops it first; it leaves the schema in place when it ends, for inspection. It runs until Ctrl-C or `--duration`, then stops the writers, lets the tail readers reach the final heads (up to 2 min), waits for maintenance to settle and checks the settled invariants (up to ten rescans), prints a report and writes `target/eb-longhaul/report.json`; `make` returns only once that is done. A second Ctrl-C skips the waiting and reports at once, with the settled invariants and the `keep` topic's completeness not judged. The exit code is 0 only if every criterion passed.

## What it writes

Three topics of 16 partitions:

| Topic | Retention | |
|---|---|---|
| `age` | `--max-age` (2m) | segments expire by the age of their newest event |
| `bytes` | `--max-bytes` (4 MiB) per partition, on disk | the floor follows the partition's size on disk, never below the bound |
| `keep` | none | segments only grow; the tail must read every event |

`--rate` (2000) events/s in total, spread over `--writers` (4) per topic, each holding its own fence key and resending under the same fence until answered. Every `--burst-every` (5m) the rate goes to `--burst-factor` (3) times for `--burst-for` (30s). Each event carries `{w, k, n}`: the writer, the key and that writer's count for the key, so readers can check per-key order.

The `fast` profile (default) sets segments of about a minute of one partition's writes at `--rate` (at least 2000 events), a rescan every 5 s and a cross-check every third rescan; `--profile default` keeps the plugin's defaults. `--settings '<json>'` is merged over the profile, e.g. `--settings '{"segment":{"period":"PT10S"}}'`.

Readers per topic: **tail** follows the head watch; **lagging** reads at 90 % of the write rate and falls below the floor of the bounded topics, which counts as expected loss once checked against `resolve(Earliest)`; **history** reads a random range once a second.

## The status line

```
00:12:35 write 2.0k/s of 2.0k/s behind=2ms append=4ms | read 2.0k/s delivery=5ms read=1ms | segments open=48 sealed=231 expired=2 detaching=1 dropped=18 tables=282 db=61MiB | err=0 trans=0 viol=0
```

- **write**: events acknowledged per second, of the target rate (`--rate`, times `--burst-factor` during a burst). `behind`: p99, per event, of how long after its due time on the writer's schedule its batch was first sent; a few ms while the backend holds the rate, growing steadily when it cannot. `append`: p99 from that send to the answer, resends included.
- **read**: events the tail readers read per second. `delivery`: p99, per event, from its `sequence_time` (the server's clock at append) to the tail read returning it, i.e. how far the readers trail the writers in time. `read`: p99 of one tail read call, up to 1000 events of one partition.
- **segments**: segment rows per state; `dropped` segments gone since the previous line; segment tables and schema size.
- `err` errors; `trans` live findings that did not repeat at the next check; `viol` violations. A violation is logged as a `VIOLATION` line with its figures. `RUST_LOG=info` adds the plugin's own logs.

The p99 figures cover the last interval only; `-` means none in it. Every backend setting `--settings` accepts, with its default, is listed by `make longhaul-eb-pg LONGHAUL_ARGS=--help`.

## Watch it

From `psql` on the host, or inside the container (no local `psql` needed - the compose file mounts these files):

```bash
psql postgres://postgres:postgres@127.0.0.1:54329/events
#   \i longhaul/observe.sql
docker exec -it evbk-longhaul-pg psql -U postgres -d events
#   \i /evbk/longhaul/observe.sql
```

```
TABLE lh_states \watch 2                      -- segment rows per topic and state
SELECT * FROM lh_segments WHERE topic = 'age' AND partition = 0 \watch 2
TABLE lh_partitions                           -- hwm, floor, stored events and MiB
TABLE lh_sessions                             -- evbk-* sessions, waits, transaction age
TABLE lh_locks                                -- evbk:seg / evbk:det advisory locks
:check                                        -- live invariants, one snapshot; no rows = intact
```

The server is sized for sustained inserts (`compose.yaml`): 2 GB of shared buffers, 16 GB of compressed WAL between timed checkpoints every 15 min, and a background writer and autovacuum that keep up, so a high `--rate` measures the plugin rather than checkpoints forced by WAL volume. After changing it, `docker compose -f longhaul/compose.yaml up -d --wait` recreates the container and keeps the data.

`\set schema other` before `\i` points the views at another schema. Lock waits and statements over 200 ms go to `docker compose -f longhaul/compose.yaml logs -f pg`; `pg_stat_statements` is loaded.

## Break it

The run survives all of these without a restart; each outage is logged with its length and listed in the report.

```bash
docker compose -f longhaul/compose.yaml restart pg                 # the server restarts
```

```sql
-- the plugin's sessions are killed
SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name LIKE 'evbk-%';
-- a long reader: DETACH ... CONCURRENTLY waits for it, is cut off by its 2 s timeout and
-- stays pending (lh_states: detaching) until this transaction ends
BEGIN; SELECT count(*) FROM evbk_longhaul.<a parent table from lh_partitions>; -- ... COMMIT later
```

Damage to the data (deleting an event row, editing a segment row) is reported as a violation: by the readers when they reach it, and by the invariant check once two checks in a row see it.

## Pass criteria

| Criterion | Holds when |
|---|---|
| no violation | nothing was counted |
| settled invariants | the settled check finds nothing once maintenance had ten rescans at most |
| segment rows plateau (age, bytes), resident memory plateau | after warmup (`max_age` plus three segments' fill time), cut into `--burst-every` windows, the last window's peak is at most 20 % above the first's; not judged with fewer than two windows |
| bytes bound | no `bytes` partition keeps more than `max_bytes` plus its two largest segments on disk in two samples in a row |
| age bound | the oldest `age` event is never older than `max_age`, plus one segment's fill time at `--rate` (an event ages out only once its segment is full and sealed), plus `--retention-grace` (60s) |
| plugin sessions steady | `evbk-*` sessions at the end within 20 % of the count at warmup end |
