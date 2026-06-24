# Observability Contract — Scenario Details (`SC-OBS-*`)

> Detail for §13 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. Per `cpt-cf-clst-nfr-observability` / ADR-004, signal names are a
> versioned contract on par with the trait signatures. Authoritative catalog:
> [OBSERVABILITY.md](../OBSERVABILITY.md); names mirrored in code as
> `cluster_sdk::observability` constants. SC-OBS-001..004 are assertable at L2 (capture
> emitted signals via a test exporter / `tracing` subscriber); SC-OBS-005 is per-plugin
> completeness (L3).

**SC-OBS-001 — spans emitted with catalogued names & attributes** · L2 · ☐
- *Intent:* every facade operation produces its named span so traces are consistent across backends.
- *Steps:* with a capturing tracing subscriber, drive each operation (e.g. `cache.get`, `leader.elect`, `lock.try_lock`, `discovery.register`).
- *Expected:* a span named per the catalog (e.g. `cluster.cache.get`) carrying the specified attributes (`provider`, `key`/`election`/`lock`/`name`, …).
- *Done-when:* asserts span name and required attributes for each catalogued operation.

**SC-OBS-002 — metrics emitted with catalogued names** · L2 · ☐
- *Intent:* dashboards built on metric names stay stable across plugin versions.
- *Steps:* drive operations with a capturing metrics exporter (the `otel` feature's adapter).
- *Expected:* metrics named per the catalog (e.g. `cluster_cache_ops_total`) are recorded with the `result` outcome label set correctly (`ok`/`conflict`/`timeout`/`contended`).
- *Done-when:* asserts metric name and bounded labels for the exercised operations.

**SC-OBS-003 — cardinality rule** · L2 · ☐
- *Intent:* unbounded values must never become metric labels (`OBSERVABILITY.md` §2) or they explode Prometheus cardinality.
- *Steps:* drive operations whose keys/lock/election/instance names are high-cardinality; inspect emitted metric labels.
- *Expected:* metric labels are confined to the bounded allowlist (`provider`, `op`, `result`, `transition`, `kind`, `primitive`); `key`/`lock`/`election`/`instance_id` appear only as span attributes / log fields, never as metric labels.
- *Done-when:* asserts no metric carries a label outside `METRIC_LABEL_ALLOWLIST`.

**SC-OBS-004 — log events use catalogued names** · L2 · ☐
- *Intent:* structured log events are part of the contract too (e.g. `cluster.leader.transition`).
- *Steps:* with a capturing subscriber, trigger event-bearing transitions (leadership change, cas conflict, lock timeout).
- *Expected:* log events named per the catalog with the expected severity and fields.
- *Done-when:* asserts the event name and fields for each catalogued log event.

**SC-OBS-005 — every plugin emits every signal** · L3 · ☐
- *Intent:* cluster is foundational; inconsistent observability across plugins leaves gaps during incidents. The contract is *complete* per plugin, not just per-SDK.
- *Steps:* run SC-OBS-001..004 against each real backend (not just the stub) in its container.
- *Expected:* each plugin emits the full set of catalogued spans/metrics/log-events applicable to the operations it supports.
- *Done-when:* the observability assertions pass per-plugin. *(L3 — runs alongside the per-backend integration suite.)*
