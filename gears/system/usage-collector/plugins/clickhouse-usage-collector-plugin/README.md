# ClickHouse Usage Collector Plugin

ClickHouse storage-backend plugin that implements the Usage Collector `UsageCollectorPluginV1` SPI. It is the durable system of record for usage records and the usage-type catalog: the Usage Collector gateway gear discovers it via the types registry and dispatches all persistence to it. The plugin owns nothing of the host's domain logic — it is pure persistence over a ClickHouse columnar OLAP database, with no coordination backend: every write is a plain read-then-insert whose convergence rests on `ReplacingMergeTree(version)` rows resolved at read time — point reads with `ORDER BY version DESC LIMIT 1 BY <sort key>`, the `list`/`aggregate` range reads with `FINAL` — with the engine's `insert_deduplication_token` window as a safeguard against storing an exact retry twice.

## Configuration

Config maps to `ClickHousePluginConfig` (`src/config.rs`). Durations are whole seconds (repo convention).

| Key | Default | Description |
| --- | --- | --- |
| `database_url` | _(required)_ | ClickHouse HTTP endpoint URL including credentials, e.g. `https://user:${CH_PASSWORD}@host:8443/db`. Held as a `secrecy::SecretString` (Debug-redacted, no Display/Serialize, zeroized on drop); `${VAR}` placeholders are expanded at startup. Only the `http` and `https` schemes are accepted; a plaintext `http://` URL is additionally rejected unless `allow_insecure_http = true` (see [TLS enforcement](#tls-enforcement) below). Credentials containing URL-reserved characters must be percent-encoded, and the encoded bytes must decode to valid UTF-8; a credential that does not is rejected by config validation at startup rather than silently altered. |
| `allow_insecure_http` | `false` | Explicit development/test opt-out that permits a plaintext `http://` `database_url`. Has no effect on a `https://` URL, and does not admit a non-HTTP scheme. **MUST NOT** be set in production. |
| `request_timeout_secs` | `30` | Per-request timeout budget in seconds (reads and writes). Drives three mechanisms: the ClickHouse *server* settings `send_timeout`/`receive_timeout`; a *client-side* deadline 5s later on every individual ClickHouse await; and, while `async_insert` is on, the budget an `INSERT` has to absorb the server-side buffer flush (which is why startup validation requires ≥ 2s in that case). The client-side one is the backstop for a connection that is accepted and then never answered (or held open by an intermediary), which the server settings cannot bound because they never reach a server. Sized 5s apart so a responsive server's own timeout fires first and callers get its descriptive error. |
| `async_insert` | `true` | Send the record-create `usage_records` `INSERT`s — the single-row write of `create_usage_record` and the multi-row write of `create_usage_records` — with the ClickHouse settings `async_insert = 1` and `wait_for_async_insert = 1`, applied **per statement** so no `SELECT` is affected. The server buffers and coalesces concurrent inserts into shared parts instead of writing one small part per request, which is what keeps `ReplacingMergeTree` part count and merge pressure down under a request-shaped ingest stream. `wait_for_async_insert = 1` is pinned, not separately configurable: without it an acknowledged record can be lost on a server restart, and `create_usage_record`'s dedup pre-read would stop seeing its own prior insert. The `INSERT` — and so the API response — returns only once the buffer has flushed and the flushed block has committed; in cluster mode that includes the foreground delivery to the owning shard (`distributed_foreground_insert = 1`) and `insert_quorum` when configured. The flush is *triggered* by the server-side `async_insert_busy_timeout_ms` (adaptive 50-200ms on ClickHouse 24.x+); that is a floor on the added latency, not a bound on the `INSERT`'s, and the whole wait is charged against `request_timeout_secs`, which must therefore be ≥ 2s while this is enabled (startup validation). The busy timeout itself is deliberately **not** exposed — it is a cluster-wide property the server adapts on its own; pin it in a ClickHouse settings profile for the plugin's DB user if you must. **Scope:** deactivation-marker `INSERT`s and all `usage_type_catalog` writes are sent with an explicit `async_insert = 0` regardless of this setting — see [Asynchronous inserts](#asynchronous-inserts). **Dedup interaction:** every `usage_records` `INSERT` carries an `insert_deduplication_token`, which the engine honours on synchronous inserts and, for asynchronous ones, on `Replicated*` tables; it is a safeguard against storing an exact retry twice, not what keeps results correct — a twin the engine does store is resolved by every read (see [Storage semantics](#storage-semantics)). Set to `false` for one synchronous part write per request on a deployment whose ClickHouse cannot do async inserts. |
| `retention_period_secs` | `31536000` (365d) | `usage_records` retention window; rows older than this are dropped via ClickHouse TTL. Must be in `(0, 100 years]`. Migration DDL creates the table with this value; on every startup `ensure_retention_ttl` issues `ALTER TABLE … MODIFY TTL` when the live interval differs from this value — see [Retention window management](#retention-window-management). |
| `vendor` | `constructorfabric` | Vendor name for GTS instance registration. Must not be empty or blank; an empty value fails startup validation. |
| `priority` | `11` | Plugin priority (lower = higher precedence when multiple plugins are registered). Defaults one below the TimescaleDB plugin's `10`, so with both registered under default config the TimescaleDB backend is selected deterministically; the host's selector breaks equal priorities by registration order, so give each instance a distinct value when overriding. |
| `cluster` | _(unset)_ | Opt-in sharded cluster topology; unset means single node. When set, the plugin reads and writes through `Distributed` tables, which the operator provisions unless `manage_schema` is on: see [Sharded cluster deployment](#sharded-cluster-deployment). Sub-keys: `name` (required; the `remote_servers` cluster name), `records_local_table` (default `usage_records_local`), `catalog_local_table` (default `usage_type_catalog_local`), `insert_quorum` (optional; `auto` or a replica count), `manage_schema` (default `false`; run the cluster migrations at startup), `migration_statement_timeout_secs` (default `3600`; budget per cluster migration statement). All names must be plain identifiers, and the local tables must not be named `usage_records`, `usage_type_catalog` or `usage_collector_migrations`. |

```yaml
gears:
  clickhouse-usage-collector-plugin:
    config:
      database_url: "https://user:${CH_PASSWORD}@host:8443/usage"
      request_timeout_secs: 30
      async_insert: true
      retention_period_secs: 31536000
      vendor: "constructorfabric"
      priority: 11
```

The plugin's only gear dependency is `types-registry` (for the registration handshake).

## Operational requirements

### TLS enforcement

- `database_url` embeds ClickHouse credentials and is the transport for every usage record, so a plaintext `http://` connection is a credential- and data-exposure risk, not just a style choice.
- On startup, `ClickHousePluginConfig::validate` (called before any connection is made) rejects a `http://` `database_url` **unless** `allow_insecure_http = true` is set explicitly. A `https://` URL never needs the override.
- The check reads the scheme of the *parsed* URL, which is normalized to lowercase, so `HTTP://` is gated exactly like `http://` rather than slipping past a raw string comparison.
- `validate` also rejects any scheme other than `http`/`https` — including the native-protocol `clickhouse://` and `tcp://` forms — since this plugin talks to ClickHouse's HTTP interface only. That is a separate failure from the TLS gate: `allow_insecure_http` is consent to skip TLS, not consent to an unusable scheme. Only the offending scheme appears in the error; the DSN never does.
- `allow_insecure_http` exists for local development/test against an unencrypted ClickHouse instance (e.g. a Docker test container) — it **MUST NOT** be set in production.
- Even with the override, `build_client` still emits a `tracing::warn!` on every plaintext connection so operators have a durable, per-startup signal that TLS is off.
- The HTTPS transport is built from the rustls `CryptoProvider` the host process installed, not from the `clickhouse` crate's own connector (which hardcodes a non-FIPS `aws-lc-rs` provider). A server started with `--features fips` therefore gets FIPS-validated crypto on the ClickHouse path too, and this plugin needs no `fips` feature of its own. If no provider has been installed — i.e. `toolkit::bootstrap::init_crypto_provider` has not run — `build_client` fails rather than falling back.

### Schema migrations

The schema is managed by a versioned migration runner (`src/infra/storage/migrator.rs`), modelled on `sea-orm-migration`. sea-orm cannot run it, since neither `sea-query` nor `sea-orm-migration` has a ClickHouse backend. Each migration is a named step (`m0001_init`, `m0002_…`) with an `up`. The runner records the steps it applied in a tracking table and, on every start, runs only the steps that are not recorded yet.

The DDL is built in Rust, with no SQL files, and one step list serves both topologies. Each step is a file under `src/infra/storage/migrator/` (`m0001_init.rs`, …) that declares logical tables once, with the design rationale for each column, key and setting. The migrator fills in the topology: plain `ReplacingMergeTree` tables on a single node; on a cluster, `ReplicatedReplacingMergeTree` local tables behind `Distributed` tables, all `ON CLUSTER`; plus the configured retention in the TTL. `src/infra/storage/ddl.rs` is the small `CREATE TABLE` builder behind it. sea-query cannot express `ENGINE`, `PARTITION BY`, `TTL`, `SETTINGS`, `ON CLUSTER` or skip indexes.

| Topology | Steps | Tracking table |
|---|---|---|
| Single node | `m0001_init`, `m0002_insert_dedup_window` | `usage_collector_migrations` |
| Cluster, with `cluster.manage_schema` | the same steps (`m0002_insert_dedup_window` is a no-op: replicated tables deduplicate on their own) | `usage_collector_migrations`, replicated to every host |

- **Existing deployments.** A deployment from before the runner has its tables but no tracking table. Its first start runs every step and records them. `m0001_init` is a no-op there, but `m0002_insert_dedup_window` alters `usage_records` when its `non_replicated_deduplication_window` is missing or differs (see [Insert dedup window](#insert-dedup-window)), so the plugin's ClickHouse user needs the `ALTER MODIFY SETTING` privilege on `usage_records`.
- **No lock.** ClickHouse has no advisory lock, so replicas that start together can run the same step concurrently. Every step must therefore be idempotent: `IF NOT EXISTS` DDL, read before alter, and guarded data movement. A duplicate record in the tracking table is harmless.
- **Adding a step.** Add the next numbered file under `src/infra/storage/migrator/` (`m0003_….rs`), declare it in `migrator.rs`, and append it to the end of `migrations()` there. The same list serves both topologies. Never rename, reorder or remove a step that has shipped, because steps are matched by name. A one-time schema fix that a restart cannot apply today, such as the indexes below, belongs in a new step.
- **Every start.** `ensure_retention_ttl` is not a step. It follows config, so it runs on every start, after the migrations.

### Retention window management

- `m0001_init` creates `usage_records` with a TTL of `retention_period_secs`.
- On every plugin `init`, after the [schema migrations](#schema-migrations), `ensure_retention_ttl` reads the live TTL from `system.tables.create_table_query` and compares it to `retention_period_secs`. When missing or different, it runs:

      ALTER TABLE usage_records MODIFY TTL created_at + INTERVAL <n> SECOND DELETE

- Changing `retention_period_secs` in config and restarting therefore updates the table TTL automatically; no manual operator `ALTER` is required for retention changes.
- Verify the effective clause with `SHOW CREATE TABLE usage_records` (ClickHouse may rewrite `INTERVAL <n> SECOND` as `toIntervalSecond(<n>)`).
- `usage_type_catalog` carries no TTL clause and is never retention-bounded.
- ClickHouse applies TTL eviction asynchronously during background merges, so rows can outlive the threshold for a while; expiry is not a synchronous delete.

### Data-skipping index management

Same class of gotcha for indexes (not TTL): everything in `CREATE TABLE IF NOT EXISTS` other than what `ensure_retention_ttl` and `ensure_insert_dedup_window` reconcile applies **only at first provisioning**.

- The schema declares two `bloom_filter` data-skipping indexes on `usage_records` — `idx_records_id` on `id` and `idx_records_corrects_id` on `corrects_id` — so that `get_usage_record` (`WHERE id = ?`) and the deactivation cascade (`WHERE id = ? OR corrects_id = ?`) prune granules instead of scanning the table. The `ORDER BY` key is deliberately unchanged, since it is the dedup identity `ReplacingMergeTree` collapses on.
- A deployment provisioned **before** these indexes existed does not get them from a restart: the DDL re-runs as a no-op. Add them manually, then materialize them over the existing parts:

      ALTER TABLE usage_records ADD INDEX idx_records_id id TYPE bloom_filter GRANULARITY 1
      ALTER TABLE usage_records ADD INDEX idx_records_corrects_id corrects_id TYPE bloom_filter GRANULARITY 1
      ALTER TABLE usage_records MATERIALIZE INDEX idx_records_id
      ALTER TABLE usage_records MATERIALIZE INDEX idx_records_corrects_id

- `MATERIALIZE INDEX` is a background mutation over existing parts; new parts are indexed on write, so read latency improves gradually until it finishes. Verify with `SHOW CREATE TABLE usage_records` and watch `system.mutations`.

### Insert dedup window

- The single-node schema creates `usage_records` with `SETTINGS non_replicated_deduplication_window = 10000`. Every `usage_records` `INSERT` carries an `insert_deduplication_token` derived from its row ids, and the window is what lets ClickHouse drop a racing identical block instead of storing a duplicate row where it honours the token (synchronous inserts; asynchronous ones on `Replicated*` engines). That bounds write amplification; a duplicate the engine does store is resolved by every read (see [Storage semantics](#storage-semantics)).
- The single-node migration step `m0002_insert_dedup_window` (`ensure_insert_dedup_window`) reads the live setting from `system.tables.create_table_query` and, when it is missing or differs, runs:

      ALTER TABLE usage_records MODIFY SETTING non_replicated_deduplication_window = 10000

  `MODIFY SETTING` is metadata-only, so a deployment provisioned before the setting existed picks it up on its next restart with no manual step. Like any step, it runs once and is then recorded, so a setting changed by hand later is not reverted.
- On a table provisioned as `ReplicatedReplacingMergeTree` the setting is accepted but inert: `replicated_deduplication_window` governs, and asynchronous inserts are deduplicated by token too (`async_insert_deduplicate = 1` is already sent).
- Verify the token is on the wire:

      SYSTEM FLUSH LOGS;
      SELECT event_time, Settings['insert_deduplication_token'], written_rows
      FROM system.query_log
      WHERE type = 'QueryFinish' AND query_kind = 'Insert'
        AND positionCaseInsensitive(query, 'usage_records') > 0
      ORDER BY event_time DESC LIMIT 5

  A deduplicated block shows `written_rows = 0` for the losing insert.

### Asynchronous inserts

`async_insert` (default `true`) moves part formation for the **record-create** `usage_records` writes — the single-row `INSERT` of `create_usage_record` and the multi-row `INSERT` of `create_usage_records` — into a server-side buffer that coalesces concurrent inserts into shared parts. The write path is one `INSERT` per request, so without it a request-shaped ingest stream writes one small part per request and drives `ReplacingMergeTree` part count and merge pressure up. It does not eliminate `TOO_MANY_PARTS` under every load: it bounds part creation by the flush rate instead of the request rate. Verified on ClickHouse 25.6, the version the integration suites and the cluster compose environment pin.

- **Dedup token.** Every `usage_records` `INSERT` carries `insert_deduplication_token` (UUIDv5 of its sorted row ids, namespaced so a deactivation-marker write never collides with the create of the same ids) and the table keeps `non_replicated_deduplication_window = 10000` (`ensure_insert_dedup_window` retrofits it on startup). The token is enforced on synchronous inserts — marker writes, and record creates when `async_insert = false` — and on asynchronous inserts only for `Replicated*` engines (`async_insert_deduplicate = 1` is sent for that case); it is not guaranteed to survive the buffer plus a `Distributed` forward. It is a safeguard against storing an exact retry twice. A twin it does not catch is one more physical row under the same sort key, which every read resolves (see [Storage semantics](#storage-semantics)).

- **Scope.** Applied per statement, on the `INSERT` only, so no `SELECT` plan changes. Every insert states its write intent (`pool::InsertIntent`); two sites are deliberately **synchronous**, with `async_insert = 0` sent explicitly so a server or user settings profile cannot buffer them either:
  - the deactivation cascade's marker write, because the cascade depends on one statement's markers flipping together (per partition) and a shared buffer flushing on its own schedule does not give that (`ch_deactivation_cascade_is_atomic` fails when they go through it); and
  - every `usage_type_catalog` write, which is control-plane traffic with no concurrent stream to coalesce and no part count to reduce.
- **Atomicity.** A flushed block commits one part per partition (`toYYYYMM(created_at)`) per shard, exactly like a synchronous statement: a batch spanning two months, or two shards, is two commits either way, and a timeout or a failed delivery may have committed some of a batch's rows. The deterministic record ids make the caller's retry converge on the rows that did land.
- **Cluster mode.** The buffer lives on the initiator node the plugin is connected to. The flush inserts the buffered block into the `Distributed` table with the flushing statement's own settings, so `distributed_foreground_insert = 1` and `insert_quorum` (when configured) apply to it: the flush completes only once every shard the block touches has committed its rows, and `wait_for_async_insert = 1` makes the `INSERT` wait for that whole chain. There is no fire-and-forget acknowledgement.
- **`wait_for_async_insert = 1` is pinned.** With `0`, `create_usage_record` would return before its row was committed — losing acknowledged records on a restart, and breaking the dedup pre-read's read-your-writes so a retry inserts a second row under a used idempotency key. There is no config field for it.
- **Latency.** Each insert now waits for its buffer to flush and the flushed block to commit. The flush is triggered by the server-side `async_insert_busy_timeout_ms` (adaptive 50-200ms on ClickHouse 24.x+); the part commit, the foreground shard delivery and any quorum wait come after it, so that timeout is a floor on the added latency, not a ceiling on the `INSERT`'s. Watch `uc_clickhouse_insert_duration_seconds{mode="single"}` and `{mode="batch"}` across the switch: a small rise is expected and correct; a rise anywhere near `request_timeout_secs` means the busy timeout is misconfigured server-side or the shards are slow to commit. Pin it in a settings profile for the plugin's DB user if you must — it is not exposed as plugin config because it is a cluster-wide property the server adapts on its own.
- **Verifying it is on the wire:**

      SYSTEM FLUSH LOGS;
      SELECT event_time, Settings['async_insert'], query_duration_ms
      FROM system.query_log
      WHERE type = 'QueryFinish' AND query_kind = 'Insert'
        AND positionCaseInsensitive(query, 'usage_records') > 0
      ORDER BY event_time DESC LIMIT 5

  Note `Settings` records only values that **differ from the server default**, so `wait_for_async_insert = 1` shows as absent (its default is already `1`) — check `system.settings` for the effective value rather than reading the blank as "not sent".
- **Confirming the queue coalesces**, and the part-count improvement it buys (run under concurrent ingest — a single writer shows no difference):

      SELECT query, first_update, total_bytes FROM system.asynchronous_inserts;

      SELECT partition, count() AS parts, sum(rows) AS rows,
             round(sum(rows) / count()) AS avg_rows_per_part
      FROM system.parts
      WHERE database = currentDatabase() AND table = 'usage_records' AND active
      GROUP BY partition ORDER BY partition

  Expect `parts` to fall sharply and `avg_rows_per_part` to rise for the current month. Corroborate merge pressure with `system.merges` and `system.part_log` (`event_type = 'MergeParts'`).

### Workload isolation and pool contention

- One `clickhouse::Client` (and therefore one underlying HTTP connection pool) serves **both** the ingestion and the query paths. This is a deliberate v1 choice, not a solved isolation guarantee.
- **The pool is not tunable from config.** There is no `pool_max_connections`-style setting: `clickhouse` 0.15.1 exposes no pool-bound builder (`with_setting`/`with_option` set ClickHouse *server* settings, and the `with_http_client` seam cannot be used from outside the crate), so no config field could drive it. Everything below is therefore an operational mitigation, not a knob.
- Risk to the ingestion-throughput NFR: a burst of aggregation or list queries competes with ingest writes for the same pool and the same ClickHouse server resources, so query bursts can delay pool acquisition on the write path and push ingest below its throughput budget. The plugin has no internal reservation, priority, or queueing that protects ingest from read traffic.
- Operator guidance:
  - Watch `uc_clickhouse_pool_acquire_duration_seconds` together with `uc_clickhouse_insert_duration_seconds{mode="batch"}` and `uc_clickhouse_query_requests_total{query_kind="aggregated"}`: rising pool-acquire time correlated with query volume is this contention, not a ClickHouse slowdown.
  - Bound read cost server-side with ClickHouse's own controls (settings profiles / quotas per query user, `max_concurrent_queries_for_user`) so heavy analytical queries cannot consume the whole server.
  - `request_timeout_secs` bounds how long any single starved request waits; sizing it too high lets a query burst hold pool capacity longer.
  - For hard separation, run **two plugin instances** with distinct GTS priorities pointed at different endpoints (write primary vs. read replica). This is an operational workaround; the plugin does not split pools internally.
- A future revision may split ingestion and query onto independently-pooled clients (additive, non-breaking to the SPI); until then this contention point is accepted and documented rather than assumed away.

### Measuring part creation and read cost

The part-count benefit of `async_insert` and the read-path cost of `FINAL` are workload-dependent; measure them on the target cluster rather than assuming either. A reproducible procedure, usable against the two-shard compose environment or any cluster:

1. Stop background merges so every part written stays observable: `SYSTEM STOP MERGES ON CLUSTER <name> <db>.usage_records_local` (single node: `SYSTEM STOP MERGES usage_records`). Restart them with `SYSTEM START MERGES` afterwards.
2. Drive N concurrent `create_usage_record` / `create_usage_records` requests once with `async_insert: false` and once with `async_insert: true` (fresh tables each time), recording p50/p95 insert latency and throughput from the load tool, and `uc_clickhouse_insert_duration_seconds` from the plugin.
3. Read part creation on every node, per run:

       SELECT partition, count() AS parts, sum(rows) AS rows,
              round(sum(rows) / count()) AS avg_rows_per_part
       FROM system.parts
       WHERE database = currentDatabase() AND table = 'usage_records_local' AND active
       GROUP BY partition ORDER BY partition;

       SELECT toStartOfMinute(event_time) AS minute, count() AS new_parts, sum(rows) AS rows
       FROM system.part_log
       WHERE event_type = 'NewPart' AND database = currentDatabase() AND table = 'usage_records_local'
       GROUP BY minute ORDER BY minute;

4. With the parts still unmerged, run representative `get`, `list` (first page and a cursor follow) and `aggregate` (ungrouped and grouped) calls and read their cost from the initiator:

       SYSTEM FLUSH LOGS;
       SELECT query_duration_ms, read_rows, memory_usage,
              ProfileEvents['OSCPUVirtualTimeMicroseconds'] AS cpu_us,
              position(query, 'usage_records FINAL') > 0 AS final_read
       FROM system.query_log
       WHERE type = 'QueryFinish' AND query_kind = 'Select'
         AND current_database = currentDatabase()
         AND positionCaseInsensitive(query, 'usage_records') > 0
       ORDER BY event_time_microseconds DESC LIMIT 20;

   The `ReplacingSorted` step `EXPLAIN PIPELINE` shows for the `FINAL` reads is where the resolution cost sits; its share grows with the number of unmerged parts a query's key range touches.

A reference run of this procedure (2026-11-03, the two-shard × two-replica compose environment on ClickHouse 25.6, one tenant, 4000 single-record creates at 32 concurrent clients, merges stopped): synchronous inserts wrote 3025 one-row parts before the shard refused further inserts with `TOO_MANY_PARTS` (975 of 4000 requests rejected, 60 accepted records/s, p50 252 ms); `async_insert` wrote 156 parts of ~26 rows (4000 of 4000 accepted, 135 records/s, p50 238 ms end to end, 171 ms at the server). On the same unmerged parts, compared with a raw scan plus a marker anti-join (the shape without `FINAL`), the `FINAL` shapes returned identical results and cost less on the 3025 one-row parts (aggregate p50 240 ms vs 510 ms, list p50 833 ms vs 948 ms, with less memory and CPU) but more on the 156 overlapping async-flush parts (aggregate p50 57 ms vs 31 ms, list p50 65 ms vs 51 ms, reading ~1.5× the rows): `FINAL` merges every part whose key range intersects the query's, so its cost follows the number and overlap of unmerged parts, not the row count. Neither shape is free; part count and part overlap are the figures to watch.

### Sharded cluster deployment

Setting `cluster` switches the plugin from a self-provisioned single node to a sharded, replicated ClickHouse cluster. By default the schema belongs to the operator: the plugin creates and alters nothing and only checks the schema at startup. With `manage_schema: true` the plugin runs the cluster [schema migrations](#schema-migrations) itself and then applies the same check.

```yaml
gears:
  clickhouse-usage-collector-plugin:
    config:
      database_url: "https://user:${CH_PASSWORD}@ch-lb:8443/usage"  # a load balancer over the nodes
      async_insert: true           # the default; buffered on the initiator, delivered to shards in the foreground
      cluster:
        name: "usage_cluster"      # from remote_servers
        # insert_quorum: "auto"    # optional, see below
        # manage_schema: true      # optional: the plugin runs the cluster DDL
```

**Layout.** `usage_records` and `usage_type_catalog` are `Distributed` tables. Behind them are shard-local `ReplicatedReplacingMergeTree` tables, `usage_records_local` and `usage_type_catalog_local`. `usage_records` is sharded by `cityHash64(tenant_id)` and `usage_type_catalog` by `cityHash64(gts_id)`. The tenant sharding key is required, not a tuning choice: a deactivation marker clones the row it marks and a duplicate create shares its `tenant_id`, so every version of a record lands on the same shard. That is what makes the `FINAL` of `list` and `aggregate`, which the `Distributed` table forwards to each shard-local table, complete: a shard resolves every version it has, and it has them all.

**Prerequisites (both modes).** On every node, configure the cluster in `remote_servers`, ClickHouse Keeper (`zookeeper`), and the `{shard}` / `{replica}` `macros`, then create the target database `ON CLUSTER`. In addition:
- point `database_url` at a load balancer or proxy over the nodes, not at a single node: the plugin tolerates an unreachable replica only if it can still reach some node;
- set `internal_replication = true` for every shard in `remote_servers`, so that a `Distributed` insert goes to one replica of the shard and replication copies it to the others;
- configure `insert_quorum: auto` only for three or more replicas per shard. `auto` means a majority of the shard's replicas, so with two replicas it needs both, and every insert fails with `TOO_FEW_LIVE_REPLICAS` while one of them is down.

**Provisioning by the operator (default).**
1. Configure `cluster` and start the plugin. With the tables missing, startup fails, and the error ends with the cluster DDL for your configured cluster name, local table names and retention (`migrator::render_ddl`: every migration step's DDL, rendered for the cluster).
2. Run that DDL once on any node in the target database (`clickhouse-client --database <db> --multiquery`). Its statements are `ON CLUSTER`, so this creates all four tables on every node.
3. Start the plugin again.

**Provisioning by the plugin (`manage_schema: true`).** Start the plugin. It runs the migration steps, which execute the same DDL, then reconciles the TTL `ON CLUSTER`. Each statement is bounded by `migration_statement_timeout_secs`, which also sets how long the server waits for every host to run an `ON CLUSTER` statement (`distributed_ddl_task_timeout`). Once the schema is up to date, startup issues no `ON CLUSTER` statement, so an unreachable host does not delay it. A pending statement waits only for the hosts ClickHouse Keeper reports active (`distributed_ddl_output_mode = 'throw_only_active'`): a host that has just gone down holds it until Keeper expires that host's session (`session_timeout_ms`, 30 s by default), and a failure on an active host still fails startup. A host that was down runs the statement from the DDL queue when it returns. Replicated tables also catch up on `ALTER`s through Keeper replication, but `CREATE TABLE` depends on the DDL queue alone, whose entries expire after `task_max_lifetime` (7 days by default). A host that comes back after that lacks the new tables: startup validation then names it, and the operator applies the DDL the error prints. The plugin's user then needs `CREATE TABLE`, `ALTER`, `INSERT` and `SELECT` on the plugin's tables and the `CLUSTER` privilege.

The integration suite (`tests/cluster_integration_ch.rs`) covers both paths, and both use this same DDL.

**Topology is fixed at first start.** A deployment runs on a single node or on a cluster from the start; the plugin does not migrate a single-node deployment's data to a cluster. If single-node `usage_records` / `usage_type_catalog` tables are found on a cluster, `CREATE TABLE IF NOT EXISTS` leaves them in place, and startup validation refuses to start because they are not `Distributed` tables.

**Startup validation.** The plugin refuses to start, listing every problem, when:
- the cluster is not declared in `system.clusters`;
- any of the four tables is missing on a reachable replica (checked through `clusterAllReplicas`);
- a local table is not `ReplicatedReplacingMergeTree` or has a different `ORDER BY` or `PARTITION BY`;
- a `Distributed` table targets another cluster, database or local table, or uses another sharding key.

Unreachable replicas do not prevent startup, so a plugin pod can restart during a rolling ClickHouse upgrade. The plugin logs a warning and checks those replicas on the next start that reaches them. If every replica of a shard is unreachable, it logs a second warning and starts anyway: requests that need that shard fail with a retryable `Transient` error (`ALL_CONNECTION_TRIES_FAILED`) until a replica is back, and tenants on the other shards keep working.

With an operator-provisioned schema, a `usage_records` TTL that differs from `retention_period_secs` only produces a warning; change it with `ALTER TABLE usage_records_local ON CLUSTER … MODIFY TTL`. With `manage_schema`, the plugin runs that `ALTER` itself. The single-node migration steps never run in cluster mode.

**What the plugin sends.** Every request carries:
- `distributed_product_mode = 'local'`, so a shard-local `IN` subquery, should one be issued, reads the shard's own rows;
- `optimize_skip_unused_shards = 1`, so tenant-pinned reads touch one shard;
- `skip_unavailable_shards = 0`, pinned so that a request needing an unreachable shard fails as a retryable `Transient` error instead of returning a partial result, even if a server settings profile enables the setting;
- `distributed_foreground_insert = 1`, so an `INSERT` — including the flush of an async-insert buffer, which runs with the flushing statement's settings — returns only after every shard it touches committed its rows; on a synchronous insert the `insert_deduplication_token` then reaches the replicated table's `replicated_deduplication_window`;
- `insert_quorum`, when configured.

`async_insert` (the default) is supported: the buffer lives on the initiator, the flush goes through the `Distributed` table in the foreground, and `wait_for_async_insert = 1` holds the request until the owning shards have committed — see [Asynchronous inserts](#asynchronous-inserts). The dedup token is not guaranteed through that path, which is why correctness rests on the reads (every read resolves versions) rather than on the token. Verified on ClickHouse 25.6 by `tests/cluster_integration_ch.rs`.

**Mutations.** `delete_usage_type` and its orphan sweep run `ALTER TABLE <local> ON CLUSTER <name> DELETE WHERE gts_id = ?` with `mutations_sync = 2`, waiting for every replica, because ClickHouse refuses mutations on a `Distributed` table. The plugin's database user therefore needs `ALTER DELETE` on the local tables and the `CLUSTER` privilege, besides `SELECT` and `INSERT` on all four tables.

**Consistency.** A read through a `Distributed` table may be served by a replica that has not yet fetched a just-committed part, so read-your-writes across replicas is bounded by replication lag (ADR-0006 accepts this). That includes the dedup pre-read of a retried create: a retry served by a lagging replica may miss the original row and insert a second version, which every read then resolves to the highest version (the engine's dedup token drops it only where it is honoured). `FINAL` resolves the versions a replica has; it does not fill in a part the replica has not fetched yet. Set `insert_quorum` (for example `auto`, with three or more replicas per shard; see the prerequisites above) to acknowledge an insert only once a quorum of the shard's replicas has it. This narrows the window at a write-latency cost.

## Storage semantics

- **Deduplication** — application-level read-before-insert, then the engine's `insert_deduplication_token` window, then `ReplacingMergeTree(version)` merges as the backstop. Nothing serializes concurrent creates: two callers with the same dedup key can both pass the pre-read and both insert, and both get `Ok`. Because the record `id` is derived from the dedup tuple, both inserts carry the same token, and where the engine honours it (a synchronous insert; an asynchronous one into a `Replicated*` table) it drops the second block — **first-writer-wins** there, no `IdempotencyConflict`. Otherwise both rows are stored under one sort key, differing only in `version`, until the merge. **Physical duplicates are allowed; duplicate API results are not**: `get` and the dedup pre-reads resolve with `ORDER BY version DESC LIMIT 1 BY <sort key>`, `list` and `aggregate` read under `FINAL`, so a logical record appears once in a page, once in a total, at its highest version. **Highest version wins** — a marker over the create it supersedes, the later of two concurrent creates over the earlier; this is not global first-writer-wins, and two concurrent submissions with conflicting payloads under one idempotency key can both be acknowledged with the higher version prevailing. Once the earlier row is visible, a later create sees it and the usual absorb / conflict rules apply. See DESIGN.md §3.6.
- **Batch ingest** — three statements per `create_usage_records` call regardless of how many usage types it spans: one catalog existence query over the batch's distinct `gts_id`s, one dedup pre-read over every record that passed it, one multi-row `INSERT`. That `INSERT` commits **one part per partition it touches**, and `usage_records` is `PARTITION BY toYYYYMM(created_at)` — so a reader sees all of a batch's new rows or none of them only when the batch falls in a single calendar month; a batch spanning two months is two commits and can be observed part-way through, and on a cluster a batch spanning two shards likewise. With `async_insert` on (the default) the statement joins the server-side buffer and the flushed block commits by the same unit, so this is still the only window; a timeout may have committed some of the rows, and the deterministic ids make the retry converge — see [Asynchronous inserts](#asynchronous-inserts).
- **Consistency profile** — on a single-node deployment: effectively immediate read-after-write for any reader. On a replicated deployment: bounded by ClickHouse's own replication lag; read-your-writes holds on the replica that committed the insert. Every read resolves `ReplacingMergeTree` versions before it filters on `status`, pages or aggregates, and none waits for a background merge. Point reads (`get`, the deactivation cascade, the dedup pre-reads) resolve in SQL (`ORDER BY version DESC LIMIT 1 BY <sort key>`) over a bloom-filter-pruned candidate set; `list` and `aggregate` read `FROM usage_records FINAL` with `do_not_merge_across_partitions_select_final = 1` and `optimize_move_to_prewhere_if_final = 0` attached per query, so the engine collapses each sort key to its highest version before the `WHERE`, the keyset `LIMIT` and the `GROUP BY` run. `FINAL` resolves the parts the query touches on the read path — a real cost, confined to the touched partitions and skipped for a partition that holds one already-merged part — and it resolves only the versions present: it does not fill in a row that has not committed or reached the replica being read.
  For strict cross-replica read-your-writes, configure ClickHouse's native `insert_quorum` write-quorum setting on the server; this plugin does not enable it by default and enabling it incurs a proportional write-latency cost — see DESIGN.md §3.8.
- **Workload isolation** — the ClickHouse client/pool is shared by both the ingestion and query paths (v1 design), so query bursts can degrade ingestion throughput; see [Workload isolation and pool contention](#workload-isolation-and-pool-contention).
- **Referential integrity** — application-emulated on both sides, with a known gap on the delete side. Create side: `create_usage_record(s)` checks that every referenced `gts_id` exists in `usage_type_catalog` (an existence read, which needs no version resolution) and rejects absent ones with `UsageTypeNotFound`. Delete side: `delete_usage_type` runs an existence read, then a capped reference probe over `usage_records` (refusing with `UsageTypeReferenced` → HTTP 409 if any row references the type), then removes the catalog row with `ALTER TABLE … DELETE` under `mutations_sync = 1`, then re-probes and sweeps any record that landed in between. **This is not race-free.** ClickHouse has no foreign key, so an insert whose own catalog check passed before the catalog row was removed can commit after the sweep and orphan a row. The reference TimescaleDB plugin closes this window with a native `FOREIGN KEY … ON DELETE RESTRICT` on `usage_records.gts_id`, admitting no window in which a concurrent `create_usage_record` can reference a type being deleted; this plugin narrows that window instead of closing it. The window is bounded, not closed: the catalog row is deleted **before** the sweep on purpose, so from that moment the insert-time check refuses new records on its own. `async_insert` (the default) widens the window to the server-side flush interval. Watch `uc_clickhouse_orphaned_reference_detected_total` — a non-zero rate means deletes are being run against types with live ingest. Prefer deleting a type while no ingest for it is in flight. See DESIGN.md §3.6.
- **Retention** — ClickHouse TTL clause on `usage_records`: `retention_period_secs` in `CREATE TABLE`, then reconciled to `retention_period_secs` on every startup via `ensure_retention_ttl` (`ALTER TABLE … MODIFY TTL` when needed) — see [Retention window management](#retention-window-management).
- **Error classification** — a failure is reported as retryable `Transient` when the client could not reach ClickHouse at all (network, timeout, compression), when the client-side deadline expires, or when ClickHouse itself answers with one of a fixed allowlist of overload/backpressure codes: `159` `TIMEOUT_EXCEEDED`, `202` `TOO_MANY_SIMULTANEOUS_QUERIES`, `203` `NO_FREE_CONNECTION`, `209` `SOCKET_TIMEOUT`, `210` `NETWORK_ERROR`, `252` `TOO_MANY_PARTS`, `279` `ALL_CONNECTION_TRIES_FAILED`, `285` `TOO_FEW_LIVE_REPLICAS`, `999` `KEEPER_EXCEPTION` — plus HTTP `502`/`503`/`504` when ClickHouse returned no readable body (typically an intermediary). Anything else, including `241` `MEMORY_LIMIT_EXCEEDED` (permanent for an over-large batch, so retrying it would loop) and `319` `UNKNOWN_STATUS_OF_INSERT`, is `Internal`. Only the unreachable-backend cases (and a client-side deadline expiry, which is classified the same way) clear the `uc_clickhouse_ready` gauge: a server that answers with backpressure is degraded, not down. The gauge is re-armed by the next successful request, so it recovers without a restart; alert on `uc_clickhouse_ready == 0` with a `for:` window so a single slow request does not page. It is never re-armed after the gear's cancellation token fires.
- **Deactivation** — applied as a single multi-row `INSERT` of versioned marker rows (depth-1 only); no `UPDATE` or `ALTER TABLE … DELETE` is issued on the request path. A plain retry of a record that races its own deactivation and misses the original row at pre-read can re-insert it as active with a higher version (the gateway's caller-side active-target check protects compensations, not retries); see DESIGN.md §3.6.
- **Schema** — single node: versioned, idempotent migration steps at startup (see [Schema migrations](#schema-migrations)); cluster: operator-provisioned, or plugin-managed with `manage_schema`, and validated either way (see [Sharded cluster deployment](#sharded-cluster-deployment)); indexes still apply only at first provisioning (see [Data-skipping index management](#data-skipping-index-management)), while TTL is updated on startup when config differs.

## SPI conformance

The crate implements `usage_collector_sdk::UsageCollectorPluginV1` (via `StorageAdapter` over the record and catalog stores). Conformance is enforced at compile time. All ten SPI methods are implemented. One does not meet its contract in full: `delete_usage_type` emulates `ON DELETE RESTRICT` without a foreign key and admits a race the SPI forbids — see [Storage semantics](#storage-semantics) and the deviations table in `docs/PRD.md`.

## Running integration tests

The real-DB suites are gated behind the `clickhouse` feature and require Docker for a ClickHouse image:

    make test-integration LANE=usage-collector-ch
    # or: cargo nextest run -p cf-gears-clickhouse-usage-collector-plugin --features clickhouse --run-ignored all

This includes `tests/cluster_integration_ch.rs`, which boots a two-shard cluster (two nodes plus embedded Keeper on a private Docker network) per test. Each test either provisions the cluster from the operator DDL (`migrator::render_ddl`) or leaves it bare for the managed migrations.

Without the feature, only unit tests run (no Docker needed):

    cargo test -p cf-gears-clickhouse-usage-collector-plugin

## Running E2E tests

The suites above exercise this crate directly. To exercise it as the bound
storage plugin behind the usage-collector gear's HTTP API — over a real server,
through a real ClickHouse container:

    make e2e-usage-collector

That target runs the usage-collector E2E suite twice, once per storage backend
(TimescaleDB, then this one), building a dedicated binary for each: every gear
linked into the server is initialized, and both plugins fail `init` without
their own live database, so they cannot share one process. The test bodies are
shared and speak HTTP only — anything they assert is a contract of the gear's
API that both plugins must satisfy, `delete_usage_type` included: the
referenced-type delete seam
(`test_delete_usage_type_referenced_by_record_is_rejected`) runs unmodified on
both backends, since the uncontended contract — 409 for a referenced type, 204
for an unreferenced one — is identical either way. The race this backend admits
between probe and delete is not reachable from a single-threaded HTTP test and
is covered by documentation rather than by an E2E assertion.

Config lives in `testing/e2e/suites/usage_collector/config-clickhouse.yaml`;
the container is managed by `ClickHouseSidecar` in
`testing/e2e/lib/sidecars.py`. The ClickHouse image and tag are pinned once,
as `CLICKHOUSE_IMAGE`/`CLICKHOUSE_TAG` in
`libs/test-containers/src/lib.rs`, and consumed from there by both
`tests/common/mod.rs` (via `test_containers::clickhouse()`) and by
`sidecars.py`; the Rust test `e2e_sidecar_pins_the_same_clickhouse_image`
checks the two stay in agreement.
