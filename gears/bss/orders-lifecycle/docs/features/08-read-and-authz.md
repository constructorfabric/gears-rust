# Feature: Read Surfaces and Authorization

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-featstatus-read-and-authz-implemented`
- [ ] `p1` - `cpt-cf-bss-orders-lifecycle-feature-read-and-authz`

## Table of Contents

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References and delivery boundaries](#14-references-and-delivery-boundaries)
- [2. Actor Flows](#2-actor-flows)
  - [2.1 Read an order and its children](#21-read-an-order-and-its-children)
  - [2.2 List and page authorized collections](#22-list-and-page-authorized-collections)
  - [2.3 Retrieve an authorized audit subset](#23-retrieve-an-authorized-audit-subset)
- [3. Processes / Business Logic](#3-processes--business-logic)
  - [3.1 Shared authorization enforcement](#31-shared-authorization-enforcement)
  - [3.2 Denials, outages and bounded internal authority](#32-denials-outages-and-bounded-internal-authority)
  - [3.3 Read access logging](#33-read-access-logging)
- [4. States](#4-states)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Shared permissions before public operation delivery](#51-shared-permissions-before-public-operation-delivery)
  - [5.2 Coherent, bounded reads and disclosure evidence](#52-coherent-bounded-reads-and-disclosure-evidence)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Reads and authorization: Interactions and Sequences](#reads-and-authorization-interactions-and-sequences)
  - [Reads and authorization: Traceability](#reads-and-authorization-traceability)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Serve current orders, scoped lists, historical versions, line fulfillment and authorized audit trails. Declare and enforce the shared platform authorization contract on every public REST and SDK operation.

### 1.2 Purpose

**Requirements**: `cpt-cf-bss-orders-lifecycle-fr-order-authorization`, `cpt-cf-bss-orders-lifecycle-fr-order-history`, `cpt-cf-bss-orders-lifecycle-fr-order-line-dates`, `cpt-cf-bss-orders-lifecycle-fr-order-subscription-linkage`, `cpt-cf-bss-orders-lifecycle-nfr-order-read-latency`, `cpt-cf-bss-orders-lifecycle-nfr-order-recovery`.

**Principles**: `cpt-cf-bss-orders-lifecycle-principle-read-row-not-chain`, `cpt-cf-bss-orders-lifecycle-principle-scope-by-relationship`, `cpt-cf-bss-orders-lifecycle-principle-no-internal-exposure`, `cpt-cf-bss-orders-lifecycle-principle-one-permission-model`.

Audit retrieval is design-introduced, not a PRD recording requirement. Product acknowledgement remains Q-20/D-70; this feature does not silently add a requirement basis.

### 1.3 Actors

| Actor / permission path | Role |
|-------------------------|------|
| `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin` | Delegated customer access within PDP-granted actions |
| `cpt-cf-bss-orders-lifecycle-actor-orders-direct-customer` | Own resource-tenant orders with explicit action grants, never membership alone |
| `cpt-cf-bss-orders-lifecycle-actor-orders-seller-operator` | Seller-scoped reads, audit and permitted operational actions |
| `cpt-cf-bss-orders-lifecycle-actor-orders-workflow` | Service-only operations and reads constrained to explicit authorized order IDs |
| `cpt-cf-bss-orders-lifecycle-actor-orders-idp-ams` | Identity/relationship evidence and delegation credential issuer; PDP decides permissions |

Payer Reader and configured event consumers are permission paths, not new audit actor classes. Payer access uses the current payer and grants reads only; consumer reads require finite explicit PDP order-ID grants. Root event access creates no order-read grant.

### 1.4 References and delivery boundaries

- [PRD](../PRD.md) §6.6, §9.1 and §11; [DESIGN](../DESIGN.md).
- [Decomposition](../DECOMPOSITION.md): `cpt-cf-bss-orders-lifecycle-feature-read-and-authz`.
- [Detailed architecture](../DESIGN.md#contract-08-1-1): complete matrix, action catalog, row scopes, response fields, logging schema and provider contracts.
- **Dependencies**: [Foundation](01-foundation.md), [Capture](02-capture.md), [Gate and pin](03-gate-and-pin.md), [Versioning](04-versioning.md), [Workflow seam](06-workflow-seam.md).

**Authorization is an early prerequisite.** Although the composed read feature depends on those later data producers, Foundation must deliver the shared adapter, write pre-guard, declared action mapping, tenant scope enforcement and bounded internal authority before exposing any mutation. Read delivery cannot be used to defer authorization or enable temporary permissive writes.

[UPSTREAM_REQS](../UPSTREAM_REQS.md) retains `cpt-cf-bss-orders-lifecycle-upreq-pdp-policy-integration`, `cpt-cf-bss-orders-lifecycle-upreq-delegation-proof-credential` and `cpt-cf-bss-orders-lifecycle-upreq-event-broker-root-tenancy`. The deployed provider, three-axis/proposed-value enforcement and service grants require separate evidence; fake PDP tests and permission registration do not establish those capabilities. [DECISIONS](../DECISIONS.md) retains Q-07 (program retention), Q-18 (Partner Admin hold conflict), Q-20 (audit surface), and D-68/D-141's PRD refusal-wording reconciliation. No open question is resolved here.

**UI applicability**: UI layout, keyboard navigation, screen-reader behavior and visual accessibility are not applicable because this feature specifies backend contracts, not a user interface. API usability, actionable errors and non-disclosing diagnostics remain applicable; consuming consoles own their UI requirements.

## 2. Actor Flows

### 2.1 Read an order and its children

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-flow-read-and-authz-point-read`

