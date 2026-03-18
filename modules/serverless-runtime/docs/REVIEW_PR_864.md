# Chief Architect Review — PR #864

**PR:** `feat(serverless-runtime): restructure domain model docs, add callable type hierarchy ADR`
**Author:** bsacrobatix
**Reviewer:** Chief Architect (automated review)
**Date:** 2026-03-18

---

## 1. PR Scope Summary

This PR makes 6,207 additions and 5,077 deletions across 8 files:

| File | Change | Purpose |
|------|--------|---------|
| `ADR/0001-cpt-cf-serverless-runtime-adr-callable-type-hierarchy.md` | +145 new | ADR: Function as base callable type (Function → Workflow hierarchy) |
| `ADR_DOMAIN_MODEL_AND_APIS.md` | -3,859 deleted | Migrated to DESIGN.md + companion files |
| `DESIGN.md` | +3,036 new | Consolidated technical design with IEEE 42010 architecture views |
| `DESIGN_GTS_SCHEMAS.md` | +1,664 new | Extracted GTS JSON Schema definitions |
| `DESIGN_RUST_TYPES.md` | +878 new | Extracted Rust domain types and runtime traits |
| `NEXT_ADR_SCOPE.md` | +7/-7 | Terminology alignment (entrypoint → function) |
| `PRD.md` | +476/-414 | Restructured PRD with glossary, heading hierarchy, expanded NFRs, CyberFabric ID alignment |
| `PRD_ORIG.md` | -796 deleted | Archived original PRD |

---

## 2. Artifizer's Review Comments

Artifizer made **2 inline comments** on the callable type hierarchy ADR, both resolved:

### Comment 1 — Option D: Dual Base Types
> "Did we consider option D when we have both function and workflow as base types and maintain schema consistency separately (not by GTS?)"

Artifizer proposed maintaining `function` and `workflow` as independent peer types with schema consistency managed outside GTS inheritance — i.e., convention-based rather than type-system-enforced consistency.

### Comment 2 — Naming Brevity for GTS IDs
> "I expect we'll have hundreds of functions, so I'd consider to shorten the `serverless.function` to something like `srvless.func` ... and then `srvless.wflow`"

Artifizer expressed concern about GTS type ID verbosity at scale, suggesting abbreviations for IDE readability.

**Artifizer approved the PR** after these comments and requested review from nonameffh.

---

## 3. Assessment of Artifizer's Comments

### 3.1 Option D (Independent Peer Types) — Architecturally Sound, Deserves Deeper Consideration

Artifizer's suggestion aligns with how every major cloud platform structures its serverless offerings:

| Platform | Function Type | Orchestration Type | Relationship |
|----------|--------------|-------------------|--------------|
| AWS | Lambda Function | Step Functions State Machine | **Independent peers** |
| Google Cloud | Cloud Function | Cloud Workflow | **Independent peers** |
| Azure | Azure Function | Durable Orchestration | **Independent peers** (shared host) |
| Temporal | Activity | Workflow | **Independent peers** |

The current ADR's Option A (Function → Workflow) claims "a workflow is a type of function." While philosophically defensible (both accept inputs and produce outputs), this creates practical problems:

1. **Liskov Substitution Principle (LSP)**: Code expecting a function (bounded latency, stateless, sync-safe) will break when given a workflow. The DESIGN.md itself acknowledges this — sync invocation of a workflow either skips durability or rejects the request. This is a behavioral contract violation.

2. **Negative matching burden**: The ADR acknowledges adapters wanting only plain functions need "negative matching" (exclude `workflow.v1~`). This is a code smell — if you need to exclude subtypes, the inheritance hierarchy is likely wrong.

3. **The `entrypoint` field contradiction**: The Function base type has a `traits.entrypoint` boolean (can be invoked externally vs. internal-only). If the base type is called "Function" and not all functions are entrypoints, the original "Entrypoint" naming was indeed wrong — but the fix should be a neutral base type (`Callable`), not overloading "Function" to mean "anything that can be invoked."

