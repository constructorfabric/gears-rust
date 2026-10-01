---
status: accepted
date: 2026-09-26
---

# Accepted runtime and consistency limitations in P1

**ID**: `cpt-cf-mini-chat-adr-runtime-consistency-limitations`

## Context and Problem Statement

DESIGN states several invariants as MUSTs that the implementation enforces differently, or only partly:
* the orphan watchdog uses database server time;
* CHECK constraints on `chat_turns` and `attachments`;
* a byte-identical `done` payload on replay;
* ToolKit leader election for the watchdog.

DESIGN also documents configuration keys that no longer have an effect or were removed, and finalization paths that do not share one function. This ADR records the accepted P1 behaviour so DESIGN can reference one place.

## Decision Drivers

* Each limitation must be visible to operators and reviewers.
* Fixing them requires schema migrations across two SQL dialects or new persistence. That is out of scope for aligning P1.

## Considered Options

* Implement every invariant exactly as written in DESIGN.
* Accept the current behaviour, document it and state what it relies on.

## Decision Outcome

Chosen option: "Accept the current behaviour, document it and state what it relies on".

| Topic | DESIGN statement | Status | Current behaviour and assumption |
|---|---|---|---|
| Watchdog clock | §4 "Orphan Watchdog": use `now()` of the DB server | Accepted limitation | The stale cutoff is computed from the application clock (`OffsetDateTime::now_utc()`). Relies on NTP-synchronized pods. The minimum `timeout_secs` is 90 s, which absorbs normal skew. |
| Watchdog leader election | §4: ToolKit leader election, one active instance | Accepted | Gear-local Kubernetes Lease (`mini-chat/src/infra/leader/k8s_lease.rs`) when built with the `k8s` feature (the Docker image and Helm chart use it); a no-op otherwise. Double finalization is prevented by the CAS guard in any case. |
| CHECK constraints on `chat_turns` (`completed_at`, `last_progress_at` vs state) and `attachments.cleanup_status` | §3.7 | Accepted limitation | Not enforced by the database; SQLite cannot add CHECK constraints to existing tables without a rebuild. The invariants are kept by the repositories. The orphan scan treats a NULL `last_progress_at` as `started_at`, so legacy rows are still recovered. |
| Replay `done` immutability | §4 "Replay done payload" | Not implemented | Replay rebuilds `quota_decision` and `downgrade_from` from the stored models. `downgrade_reason` is not persisted and is omitted. Citations are not persisted, so replay sends `stream_started`, `delta` and `done` only. |
| `done.usage.model` | §3.3 | Removed | The `usage` object carries token counts only; the model is in `done.effective_model`. |
| SSE `ping` | §3.3: every 15 s of idle time, at any point before the terminal event | Accepted (different) | `event: ping` is sent only between `stream_started` and the first `delta`/`tool` event (`api/rest/sse.rs`, `StreamPhase`), at `sse_ping_interval_seconds`. After content starts, Axum sends an SSE comment keep-alive every 30 s, which keeps proxies from closing the connection. |
| Terminal event of a CAS loser | §5.7: close without a terminal event | Accepted (different) | When the provider task ends without sending a terminal event (CAS lost to the orphan watchdog, or a panic), the relay sends `error{code: "stream_interrupted"}`, which matches the `failed` state the winner committed. After a client disconnect nothing is sent. |
| Separate finalization paths | §5.7 "FinalizeTurn Invariant": every terminal path uses one shared finalization function | Accepted (different) | The stream terminal paths use `FinalizationService::finalize_turn_cas`. The orphan watchdog uses `FinalizationService::finalize_orphan_turn`: its own CAS (`cas_finalize_orphan`, which re-checks the stale-progress predicate) and the shared helpers `derive_billing_outcome`, `QuotaSettler::settle_in_tx` (estimated path) and `OutboxEnqueuer`; its usage and audit events carry `selected_model` = the effective model (the selected model is not persisted on the turn) and the audit quota decision `"unknown"`, and settlement is skipped with a warning when the turn's reserve fields are NULL. A retry/edit turn whose setup fails before the reserve is finalized by `StreamService::fail_unstarted_turn`: a CAS to `failed` (`turn_setup_failed`, `context_length_exceeded` or, after the reserve re-check, `quota_exceeded`) with no settlement and no outbox event, since no reserve was taken. |
| Deprecated configuration fields | Appendix B | Accepted until removal | `cleanup_worker.{enabled, poll_interval_secs, reconcile_interval_secs, stale_in_progress_timeout_secs, batch_size}` and `thread_summary_worker.reconcile_interval_secs` are parsed, not validated, and have no effect. A warning naming the field is logged at startup when one is set to a non-default value. The cleanup and thread-summary work runs as outbox handlers. The fields are kept for a staged removal: `orphan_watchdog`, `thread_summary_worker` and `cleanup_worker` do not use `deny_unknown_fields`, so a removed key would be silently ignored; keeping the field lets the startup warning tell operators that the setting has no effect before the field is dropped. |
| Removed configuration fields | Appendix B | Removed | `providers.<id>.supports_file_search_filters` and `streaming.web_search_context_size` are removed. `providers.<id>` and `streaming` use `deny_unknown_fields`, so a config that still contains either key fails validation at startup. The `web_search` tool takes its search context size from the catalog entry's `web_search_context_size`. |

### Consequences

* Good, because the documents no longer claim guarantees the database does not enforce.
* Bad, because clock skew above the watchdog timeout, or a replay client that depends on `downgrade_reason`, can still observe the gaps listed above.

### Confirmation

* `mini-chat/src/infra/db/repo/turn_repo.rs` tests cover the stale-progress fallback to `started_at` (`find_orphan_candidates_includes_stale_turn_without_progress`, `find_orphan_candidates_excludes_recent_turn_without_progress`).
* `mini-chat/src/domain/service/replay.rs` tests pin the replay event set (`replay_turn_happy_path`: `stream_started`, `delta`, `done`) and the rebuilt downgrade fields (`replay_turn_downgrade_detected`, `replay_turn_no_downgrade`).

## More Information

* Revisit the watchdog clock if multi-region deployments with independent clocks are introduced.
* Revisit replay immutability together with citation persistence.

## Traceability

* **DESIGN**: [DESIGN.md](../DESIGN.md) §3.3, §3.7, §4, §5.7, Appendix B

This decision directly addresses the following requirements or design elements:

* `cpt-cf-mini-chat-component-orphan-watchdog`
* `cpt-cf-mini-chat-design-turn-lifecycle`
* `cpt-cf-mini-chat-dbtable-chat-turns`
* `cpt-cf-mini-chat-dbtable-attachments`
* `cpt-cf-mini-chat-contract-sse-streaming`
* `cpt-cf-mini-chat-nfr-resilience-recovery`
