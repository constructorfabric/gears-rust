# Architecture Review: PR #619 — Usage Collector PRD & DESIGN

**Reviewer**: Chief Architect
**Date**: 2026-03-18
**PR**: cyberfabric/cyberfabric-core#619 (`docs(usage-collector): PRD and DESIGN`)
**Scope**: PRD.md, DESIGN.md, ADR-0001, ADR-0002

---

## Executive Summary

The Usage Collector design is ambitious and well-structured. It establishes a centralized metering layer with sound fundamentals: source-side persistence via outbox, fail-closed authorization, and pluggable storage backends. However, the design has a **critical architectural gap** around quota-enforced usage types and several structural issues that need resolution before implementation begins.

**Verdict**: Approve with required changes (see Critical and High items below).

---

## 1. CRITICAL: Quota Enforcement Gap — Direct `emit()` Bypass Risk

### The Problem

The design provides `authorize_emit()` → `emit()` as a two-phase pattern (ADR-0002), but `authorize_emit()` only checks **PDP authorization** (can this service emit this metric type?). It does NOT enforce **business-level quotas** (does this tenant have remaining capacity for AI tokens, API calls, etc.).

This creates a fundamental bypass vector:

```
// Any service can emit usage for quota-controlled resources
// without the quota system ever being consulted
let auth = client.authorize_emit(&ctx, &metric).await?;
client.emit(&tx, auth, record).await?;  // ← quota never checked
```

For metrics like `mini-chat.tokens` or `api-gateway.calls`, this means usage can be recorded that exceeds tenant quotas, leading to overbilling disputes or resource exhaustion.

### Why This Matters

The existing codebase already solves this correctly in **mini-chat** via the `preflight_reserve()` → work → `settle()` pattern (`modules/mini-chat/mini-chat/src/domain/service/quota_service.rs`). But the Usage Collector design doesn't account for the fact that some usage types MUST flow through quota enforcement, while others (monitoring counters, audit logs) are quota-free.

### Recommended Solution: Usage Type Enforcement Classification

Introduce an enforcement mode at the **usage type registration** level in `types-registry`:

```rust
/// Declared when a usage type is registered in types-registry.
enum EmitEnforcementMode {
    /// Usage MUST be emitted via a quota-aware settlement path.
    /// Direct emit() calls for this type are rejected by the SDK.
    /// The quota service calls emit() internally after settlement.
    QuotaGated,

    /// Usage can be emitted directly by any authorized source.
    /// No quota check required — suitable for monitoring/audit metrics.
    Direct,
}
```

**How it works:**

| Mode | Who calls `emit()` | Quota checked? | Distributed tx? |
|------|-------------------|----------------|-----------------|
| `QuotaGated` | Quota service internally, after `settle()` | Yes, before work begins via `preflight_reserve()` | **No** — quota reserve is pre-tx; settlement + outbox enqueue happen in caller's local tx |
| `Direct` | Any authorized source via SDK | No | No |

**For `QuotaGated` types**, the flow becomes:

```
1. Domain service → quota_service.preflight_reserve(ctx, metric, estimated)
   → Returns ReserveToken or Rejection (429/403)
2. Domain service does the work
3. Domain service → quota_service.settle(tx, reserve_token, actual_usage)
   → Internally: quota settlement + usage_collector.emit() in same local tx
4. Outbox delivers usage record to collector gateway
```

**For `Direct` types**, the existing flow works as-is:

```
1. Source → authorize_emit(ctx, metric)   // PDP only
2. Source → emit(tx, auth, record)        // In local tx
3. Outbox delivers to gateway
```