**Recommendation:** Revisit this decision. Option C (`Callable → Function | Workflow`) or Artifizer's Option D both avoid the LSP violation while preserving polymorphic invocation where genuinely needed. The ADR's dismissal of Option C ("Callable is a synonym for function") is debatable — `Callable` carries different semantics than `Function` in most programming ecosystems (Python's `Callable`, Java's `Callable<V>`).

**Severity: HIGH — Foundational type hierarchy decision that cascades through the entire codebase.**

### 3.2 Naming Brevity (srvless.func) — Should Be Rejected at Type ID Level

GTS type IDs are machine-resolved identifiers, not display strings. Abbreviations introduce ambiguity:
- `func` = function? functor? functional interface?
- `wflow` = workflow? waterflow?

**However**, the underlying concern about IDE readability at scale is valid. The correct solution is a **display name registry** — a mapping from canonical GTS IDs to human-friendly short labels, maintained separately from the type system. This is analogous to:
- Kubernetes: resource type `persistentvolumeclaim` → alias `pvc`
- DNS: FQDN `api.us-east-1.prod.example.com` → CNAME `api.example.com`

**Recommendation:** Keep canonical GTS IDs unabbreviated. Add a separate display alias mechanism if IDE tooling requires shorter names.

**Severity: LOW — Cosmetic concern with a well-known solution pattern.**

---

## 4. Chief Architect Design Review — Industry Criteria Assessment

### 4.1 Document Structure and Standards Alignment

**Positive:**
- DESIGN.md follows IEEE 42010 architecture views (context, functional, information, deployment)
- ADR follows MADR format with proper decision drivers, options analysis, and traceability
- PRD restructured with proper glossary, actor model, and CyberFabric ID alignment (`cpt-cf-serverless-runtime-*`)
- Extraction of GTS schemas and Rust types into companion files is a significant improvement — keeps the design document readable while making schemas machine-extractable
- RFC 9457 Problem Details for error responses is industry-standard
- CEL subset for event filtering is well-chosen (industry standard from Google)
- OData-style filtering and cursor-based pagination follow established patterns

**Gaps:**
- No OpenAPI spec yet (acknowledged in Non-Applicable Domains section — acceptable for this stage)
- No C4 or arc42 component diagrams — Mermaid state machines are present but component interaction diagrams are missing
- No explicit conformance to TOGAF ADM or Zachman Framework alignment (minor — IEEE 42010 is sufficient)

### 4.2 Cross-Document Consistency (GTS ID Integrity)

This is the most critical issue found. The PR introduces a new type hierarchy (`function.v1~` as base) but the Starlark ADR (from a previous PR, not part of this diff) uses different IDs. The automated reviewers (Qodo, CodeRabbit) flagged these:

| Issue | Where | Expected (per DESIGN.md) | Actual (per ADR_STARLARK_RUNTIME.md) | Severity |
|-------|-------|--------------------------|--------------------------------------|----------|
| Base type ID mismatch | ADR Starlark line 382-388 | `gts.x.core.serverless.function.v1~` | `gts.x.core.serverless.func.v1~` | **Critical** |
| Error ID namespace drift | ADR Starlark line 240-243 | `err.v1~x.core.serverless.err.*` | `err.v1~x.core._.*` (placeholder) | **Critical** |
| Undefined error type | DESIGN.md line 4742 (per Qodo) | Should be in error taxonomy | `err.v1~x.core.errors.runtime_timeout.v1~` (wrong namespace) | **High** |

**Impact:** These are not documentation-only issues. GTS IDs are machine-resolved. Any code implementing against these documents will produce runtime type resolution failures if the IDs don't match exactly.

**Recommendation:** Establish a **single canonical GTS ID registry** (e.g., a `gts-registry.json` or dedicated section in DESIGN_GTS_SCHEMAS.md) that is the authoritative source. All other documents reference it by inclusion, not by copy. This prevents drift.