**Actors**: authorized customer, partner, seller, current payer or explicitly scoped service. **Use case**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`.

1. Receive the authenticated context, target and optional delegation proof reference. Apply the common wrapper to order detail, versions, lines and acceptance reads, including in-process SDK calls.
2. Prefetch only the target's minimal authorization properties under the approved point-read exception. Nothing from this prefetch is a response or grant.
3. Request the exact resource/action with current properties and unvalidated proof reference through PolicyEnforcer, requiring constraints. If the target is absent, still make the target-ID call with empty properties and perform the scoped re-read; discard results and take the not-found arm. Provider outage returns the same sanitized 503 on both arms.
4. Re-read the parent and children through PDP scope in one consistent database snapshot started after authorization. If authorization facts changed, restart authorization before disclosure. Never return the prefetched row. An already authorized in-flight snapshot may finish; each subsequent request/page uses current relationships.
5. Compose the current aggregate/version without walking the version chain. Return stored lines/pins/total, declared pre-tax/overlay exclusions, line fulfillment/subscription linkage, requested and deferred dates, expected fulfillment time, and stored fulfillment inputs (`overlap_scope_key`, market and version payer). All authorized order readers see these commercial facts.
6. Return the current commercial version as ETag and a coherent `draftRevision` while draft. ETag alone does not protect mutable draft edits. Historical children remain authorized through the current parent; a historical payer grants no access. An authorized missing version uses `version-not-found`.
7. Persist any required served access log before returning data. Apply §3.3 failure semantics; never return partial/stale content, guard state, registry/outbox/dead-letter contents or diagnostics.

### 2.2 List and page authorized collections

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-flow-read-and-authz-collections`

**Actors**: any independently authorized read path. **Output**: one authorized page and optional continuation.

1. Validate supported filters, page size and cursor before the access decision. Default size is 50; outside 1–200 returns `page-size-exceeded`. Invalid filters/cursors return `filter-invalid`/`cursor-invalid` without an access-log row.
2. Authorize each page afresh. For order list request untargeted `order × read`; per-order collections use their current parent. Apply PDP scope, filters and strict cursor boundary in SQL before sorting/limiting. Never post-filter an unrestricted result or synthesize scope from the token.
3. Use these immutable orders: orders `(created_at, order_id)`; versions descending `version`; lines `(created_at, line_id)` from line identity; acceptance descending `accepted_version`; audit ascending `(created_at, audit_id)` with binary UUID comparison. Join line identity to requested/current version membership (draft working membership while draft), excluding removed draft lines.
4. Query at most page size + 1 authorized rows, return at most page size and issue a cursor from the last returned tuple only if the extra row exists. Preserve timestamp microseconds. Tokens bind endpoint, parent, authenticated principal/tenant, normalized filters and sort, and validate version/structure/precision.
5. Classify logging from effective authorized query scope, including mixed and empty pages; log one row per request when required. A page containing only own-tenant results cannot erase a broader scope's logging obligation.

A cursor is position only. Reauthorization may narrow or deny later pages, including payer reassignment or proof revocation. Continue from its tuple if the cursor row was deleted by retention; do not fetch that row to validate position.

### 2.3 Retrieve an authorized audit subset

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-flow-read-and-authz-audit`

**Actors**: authorized Partner Admin or Seller Operator; separate operational grants for unresolved rows. **Use case**: `cpt-cf-bss-orders-lifecycle-usecase-order-amendment`.

1. Establish current order access and the distinct `audit × read` permission; apply the common wrapper and pagination contract.
2. Include resolved committed/refused rows within current order-audit scope. Include unresolved rows matching `requested_order_ref` only with the additional `audit-unresolved × read` grant scoped to stored `subject_tenant_id`. ID equality and immutable audit namespace confer no authority.
3. Denial of the optional unresolved grant omits those rows; provider outage fails the whole response with sanitized 503. A nonexistent order acquires no readable trail. Existing customer/partner/seller permissions do not automatically grant operational audit access.
4. Merge the two disjoint authorized branches into one timestamp/UUID ordered set before LIMIT. Do not concatenate separately paged branches. Persist the required served log, then return authorized actor/reason/idempotency/correlation evidence without identifying enrichment or internal diagnostics.

This is a live view, not an exhaustive denial listing or incremental export. Each page has a fresh snapshot; a transaction committed later with a tuple behind the cursor may be absent from the ongoing walk. A fresh scan is required to observe it. Audit chain verification remains sequence-based and independent of API order.

## 3. Processes / Business Logic

### 3.1 Shared authorization enforcement

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-read-and-authz-enforcement`

**Input**: Declared operation, authenticated SecurityContext/proof reference, existing/proposed authorization properties and selected-provider decision.

**Output**: Validated PDP constraints for a complete authorized path, or fail-closed denial/integration failure before mutation or disclosure.

Initialize the mandatory `authz-resolver` SDK client from ClientHub, one shared PolicyEnforcer adapter, registered resource/action constants and the architecture's complete permission catalog. Missing wiring or an undeclared operation fails startup. REST and SDK invoke the same service-level checks; gateway authentication alone is insufficient.

