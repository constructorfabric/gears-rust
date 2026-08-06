# ADR — Identified White Spots And Next ADRs Scope

**Source:** Consistency review of `DESIGN.md` against `PRD.md`
**Date:** 2026-01-21
**Updated:** 2026-05-14 (renumbered ADR-2–7 → ADR-3–8 after ADR-0002 JSON-RPC/MCP was written; then ADR-3–8 → ADR-5–10 after ADR-0003 Workflow DSL and ADR-0004 Temporal Workflow Engine were written; then ADR-5–10 → ADR-6–11 after ADR-0005 Thin Host Gear, Fat Runtime Plugins was written); 2026-07-30 (section 3 added — gaps found during the consumer-SDK review)

---

## 1. Unaddressed PRD Requirements

### P0 Requirements Not Fully Addressed

| PRD ID | Requirement                                | Gap Description                                                                                                                                                                                                  | Severity    | New |
|--------|--------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------------|-----|
| BR-006 | Execution identity context                 | No model for selecting execution identity (system / API client / user) per function; `SecurityContext` is passed through but function definitions have no field to declare or constrain which identity the execution runs under; critical for scheduled and event-triggered executions that have no real-time caller | **Blocker** | Yes |
| BR-025 | Secure handling of secrets                 | No `secret_ref` type or secret binding model for workflows to reference secrets                                                                                                                                  | **Blocker** |     |
| BR-030 | Execution error boundaries                 | No error boundary mechanism to contain failures within specific workflow sections and prevent cascading failures; the PRD's mandatory requirement has no corresponding domain concept                                     | **Blocker** | Yes |
| BR-038 | Injection attack prevention                | No input sanitization rules or validation constraints in schema definitions                                                                                                                                      | **Blocker** |     |
| BR-039 | Privilege escalation prevention            | No privilege scope constraints or execution identity validation model                                                                                                                                            | **Blocker** |     |
| BR-008 | Runtime capabilities (HTTP, events, audit) | No SDK/capability interface for workflow authors to invoke platform services                                                                                                                                      | High        |     |
| BR-009 | Durability / suspension policy             | `max_suspension_days` exists on function but PRD requires tenant-level configurable suspension policy with three handling options (auto-cancel with notification, indefinite suspension, escalation); `TenantRuntimePolicy` lacks this; state machine only shows `suspended → failed` on timeout | High        | Yes |
| BR-013 | Long-running credential refresh            | No model for token refresh or credential lifecycle in security context                                                                                                                                           | High        |     |
| BR-017 | Data protection and privacy controls       | No sensitive field annotations, data classification model, or controls for restricting who can view sensitive inputs/outputs in execution history; broader requirement than BR-025 (secrets)                       | High        | Yes |
| BR-023 | Audit log integrity                        | No model for protecting audit records from unauthorized modification/deletion or ensuring availability within configured retention period                                                                          | High        | Yes |
| BR-026 | State consistency                          | No concurrency control or consistency guarantee model for concurrent operations and system failures; checkpointing strategy alone does not address "no partial updates or corrupted states" requirement            | High        | Yes |
| BR-034 | Audit trail for definition changes         | Implementation considerations mention audit but no audit event schema for definition CRUD operations (create, modify, enable/disable, delete) with required tenant/actor/correlation fields                       | High        | Yes |
| BR-040 | Resource exhaustion protection             | Limits defined but no detection/termination model for spinning loops or memory leaks                                                                                                                             | High        |     |
| BR-033 | Encryption controls                        | No encryption-at-rest or in-transit specifications in the domain model; may be infrastructure-level but the ADR should reference the requirement and declare expectations                                          | Medium      | Yes |

### P1 Requirements Not Addressed