**Note:** The within-PR documents (DESIGN.md, DESIGN_GTS_SCHEMAS.md, DESIGN_RUST_TYPES.md, ADR callable hierarchy) are **internally consistent** with each other. The drift is between this PR's new documents and the existing Starlark ADR (separate PR). The Starlark ADR should be updated to match.

### 4.3 Domain Model Completeness

**Well-designed entities:**

| Entity | Assessment |
|--------|-----------|
| Function (base type) | Comprehensive: schema, traits, implementation, owner, versioning, status lifecycle |
| Workflow | Clean extension via `workflow_traits` (compensation, checkpointing, suspension) |
| InvocationRecord | Full lifecycle tracking with observability (correlation_id, trace_id, span_id, metrics) |
| Schedule | Complete: cron/interval, missed policies (skip/catch_up/backfill), concurrency (allow/forbid/replace) |
| Trigger | Event-driven with CEL filtering, batching, DLQ, connection health monitoring |
| Webhook Trigger | Multiple auth types (HMAC, bearer, basic, API key), IP allowlisting |
| TenantRuntimePolicy | Quotas, retention, runtime allowlist, outbound domain allowlist, idempotency config |

**Notable design strengths:**
- Two-layer compensation model (function-level platform-managed + step-level executor-specific) is well thought out
- CompensationContext schema is detailed and actionable for handler authors
- Rate limiting as a plugin system (not hardcoded) with strategy GTS types is extensible
- Response caching with owner-scoped cache keys and strict activation conditions (idempotency key + is_idempotent + max_age > 0)
- Dry-run mode with synthetic invocation records and explicit "what it does NOT do" documentation
- `entrypoint` boolean trait for internal-only functions is a good capability (though see naming concern in 3.1)

**Missing domain concepts** (acknowledged in NEXT_ADR_SCOPE.md — tracking here for completeness):

| Gap | Impact | PRD Requirement | Next ADR |
|-----|--------|----------------|----------|
| Execution identity model | Cannot determine who a scheduled execution runs as | BR-006 (P0 Blocker) | ADR-2 |
| Secret reference type | No way for workflows to securely reference secrets | BR-025 (P0 Blocker) | ADR-2 |
| Error boundaries | Failures cascade across workflow sections | BR-030 (P0 Blocker) | ADR-5 |
| Input sanitization rules | Injection attack surface | BR-038 (P0 Blocker) | ADR-2 |
| Privilege scope constraints | Privilege escalation risk | BR-039 (P0 Blocker) | ADR-2 |
| SDK capability interface | No HTTP/event/audit API for workflow code | BR-008 (High) | ADR-3 |
| Credential lifecycle | Long-running workflows cannot refresh tokens | BR-013 (High) | ADR-2 |
| Adapter health model | Cannot reject starts when adapter disconnected | BR-136 (promoted to P0) | ADR-3 |
| Child workflow composition | No parent-child invocation model | BR-104 (P1) | ADR-5 |
| Parallel execution model | No step-level concurrency | BR-105 (P1) | ADR-5 |

### 4.4 API Design Quality

**Strengths:**
- RESTful with resource-action pattern (`POST /functions/{id}:validate`, `:publish`, `:deprecate`)
- Consistent response shapes (single resource, list with cursor pagination, RFC 9457 errors)
- Clear separation: Registry API, Invocation API, Schedule API, Trigger API, Tenant API
- Idempotency via `Idempotency-Key` header with configurable tenant deduplication window
- Function versioning with major/minor semantics and version pinning for in-flight executions
- Deprecation API with successor reference and sunset date

**Concerns:**