Caller-driven reads and writes use PDP-produced constraints, propagated SecurityContext and SecureConn/SecureTx. The aggregate uses `no_tenant`, `no_owner`, `no_type`, its standard resource ID and explicit `resource_tenant_id`, `seller_tenant_id`, `payer_tenant_id` mappings. Neither `unrestricted` nor a generic `owner_tenant_id` substitution is allowed. Inserts must explicitly populate every authorization-relevant value because the toolkit insert validator skips `NotSet` fields; existing-row scoping alone does not prove proposed-value authorization. Complete paths are OR alternatives, each path's requirements are AND conditions; incomplete paths cannot be combined. Workflow/event-consumer read alternatives must each contain finite explicit authorized order IDs.

Use the exact action catalog in [architecture §4.3](../DESIGN.md#contract-08-4-3): commercial PATCH uses `order × write`, administrative-only PATCH uses `order × edit`, submit/amend/preview/cancel/hold/resume have separate grants, acceptance recording uses `acceptance × record`, acceptance reads use `order × read`, and audit has a separate read grant. Select PATCH trigger from field classes before any state read; an authorized commercial PATCH outside draft then fails engine admissibility. Seller-only edit/preview and customer amendment/audit remain denied. Workflow never authors commercial content.

For creation authorize the complete proposed arrangement. For mutations authorize the existing order and, if axes change, the complete proposed arrangement constructed from stored values plus validated delta. Authorize payer use independently at create, Preview, payer change and submit before disclosing commercial facts. Lock/recheck authorization properties and expected version through commit; a mismatch conflicts without silent rebase, mutation or key settlement. Prefer safely observable `version-conflict`, otherwise `authorization-context-changed`; preserve non-disclosure on lost access. Business immutability guards still apply after both grants.

Acceptance authorization does not replace Preconditions' recording-party guard: customer-party membership/action belongs to PDP; comparison against stored creator/submitter/amender actors remains Lifecycle's `acceptance-recording-party-barred` guard. Self-service submit acceptance remains the narrowly defined submit effect, not an independent record grant.

Orders forwards delegation proof references, never verifies signatures/expiry/revocation itself. PDP evaluates required paths and complete alternatives. Audit/log accepted references if reported, otherwise record supplied references explicitly as supplied on an allowed request, never verified by Orders.

### 3.2 Denials, outages and bounded internal authority

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-read-and-authz-denial`

**Input**: Target existence and minimal authorization properties, operation/PDP result, required follow-up read result, and caller or private-worker context.

**Output**: Non-disclosing refusal or sanitized infrastructure failure, or bounded authority for named private operations; never an authorization fallback for callers.

Untargeted PDP denials map proof absence/invalidity to `delegation-proof-required`/`delegation-proof-invalid`, otherwise `operation-not-permitted-for-actor`. Targeted proof denials always return `order-not-found`. Other targeted denials return 403 only if a follow-up `order × read` allows the same target, otherwise indistinguishable 404.

Every denied targeted action other than `order × read` performs that follow-up, including proof denials and missing-row arms (empty properties; discard the absent-row result). A denied `order × read` performs no redundant follow-up. Hidden and missing targets preserve the same call pattern, status/body/reason and outage behavior; residual database timing is not claimed closed.

An unavailable PDP on any required call returns sanitized retryable 503, no protected payload/business/outbox effects and no new settlement or altered existing registry outcome. Retry requires fresh authorization. Missing/invalid required constraints fail closed as integration errors, not invented permission denials. No local evaluator, unrestricted scope or service-identity fallback is allowed.

Foundation's private audit/idempotency/outbox effects use bounded configured database authority without per-table PDP calls. Refusal evidence uses the restricted writer and authenticated subject/request reference, never a caller audit-write grant or target lookup merely for audit. If persistence fails, preserve confidentiality and emit sanitized operational failure telemetry without claiming durable audit.

The architecture's explicit maintenance exception is limited to expiry, draft auto-void, registry cleanup, retention purge and audit verification/checkpointing. Only private lifecycle-owned code receives its configured capability. Broad discovery scopes must narrow to actual targets before writes; retention grants and verifier/checkpoint privilege separation remain binding. Workers may run during PDP outage under their independent authority, never as fallback for user requests. Broker producer/consumer root permissions are separate explicit platform grants.

### 3.3 Read access logging

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-read-and-authz-access-log`

**Input**: Read outcome, effective scope/current resource tenant, authenticated subject, supplied/accepted proof reference and requested/resolved target.

**Output**: Required served/refused log or explicit no-log outcome; required served-log failure blocks disclosure, while refused-log failure preserves refusal and emits an operational signal.

| Outcome / request | Required behavior |
|-------------------|-------------------|
| Allowed point/child read with supplied proof or foreign current resource tenant | Append served log before disclosure |
| Allowed collection with proof or scope not provably confined to subject resource tenant | One served log per request, including empty/mixed pages |
| Allowed own-resource-tenant read without proof; provably confined collection without proof | No log |
| Access refusal | Append refused log; return no protected payload |
| Input validation failure or invalid/missing PDP constraints | No access-log row; constraints are integration failures |

Persist trusted immutable actor UUID/class, operation, outcome, timestamp, optional proof, requested target and known resolved target. Lists have both target columns NULL; missing target reads keep `requested_order_ref` with NULL foreign-key `order_id`. Targeted proof denials retain public `order-not-found` in `refusal_reason` and classified proof failure in operational-only `internal_refusal_detail`, never on public read surfaces.

Required served-log failure returns `read-store-unavailable` with no payload; refused-log failure preserves the original refusal and emits the infrastructure/security signal. Retention is 90 days, separate from committed commercial evidence. Identity deletion does not rewrite actor evidence or authorize identifying enrichment.

## 4. States

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-state-read-and-authz-outcomes`

This feature adds no order state or transition. A validated request proceeds to authorization, a scoped consistent read, required logging and disclosure. Denial ends in a non-disclosing refusal; infrastructure failure ends without protected payload. Only served/refused access evidence is appended. A read never replays events, walks the version chain or derives order state; replica/stale-cache reads are forbidden.

## 5. Definitions of Done

### 5.1 Shared permissions before public operation delivery

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-dod-read-and-authz-permissions`

The implementation MUST deliver the Foundation authorization prerequisite and architecture's complete action/relationship contract. Required scopes and old/proposed arrangement checks must be demonstrated on PostgreSQL and the selected provider; catalog registration alone cannot close this item.

**Implements**: `cpt-cf-bss-orders-lifecycle-algo-read-and-authz-enforcement`, `cpt-cf-bss-orders-lifecycle-algo-read-and-authz-denial`.

**Touches**: all public operations, permission catalog, engine pre-guard, current aggregate property mappings and bounded private persistence/worker entries. **Constraints**: `cpt-cf-bss-orders-lifecycle-constraint-delegation-proof-required`.

### 5.2 Coherent, bounded reads and disclosure evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-dod-read-and-authz-surfaces`

The implementation MUST deliver the common wrapper to every REST/SDK read, correct immutable cursor ordering, current-parent isolation, and logging before required disclosures. Index and query-plan validation must cover each filter shape and mixed-axis scopes at production row counts. Observe latency by shape, page-size distribution, logging failures, and internal invalid-proof counts even when public denials are sanitized; alert on SLO burn, sustained invalid proof and store unavailability.

**Implements**: `cpt-cf-bss-orders-lifecycle-flow-read-and-authz-point-read`, `cpt-cf-bss-orders-lifecycle-flow-read-and-authz-collections`, `cpt-cf-bss-orders-lifecycle-flow-read-and-authz-audit`, `cpt-cf-bss-orders-lifecycle-algo-read-and-authz-access-log`, `cpt-cf-bss-orders-lifecycle-state-read-and-authz-outcomes`.

**Touches**: order/list, version detail/list, lines, acceptance history and audit reads; `cpt-cf-bss-orders-lifecycle-dbtable-read-access-log`; current aggregate, version, line identity/projection and scoped audit tables. **Constraints**: `cpt-cf-bss-orders-lifecycle-constraint-read-fails-closed`, `cpt-cf-bss-orders-lifecycle-constraint-bounded-page-size`.

## 6. Acceptance Criteria

- [ ] Route census and SDK tests prove every operation uses the exact catalog action, trusted caller/proof context and required constraints; missing wiring/declaration fails startup. A recording PDP verifies questions and enforcement without claiming real-provider policy correctness.
- [ ] Same-tenant principals with different grants, cross-axis paths and overlapping roles prove membership alone grants nothing and incomplete paths cannot combine. Seller-only PATCH/Preview and unauthorized customer amendment/audit are denied with the defined 403/404 mapping.
- [ ] Current payer reassignment removes former-payer access to current/historical children, cursors and replay requests; independent complete grants remain usable. Workflow/consumer reads reject arbitrary correlations, unbounded/tenant-only branches and missing/revoked explicit order grants.
- [ ] Existing/proposed authorization tests cover old-allow/new-deny, old-deny/new-allow, both-allow with immutable-axis refusal, unauthorized create payer, and concurrent axis/version changes with no mutation or key settlement.
- [ ] Every targeted hidden/missing denial preserves the specified calls and non-disclosing response, including proof denials; outages on first/follow-up calls return sanitized 503 on both arms. Invalid constraints never broaden scope.
- [ ] Each paged collection tests sizes 0/1/200/201, N/N+1 boundaries, immutable ties, malformed/cross-parent/cross-principal/filter-mismatched tokens, and revoked access. Microsecond/binary UUID ordering and retention of cursor position prevent fixed-dataset duplicates/skips.
- [ ] Audit tests merge both scoped branches before LIMIT, separate operational unresolved grants, omit optionally denied rows and fail on outage. Concurrent commits behind/ahead of the cursor demonstrate the documented live-view limitation; no snapshot-complete export is claimed.
- [ ] All REST/SDK surfaces test the logging table, including proof-bearing own-tenant reads, direct seller/payer cross-tenant reads, mixed/empty collections, missing targets, served/refused log failures and operational-only proof details.
- [ ] Store failure serves no stale/cache/replica result; required served-log failure discloses nothing. Current views expose coherent version/draftRevision, fulfillment inputs, requested/actual dates and total exclusions without internal state or identifying enrichment.
- [ ] PostgreSQL races prove scoped isolation and rollback of failed audit/outbox effects; public callers cannot reach worker capabilities, broad discovery scopes cannot write, and retention/verifier grants stay restricted.
- [ ] Production-scale query plans/load tests verify read/list p95 < 200 ms by filter shape, deep version history and mixed-axis scope. Selected-provider policy/delegation/three-axis tests and deployed broker grants are separately recorded before integration acceptance; documentation alone marks none complete.

- [ ] Create/insert integration tests populate and enforce every authorization-relevant axis, detect omitted (`NotSet`) values before persistence, and exercise each axis independently; scoped-update tests alone cannot close this requirement.

## 7. Detailed Behavior Contracts

**Contract namespace 08.** Section numbers inside the detailed contracts below resolve
through the [contract address index](../DECOMPOSITION.md#4-contract-address-index),
not the overview section numbers above.

The following sections retain the complete algorithms, guards, state transitions and
feature-local rules. The preceding flows and acceptance criteria summarize these contracts;
shared schemas and interfaces are defined in [DESIGN.md](../DESIGN.md).

<a id="contract-08-3-6"></a>

<!-- contract:08-read-and-authz:3.6 -->
### Reads and authorization: Interactions and Sequences

**Common read execution contract (OL-66/67).** Every REST and public SDK read executes this
wrapper, including order detail/list, version detail/list, lines, acceptance and audit. The
sequences below specialize it, never exempt a surface from its checks.

1. Validate filters and the cursor token before the access decision and before storage access.
   Every collection (orders, versions, lines, acceptance history and audit) defaults to 50, rejects page_size outside
   1–200 with `page-size-exceeded`, and queries at most limit+1 scoped rows using §2.2's cursor.
   A cursor that fails §2.2's cursor contract — structure, supported version, precision, or its
   binding to endpoint, parent order, principal, normalized filters and sort — returns
   `cursor-invalid` (D-139). These are input-validation failures: they append no access-log row
   (§4.4). Scalar reads have no page limit.
2. Authenticate and request the exact §4.3 resource/action through PolicyEnforcer. Audit uses
   `audit × read`, not merely `order × read`; acceptance uses its declared read permission.
   **Denial mapping (D-114, D-141)** — the one definition every read and write path cites.
   A PDP denial of a request with **no target** (List, Create, Preview) maps to the
   delegation-proof reason item 3 classifies, where the deny reason reports proof, and otherwise
   to `operation-not-permitted-for-actor` (403). A PDP denial of a **targeted** request never
   discloses a delegation-proof reason: a delegation-proof denial is always `order-not-found`
   (404), since the reason alone would confirm the target exists (D-141). Any other denial of a
   targeted request maps to `operation-not-permitted-for-actor` only if a follow-up
   `order × read` decision on the same target allows it; otherwise it maps to the
   indistinguishable `order-not-found` (404). The follow-up is made on the deny path only, never
   on an allowed request, and on every denial of a targeted request whose action is not
   `order × read`, whatever the deny reason, so a proof denial and any other denial cost the same
   calls. It is made on **both** arms: where the prefetch found no row, Orders makes it with the
   target ID and an empty property set and discards its result, so a hidden and a nonexistent
   target make the same PDP calls (D-68). A denied `order × read` makes **no** follow-up on either
   arm, since the follow-up would be the same decision, and is therefore always `order-not-found`.
   Despite the reason's historical name, PDP decisions, not a local actor-class evaluator,
   determine it. Provider outage — on the first decision or the follow-up, on either arm — is
   infrastructure failure, not permission denial, and returns §3.5's sanitized 503.
3. Pass the caller's supplied delegation proof reference, if any, as request context on the step 2
   PolicyEnforcer call; Orders does not decide that a path is delegated or validate the proof
   (D-111). Classify a PDP denial whose reason reports required proof as absent as
   `delegation-proof-required`, and one reporting supplied proof as invalid, expired, revoked or
   wrong-scope as `delegation-proof-invalid`. Item 2 returns that reason on an untargeted request
   only; a targeted request returns `order-not-found`, and its access-log row records
   `refusal_reason` = order-not-found and the classified proof reason in
   `internal_refusal_detail` (§3.7), which only operational readers see and which is never
   returned to the caller, so the refused row keeps AC-16's proof fact (D-141; review RR-L2,
   2026-09-24); the scoped internal log/metric still counts it. Count invalid credentials separately, the sanitized ones
   included, so the security alert has an actual signal. Whether an independently complete
   direct path needs delegation merely for crossing a tenant axis is PDP policy (it does not, per
   §2.1).
4. Query through PDP scope and the authorized current-parent snapshot. Historical-version and
   line access never inherit a former payer's authority. An authorized parent with an absent
   requested version returns Versioning's `version-not-found`; unauthorized parents do not.
   For lines, join identity to current/requested version membership (draft working membership
   while draft) before ordering; removed draft identities are not current lines.
5. Apply the served/refused logging policy below to **every** surface. Version-list,
   version-detail, line and acceptance handlers are explicit writers of `orders_read_access_log`,
   using the exact operation name, trusted actor, proof reference when used, requested target,
   resolved target only when known, timestamp and outcome. A collection writes one entry per
   request, not per row. Cross-tenant direct access logs too; delegation is not the only trigger.
   Persist a required served log before returning data. Refused-log failure emits the required
   infrastructure/security signal without turning refusal into access.

The route census test must cover served and refused calls for each operation; invalid/missing
proof, which returns `delegation-proof-required` / `delegation-proof-invalid` on list, create and
preview and `order-not-found` on every targeted read and write (D-141); for a targeted denial
whose action is not `order × read`, the same PDP calls on the hidden-row and no-row arms,
follow-up included, and a sanitized 503 on an outage of either call; distinct audit grants, missing versions, 0/1/200/201 page sizes, cursor ties, a
`cursor-invalid` token on each paged collection with no access-log row, removed draft lines,
logging failure, and a point read against a nonexistent order during a PDP outage, which returns
the sanitized 503 exactly as an existing order would, never `order-not-found`. The scoped query and log rules apply equally to in-process SDK calls.

<a id="contract-08-scoped-read"></a>

#### Scoped read

**ID**: `cpt-cf-bss-orders-lifecycle-seq-scoped-read`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`, `cpt-cf-bss-orders-lifecycle-actor-orders-direct-customer`, `cpt-cf-bss-orders-lifecycle-actor-orders-seller-operator`

**Algorithm: Read One Order**

Input: order_id, security_context
Output: the composed read view, or a registered refusal

1. [ ] - `p1` - Use the authenticated SecurityContext and the caller's supplied delegation proof reference, passed unvalidated as PDP request context, as inputs to the shared platform PolicyEnforcer adapter; do not derive a grant from actor class or classify a delegated path locally - `inst-sr-resolve-actor`
2. [ ] - `p1` - Load the aggregate row, whose tenant axes are the input the relationship check of step 3 is evaluated against. The row loaded here serves the authorization decision **only**: no part of it, and no fact derived from it — not its existence, not its state, not its axes — **MUST** reach a caller who fails step 3 - `inst-sr-load-aggregate`
3. [ ] - `p1` - Request `order × read` through PolicyEnforcer with the target ID, prefetched current tenant properties and the step 1 proof context, requiring constraints. Where step 2 found no row, Orders still makes this call, with the target ID and an empty property set, and still performs the scoped re-read, then discards both results and takes the not-found arm below, so neither arm skips a round trip (D-68); PDP unavailability on either arm returns §3.5's sanitized 503, never `order-not-found`. **IF** no aggregate row exists **OR** PDP denies, whatever its deny reason — a delegation-proof reason included, since this request is targeted and never discloses one (D-141) **OR** the scoped aggregate re-read finds no accessible row — a denied `order × read` makes no follow-up decision on either arm, so the wrapper's item 2 denial mapping (D-114) is always `order-not-found` here: - `inst-sr-if-no-relationship`
   1. [ ] - `p1` - Append the read access-log row: this operation, `outcome` = `refused`, `refusal_reason` = order-not-found, `requested_order_ref` = the requested identifier, `order_id` = that identifier where the row exists and NULL where it does not (§3.7), and the delegation proof reference where one was presented. A delegation-proof deny reason is not written to `refusal_reason`; it goes to the row's operational-only `internal_refusal_detail`, never returned to the caller, and to the scoped internal log/metric (common wrapper item 3) - `inst-sr-log-not-found`
   2. [ ] - `p1` - **RETURN** order-not-found, so existence is not leaked - `inst-sr-return-not-found`
4. [ ] - `p1` - Load the current version's lines with their pins and resolved total through the PDP-scoped current parent order, using the authorized read snapshot described below - `inst-sr-load-current-version`
5. [ ] - `p1` - Load the per-line fulfillment projection through the same scoped parent and snapshot - `inst-sr-load-projection`
6. [ ] - `p1` - **IF** any line's quoted service-activation date precedes expected fulfillment time: - `inst-sr-if-deferred`
   1. [ ] - `p1` - Include expected fulfillment time and the per-line deferral - `inst-sr-include-deferral`
7. [ ] - `p1` - Include the resolved total's declared exclusions - `inst-sr-include-exclusions`
8. [ ] - `p1` - Apply §4.4's logging decision table: append a served access-log row when a delegation proof reference was supplied on the allowed request or when the current resource tenant differs from the authenticated subject tenant, including direct seller/payer access. Record the proof reference PDP reports accepting, else the supplied one; an own-resource-tenant read with no supplied proof appends nothing - `inst-sr-log-served`
9. [ ] - `p1` - **RETURN** the composed view with the current version as the ETag and, for a draft, draftRevision from the same coherent snapshot as its commercial content - `inst-sr-return-view`

**Description**: Step 3 returns not-found rather than forbidden by design: to a caller with no
relationship, the difference between "this order is not yours" and "no such order" is itself
information about another tenant's activity. Its two arms — no row, and a row the caller has no
relationship to — **MUST** be indistinguishable in the response: same reason, same body, same
status, the same error shape, no skipped round trip — step 3 makes the PDP call and the scoped
re-read on both arms — and no outage-behaviour difference, since a PDP outage returns the same
sanitized 503 on both. Residual database timing between the arms is outside this guarantee:
the design does not claim to close that channel.

**Prefetch and authorized snapshot.** Step 2 uses the platform's approved point-read prefetch
exception, restricted to the target and the minimal authorization properties. It supplies PDP
inputs, not a response or an independently granted scope. Step 3 must re-read through the
PDP-produced AccessScope; never return the prefetched row directly. The scoped parent re-read
and child reads use one consistent database snapshot started after the PDP decision. If
authorization-relevant properties differ from the prefetch, restart authorization before
disclosure. Versions, lines and acceptance reads follow this same parent-scoped rule even for
historical versions. A payer change committed before that snapshot removes the former-payer
path; an already authorized in-flight snapshot may finish. New requests and pages reauthorize.
PDP unavailability or invalid constraints fail closed with a sanitized failure, never a local
fallback or a partially composed view; unavailable PDP and refusal-log behavior follow §3.5's
outage contract, including sanitized HTTP 503 and no unauthorized audit-write fallback. A point
read is targeted, so a PDP delegation-proof denial takes step 3's non-disclosing not-found arm
like every other denial; `delegation-proof-required` / `delegation-proof-invalid` are never
returned here (D-141).

<a id="contract-08-paginated-list"></a>

#### Paginated list

**ID**: `cpt-cf-bss-orders-lifecycle-seq-list-orders`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`, `cpt-cf-bss-orders-lifecycle-actor-orders-seller-operator`

**Algorithm: List Orders**

Input: security_context, filters, page_size, cursor
Output: a page of order summaries, or a registered refusal

1. [ ] - `p1` - **IF** page_size exceeds the configured maximum: **RETURN** page-size-exceeded refusal. This is input validation, not an access decision, so it appends no access-log row — logging it would let an unauthenticated-shaped caller drive one durable write per malformed request - `inst-lo-if-page-too-large`
2. [ ] - `p1` - **IF** a filter names an unsupported field or value: **RETURN** filter-invalid refusal; input validation, so no access-log row - `inst-lo-if-filter-invalid`
3. [ ] - `p1` - **IF** a cursor is supplied and fails §2.2's cursor contract — structure, supported version, precision, or its binding to this endpoint, the authenticated principal, the step 2 normalized filters and the sort: **RETURN** cursor-invalid refusal (D-139); input validation, so no access-log row - `inst-lo-if-cursor-invalid`
4. [ ] - `p1` - Request `order × read` without a target ID through PolicyEnforcer, requiring constraints and carrying the caller's supplied delegation proof reference as request context, on every page request; this request is untargeted, so under the common wrapper's item 2 denial mapping a PDP delegation-proof denial returns the reason item 3 classifies, `delegation-proof-required` or `delegation-proof-invalid` (D-141), and any other denial `operation-not-permitted-for-actor` (D-114); no follow-up decision is made. PDP unavailability is not a denial: it returns §3.5's sanitized 503 - `inst-lo-resolve-scope`
   1. [ ] - `p1` - **IF** PDP denies: append the read access-log row — this operation, `outcome` = `refused`, `refusal_reason` = the mapped reason, `order_id` and `requested_order_ref` both NULL, and the delegation proof reference where one was presented — and **RETURN** the mapped refusal. The append uses §3.5's private writer; its failure preserves the refusal (§4.4, D-35) - `inst-lo-if-denied`
5. [ ] - `p1` - Compile the PDP constraints to AccessScope; apply alternative complete paths as OR and conditions within each path as AND, never an Orders-owned actor-class match - `inst-lo-match-actor`
   1. [ ] - `p1` - Enforce seller-path constraints against current seller_tenant_id - `inst-lo-when-seller`
   2. [ ] - `p1` - Enforce partner-path constraints against current resource_tenant_id exactly as PDP returned them; PDP includes a delegated path only where its policy accepted the supplied proof, and Orders adds no local delegation check (D-111) - `inst-lo-when-partner`
   3. [ ] - `p1` - Enforce customer-path constraints against current resource_tenant_id and payer-reader constraints against current payer_tenant_id; neither follows from tenant membership alone - `inst-lo-when-customer`
   4. [ ] - `p1` - For a Workflow or event-consumer read path, require explicit PDP resource-ID constraints for the authorized orders and apply them in SQL through the aggregate's standard `id` mapping; service identity or a supplied correlation alone grants no read access - `inst-lo-when-workflow`
   5. [ ] - `p1` - If an allowed decision's constraints are missing or invalid, fail closed as a policy/integration error, not a refusal: no refusal reason and no refused access-log row, and never a locally constructed or unrestricted scope - `inst-lo-otherwise-refuse`
6. [ ] - `p1` - Apply the PDP scope, validated filters and cursor boundary in SQL before ordering and limiting; bind child data to the current scoped parent. A cursor carries no authority and cannot retain access through a former payer - `inst-lo-apply-predicate`
7. [ ] - `p1` - Apply §4.4's logging decision table to the effective query scope, including mixed and empty pages: append one served access-log row with null order_id if a delegation proof reference was supplied on the allowed request or the scope is not provably confined to the subject's resource tenant. Record the proof reference PDP reports accepting, else the supplied one - `inst-lo-log-served`
8. [ ] - `p1` - **RETURN** the page with a cursor for the next - `inst-lo-return-page`

**Description**: The scope predicate is part of the query rather than a post-filter, so a page is
never partially discarded and the page size means what it says. Scoping is by relationship, which
is what makes the partner path work without widening to the partner's whole customer base.

<a id="contract-08-audit-retrieval"></a>

#### Audit retrieval

**ID**: `cpt-cf-bss-orders-lifecycle-seq-audit-read`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-amendment`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-seller-operator`, `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`

```mermaid
sequenceDiagram
    participant O as Seller Operator
    participant R as Read projection
    participant P as Platform PDP via shared adapter
    participant A as Audit store
    participant L as Read access log
    O ->> R: GET audit for order
    R ->> P: authorize order read and audit read with trusted properties
    P -->> R: decisions and scopes, or infrastructure failure
    alt PDP unavailable
        R -->> O: sanitized 503; no protected payload
    else PDP denies - no authorized path, or its delegation-proof reason
        R ->> L: append row - refused, with the reason and any proof reference
        L -->> R: committed
        R -->> O: the registered refusal
    else authorized
        R ->> P: request additional audit-unresolved read scope
        P -->> R: scoped grant or denial (omit unresolved rows)
        R ->> A: read authorized entries only, ordered by (created_at, audit_id)
        A -->> R: authorized committed/refused rows; unresolved rows only within extra grant
        R ->> L: append row - served, when delegated or direct cross-tenant; proof only if used
        L -->> R: committed
        R -->> O: the authorized trail subset, not an exhaustive denial listing
    end
```

**Diagram failure boundaries.** Refusal/access-log appends use the private writer of §3.5,
not another PDP decision. The served log append follows §4.4's delegated-or-cross-tenant rule, including direct seller/payer reads. Denial of
the optional unresolved-row grant omits those rows; an unavailable provider is not a denial
and returns the sanitized 503 without a partial trail. Log persistence failures follow the
served/refused rules below; an arrow labelled committed is not a promise that storage cannot fail.

**Description (D-105)**: The trail includes committed and refused entries only within the
reader's current authorization. Resolved business refusals follow order-audit access; unresolved
authorization denials additionally require the explicit subject-tenant-scoped operational grant
below. Ordinary order access does not promise all denied attempts, and this endpoint is not an
exhaustive operational-denial listing. Absence from a response is not proof that no denial was
recorded. Authorized rows carry actor, reason, idempotency and correlation references, never
internal diagnostics. No permission is broadened to make a diagram promise complete visibility.

**Unresolved transition attempts (D-98).** An early authorization denial records the requested
identifier without resolving the aggregate ([01 §3.7](../DESIGN.md#contract-01-3-7)): its `order_id`, states and version are
NULL, even if an inaccessible order actually exists. The audit reader MUST NOT interpret these
NULLs as proof of nonexistence. For an existing order, unresolved attempts may be included by
`requested_order_ref` only when the reader is authorized for both the order and those audit rows
under platform-scoped access; UUID equality alone MUST NOT confer access across tenants. Apply
that scope before pagination and preserve the existing cursor contract. An unknown order does
not acquire a readable trail or bypass the normal not-found/relationship checks. Inspection of
unresolved attempts outside an authorized order trail is restricted to separately authorized
operational audit access; no new customer-facing listing endpoint is introduced. D-104 defines
the missing row scope: stored `subject_tenant_id`, copied from the authenticated actor, owns an
unresolved refusal. The `audit-unresolved` resource has `read` for explicitly assigned operational
auditors, with PDP constraints on those subject tenants. Append is private engine persistence
under configured database authority per §3.5, not a separate PDP action. Existing customer,
partner and seller roles gain no operational audit-read permission automatically. An order reader
without this extra read grant receives only resolved rows, not denials belonging to other callers'
tenants. Check both grants before the D-101 merge/page; caller-controlled target IDs never confer
scope. The immutable `audit_tenant_id` is a chain namespace, not a substitute for current order
authorization or the unresolved-refusal subject scope.

**The audit read writes the access log on the same served-and-refused pattern as the other read
paths.** A **cross-tenant** audit read **MUST** append its `orders_read_access_log` row —
`operation` = the audit read, `outcome` = `served`, with the delegation proof reference only if used — and that
row **MUST** be committed **before** the trail is returned. A refused audit read appends its row on
exactly the terms of *Read One Order* step 3.1, which a delegation-proof denial takes too (D-141), including the `requested_order_ref` rule of
§3.7 for an order that does not exist. An own-resource-tenant audit read with no supplied delegation proof reference appends nothing, per §4.4. An allowed proof-bearing read always appends a served log, even when an independent own-tenant path authorizes it. Of all
the read surfaces this is the one where the omission mattered most: the audit trail is the widest
disclosure the gear makes, and an unlogged cross-tenant read of it would leave the §4.4 claim —
that a review can establish under whose authority an order was read — untrue precisely where it is
most often asked.

**A failed access-log write blocks a served response and never blocks a refusal.** The write-path
twin aborts its transaction when the audit append fails, so no state change survives without its
trail ([01-foundation — Interactions and Sequences](01-foundation.md#contract-01-3-6) *Attempt Transition* step 23.1). A read has no state change to roll
back, so the rule is stated on disclosure instead. Where the append fails on a **served**
cross-tenant read — the audit read or any other — the read **MUST** fail with
read-store-unavailable and **MUST NOT** return the payload: returning it would produce exactly the
unlogged cross-tenant access §4.4 asserts cannot happen, and §2.2's fail-closed constraint already
prefers an unhealthy answer to an untruthful one. Where the append fails on a **refused** read the
refusal **MUST** still be returned unchanged — a log-write failure **MUST NOT** make a response
more disclosive than it would otherwise have been — and the failure is raised on the
read-access-log write-rate signal of §3.8 rather than converted into a different refusal.


<!-- /contract -->

<a id="contract-08-5"></a>

<!-- contract:08-read-and-authz:5 -->
### Reads and authorization: Traceability

- **PRD**: [`../PRD.md`](../PRD.md) — §6.6 authorization, §9.1 read operations, §11 the order console and customer order view
- **Gear design**: [`../DESIGN.md`](../DESIGN.md) — realises `cpt-cf-bss-orders-lifecycle-component-read-and-authz`
- **Engine**: [`01-foundation`](../DESIGN.md#contract-01-1-1) — the authorization pre-guard, the aggregate row, the audit store
- **Depends on**: [`02-capture`](../DESIGN.md#contract-02-1-1) and [`03-gate-and-pin`](../DESIGN.md#contract-03-1-1) for the content read; [`04-versioning`](../DESIGN.md#contract-04-1-1) for the version reader; [`06-workflow-seam`](../DESIGN.md#contract-06-1-1) for the per-line projection
- **ADRs**: [`ADR/0001`](../ADR/0001-cpt-cf-bss-orders-lifecycle-adr-transition-through-engine.md) transition through the engine; [`ADR/0002`](../ADR/0002-cpt-cf-bss-orders-lifecycle-adr-slice-decomposition.md) the foundation-plus-seven-slices decomposition

<!-- /contract -->