**Why no distributed transaction:**
- `QuotaGated` quota reservation happens BEFORE the transaction opens (same as ADR-0002's PDP call pattern).
- Settlement and usage emission happen in the **caller's local database transaction** — the outbox row is a local INSERT, not a remote call.
- The collector gateway processes the outbox message asynchronously — no synchronous cross-service transaction.

**SDK enforcement:**

```rust
impl ScopedUsageCollectorClientV1 {
    pub async fn emit(
        &self,
        tx: &DatabaseTransaction,
        auth: EmitAuthorization,
        record: UsageRecord,
    ) -> Result<(), EmitError> {
        // If the usage type is QuotaGated, verify the auth token
        // contains a SettlementProof from the quota service.
        // Reject if the caller is trying to bypass quota.
        if self.registry.enforcement_mode(&record.metric) == QuotaGated {
            auth.require_settlement_proof()?;
        }
        // ... proceed with outbox enqueue
    }
}
```

This design ensures that:
- **Bypass is impossible at the SDK level** — the type system enforces the constraint.
- **No distributed transactions** — all persistence is local; delivery is async.
- **Quota-free metrics remain simple** — no unnecessary coupling to quota infrastructure.

---

## 2. HIGH: Two-Phase Authorization TOCTOU Window (ADR-0002)

ADR-0002 acknowledges a TOCTOU window between `authorize_emit()` and `emit()`, bounded by request handler execution time (~500ms). This is acceptable for PDP policy changes, but becomes more significant when combined with the quota enforcement gap above.

**With the QuotaGated/Direct classification** (recommendation #1), this concern is mitigated:
- For `QuotaGated` types, the reservation is a resource lock with finite TTL, not just a policy check. The TOCTOU window is irrelevant because the reservation is consumed atomically during settlement.
- For `Direct` types, the existing bounded TOCTOU for PDP is acceptable.

**Remaining concern**: The `EmitAuthorization` token must include a **monotonic timestamp or nonce** to prevent replay. The current design says tokens are opaque and cannot be reused, but enforcement is via "code review." This should be a runtime check:

```rust
struct EmitAuthorization {
    issued_at: Instant,
    nonce: u64,
    // ... PDP constraints
}

impl EmitAuthorization {
    fn validate_freshness(&self) -> Result<(), EmitError> {
        if self.issued_at.elapsed() > MAX_AUTH_AGE {
            return Err(EmitError::AuthorizationExpired);
        }
        Ok(())
    }
}
```

---

## 3. HIGH: Counter Semantics — No Running Totals Table

The design stores counter values as individual delta records and derives totals via `SUM(value) WHERE status = 'active'`. This is correct for append-only time-series workloads, but has query performance implications:

**Problem**: Computing `SUM` over millions of active records for a single `(tenant_id, metric)` pair becomes expensive as data grows, even with ClickHouse.

**Recommendation**: Add a **materialized running-total view** (or ClickHouse `AggregatingMergeTree`) that incrementally maintains totals. This is an implementation detail the plugin can own, but the DESIGN should acknowledge it as an expected optimization and define the consistency model (eventual vs. strong).

The PRD's ≤500ms p95 query latency for 30-day ranges will be hard to guarantee without pre-aggregation for high-cardinality counter metrics.

---

## 4. HIGH: Backfill `tenant_id` Contradiction

The review comments correctly identified that `BackfillRequest` exposes a client-specified `tenant_id`, violating the stated constraint that tenant identity is always derived from `SecurityContext`. The DESIGN states:

> "Tenant identity is always derived from the authenticated SecurityContext, never from request payloads."

Yet backfill requests include `tenant_id` in the payload. This needs resolution:

- **Option A** (Recommended): Remove `tenant_id` from `BackfillRequest`. Derive from `SecurityContext`. Backfill for a different tenant requires impersonation through the standard platform impersonation mechanism.
- **Option B**: Keep `tenant_id` but validate it matches `SecurityContext.tenant_id` exactly, with a specific PDP permission for cross-tenant backfill.

---

## 5. MEDIUM: SDK Buffering vs. Zero-Loss Guarantee Contradiction

The PRD contains conflicting statements:
- Section on SDK behavior describes "drop-on-full" buffer semantics
- Acceptance criteria states "zero silent record loss"

These are mutually exclusive. The outbox pattern already solves this — records are persisted in the local DB transaction, so loss only occurs if the domain transaction itself fails (which is expected behavior).

**Recommendation**: Remove all references to in-memory buffering and drop-on-full semantics. The outbox IS the buffer, and it's durable by construction. The SDK should not have a secondary in-memory buffer. This aligns with the existing mini-chat outbox implementation (`modules/mini-chat/mini-chat/src/infra/outbox.rs`) which has no in-memory buffering layer.

---

## 6. MEDIUM: Missing Relationship Between Usage-Collector and Usage-Query

The PR title mentions both `usage-collector` and `usage-query` modules. The DESIGN describes query capabilities (aggregation, raw queries) as part of the collector gateway. If these are intended to be separate modules, the boundary needs to be explicit:

| Concern | Usage-Collector | Usage-Query |
|---------|----------------|-------------|
| Ingestion | Yes | No |
| Storage plugin management | Yes | ? |
| Aggregation queries | ? | Yes |
| Raw record queries | ? | Yes |
| Retention management | Yes | No |

**Recommendation**: Either (a) consolidate into a single module if the read/write paths share the same plugin and storage, or (b) clearly define the interface contract between collector and query modules, especially around plugin ownership and connection pooling.

---

## 7. MEDIUM: Rate Limiting Scope Ambiguity

The PRD defines "per-(source, tenant) emission quotas" for rate limiting. The existing codebase has two distinct rate limiting implementations:

1. **OAGW** (`modules/system/oagw/`): Token bucket with burst, per-route
2. **API Gateway** (`modules/system/api-gateway/`): `governor` crate, per-route

The Usage Collector should reuse the platform's rate limiting primitives rather than implementing its own. The DESIGN should specify:
- Whether rate limits are enforced at the **gateway** (HTTP/gRPC ingestion endpoint) or at the **SDK** level
- Whether the rate limiter state is local (per-gateway-instance) or distributed (shared across instances)
- How rate limit configuration is managed (types-registry? per-module config?)

---

## 8. MEDIUM: Dead Letter Queue Design Gap

The PR discussion raised concerns about DLQ and "zombie usage" after account deactivation. The response correctly notes that authorization fails before emission for deactivated accounts. However, the DLQ concern is valid for a different scenario:

**Scenario**: The outbox delivers a usage record to the collector gateway, but the gateway rejects it (schema validation failure, type not found, etc.). The outbox exhausts retries and the record lands in dead-letter store.

**Missing from DESIGN**:
- What triggers a dead letter? Only network failures, or also 4xx rejections?
- Who monitors and replays dead letters? Operational runbook?
- Should 4xx rejections (permanent failures) be separated from 5xx (transient)?
- Dead letter records should include the original rejection reason for debugging.

The existing modkit-db outbox has dead letter support (`libs/modkit-db/examples/outbox_dead_letters.rs`), but the DESIGN should specify the rejection classification strategy.

---

## 9. LOW: ADR-0001 Source Attribution — Defense-in-Depth

ADR-0001 correctly notes that source attribution via `for_module()` is "self-asserted at the SDK level rather than cryptographically bound." The current design accepts this for accidental-misuse prevention.

**Suggestion**: Consider adding server-side validation at the collector gateway. When the gateway receives a usage record, it can verify that the `source_module` in the record matches the `SecurityContext` of the delivering service. This doesn't require cryptographic binding — just a gateway-side check that the claimed source matches the authenticated identity. This is a defense-in-depth measure, not a blocking concern.

---

## 10. LOW: Outbox Partition Strategy

The DESIGN doesn't specify the outbox partition key strategy. The existing mini-chat implementation partitions by `tenant_id` hash for per-tenant ordering guarantees. The Usage Collector should define:

- Partition key: `tenant_id`, `source_module`, or `(tenant_id, source_module)`?
- Ordering guarantees: Are counter deltas order-sensitive? (They shouldn't be if idempotency is correct.)
- Partition count: Static or dynamic?

---

## Alignment with Existing Codebase Patterns

| Pattern | Existing (mini-chat) | Proposed (usage-collector) | Aligned? |
|---------|---------------------|---------------------------|----------|
| Outbox for durable delivery | `InfraOutboxEnqueuer` in local tx | SDK `emit()` in local tx | Yes |
| Two-phase quota | `preflight_reserve()` → `settle()` | `authorize_emit()` → `emit()` (PDP only) | **Partial** — missing quota phase |
| ClientHub registration | `register::<dyn Trait>(impl)` | `for_module()` scoped wrapper | Yes, but adds new wrapper concept |
| Fail-closed authorization | PDP via `authz-resolver-sdk` | Same PDP, 150ms timeout | Yes |
| Plugin via GTS | Tenant-resolver plugin discovery | Storage backend via GTS plugin | Yes |
| Error model | `DomainError` → RFC-9457 Problem | Not yet specified | **Gap** — needs error type catalog |

---

## Summary of Required Actions

| # | Severity | Issue | Action |
|---|----------|-------|--------|
| 1 | CRITICAL | Quota bypass via direct `emit()` | Add `EmitEnforcementMode` (QuotaGated/Direct) to usage type registration; SDK enforces |
| 2 | HIGH | TOCTOU token replay | Add runtime freshness + nonce validation to `EmitAuthorization` |
| 3 | HIGH | Counter query performance | Acknowledge materialized aggregation as plugin optimization; define consistency model |
| 4 | HIGH | Backfill `tenant_id` in payload | Remove from payload or validate against SecurityContext |
| 5 | MEDIUM | Buffer/drop vs. zero-loss conflict | Remove in-memory buffer references; outbox is the only buffer |
| 6 | MEDIUM | Usage-Collector vs. Usage-Query boundary | Define or consolidate |
| 7 | MEDIUM | Rate limit implementation strategy | Specify layer, state scope, and config source |
| 8 | MEDIUM | DLQ rejection classification | Specify permanent vs. transient failure handling |
| 9 | LOW | Source attribution defense-in-depth | Gateway-side source validation |
| 10 | LOW | Outbox partition strategy | Define partition key and ordering guarantees |

---

## Appendix: Quota-Enforced vs. Direct Emit — Decision Framework

Use this framework when registering a new usage type to decide its enforcement mode:

```
Is there a finite, enforceable quota for this usage type?
├── YES → QuotaGated
│   Examples: AI tokens, API calls (rate-limited plans),
│   storage bytes (capped tiers), compute hours
│   Flow: reserve → work → settle (emits internally)
│
└── NO → Direct
    Examples: login counts, page views, audit events,
    feature usage analytics, error counts
    Flow: authorize_emit → emit (standard two-phase)
```

**Key principle**: The Usage Collector is a **metering** system, not a **quota** system. Quota enforcement belongs to the domain services (or a dedicated quota module). But the Usage Collector SDK must be able to **refuse direct emission for quota-gated types** to prevent accidental bypass. The type classification makes the boundary explicit and enforceable at the SDK level.

---

*Review prepared against PR #619 at commit HEAD of `capybutler:feature/usage-collector-specs`*