| PRD ID | Requirement                                     | Gap Description                                                                                         | New |
|--------|-------------------------------------------------|---------------------------------------------------------------------------------------------------------|-----|
| BR-123 | Extensible sharing                              | `OwnerRef` defines default visibility but no sharing mechanism or extension point for cross-user/group/tenant sharing | Yes |
| BR-136 | Graceful disconnection handling (promoted to P0) | No adapter health model or API for rejecting starts when adapter disconnected                           |     |
| BR-101 | Debugging with breakpoints                      | No debugging API or breakpoint model                                                                    |     |
| BR-102 | Step-through execution                          | No step-through control model                                                                           |     |
| BR-104 | Child workflows / modular composition           | No parent-child invocation relationship model                                                           |     |
| BR-105 | Parallel execution with concurrency caps        | No parallel execution model or concurrency controls for steps                                           |     |
| BR-108 | External signals and manual intervention        | Partial (suspend/resume exists), but no signal delivery model                                           |     |
| BR-109 | Alerts and notifications                        | No notification/alert model or subscription mechanism                                                   |     |
| BR-114 | Dependency management                           | No dependency declaration or compatibility model                                                        |     |
| BR-115 | Distributed tracing                             | `trace_id` field exists but no integration or propagation model                                         |     |
| BR-117 | Environment customization (timezone, locale)    | No execution environment configuration model                                                            |     |
| BR-119 | Monitoring dashboards                           | No dashboard model (implementation concern)                                                             |     |
| BR-120 | Performance profiling                           | No profiling model or data schema                                                                       |     |
| BR-121 | Blue-green deployment                           | No deployment strategy model                                                                            |     |
| BR-122 | Publishing governance                           | No review/approval workflow model                                                                       |     |
| BR-125 | Workflow visualization                          | No visualization data model or API                                                                      |     |
| BR-127 | Debugging access control with sensitive masking | No sensitive field annotations in schemas                                                               |     |
| BR-129 | Standardized error taxonomy                     | Base error type exists; specific error types not enumerated                                             |     |
| BR-130 | Debug call trace (masked secrets)               | Call trace not modeled; masking rules not defined                                                       |     |

### P2 Requirements Not Addressed

| PRD ID | Requirement                   | Note                         |
|--------|-------------------------------|------------------------------|
| BR-201 | Long-term archival            | Future scope                 |
| BR-202 | Import/export                 | Future scope                 |
| BR-203 | Execution time travel         | Future scope                 |
| BR-204 | A/B testing                   | Future scope                 |
| BR-205 | Canary releases               | Future scope                 |
| BR-206 | Stronger isolation boundaries | Future scope (sandbox model) |

---

## 2. Next ADR Scope (Recommended)

> **ADR-0002** (`0002-cpt-cf-serverless-runtime-adr-jsonrpc-mcp-protocol-surfaces-v1.md`) has been written and covers JSON-RPC 2.0 and MCP protocol surfaces (BR-209–212).
> **ADR-0003** (`0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md`) has been written and adopts the Serverless Workflow Specification as the workflow DSL.
> **ADR-0004** (`0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md`) has been written and selects Temporal as the durable execution backend.
> **ADR-0005** (`0005-cpt-cf-serverless-runtime-adr-thin-host.md`) has been written and commits to a thin host gear with fat runtime plugins (rejecting the PR 1279 three-tier orchestrator boundary).
> The ADRs below are renumbered accordingly.

### ADR-2 (Completed): JSON-RPC/MCP Protocol Surfaces

See [ADR-0002](ADR/0002-cpt-cf-serverless-runtime-adr-jsonrpc-mcp-protocol-surfaces-v1.md).

### ADR-3 (Completed): Serverless Workflow Specification as Workflow DSL

See [ADR-0003](ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md).

### ADR-4 (Completed): Temporal-based Workflow Engine

See [ADR-0004](ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md).

### ADR-5 (Completed): Thin Host Gear, Fat Runtime Plugins

See [ADR-0005](ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md).

### ADR-6 (Next): Security Model (P0 — Blocker)

**Scope:**