| Issue | Detail | Severity |
|-------|--------|----------|
| No API versioning strategy | Base URL is `/api/serverless-runtime/v1` but no discussion of how v2 would coexist | Medium |
| No bulk operations | No batch create/update/delete for functions, schedules, or triggers | Low |
| Register API uses raw GTS schema as request body | `POST /functions` takes a full GTS JSON Schema `definition` object — this is very different from a typical "create resource" API. No simpler "create from template" path exists | Medium |
| Error format inconsistency | Register errors use `{"error": "...", "message": "..."}` while invocation errors use RFC 9457. Should be consistently RFC 9457 everywhere | Medium |
| No HATEOAS / resource links | Response bodies don't include links to related resources (e.g., function → its schedules, invocation → its function) | Low |

### 4.5 NFR Analysis

The PRD defines ambitious NFR targets:

| NFR | Target | Assessment |
|-----|--------|-----------|
| Availability | >= 99.95% monthly | Requires multi-zone deployment at minimum. No HA design in DESIGN.md |
| Start latency | p95 <= 100ms | Achievable for warm-path; cold-start scenario not addressed |
| Step dispatch | p95 <= 50ms | Requires in-process or very low-latency executor dispatch |
| Visibility query | p95 <= 200ms | Standard for indexed DB queries |
| Schedule accuracy | Within 1s of scheduled time | Requires dedicated scheduler with sub-second tick |
| Execution success | >= 99.9% (excluding business logic) | Requires retry + compensation working correctly |
| RTO | <= 30s | Requires stateless API tier with fast failover |
| RPO | <= 1 min | Requires synchronous or near-synchronous state replication |
| Concurrent executions | >= 10K | Requires horizontal scaling model |
| Start throughput | >= 1K/s | Requires low-contention admission path |

**Missing from design documents:**

| Gap | Industry Standard | Impact |
|-----|-------------------|--------|
| **Capacity model** | AWS Well-Architected, Google SRE | Cannot validate 10K concurrent / 1K starts/sec without sizing model |
| **Backpressure / admission control** | Circuit breaker pattern, load shedding | What happens at 10,001 concurrent? Risk of cascading failure |
| **Cold-start strategy** | AWS Lambda provisioned concurrency, Azure pre-warmed instances | p95 latency target may fail on cold starts |
| **Multi-zone / DR design** | Active-active or active-passive with RPO < 1 min | RTO 30s + RPO 1 min is unachievable without explicit replication design |
| **Observability contract** | CNCF OpenTelemetry semantic conventions | The design mentions trace_id/span_id but doesn't specify OTel attribute names, meter names, or log formats |
| **SLO burn-rate alerting** | Google SRE error budget model | 99.95% requires proactive error budget tracking |
| **Graceful degradation strategy** | Bulkhead pattern | No discussion of what degrades first under load (schedule accuracy? start latency? observability?) |

### 4.6 Security Analysis

**Present in this PR:**
- Tenant isolation at every layer (registry, execution, secrets, queries)
- Code scanning for forbidden constructs (Starlark static analysis)
- URL allowlisting for outbound HTTP calls
- CEL filter sandboxing
- Webhook authentication with multiple methods + IP allowlisting + secret rotation
- Audit event envelope with tenant_id, actor_id, correlation_id

**Absent (acknowledged as P0 blockers in NEXT_ADR_SCOPE.md):**
- Execution identity model (BR-006)
- Secret reference type (BR-025)
- Input sanitization rules (BR-038)
- Privilege scope constraints (BR-039)
- Data classification / sensitive field masking (BR-017)
- Audit log integrity protection (BR-023)

**Recommendation:** Add a prominent warning banner at the top of DESIGN.md:
```
> **SECURITY MODEL INCOMPLETE** — This design document does not yet include the
> execution identity model, secret handling, injection prevention, or privilege
> constraints. See [NEXT_ADR_SCOPE.md](./NEXT_ADR_SCOPE.md) for the planned
> Security Model ADR (ADR-2, P0 Blocker). Do not implement authentication or
> authorization logic based on this document alone.
```

### 4.7 Starlark Runtime ADR (Not in This PR, but Referenced)

The existing Starlark ADR (from a prior PR) is referenced but creates the cross-document consistency issues noted in 4.2. Additionally:

| Gap | Detail |
|-----|--------|
| Resource limits mapping | How do Starlark CPU/memory limits map to the domain model's `Limits` type? The DESIGN_GTS_SCHEMAS.md defines `starlark.limits.v1~` with `memory_mb` and `cpu` — but the Starlark ADR should reference this schema |
| Escape hatch for I/O limitations | Starlark intentionally has no I/O, no threads. The DESIGN.md mentions HTTP client and event capabilities. The bridge between Starlark's restrictions and these runtime capabilities needs explicit documentation |
| Version pinning | Which Starlark specification version is the contract? |

### 4.8 Document Consolidation Assessment

**Positive:**
- Moving from a monolithic `ADR_DOMAIN_MODEL_AND_APIS.md` (3,859 lines) to separated concerns (DESIGN + schemas + Rust types) is the right architectural call
- DESIGN.md at 3,036 lines is large but manageable since it's well-sectioned with IEEE 42010 views
- Companion files (DESIGN_GTS_SCHEMAS.md, DESIGN_RUST_TYPES.md) are purely machine-extractable content — clean separation
- NEXT_ADR_SCOPE.md terminology updated consistently (entrypoint → function)

**Concerns:**
- `ADR_DOMAIN_MODEL_AND_APIS.md` deletion loses git blame history. Consider adding a redirect note in the deletion commit or keeping a stub that points to the new files
- `PRD_ORIG.md` deletion is fine — the content is superseded by the restructured PRD.md
- The PRD diff (+476/-414) is a structural reorganization (heading levels, glossary addition, ID prefix change from `cpt-serverless-runtime-` to `cpt-cf-serverless-runtime-`). Content is preserved — this is clean

---

## 5. Comparison with Industry Serverless Platforms

The DESIGN.md includes a detailed comparison table (Section 3, "Comparison with Similar Solutions"). Key differentiators:

| Aspect | This Design | Industry Norm | Assessment |
|--------|------------|---------------|-----------|
| Unified function/workflow schema | Single definition type with optional workflow_traits | AWS/GCP/Azure all split function and orchestration into separate products | **Differentiator** — reduces API surface but increases type system complexity |
| GTS-based type identity | All entities carry GTS IDs with schema validation | ARN/resource names without schema validation | **Differentiator** — stronger type safety, but adds GTS dependency |
| Structured debug endpoint | `GET /invocations/{id}/debug` with location + stack | No standard debug API; relies on logs/traces | **Differentiator** — significant DX improvement |
| Response caching | Built-in per-function caching with owner-scoped keys | Typically external (API gateway/CDN) | **Differentiator** — reduces integration complexity |
| Pluggable rate limiting | Plugin system with GTS-typed strategies | Typically built into API gateway | **Unusual** — adds flexibility but also complexity |
| Pluggable executors | Abstract `ServerlessRuntime` trait with adapter model | Typically single executor per product | **Differentiator** — enables technology flexibility |

---

## 6. Summary Findings Table