- Execution identity model: how functions declare and constrain execution context (system / API client / user);
  how scheduled and event-triggered executions resolve identity (BR-006)
- Credential lifecycle and token refresh model for long-running workflows (BR-013)
- Secret reference model (`secret_ref` type) and secret binding for functions (BR-025)
- Privilege scope constraints and execution identity validation (BR-039)
- Input sanitization rules and injection prevention patterns (BR-038)
- Data protection model: sensitive field annotations, data classification, masking rules (BR-017, BR-127, BR-130)
- Encryption controls: reference infrastructure requirements for at-rest and in-transit encryption (BR-033)
- Audit log integrity model: protection from modification/deletion, retention guarantees (BR-023)
- Audit event schema for definition CRUD and execution lifecycle events with tenant/actor/correlation fields (BR-034)
- Security context propagation to individual workflow/function steps (BR-024)
- Sandbox isolation model and boundaries

**PRD Coverage:** BR-006, BR-013, BR-017, BR-023, BR-024, BR-025, BR-033, BR-034, BR-038, BR-039, BR-127, BR-130, PRD Risks

### ADR-7: Runtime Capabilities SDK (P0 — High Priority)

**Scope:**

- Capability interface for workflows (HTTP client, event publisher, audit logger)
- Platform operation invocation model
- Resource exhaustion detection and termination model (CPU spinning, memory leaks, excessive I/O)
- Adapter health model and disconnection handling (reject new starts, graceful in-flight handling)

**PRD Coverage:** BR-008, BR-040, BR-136

### ADR-8: Debugging and Observability (P1)

**Scope:**

- Debugging API (breakpoints, step-through, inspection)
- Debug session model and access control
- Call trace schema with duration and masked I/O
- Performance profiling data model
- Distributed tracing propagation model

**PRD Coverage:** BR-101, BR-102, BR-115, BR-120, BR-130

### ADR-9: Advanced Workflow Patterns (P1)

**Scope:**

- Parent-child workflow relationship model
- Parallel execution and concurrency control model
- External signal delivery to suspended workflows
- Dependency declaration and compatibility
- Error boundary mechanisms for containing failures within workflow sections (BR-030)
- State consistency model for concurrent operations and system failures (BR-026)
- Suspension timeout policy: tenant-level configurable handling options (auto-cancel, indefinite, escalation) (BR-009)

**PRD Coverage:** BR-009, BR-026, BR-030, BR-104, BR-105, BR-108, BR-114

### ADR-10: Deployment and Governance (P1)

**Scope:**

- Blue-green deployment strategy model
- Publishing governance (review/approval) workflow
- Alerts and notification model
- Execution environment customization (timezone, locale)
- Extensible sharing model for cross-user/group/tenant definition access (BR-123)

**PRD Coverage:** BR-109, BR-117, BR-121, BR-122, BR-123

### ADR-11: Error Taxonomy (P1)

**Scope:**

- Enumerate specific error types for all failure categories
- Error code registry and documentation
- Error-to-retry-policy mapping
- **Authorization error type** — see gap G-01 in section 3

**PRD Coverage:** BR-129

---

## 3. Gaps Found During Consumer-SDK Review (2026-07-30)

Surfaced while writing `serverless-sdk/docs/PRD.md` §1–§4 against these host documents. Each
is a host-side decision; the consumer SDK forwards or works around rather than inventing an
answer, so none is resolved here.

| ID | Gap | Impact | Where |
|----|-----|--------|-------|
| G-01 | **No authorization error type.** The invocation contract enumerates five error types (`not_found`, `not_active`, `validation`, `quota_exceeded`, `sync_suspension`) and none of them means "not authorised", although authorisation is asserted throughout the design. Three further reachable conditions are also unenumerated: a forbidden control transition, no plugin registered for a callable's adapter type, and plugin unavailability. | The consumer SDK declares `AccessDenied`, `UnsupportedControl`, `NoPluginAvailable` and `ServiceUnavailable` variants with no host error type to map onto, so in-process and REST behaviour cannot yet be proven to agree for those four. | `DESIGN_GTS_SCHEMAS.md` error catalog; fold into ADR-11 and/or ADR-6 |
| G-02 | **`retry` has no state transition.** `InvocationControlAction::Retry` is defined as "retry a failed invocation with same parameters", but the status state machine gives `failed` no outgoing edge back to `queued`/`running` — only to `compensating` or `dead_lettered`. So it is undefined whether `retry` resumes the same invocation or mints a new one, as `replay` explicitly does. Note also that ADR-0005 assigns retry *execution* to the plugin; what the host owns is the `RetryPolicy` schema on the function definition. | If `retry` mints a new invocation ID, it is not a control action at all and belongs beside `replay` — which changes the published consumer trait's shape. | `DESIGN.md` §Invocation Status State Machine; `DESIGN_RUST_TYPES.md` `InvocationControlAction` |
| G-03 | **SDK directory names do not match crate names.** On disk the crate directories are `serverless-sdk/` and `serverless-plugin-sdk/`, while every document calls the crates `serverless-runtime-sdk` and `serverless-runtime-plugin-sdk`. Elsewhere in the repo the directory equals the crate name (`gears/credstore/credstore-sdk/`), as it already does for `serverless-runtime/serverless-runtime/`. | Cosmetic today (no `Cargo.toml` exists yet), but it will become a real inconsistency the moment the crates are scaffolded. Decide before F-01. | `gears/serverless-runtime/` layout |
| G-04 | **ADR-0002 filename carries a `-v1` suffix its declared ID does not.** The file is `0002-…-jsonrpc-mcp-protocol-surfaces-v1.md` while its `**ID**` is `cpt-cf-serverless-runtime-adr-jsonrpc-mcp-protocol-surfaces`. ADRs 0001 and 0003–0005 have matching filenames and IDs. | Anyone citing the ADR by its filename stem produces an ID that does not exist, which breaks ID-integrity checks. | `docs/ADR/0002-*.md` |
| G-05 | **What an idempotency-deduplicated request returns is unspecified, and it is indistinguishable from a cache hit.** Two separate mechanisms exist: the `Idempotency-Key` header "prevents duplicate starts" within `deduplication_window_seconds` (§ Invocation request, BR-134), while response caching additionally requires `traits.is_idempotent: true` **and** `traits.caching.max_age_seconds > 0` (§ Response Caching, BR-118/BR-132). Caching states that a hit returns the original record with `cached: true`. Deduplication states no response shape at all — whether the caller receives the original invocation, an error, or a new record pointing at the original is undefined. | A caller that retries a request it is unsure about cannot tell whether its work was started, deduplicated onto an earlier run, or served from cache. Only the cache case is observable, so the deduplication case is silent. Blocks the consumer SDK's `cpt-cf-serverless-runtime-sdk-fr-idempotency`, which requires the caller to be able to tell. | `DESIGN.md` §Invocation request / §Response Caching |
| G-06 | **Whether `failed` and `canceled` are terminal is self-contradictory.** The status state machine gives `failed → compensating \| dead_lettered` and `canceled → compensating \| [*]`, so neither is an end state when compensation is configured. But prose calls them terminal in two places: "Replay is valid from `succeeded` or `failed` terminal states" (line 481) and "When a Workflow invocation enters a terminal `failed` or `canceled` state" (line 1108). | No caller can reliably answer "has this run finished, and did it succeed?" for 2 of the 9 states — the two that matter most for failure handling. Blocks the consumer SDK's `cpt-cf-serverless-runtime-sdk-fr-run-states`. Resolving it needs one statement per state, most likely "terminal unless a compensation handler is configured". | `DESIGN.md` §Invocation Status State Machine, lines 481 and 1108 |