| # | Finding | Category | Severity | Status | Recommendation |
|---|---------|----------|----------|--------|---------------|
| F-01 | Type hierarchy (Function → Workflow) violates LSP | Architecture | **High** | Open | Revisit: adopt Callable base (Option C) or independent peers (Option D / Artifizer) |
| F-02 | GTS ID mismatch: `func.v1~` vs `function.v1~` between Starlark ADR and DESIGN.md | Consistency | **Critical** | Unresolved (flagged by Qodo) | Align Starlark ADR to use `function.v1~`; establish canonical ID registry |
| F-03 | Error ID namespace drift: `x.core._.*` vs `x.core.serverless.err.*` between Starlark ADR and DESIGN.md | Consistency | **Critical** | Unresolved (flagged by Qodo) | Align Starlark ADR error IDs to canonical taxonomy |
| F-04 | Undefined error type `x.core.errors.runtime_timeout.v1~` in DESIGN.md example | Correctness | **High** | Unresolved (flagged by Qodo) | Fix to `x.core.serverless.err.runtime_timeout.v1~` |
| F-05 | Security model incomplete (5 P0 blockers) | Security | **High** | Acknowledged (NEXT_ADR_SCOPE.md) | Add security incomplete banner to DESIGN.md |
| F-06 | No capacity model backing NFR targets (10K concurrent, 1K starts/sec) | Performance | **Medium** | Not addressed | Add capacity sizing section or defer to implementation ADR |
| F-07 | No backpressure / admission control design | Reliability | **Medium** | Not addressed | Define load shedding behavior at capacity limits |
| F-08 | No multi-zone / DR design for RPO <= 1 min | Reliability | **Medium** | Not addressed | Add HA/DR section or defer to deployment ADR |
| F-09 | No OpenTelemetry semantic conventions for observability | Observability | **Medium** | Not addressed | Define OTel attribute names, meter names, log formats |
| F-10 | Error format inconsistency (RFC 9457 vs custom `{"error":...}`) in Registry API | API Design | **Medium** | Not addressed | Standardize all errors to RFC 9457 |
| F-11 | Duplicate glossary entry: "Function" and "Function (base type)" / "Callable Base Type" | Documentation | **Low** | Unresolved (flagged by CodeRabbit) | Consolidate to single clear definition |
| F-12 | Stale section reference: "Section 4" should be "Section 5" for use cases | Documentation | **Low** | Unresolved (flagged by CodeRabbit) | Fix cross-reference |
| F-13 | BR-136 priority inconsistency: promoted to P0 but listed under P1 | Documentation | **Low** | Unresolved (flagged by CodeRabbit) | Move to P0 section or add explicit note |
| F-14 | Typo in Starlark ADR: "exists on error" should be "exits on error" | Documentation | **Low** | Resolved | Already fixed |
| F-15 | Validation timing: Starlark ADR incorrectly says params validated at registration | Correctness | **Low** | Resolved | Already fixed |
| F-16 | No component interaction diagrams (C4 or similar) | Documentation | **Low** | Not addressed | Add Mermaid sequence/component diagrams |
| F-17 | Git blame history lost for deleted ADR_DOMAIN_MODEL_AND_APIS.md | Process | **Low** | Inherent to restructuring | Acceptable — content is preserved in DESIGN.md |
| F-18 | GTS type names are verbose for IDE display at scale (Artifizer concern) | DX | **Low** | Acknowledged | Keep canonical IDs unabbreviated; add display alias mechanism separately |

---

## 7. Verdict

### Blocking Issues (must resolve before merge)

1. **F-02, F-03: GTS ID consistency** — Cross-document type ID mismatches between Starlark ADR and this PR's DESIGN.md will cause runtime type resolution failures. Either update the Starlark ADR in this PR or create a follow-up PR that must merge simultaneously.

2. **F-04: Undefined error type** — The `x.core.errors.runtime_timeout.v1~` reference in DESIGN.md is neither in the error taxonomy nor follows the `x.core.serverless.err.*` namespace convention. Fix the example.

### Strongly Recommended (should resolve before merge)

3. **F-01: Type hierarchy** — The Function → Workflow hierarchy has LSP concerns and diverges from all major industry precedents. At minimum, document the LSP trade-off explicitly in the ADR's "Consequences" section. Ideally, reconsider Option C or D.

4. **F-05: Security incomplete banner** — Add a visible warning to DESIGN.md that the security model is pending.

5. **F-10: Error format consistency** — Standardize all API error responses to RFC 9457 before the API contract is consumed by implementers.

### Acceptable to Defer

6. All other findings (F-06 through F-09, F-11 through F-18) are real but can be addressed in follow-up PRs without creating implementation risk.

### Overall Assessment

**REQUEST CHANGES** — The document restructuring is a significant improvement in organization and readability. The domain model is thorough and well-designed. The API contracts are comprehensive. However, two blocking issues (GTS ID consistency and undefined error type) and the type hierarchy concern must be resolved before this becomes the canonical design reference.
