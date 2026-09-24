<!-- CONFLUENCE_TITLE: [BSS]: Orders Lifecycle — Read Surfaces and Authorization (Slice 8) -->
<!-- Related: ../DESIGN.md, ../PRD.md, ./README.md, ./01-foundation.md | Owners: BSS Orders team -->

# DESIGN — Read Surfaces and Authorization (Slice 8)


<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles and Constraints](#2-principles-and-constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions and Sequences](#36-interactions-and-sequences)
  - [3.7 Database Schemas and Tables](#37-database-schemas-and-tables)
  - [3.8 Deployment Topology](#38-deployment-topology)
- [4. Additional Context](#4-additional-context)
  - [4.1 The read projection (normative)](#41-the-read-projection-normative)
  - [4.2 What a read exposes (normative)](#42-what-a-read-exposes-normative)
  - [4.3 The permission model (normative)](#43-the-permission-model-normative)
  - [4.4 Delegation proof (normative)](#44-delegation-proof-normative)
  - [4.5 Policy values](#45-policy-values)
- [5. Traceability](#5-traceability)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-design-read-and-authz`

## 1. Architecture Overview

### 1.1 Architectural Vision

This slice owns everything that reads an order and the rules about who may. It serves the
current-version projection, the tenancy-scoped list, historical versions, the per-line
fulfillment view and the audit trail — and it owns the per-actor permission set enforced on every
operation in the gear, not only on reads, through one shared platform PDP adapter that the engine pre-guard
and the read paths both invoke
([`../PRD.md`](../PRD.md) §6.6, §9.1).

Two properties drive the design. The first is **latency**: order consoles and downstream systems
query state frequently against a 200 ms budget, and the version chain is the wrong structure to
read from — walking it to find current state would make read cost grow with amendment count. So
the aggregate row carries denormalized current state and a current-version pointer, and reads
resolve one row plus that version's lines. No chain walk, no event replay, no derivation.

The second is **confidentiality**. This is a multi-tenant BSS gear where cross-tenant leakage is
a critical failure, and the partner path makes it non-obvious: a partner admin legitimately reads
orders whose `resourceTenantId` is a customer's, which means scoping cannot be a simple equality
check against the caller's tenant. Scoping is by **relationship** — the caller's delegated scope
for the resource axis, or seller scope for the seller axis — and only an access path exercised on behalf of another tenant requires delegation proof.
Direct seller/current-payer grants remain independent paths; cross-tenant disclosure still logs.

The slice reads and refuses. It registers no transition, derives no state, and exposes nothing
internal: not guard state, not idempotency records, not engine diagnostics.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|------------------|
| `cpt-cf-bss-orders-lifecycle-fr-order-authorization` | The per-actor permission set is declared here as data and enforced for **every** operation through one shared platform PDP adapter — invoked by the engine pre-guard on writes and directly on reads — so no slice can widen scope and reads and writes cannot drift. |
| `cpt-cf-bss-orders-lifecycle-fr-order-history` | Any version is addressed by `(order_id, version)` and read directly, alongside the version list with actor, timestamp, reason and supersession. |
| `cpt-cf-bss-orders-lifecycle-fr-order-line-dates` | The read exposes expected fulfillment time and the per-line deferral wherever the activation barrier deferred a line past its quoted date, so the requested and actual dates are both visible. |
| `cpt-cf-bss-orders-lifecycle-fr-order-subscription-linkage` | The per-line line-to-subscription mapping is exposed on the read, which is what makes "which subscription did this order produce" answerable without consuming an event. |

**One surface in this slice has no requirement basis.** The **audit read** is grounded in a
rationale — a complete audit nobody can read is not an audit — and *not* in a requirement:
PRD §6.1 and `nfr-order-audit-completeness` oblige the system to **record** transitions, not to
**expose** them, and PRD §9.1 contains no audit-retrieval operation. It is therefore a
design-introduced surface needing Product's acknowledgement, disclosed as such in
[`../DESIGN.md`](../DESIGN.md) §3.3 and [`../DECISIONS.md`](../DECISIONS.md) D-70 and routed as
Q-20. It is deliberately absent from the table above, because listing it would present a reason
as a basis.

#### NFR Allocation

| NFR ID | NFR Summary | Allocated To | Design Response | Verification Approach |
|--------|-------------|--------------|-----------------|----------------------|
| `cpt-cf-bss-orders-lifecycle-nfr-order-read-latency` | Read and paginated list p95 < 200 ms | Read projection | The aggregate row carries denormalized state and a version pointer; a read resolves one row plus its version's lines, never the chain. Filters are index-backed and page size is bounded | Benchmarks at production row counts and page sizes, including the tenancy-scoped filter paths and deep amendment chains |
| Security vector, `cpt-cf-bss-orders-lifecycle-fr-order-authorization` | No cross-tenant readability | Permission model | Scoping is by relationship, not caller equality; every read path applies it through the shared platform PDP adapter, and a delegated read demands verifiable proof, evaluated by PDP policy; delegated and direct cross-tenant reads are recorded in the access log | Negative tests per actor asserting refusal outside scope; a test asserting no read path bypasses PDP; a test asserting every cross-tenant read writes an access-log row |
| `cpt-cf-bss-orders-lifecycle-nfr-order-recovery` | Reads never serve wrong state | Read projection | The projection is the aggregate row itself, so there is no replication lag between state and its read; on store unavailability the read fails rather than serving stale | Fault-injection test asserting the read reports unhealthy rather than degrading |

#### Key ADRs

The seven gear ADRs govern this slice. One decision taken here is recorded in the register: **the read projection is the aggregate row
rather than a separately maintained materialised view**
([`../DECISIONS.md`](../DECISIONS.md) D-81, §4.1), whose alternative was an asynchronously
updated projection.

### 1.3 Architecture Layers

Inherited from [`01-foundation`](./01-foundation.md) §1.3. This slice adds read surfaces at the
presentation layer and the permission declaration at the domain layer; it adds no store.

## 2. Principles and Constraints

### 2.1 Design Principles

#### Read the row, never the chain

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-read-row-not-chain`

Current state and the current-version pointer live on the aggregate row, so read cost is
independent of amendment count. No read walks the version chain, replays events or derives state.
The trade is one denormalized column maintained inside the transition that changes it — which is
safe precisely because the engine is the single writer.

#### Scope by relationship, not by equality

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-scope-by-relationship`

A caller's readable set is defined by their relationship to the order's axes — delegated scope
over the resource axis, seller scope over the seller axis, ownership of their own orders — not by
their tenant equalling one of them. A naive equality check would either break the partner path
or, if widened to fix it, leak across customers of the same partner.

#### Nothing internal is readable

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-no-internal-exposure`

Guard state, idempotency records, outbox rows, dead-letter contents and engine diagnostics are
not exposed on any surface, and no error body carries them. What a caller sees is the commercial
document, its history, its audit trail and registered business reasons.

#### One PDP adapter, invoked from two places

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-one-permission-model`

The declaration owned here governs every operation in the gear. Authorization decisions belong
to the platform PDP, reached through **one shared PolicyEnforcer adapter** used by the engine
pre-guard and the read paths. The adapter prepares trusted inputs and enforces returned scopes;
it is not an Orders-owned policy evaluator. Business guards remain in Orders and cannot grant
access. This preserves D-34's single permission model while replacing its local-evaluator
mechanism ([`../DECISIONS.md`](../DECISIONS.md) D-34, amended for platform PDP).

### 2.2 Constraints

#### Fail closed on store unavailability

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-constraint-read-fails-closed`

A read whose store is unavailable **MUST** fail rather than serve a cached or stale answer.
Orders are financial commitments and a stale state read can cause a wrong operational decision —
an operator cancelling an order that has already completed, for instance. Unhealthy is a
truthful answer; stale is not.

#### Cross-tenant access requires delegation proof

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-constraint-delegation-proof-required`

Delegation is determined by the **authorized access path**, not by comparing the caller's
tenant to a single presumed order owner. Customer access uses the resource-tenant relationship,
seller access the seller relationship, and payer-reader access the current payer relationship,
each subject to its PDP action grant. A seller or payer needs no resource-tenant delegation
merely because that resource tenant differs from its own. Acting on behalf of another tenant
through a delegated path—ordinarily the partner path—**MUST** carry explicit, auditable proof
covering that path and operation. **The platform PDP evaluates that proof** (D-111): Orders passes
the delegation proof reference the caller presents on the request, unvalidated, as request context
on every PolicyEnforcer call, read and write; PDP policy decides whether an authorized path needs
delegation and whether the supplied proof is valid for it, and answers allow with constraints or
deny with a reason. Orders never classifies a path as delegated and never verifies proof itself.
The PDP policy obligation keeps the path semantics: missing or invalid required proof refuses that
path; it does not veto an independently complete non-delegated path. Orders must not choose or
broaden a path locally to evade PDP policy, and it maps PDP delegation denials to
`delegation-proof-required` (proof absent) and `delegation-proof-invalid` (proof supplied but
rejected) on an untargeted request — list, create, preview — only. A targeted request answers
`order-not-found` on any PDP denial, a delegation-proof denial included, because disclosing the
reason would confirm that the target exists (§3.6 common read wrapper item 2, D-141). The proof
reference PDP reports accepting is recorded in the applicable audit/access-log entry; while the PDP
response cannot name it, Orders records the supplied reference and the entry means "supplied on an
allowed request", not "verified by Orders". For the same reason the create's
`orders_order.sales_path` is `partner_placed` iff the allowed create carried a proof reference
(`01 §3.7`, D-140): Orders has no path marker to read. It describes the create only, so no
acceptance control keys on it alone (`05 §4.2`, D-146).

**The rule is scoped by the axes a request names, not by whether an order exists.** Preview creates
no order, so a requirement phrased against "the order's own" tenant does not reach it — and Preview
resolves axis validity, contract-active status and party eligibility under a referenced contract, the order market from the
**payer's** commercial profile, and overlap presence against existing subscriptions, over a basket
whose tenant axes the caller supplies. Without this clause any authenticated partner could
enumerate baskets against an arbitrary `payerTenantId` and learn whether that tenant exists, holds
an active contract, which market it binds to, and whether it already subscribes to a named
product. Therefore Preview **MUST** establish PDP authority for the requested operation and
named relationships, including payer-use authority, before resolving or disclosing their
commercial facts. If authority relies on delegation, PDP policy decides that the supplied proof
covers that relationship; an independent direct seller/customer relationship is not itself a delegation.
Missing authority or required proof refuses Preview rather than returning a partial verdict.
`03 §4.6`'s fourth absolute prohibition remains: Preview **MUST NOT** return, in a gate result,
reason detail or total, any fact about a party or relationship outside the caller's PDP-authorized
assessment scope. This does not give Payer Reader Preview access.

#### Page size is bounded

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-constraint-bounded-page-size`

**Every collection response is paged**, not only the order list: the order list, the version
list, the per-line read, the acceptance history and the audit read each take a page size and
return a cursor. Default 50,
maximum 200, set here as working baselines rather than left unchosen, because the 200 ms budget is
stated per page and an unbounded page would make it meaningless. A request exceeding the maximum is refused rather
than silently truncated.

The audit read is the one that makes this load-bearing. ADR-0005 commits a row on **every**
refusal, so that collection grows with request traffic rather than with commercial activity — a
client retrying against a failing guard can put thousands of rows on one order. An unpaged audit
read is therefore a memory-amplification vector for any caller holding the audit-read permission,
and it is the collection least able to assume a small result.

**Cursor ordering is declared for every paged collection, and every sort key is immutable.** All
five collections of the preceding paragraph carry a declared order, because a cursor over an
undeclared order can duplicate or skip rows at a page boundary:

| Collection | Ordering | Immutable tiebreaker |
|------------|----------|----------------------|
| order list | `(created_at, order_id)` | `order_id` |
| version list | `version`, descending | `version` itself — UNIQUE per order under `orders_order_version`'s `(order_id, version)` primary key, so it needs no second key |
| per-line read | `(created_at, line_id)` taken from `orders_order_line_identity` | `line_id` |
| acceptance history | `accepted_version`, descending, as the version list | `accepted_version` itself — UNIQUE per order under `orders_acceptance`'s `(order_id, accepted_version)` primary key ([`05-preconditions`](./05-preconditions.md) §3.7), so it needs no second key |
| audit read | `(created_at ASC, audit_id ASC)` for every outcome (D-101) | `audit_id`, unsigned binary UUID order |

The four per-order collections are bounded to one aggregate by their own table's primary key, so a
page never scans across orders ([`01-foundation`](./01-foundation.md) §3.7). The per-line read composes
the **mutable** fulfillment projection, so its sort key **MUST** come from the append-only line
identity row rather than from the projection — a line whose fulfillment status advances between
two pages must not move.

A sort key **MUST NOT** be a mutable column. `state_entered_at` is deliberately **not** one: the
engine rewrites it on every state change, so a row could move between pages and be returned twice
or skipped entirely — silently, with a 200 and a valid-looking cursor. The same prohibition rules
out ordering the version or line collections by anything a later transition rewrites, which is why
neither takes its order from the aggregate row.

**Cursor contract, all five paged collections (D-139).** The order list, the version list, the
per-line read, the acceptance history and the audit read share one cursor token contract; each applies it to its own
ordering from the table above.

* **Exact continuation.** For the last returned row's sort tuple, select only rows strictly after
  it in the collection's declared order — for the audit read, rows where `created_at > t` or
  (`created_at = t` and `audit_id > id`); for the version list, `version` below the last one
  returned, and likewise `accepted_version` for the acceptance history. Preserve stored microseconds and UUID binary comparison exactly; never round cursor
  timestamps to milliseconds or use textual UUID collation. Read up to page_size + 1 authorized
  rows; return at most page_size and emit a next cursor from the last returned row only when the
  extra row exists. Existing default 50 and maximum 200 apply. No cursor/empty continuation means
  no further rows in that page's read, not that no future rows can arrive.
* **Cursor is position, not authority.** Use an opaque versioned token containing the tuple and
  binding it to the endpoint, the parent order (for the version, line, acceptance and audit collections), the
  authenticated principal/tenant context, the normalized supported filters and the sort
  direction. Validate its structure, supported version, precision and request binding at input
  validation (§3.6 common read wrapper step 1); any failure returns `cursor-invalid` (400) and
  appends no access-log row, because input validation precedes the access decision. Reauthorize
  every request against current permissions and delegation validity. Never reconstruct an access
  scope from cursor-supplied claims or use a token to preserve revoked access. No new filters are
  introduced by this contract.

**Audit concurrency contract (D-101).** This refines only the audit collection, not the version,
order or line endpoints. One page is one consistent database read of the currently authorized
rows. The next page is a new read, not a continuation of a retained database snapshot.

* **One ordered set.** Apply the order/tenant/delegation scope and any supported filters before
  sorting or limiting. Combine resolved rows for `order_id` with independently authorized
  unresolved rows where `order_id IS NULL` and `requested_order_ref` matches (D-98). These branches
  are disjoint; a row must not appear twice. Merge them by the same timestamp/UUID ordering,
  not by concatenating separately paged result sets. Use the two indexes in `01 §3.7`.
* **Live-view limits.** Immutable keys prevent previously returned rows from moving and being
  repeated. They do not establish commit ordering: an earlier-timestamp transaction may commit
  after a page is read, placing its row behind the cursor. Such a row is not guaranteed to appear
  in the ongoing walk; a fresh scan is needed. Rows inserted ahead of the cursor may appear on
  later pages. A timestamp upper bound alone would not turn this into a consistent snapshot.
  This endpoint MUST NOT be advertised as a complete incremental consumption/export mechanism;
  a snapshot-complete export would need a separate contract.
* **Retention and changing access.** A refused row may expire between pages; continue from the
  encoded tuple even if the row that issued it no longer exists. Do not fetch that row to validate
  the position, reset the cursor or return expired evidence. Current authorization can narrow
  results or refuse continuation; it never freezes the initial page's disclosure permissions.

**Acceptance**: mixed outcomes with identical timestamps paginate without duplicates or skips
over a fixed authorized dataset; sequence order differing from timestamp order does not change
the result sort. Test microsecond precision, UUID tiebreaks, both D-98 query branches, N/N+1 page
boundaries, malformed/cross-order/cross-principal/filter-mismatched tokens (each `cursor-invalid`
on every paged collection, with no access-log row), revoked delegation,
retention deleting the cursor row, and concurrent commits both ahead of and behind the cursor.
The late-commit test MUST demonstrate the documented live-view limitation, not assert snapshot
completeness. Chain verification must remain sequence-based and independent of this API.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-entity-order-read-view`

The composed read of an order: the aggregate's identity, number, category, axes, state and
contract reference; the current version's lines with their pins and resolved total; the per-line
fulfillment status and subscription linkage; expected fulfillment time and per-line deferral
where they apply; and the declared exclusions on the resolved total.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-entity-permission-declaration`

The per-actor permission set: for each actor class, the operations permitted, the axis the scope
is evaluated against, and whether delegation proof is required. Declared as data, read by the
shared platform PDP adapter; the delegation-proof requirement is declared for PDP policy to
enforce, not for Orders to check (D-111).

**Relationships**:
- `Order read view` → `Order root`: one-to-one, resolved from the aggregate row and its current version.
- `Permission declaration` → every operation in the gear: many-to-many, evaluated before any slice guard on writes and before anything is disclosed on reads — which on an order-scoped read is immediately after the aggregate's tenant axes are loaded, because the relationship is evaluated against them (§3.6).
- `Order read view` → `Line fulfillment projection`: embeds the projection owned by [`06-workflow-seam`](./06-workflow-seam.md), read-only on both sides.

### 3.2 Component Model

This slice realises `cpt-cf-bss-orders-lifecycle-component-read-and-authz`
([`../DESIGN.md`](../DESIGN.md) §3.2) as two internal parts.

#### Read projection

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-component-read-projection`

##### Why this component exists

Consoles and downstream systems poll order state, and a read that grew with amendment count or
lagged behind the write would either miss the budget or mislead the reader.

##### Responsibility scope

The current-version composed read; the paginated tenancy-scoped list with its state, date-range
and contract filters; historical version reads; the audit-trail read; exposure of expected
fulfillment time, per-line deferral and the resolved total's declared exclusions; and the
fail-closed posture on store unavailability.

##### Responsibility boundaries

It registers no transition, derives no state, and exposes nothing internal. It invokes the shared
PDP adapter but does not declare the model, and it writes exactly one thing — the read access-log
row.

##### Related components (by ID)

- `cpt-cf-bss-orders-lifecycle-component-transition-orchestrator` — depends on
- `cpt-cf-bss-orders-lifecycle-component-versioning-reader` — shares model with
- `cpt-cf-bss-orders-lifecycle-component-seam-line-projection` — depends on

#### Permission declaration

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-component-authz-declaration`

##### Why this component exists

Per-actor authorization stated once and enforced centrally is the only version of it that cannot
drift between capabilities.

##### Responsibility scope

The declaration of every actor class's permitted operations across all twenty-four endpoints, the
axis each scope is evaluated against, the delegation-proof requirement, and the recording-party
rule for acceptance; plus the **shared platform PDP adapter** the engine pre-guard and the read paths both
invoke, and the startup check that fails if an operation exists with no declaration.

##### Responsibility boundaries

It defines no authentication. Authentication is platform-owned at the gateway, and the
service-principal check for the workflow-only operations is specified in
[`06-workflow-seam`](./06-workflow-seam.md) §3.3.

##### Related components (by ID)

- `cpt-cf-bss-orders-lifecycle-component-transition-orchestrator` — owns data for

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-interface-read-ops`

- **Requirement**: `cpt-cf-bss-orders-lifecycle-interface-order-ops`
- **Technology**: REST/OpenAPI via `OperationBuilder`; RFC 9457 problems; ETag carries the current commercial version. Draft reads also expose `draftRevision`; draft commercial writes and submit must supply it as `expected_draft_revision`, separately from expected_version; on `draft-mutate` its absence is not a boundary rejection, and the engine compares it only after admissibility, so a post-draft commercial `PATCH` refuses `not-admissible` (D-147). ETag alone cannot detect mutable draft edits.

| Method | Path | Description | Stability |
|--------|------|-------------|-----------|
| `GET` | `/bss-orders-lifecycle/v1/orders/{orderId}` | The composed current read, including deferral and declared exclusions | unstable |
| `GET` | `/bss-orders-lifecycle/v1/orders` | Paginated list scoped to the caller's relationship; filters on state, date range and contract | unstable |
| `GET` | `/bss-orders-lifecycle/v1/orders/{orderId}/versions` | The version list, **paged**, served with the version reader owned by [`04-versioning`](./04-versioning.md) | unstable |
| `GET` | `/bss-orders-lifecycle/v1/orders/{orderId}/versions/{version}` | One historical version, served with the version reader owned by [`04-versioning`](./04-versioning.md) | unstable |
| `GET` | `/bss-orders-lifecycle/v1/orders/{orderId}/lines` | Per-line fulfillment status and subscription linkage, **paged** | unstable |
| `GET` | `/bss-orders-lifecycle/v1/orders/{orderId}/audit` | The transition audit trail, including refused attempts; **paged**, because ADR-0005 makes it grow with request traffic | unstable |

**Reasons contributed to the registry**: order-not-found (returned in preference to a
forbidden response where the caller has no relationship to the order, so existence is not
leaked), delegation-proof-required, delegation-proof-invalid, operation-not-permitted-for-actor,
page-size-exceeded, filter-invalid, cursor-invalid (a cursor token that fails §2.2's cursor
contract, D-139), read-store-unavailable.

### 3.4 Internal Dependencies

| Dependency Gear | Interface Used | Purpose |
|-------------------|----------------|----------|
| `toolkit-db` | Runtime-scoped access | The aggregate, version, line, projection and audit reads |
| `authz-resolver-sdk` | `PolicyEnforcer`, `ResourceType`, `AccessRequest` | Shared platform authorization adapter and compilation of PDP constraints |

### 3.5 External Dependencies

| Dependency Gear | Interface Used | Purpose |
|-------------------|---------------|---------|
| `account-management` | SDK client | Resolution of the caller's delegated scope and the relationships the permission model evaluates |
| `authz-resolver` | `AuthZResolverApi` resolved through ClientHub | Platform PDP decisions for all access paths; mandatory gear dependency |

Authentication is terminated at the inbound gateway and the gear receives an authenticated
`SecurityContext`; this slice re-implements none of it.

**Selected integration pattern: Pricing.** Reuse Pricing's shared `PolicyEnforcer` integration
(`pricing/src/authz.rs`), registered permission catalog (`pricing/src/gts/permissions.rs`) and
PDP-produced database scopes. Orders supplies its own resource/action constants, tenant-axis
properties and business guards. Do not copy Pricing's caller-tenant-only repository predicate
or treat its permission catalog as proof that the deployed PDP enforces role assignments.
This selects an integration pattern, not a new Orders policy engine or a new toolkit API.
Caller-driven operations use PDP; the explicitly bounded internal-worker exception below adopts
Pricing's trusted system-context pattern.

**Trusted internal maintenance (Pricing pattern).** The five Orders-owned workers—per-state
expiry, draft auto-void, idempotency cleanup, retention purge and audit verification/checkpointing—
operate under configured system authority without a per-pass or per-row PDP decision. This is
an explicit exception to blanket PDP-derived scopes, modeled on Pricing's `infra/jobs.rs`:
bounded cross-tenant candidate scans may use `AccessScope::allow_all()`, but each subsequent
operation must narrow to its actual target and applicable scope. It is not an exception for
REST, public SDK calls, administrative requests, Orders Workflow calls or failed user requests.
Never select this authority using a caller-supplied actor class, flag or tenant ID.

Only lifecycle-owned worker code receives the internal capability, through a non-public entry
point and configured restricted database roles. A public caller must not be able to enqueue
arbitrary work under this capability. The allowed work is closed:

| Worker | Authority and mandatory restriction |
|--------|-------------------------------------|
| Per-state expiry / draft auto-void | Discover due orders; invoke only the configured expiry/auto-void transitions through the engine, rechecking current state, deadlines and tenant properties under the aggregate lock |
| Idempotency cleanup | Delete only records eligible under the existing registry retention/lease rules; never execute business transitions |
| Retention purge | Delete only eligible expired rows under the existing table-specific retention grants; no deletion of retained committed audit evidence |
| Audit verifier / checkpoint phase | Verifier SELECT-only; checkpoint phase separately authorized INSERT/SELECT on checkpoint tables, scoped to the immutable audit namespace; no repair, rehash or historical update/delete |

Adapt Pricing's per-tenant narrowing to Orders: the aggregate has `no_tenant` and three custom
properties, so do not use `for_tenant()` as if it selected an Orders business axis. Build the
internal operation's scope from persisted target IDs and the explicitly named properties or
audit/subject namespace appropriate to that table. These locally constructed scopes are allowed
only inside this worker exception. Never carry a broad discovery scope into a write. Existing
Foundation §3.8 advisory locks, bounded batches, transactional eligibility checks and database-role separation
remain mandatory; a lease is coordination, not authorization.

Orders intentionally differs from Pricing's nil/anonymous worker attribution: use a real
configured service actor for evidence, never an invented human, nil UUID or caller impersonation.
Expiry and auto-void still use the transition engine and commit audit/outbox effects atomically;
there is no exemption from audit completeness. The verifier must not acquire the retention
role's delete privileges. Detailed per-table scope mappings must be checked in implementation.

These jobs may continue during PDP outages because their authority is configured independently,
not obtained as a fallback after a PDP denial. Missing internal authority or database grants
fails the affected job closed. Request-driven refusal evidence uses the separately bounded
private persistence path below, not a worker capability borrowed by a caller. Tests must prove public callers cannot reach the internal path,
discovery scopes never reach writes, target/retention restrictions hold, and worker transitions
retain configured actor attribution and transactional audit.

The repository donor describes this pattern as sanctioned; this design records the bounded
Orders exception explicitly rather than claiming blanket PDP compliance or independently
verified platform-wide approval.

**Platform authorization wiring (normative).** Declare the `authz-resolver` gear dependency and
use its SDK, not its implementation crate. During initialization resolve `dyn AuthZResolverApi`
from ClientHub, construct one `PolicyEnforcer`, and share it through the Orders authorization
adapter with the engine, read services and internal service entry points. Missing client wiring
is a startup failure, never a switch to local authorization. REST and in-process SDK calls must
use the same service-level enforcement; endpoint authentication alone is not authorization.
Preserve the authenticated caller's SecurityContext and use separately configured service
contexts only for explicitly service-owned work, not to elevate denied user operations.

The adapter accepts a declared resource/action, target ID where applicable, and trusted
authorization properties. Stored properties come from the target order; proposed properties
are validated request values submitted for authorization, not trusted claims of authority.
Use `access_scope_with` and require constraints for the scoped database paths. Pass returned
AccessScopes to SecureConn/SecureTx; do not synthesize grants or scopes from actor classes or
Account Management results. Account Management remains a source of identity/relationship
evidence and the issuer of delegation proof; PDP owns permission decisions, including evaluation
of the supplied delegation proof, which the adapter forwards as request context on every call and
never validates (D-111). The approved minimal point-read prefetch
exception in §3.6 is not a general database bypass. PDP failures never activate a permissive
fallback. PDP outage behavior follows the contract below.

**Business-operation authorization boundary.** Follow Pricing's integration at the business
operation boundary, not at each internal table write. PDP authorizes the requested action and
its relevant existing/proposed tenant relationships; this need not be a single network call.
The engine enforces returned scopes and business guards. Audit append, request-bound idempotency
handling and transactional outbox enqueue are private persistence effects, not independently
grantable caller actions and not additional PDP round trips. They use restricted service database
roles and SecureConn/SecureTx scopes bound to the authorized order, authenticated principal/key,
or configured producer queue, as applicable. Locally derived internal scopes cannot widen the
business decision, return another principal's outcome, or authorize another order mutation.
No PDP call is introduced inside the business transaction for these effects.

Refusal evidence is the narrow exception for an unsuccessful operation: the private audit writer
may append under its configured database authority using the authenticated subject tenant and
validated request reference, without granting the caller access to the target. It does not require
a separate `audit-unresolved × append` PDP permission. This supersedes the earlier per-append
permission proposal while retaining D-104's subject-tenant isolation, restricted writer and
no-target-lookup-for-audit rule. Audit **reads** still require their separate PDP permissions.
Transactional audit/outbox failures still abort business changes. Worker authority remains the
separate bounded exception above; none of these internal paths is a public authorization bypass.

**PDP outage contract (PDP-governed paths).** A timeout or unavailable authorization provider returns a sanitized,
retryable service-unavailable error (HTTP 503), not a business permission denial. Perform no
business mutation, return no protected read/replay payload, and do not settle the idempotency
key or alter a pre-existing registry outcome. Abort any uncommitted effects of the failed
attempt. Retries obtain fresh authorization and retain the existing fingerprint rules. Never
use an Orders-local evaluator, unrestricted scope or emergency service-identity elevation as
a fallback. Invalid constraints also fail closed, but are not automatically classified as a
retryable outage; they remain a policy/integration error.

The private audit writer may record an authenticated failed attempt under its configured
database authority even when business PDP is unavailable; this records failure and cannot
authorize execution or settle the key. There is no separate audit-PDP dependency. If the
writer's authority or storage is unavailable, do not bypass database grants or report the
attempt as durably audited. Emit operational error counters and bounded structured
logs with a safe correlation reference and infrastructure failure class; exclude request
payloads, credentials, delegation proofs and commercial/tenant details. This telemetry is not
the business audit trail and does not promise later reconstruction. If authorized refusal
persistence is available, it may record the failed attempt without settling the key, but a
durable audit claim requires its commit to succeed.

The guarantee remains absolute for committed business transitions: none may commit without its
transactional audit entry. Authorization-infrastructure failures before business commit may
leave only operational telemetry. Test business-PDP timeout, audit-store/grant failure, no business or
outbox effects, unchanged existing registry records, sanitized 503 responses and successful
freshly authorized retry after recovery.

Register these Orders-owned authorization resource labels, separate from event/payload types:

| Logical resource | Registered authorization label |
|------------------|--------------------------------|
| `order` | `gts.cf.bss.orders.order.v1~` |
| `acceptance` | `gts.cf.bss.orders.acceptance.v1~` |
| `audit` | `gts.cf.bss.orders.audit.v1~` |
| `audit-unresolved` | `gts.cf.bss.orders.audit_unresolved.v1~` |

Follow Pricing's registered resource schemas and `AuthzPermissionV1` instances. Use shared
constants for catalog entries, ResourceType descriptors and enforcement calls. Permission
instance IDs use the existing platform `AuthzPermissionV1` schema prefix followed by the
Orders instance suffix `cf.bss.orders.<resource>_<action>.v1`,
with hyphens normalized to underscores only in the instance-name suffix; the action strings
remain those in §4.3. For example, `order × read` registers `...cf.bss.orders.order_read.v1`.
Registration declares available permissions; it does not issue role grants. Platform policy
provisioning must implement the matrix and tenant-axis constraints before deployment; Orders
must not fabricate default grants when provisioning is absent.

**Provider capability must be verified separately.** In the repository implementations inspected
for this decision, the static plugin supplies development tenant constraints and the
tenant-resolver plugin evaluates tenant hierarchy using `owner_tenant_id`; neither establishes
Orders' complete action-specific, three-axis and payer-use policy. The resolver routes to the
selected plugin rather than adding those missing policy decisions itself. Platform ownership
must identify the intended deployed provider and demonstrate that it enforces the declared
permission matrix and tenant relationships. Do not infer capability from successful permission
registration or from a successful call using a development plugin.

This is the same ownership split as Pricing's integration: Orders owns the declared contract,
decision requests and scope enforcement; platform authorization/deployment owners own the
selected provider, policy evaluation and role/relationship provisioning. Track confirmation and
integration evidence under `cpt-cf-bss-orders-lifecycle-upreq-pdp-policy-integration`
([`UPSTREAM_REQS.md §2.9`](../UPSTREAM_REQS.md#29-platform-authorization-policy)). This is not
an instruction to implement a policy engine or new validation API in Orders.

Acceptance must include principals with the same tenant relationship but different action
grants, and proposed payer changes where old-order access is allowed but payer-use authority
is denied. A permissive response that ignores supplied properties is not proof of proposed-value
authorization. Keep constraints required for scoped database paths; disabling the requirement
is not a substitute for the missing policy. Proposed-value enforcement remains to be validated
with the existing provider/toolkit APIs before deciding whether any upstream extension is
necessary. No mandatory new proposed-value validation API is selected by this decision.

**Implementation and verification plan (Pricing pattern).** Pricing records shared PEP/catalog
enforcement in `pricing/docs/design/05-governance.md`'s RBAC & Isolation definition of done and
§9 acceptance criteria. Its `pricing/tests/rest_authz.rs` provides route-set coverage and
explicitly separates gear enforcement tests from the PDP-owned role matrix. Orders adopts
that separation, not a new authorization framework:

| Layer / owner | Required evidence |
|---------------|-------------------|
| Orders implementation | Mandatory AuthZ dependency, shared adapter, registered constants/catalog, service-level enforcement on REST and public SDK paths, scoped database access and bounded internal persistence/worker exceptions |
| Orders catalog and route tests | A recording PDP test double verifies the actual resource/action and properties for every public operation. Compare coverage against registered routes so an added or mis-gated route fails; verify catalog consistency and caller-context propagation |
| Orders enforcement tests | Missing dependency fails initialization; denied operations leave business state/outbox unchanged; missing required constraints and invalid responses fail closed; outages return sanitized 503 without settling keys; refusal persistence follows the private audit contract |
| Orders-specific isolation tests | Resource, seller and current-payer paths; combined complete grants; required delegation; historical child reads; former-payer loss of access including cursor/replay requests; audit-read separation; old-side allow/new-side deny and the reverse |
| Orders database integration tests | On PostgreSQL, race authorization against tenant/version changes and verify conflict without mutation or key settlement, transactional audit/outbox rollback, and isolation of concurrent idempotent attempts. Test worker target/retention limits and rejection of public access to internal capabilities |
| Platform/deployment verification | Against the selected real provider, verify role assignments and action/relationship policy behavior under `cpt-cf-bss-orders-lifecycle-upreq-pdp-policy-integration`; record provider and policy revisions |

An in-process fake PDP proves that Orders asks the correct question and enforces the supplied
answer; it cannot prove the deployed provider makes the correct decision. SQLite tests cannot
prove PostgreSQL lock/isolation behavior. Documentation invariant checks prove neither runtime
integration nor deployed policy enforcement. No test or deployment result is claimed by recording
this plan. Implementation remains pending, and platform verification remains separately owned;
the exact proposed-value enforcement must be demonstrated using the selected provider/toolkit
before implementation acceptance, without assuming a new helper API is required.

**Dependency Rules** (per project conventions):
- No circular dependencies
- Always use SDK modules for inter-gear communication
- No cross-category sideways deps except through contracts
- Only integration/adapter gears talk to external systems
- `SecurityContext` must be propagated across all in-process calls

### 3.6 Interactions and Sequences

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
identifier without resolving the aggregate (`01 §3.7`): its `order_id`, states and version are
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
§3.7 for an order that does not exist. A non-delegated own-resource-tenant audit read appends nothing, per §4.4. Of all
the read surfaces this is the one where the omission mattered most: the audit trail is the widest
disclosure the gear makes, and an unlogged cross-tenant read of it would leave the §4.4 claim —
that a review can establish under whose authority an order was read — untrue precisely where it is
most often asked.

**A failed access-log write blocks a served response and never blocks a refusal.** The write-path
twin aborts its transaction when the audit append fails, so no state change survives without its
trail ([`01-foundation`](./01-foundation.md) §3.6 *Attempt Transition* step 23.1). A read has no state change to roll
back, so the rule is stated on disclosure instead. Where the append fails on a **served**
cross-tenant read — the audit read or any other — the read **MUST** fail with
read-store-unavailable and **MUST NOT** return the payload: returning it would produce exactly the
unlogged cross-tenant access §4.4 asserts cannot happen, and §2.2's fail-closed constraint already
prefers an unhealthy answer to an untruthful one. Where the append fails on a **refused** read the
refusal **MUST** still be returned unchanged — a log-write failure **MUST NOT** make a response
more disclosive than it would otherwise have been — and the failure is raised on the
read-access-log write-rate signal of §3.8 rather than converted into a different refusal.

### 3.7 Database Schemas and Tables

This slice introduces **one** table, `orders_read_access_log`, specified at the end of this
section. The rest of its contract is expressed as index requirements on tables owned by
[`01-foundation`](./01-foundation.md) §3.7:

- The canonical `orders_order` index set in `01 §3.7` serves resource, seller and current-payer read paths. D-23's earlier removal of an unused payer composite predates payer-reader access; payer keyset indexes are now required. Current-payer scope uses the aggregate, never a historical version's payer.
- **Keyset indexes are a baseline, not a query-plan guarantee.** The cursor sorts by `(created_at, order_id)`, not mutable `state_entered_at`. Foundation declares the index set once. Validate each filter shape and mixed-axis OR scope with PostgreSQL EXPLAIN and production-scale load tests, including deduplication and payer reassignment; do not promise that every combination avoids sorting.
- **The cost of that is stated rather than hidden.** The index count on the gear's hottest write target is real write amplification on every transition, and it is accepted because the alternatives are worse: sorting on `state_entered_at` is forbidden by §2.2 for correctness, not for performance, and sorting the scoped set per page fails the budget. The benchmark of §1.2 **MUST** cover each filter shape at production row counts, since it is what establishes that these indexes are the ones the planner actually chooses.
- The "in this state since" filter reads `orders_order.state_entered_at`, maintained by the engine inside the transition that changes state. It is the same column the expiry sweep reads, so the two slices no longer specify opposite sources for one fact — and it is a *filter* input only, never a sort key (§2.2).
- **`orders_transition_audit` needs an index its unique constraint does not supply.** The audit read is ordered and paged by `(created_at, audit_id)` per §2.2, because a refused entry carries a **NULL `sequence`** and so joins no `sequence` ordering at all ([`01-foundation`](./01-foundation.md) §3.7). `(order_id, sequence)` UNIQUE therefore serves the chain and the per-order committed lookup, and **cannot** serve this page: a cursor in one order over rows returned in another repeats and skips rows at page boundaries, silently, with a 200 and a valid-looking cursor. The read needs **`(order_id, created_at, audit_id)`**, declared in `01 §3.7` with the rest of the audit table's index set.
- The **version list**'s cursor of §2.2 is served by `orders_order_version`'s `(order_id, version)` primary key with no further index, in either direction, and the **acceptance history**'s by `orders_acceptance`'s `(order_id, accepted_version)` primary key likewise. The **per-line read**'s cursor is `(created_at, line_id)` over `orders_order_line_identity`, whose primary key orders by `line_id` instead, so that table **MUST** carry `(order_id, created_at, line_id)`; no line count is bounded anywhere in this design, so the page cannot rely on the set being small enough to sort.

#### Table: orders_read_access_log

**ID**: `cpt-cf-bss-orders-lifecycle-dbtable-read-access-log`

**Schema**: `access_id`, `order_id` (nullable — NULL for a list call, and NULL for an order-scoped
call whose aggregate does not exist, since the FK below cannot be satisfied against a row that is
not there), `requested_order_ref` (nullable — the identifier the caller asked for, carrying **no**
foreign key, populated on every order-scoped call whether or not the aggregate exists, and NULL
only for a list call), `actor`, `actor_class` (`system`, `service` or `user` — the closed class
shared with the transition audit and derived from the authenticated context only, `01 §3.7`
*Actor class*, D-115), `operation`, `outcome` (`served` or `refused`),
`refusal_reason` (nullable), `internal_refusal_detail` (text, nullable — on a refused targeted
request whose PDP denial reported delegation proof, the classified reason
`delegation-proof-required` or `delegation-proof-invalid` that `refusal_reason` =
`order-not-found` hides (§3.6 common read wrapper item 3, D-141); NULL otherwise; visible only to
operational readers of this log and **never** returned to the caller or exposed through any read
surface), `delegation_proof_ref` (nullable), `accessed_at`.

**Actor identity (D-96/D-103)**: `actor` is the immutable `SecurityContext.subject_id()` rendered
as lowercase hyphenated UUID text, not a name, email or caller-supplied label. The shared minimization and
identity-lifecycle contract is authoritative in `../DESIGN.md` §4.3. Identity removal MUST NOT
update this log, and audit responses MUST NOT enrich it with identifying attributes. Existing
served/refused behavior, access controls and retention are unchanged.

**PK**: access_id

**Indexes**: `(accessed_at)` for the 90-day purge; `(order_id, accessed_at)` for the per-order access history a review asks for; `(requested_order_ref, accessed_at)`, which is the one that answers "who has been asking for orders that do not exist".

**Constraints**: append-only; FK on `order_id` to `orders_order` where present. Written by the read
paths — by §3.6's common wrapper for order detail/list, version detail/list, lines, acceptance
and audit reads on the same served-and-refused pattern — and the reason it exists at all:
reads register no transition, so the audit store
cannot record them, and a cross-tenant read with no record would make the delegation-proof audit
claim untrue ([`../DECISIONS.md`](../DECISIONS.md) D-35).

**Why `order_id` and `requested_order_ref` are both there.** They are not redundant, and the
not-found refusal is the case that forces the distinction. `order_id` is the foreign-key column: it
can hold only an identifier that names a real `orders_order` row, so on the missing-aggregate arm of
*Read One Order* step 3 it **MUST** be NULL — writing the requested identifier there would violate
the FK and cost the refusal its log row entirely, which is the one row a probe should always leave.
`requested_order_ref` is a plain value column and therefore records what was asked for without
asserting that it exists. The resulting representation is exact in all four shapes: a list call has
both NULL; a served or refused order-scoped call against an existing order has both set to the same
identifier; a call against an order that does not exist has `requested_order_ref` set and `order_id`
NULL; and no row anywhere loses the identifier the caller supplied. That last shape — `order_id`
NULL with `requested_order_ref` set on an order-scoped `operation` — is the signature of a probe
against a non-existent identifier, so enumeration is countable per `actor` off the
`(requested_order_ref, accessed_at)` index, which a not-found refusal that recorded nothing would
have made impossible. Both refusal arms of step 3 still return the identical response (§3.6); the
distinction is internal to the log.

**Logging scope.** Apply §4.4's decision table on every read surface. Both delegated
and direct cross-tenant served reads log; proof is nullable for direct grants. Refused access
attempts log under the private writer's authority, including a denied list call (*List Orders*
step 4.1, both identifier columns NULL). Non-delegated own-resource-tenant served reads and
input-validation errors — `page-size-exceeded`, `filter-invalid`, `cursor-invalid` — do not log,
nor do missing or invalid PDP constraints, which are an integration failure rather than a
refusal. Collection logging is based on its effective
query scope, not whether a particular page happened to contain foreign rows. This preserves
an operational record for empty and mixed-scope queries without writing one row per result.

**Additional info**: carries a bounded **90-day** retention distinct from the commercial audit
trail, since it grows with traffic rather than with commercial events (§4.5).

The **permission declaration** is configuration rather than schema, versioned with the gear so a
permission change is a reviewable deployment rather than a runtime edit.

### 3.8 Deployment Topology

Inherited from [`01-foundation`](./01-foundation.md) §3.8. Read paths are stateless and scale
horizontally. **Replica reads are forbidden.** Because the read projection *is* the aggregate row
there is no lag to tolerate, and a lagging replica would answer *successfully* with stale state
that nothing detects — which is the one failure the fail-closed rule of §2.2 cannot catch, since
it is not an error. The claim in §1.2 that there is no replication lag between state and its read
holds only under this prohibition ([`../DECISIONS.md`](../DECISIONS.md) D-51).

**How this satisfies PRD §12 AC-13, AC-16 and AC-21.** AC-21 says a cross-tenant attempt
"**MUST** be denied with an authorization error", AC-13 that a Direct Customer acting on another
tenant's order is denied "with a business-level authorization failure", and both that no data
from the other tenant is disclosed. AC-16's cross-tenant attempt without delegation proof keeps
its `delegation-proof-required` / `delegation-proof-invalid` reason and audit row on an untargeted
request (list, create, preview); on a targeted request it returns `order-not-found`, with the
classified reason kept in the internal log/metric and, for a read, on the refused access-log
row's operational-only `internal_refusal_detail` (§3.6 common read wrapper items 2–3, §3.7, D-141). This design denies with `order-not-found` rather than a forbidden response wherever the caller has no
relationship to the order, because a forbidden response confirms the order exists and turns the
read surface into an enumeration oracle. The confidentiality half of the criterion is met in full;
the literal refusal *kind* differs, so the criteria's wording needs amending and is routed with
the other PRD-fidelity items ([`../DECISIONS.md`](../DECISIONS.md) D-68).

**Two boundary notes.** The **Order Console and Customer Order View of PRD §11 are UI surfaces
owned by the frontend design set**, not by this gear; this slice owns the read and API contract
behind them, including the contract-governed renewal terms it surfaces from the contracts port for
[`02-capture`](./02-capture.md) §4.3. And PRD §11's console step 4 lists **hold** among a Partner
Admin's actions, while §5.1 and §6.3 name only the seller operator and Orders Workflow as hold
actors; this design follows the stricter reading, and the conflict is routed as
[`../DECISIONS.md`](../DECISIONS.md) Q-18 rather than left for a UI team to hit at integration.

**Observability owned here**: read and list latency by shape — single read, list by state, list by
date range, list by contract — because one filter regressing is invisible in an aggregate p95;
refusals split into order-not-found, delegation-proof-required and delegation-proof-invalid, since the last is a
security signal and the first is not; page-size distribution against the maximum; and the
read-access-log write rate. Alerts fire on read-latency SLO burn, on any sustained rate of
delegation-proof-invalid (including failures a targeted request answered `order-not-found`, counted internally, D-141; a credential problem or an attack), and on the read store reporting not
ready.

## 4. Additional Context

### 4.1 The read projection (normative)

Current state and the current-version pointer **MUST** be denormalized onto the aggregate row
and maintained inside the transition that changes them. A read **MUST NOT** walk the version
chain, replay events or otherwise derive state.

The rejected alternative was an asynchronously maintained materialised view. It was rejected
because it introduces a window in which a committed order reads as its prior state, and an
operator acting on a stale read of a financial commitment can cancel an order that has already
completed. Since the engine is the single writer, maintaining the denormalized column inside the
transition costs one column update and removes the window entirely.

A read **MUST** fail rather than serve a stale or cached answer when its store is unavailable,
and the gear **MUST** report unhealthy in that condition rather than degrading.

### 4.2 What a read exposes (normative)

The composed read **MUST** carry the order document at its current version, the per-line pin
references, the stored resolved total, and the per-line fulfillment status with the subscription
linkage.

It **MUST** also carry the **fulfillment inputs** of the current version: each line's stored
`overlap_scope_key`, the version's market (`market_currency`, `market_region`) and the version's
`payer_tenant_id`. Workflow executes the activation re-check that
[`03-gate-and-pin`](./03-gate-and-pin.md) §3.6 *Re-check Activation Preconditions* specifies —
step 5 compares against the market frozen at submit, step 6 reads occupancy for each line's
stored key with the version's payer — and reaches Lifecycle data only through this read
([`06-workflow-seam`](./06-workflow-seam.md) §4.3, `UPSTREAM_REQS.md` §2.6). They are stored
commercial facts of the version, not guard state, so the prohibition below does not reach them.
This slice restricts no composed-read field by principal, and these follow that pattern: every
principal the PDP lets read the order sees them, since none is a secret
([`../DECISIONS.md`](../DECISIONS.md) D-144).

It **MUST** expose **expected fulfillment time and the per-line deferral** wherever the
activation barrier deferred a line past its quoted service-activation date, alongside the quoted
date retained as requested — so a reader can see both what was asked for and what will happen.

It **MUST** state the resolved total's **declared exclusions**, per
[`03-gate-and-pin`](./03-gate-and-pin.md) §4.5: the total omits overlays needing
subscription-level context and is pre-tax, carrying no tax figure; a response that omitted these exclusions silently would misrepresent itself.

Beyond the fulfillment inputs above, it **MUST NOT** expose guard state, idempotency records,
outbox or dead-letter contents, or any engine diagnostic, and no error body **MAY** carry internal diagnostics.

### 4.3 The permission model (normative)

Permissions are declared by the matrix and evaluated by **platform PDP through the shared adapter**
for caller-driven operations, including Workflow; §3.5 defines the bounded exceptions for
private persistence and maintenance. It is invoked by the engine pre-guard **before** any slice guard on
the write path, and invoked directly on the read path **before any part of the store is
disclosed**, because a read registers no transition and so cannot reach the pre-guard. On an
order-scoped read the PDP adapter runs immediately after the aggregate's tenant axes are loaded and
before anything is returned, since the relationship it decides is evaluated against those axes and
cannot be decided without them; the row read to reach the decision **MUST NOT** be disclosed to a
caller the decision refuses (§3.6). Startup **MUST** fail if an operation
exists with no declaration. The matrix below is exhaustive over the twenty-four endpoints of
[`../DESIGN.md`](../DESIGN.md) §3.3 — ten matrix rows covering twenty-four endpoints, since the
authoring, read and workflow-only endpoints each share one declaration. An operation absent from
it is a startup failure, not a default-deny.

| Operation | Partner Admin | Direct Customer | Seller Operator | Orders Workflow | Payer Reader | Event Consumers |
|-----------|---------------|-----------------|-----------------|-----------------|--------------| ---------------- |
| create order · commercial draft edit (order or line `PATCH`) · line insert/remove · submit | ✓ delegated scope | ✓ own orders | — | — | — | — |
| amend (append a version) | ✓ delegated scope | — | — | — | — | — |
| administrative edit (order or line `PATCH`, administrative fields, any non-terminal state) | ✓ delegated scope | ✓ own orders | — | — | — | — |
| preview | ✓ authorized resource/payer scope; permitted seller relationship, delegation where acting for another party | ✓ own resource/payer axes; permitted seller may differ | — | — | — | — |
| cancel | ✓ delegated scope | ✓ own orders | ✓ seller scope, reason mandatory | via workflow-cancel only (from `in_fulfillment`, or `on_hold` with pre-hold `in_fulfillment`) | — | — |
| hold · resume | — | — | ✓ seller scope | ✓ with service principal | — | — |
| **record acceptance** | ✓ **only as a `resourceTenantId` principal and not the version-1 creator, the submitter or the amender of `expected_version`** (Lifecycle guard, `acceptance-recording-party-barred`, D-130) | ✓ own orders (initial submit records it; amended-version assent uses this operation) | — | — | — | — |
| read order · list · version list · version read · line read · acceptance read | ✓ delegated scope | ✓ own orders | ✓ seller scope | ✓ with service principal and explicit PDP order-ID constraints | ✓ PDP-granted scope on current `payerTenantId` | ✓ service principal with explicit PDP order-ID constraints |
| audit read | ✓ delegated scope | — | ✓ seller scope | — | — | — |
| approval-reflection · begin-fulfillment · spawn-signal · fulfillment-acknowledgement · workflow-cancel | — | — | — | ✓ with service principal | — | — |

**The Workflow seam reaches two rows from `on_hold`.** The `fulfillment-acknowledgement` and
`workflow-cancel` declarations above cover `acknowledge-failed` and `cancel-workflow-mediated` from
`on_hold` with pre-hold `in_fulfillment` (`01 §4.3` rows 26 and 27) as well as from
`in_fulfillment` (rows 14 and 16). They add no endpoint, no PDP action and no matrix row: the same
two endpoints and the same service-only grants apply, so only the Workflow service principal can
drive either trigger from a hold, and the seller operator's hold/resume grant confers neither
([`../DECISIONS.md`](../DECISIONS.md) D-109).

**Event-consumer read path.** Event Consumers means the explicitly configured Workflow,
Subscriptions and Billing service principals validating lifecycle notifications under Foundation
§4.4. It grants only the existing read row, with the same finite PDP order-ID restrictions as
Workflow below; it grants no authoring, audit access or Workflow mutation seam. This is a
permission path, not a new actor class: access logs record the configured service principal's
`service` class (`01 §3.7` *Actor class*, D-115) and its trusted service subject. Root event access or knowledge of an event's order ID creates no grant.
Provisioning these target grants and their deployed verification remain the open integration
requirement in `UPSTREAM_REQS.md §2.9` (`cpt-cf-bss-orders-lifecycle-upreq-pdp-policy-integration`);
consumer-side read and retry behaviour stays in `UPSTREAM_REQS.md §2.7`. Every consumer read uses the common wrapper and logging
table, including direct cross-tenant service access and read failure behavior.

**Combining permissions.** The columns describe independent authorization paths, not mutually
exclusive actor classifications; Payer Reader is a permission, not a new audit actor class — a
payer reader logs as `user`, since `actor_class` comes from the authenticated context and never
from the column or PDP path that authorized the request (D-115).
For a requested operation, the platform PDP may authorize any complete applicable path. Every
condition within the selected path **MUST** hold; conditions from incomplete paths **MUST NOT**
be combined to manufacture a grant. A dash means that path grants no permission, not that holding
the role vetoes a grant through another path. For example, payer-reader status neither grants
cancellation nor removes cancellation independently authorized through the seller path, including
its seller scope and mandatory reason. Common requirements, such as payer-use authorization
where applicable and business guards, remain mandatory regardless of the access path. This rule
does not override explicit PDP denials or platform policy restrictions. Acceptance tests must
cover overlapping payer/seller permissions and reject combinations of incomplete access paths.

**Single order resource and named properties (normative).** All customer, partner, seller,
payer-reader, Workflow and event-consumer order access uses the same logical `order` resource, not separate
resources per perspective. The PolicyEnforcer ResourceType must advertise the supported named
properties and entity `pep_prop` mappings must resolve them to the current aggregate columns:

| PDP property | Current order column | Access path |
|--------------|----------------------|-------------|
| `resource_tenant_id` | `resource_tenant_id` | Customer or delegated partner |
| `seller_tenant_id` | `seller_tenant_id` | Seller |
| `payer_tenant_id` | `payer_tenant_id` | Current payer-reader |
| Standard resource ID (`id`) | Order primary key | Target-order restriction |

PDP decides the authorized paths and their tenant/resource constraints, including which paths need
delegation and whether the supplied proof satisfies them (§4.4, D-111). Orders compiles and
enforces them through PolicyEnforcer and SecureConn; it does not independently turn actor
classes into grants or construct production AccessScopes. Multiple complete paths are OR
alternatives; all conditions within a path are AND requirements. Requested filters and cursor
boundaries only narrow that scope. Unknown or invalid properties must never broaden access.
Workflow and event-consumer reads require PDP constraints naming a finite explicit set of authorized values of the
standard `id` property, resolved to the existing order primary key. Every OR alternative
authorizing either service read path must contain that restriction; one bounded branch cannot make an
unbounded branch safe. Apply this requirement to point reads, lists and every child read through its
current parent. A tenant-only or unconstrained service read grant is insufficient. The adapter must
reject a Workflow/event-consumer read decision lacking the required resource restriction; requested IDs and
filters may narrow a PDP scope but never create one. Provisioning and revoking the service's
explicit order grants belong to the platform policy owner and must be verified under the PDP
integration requirement before enabling either service read path. A missing grant fails closed.

There is no Orders-owned open execution/correlation relationship to join. The transition-request
correlation is diagnostic data on the seam projection/audit, has no open/closed lifecycle, and
is never authorization evidence. This replaces the earlier unimplementable correlation predicate
without adding a workflow store to Orders. Tests must cover a granted order, another order in
the same seller scope, an arbitrary caller-supplied correlation, missing/revoked grants and all
read variants. Pricing demonstrates the resource-ID property and constrained PolicyEnforcer
mechanism; it does not demonstrate provisioning of these Workflow grants.

The immutable `audit_tenant_id` is not an order-read authorization axis. Nor may the caller's
subject tenant be substituted for all three business axes. **Aggregate storage mapping:**
declare `orders_order` with `no_tenant`, its order primary key as `resource_col`, `no_owner`
and `no_type`, plus explicit `pep_prop` mappings for `resource_tenant_id`, `seller_tenant_id`
and `payer_tenant_id`. No business axis or audit namespace is designated as `tenant_col`, and
no extra storage-owner tenant is introduced. `no_tenant` means there is no single canonical
owner-tenant dimension; it **does not** mean an unrestricted table. `unrestricted` is forbidden
for this aggregate. PDP-produced constraints on the named properties enforce tenant isolation
through SecureConn/SecureTx. Do not add Pricing's caller-tenant-equals-storage-owner predicate,
which would exclude otherwise authorized sellers, payers and delegated partners.

The `order` ResourceType must not advertise `owner_tenant_id` as a substitute for these axes.
Policies must return supported named-property constraints, not a generic owner-tenant scope.
This mapping applies to the aggregate, not automatically to every Orders table: child reads
remain scoped through the current parent, while audit and operational tables retain their
separately defined scopes. Existing business rules still determine whether an axis may change;
omitting `tenant_col` does not relax those rules or the check-both authorization contract below.

This combines existing toolkit capabilities; it is not a proven three-axis mutation pattern
copied from Pricing. Before implementation acceptance, integration tests must demonstrate
custom-property scope enforcement with `no_tenant`, rejection of inappropriate owner-tenant
constraints, and the complete create/update path. Inserts must explicitly populate every
authorization-relevant value: the current toolkit insert validator skips `NotSet` fields.
A scoped existing-row update alone is insufficient proof of proposed-value authorization or
concurrency safety. The exact proposed-value enforcement mechanism and transaction algorithm
remain integration work; do not bypass them with raw writes or locally fabricated scopes.
Audit retains its separate resource/action permissions and unresolved-refusal subject scope.
Validation must exercise each axis independently, multiple simultaneous paths, current-payer
reassignment between pages, scoped historical-child reads, invalid constraints and scoped
pagination without duplicate orders or post-query filtering.

**PDP resource–action catalog (normative).** Follow Pricing's separation of object resources
and grantable actions (`pricing/src/gts/permissions.rs` and `pricing/src/authz.rs`), not a
generic read/write permission covering every operation. The names below are Orders-local
logical resource/action names; their fully qualified registered labels must be shared by the
permission catalog and enforcement calls. A grouped matrix row does not imply a single grant.

| Logical resource | Action | Covered operation |
|------------------|--------|-------------------|
| `order` | `create` | Create a draft |
| `order` | `write` | Commercial draft edits: line insert, commercial line `PATCH` and line `DELETE` in `draft`, and a commercial order-header `PATCH` in `draft` (`draft-mutate`); also any commercial order or line `PATCH` in any state (refused `not-admissible` outside `draft`, D-145); not post-submit version amendment |
| `order` | `submit` | Submit the commercial document |
| `order` | `amend` | Append a commercial amendment version |
| `order` | `edit` | Administrative-only order or line `PATCH` (`administrative-edit`) in any non-terminal state (D-117); within its existing field restrictions |
| `order` | `preview` | Assess a basket without creating or committing an order |
| `order` | `cancel` | User-facing cancellation; not the workflow-cancel seam |
| `order` | `hold`, `resume` | Separate hold and resume grants |
| `order` | `read` | List/read orders, versions, lines and acceptance information |
| `acceptance` | `record` | Record customer acceptance, subject to the existing party and separation-of-actors rules |
| `audit` | `read` | Read the order audit trail under existing order-access requirements |
| `audit-unresolved` | `read` | Separate operational audit-read permission; append is private persistence, not a PDP action |
| `order` | `approval-reflection`, `begin-fulfillment`, `spawn-signal`, `fulfillment-acknowledgement`, `workflow-cancel` | Separate service-only grants for the five Workflow seam operations |

The actor eligibility and all conditions in the matrix remain binding for each mapped action.
A `PATCH` is authorized under the action of the one trigger its fields select (`02 §4.3` *One
request, one trigger*; `02 §3.6` *Edit Order*, *Edit or Remove Line*): `draft-mutate` requires
`order × write`, `administrative-edit` requires `order × edit`, so a draft commercial `PATCH`
never passes on an `edit` grant alone (D-116, D-117). The fields' classes alone select that
trigger, with no state read before authorization: any commercial field selects `draft-mutate`,
administrative fields only select `administrative-edit`, and a commercial `PATCH` outside `draft`
is authorized under `order × write` and then refused `not-admissible` (D-145).
`order × write` does not imply `submit`, `amend` or `edit`; `order × read` does not imply
`audit × read` or `acceptance × record`. Payer Reader is a scoped grant of `order × read`
through the current payer axis, not a separate resource or an unrestricted read grant. A read
grant on `acceptance` is unnecessary: its existing read surface inherits `order × read`.
Self-service submission's automatic acceptance remains part of the authorized submit business
flow; it does not confer permission to invoke the separate acceptance-recording endpoint. It is
written only where the allowed submit carried no delegation proof reference and the submitter's
subject tenant equals the order's `resourceTenantId` ([`05-preconditions`](./05-preconditions.md)
§4.2, D-146), so a delegated partner's submit — including one within delegated scope on a
`self_service` order — never writes it.

Register these pairs using the platform's `AuthzPermissionV1` catalog mechanism, with shared
resource/action constants used by `PolicyEnforcer` calls and tests that detect catalog-to-route
drift. As in Pricing, sensitive Orders resources require explicit grants; do not place their
labels under a namespace that automatically gives generic platform Reader/Contributor/Owner
roles these permissions. Do not copy Pricing's single-tenant-per-request repository restriction:
Orders must retain the three-axis relationship scopes defined here. Nor does adopting the
catalog import Pricing's audit-export surface; Orders has no such endpoint in this design.

This catalog settles the public-operation permission boundaries. Payer-use authority is an
additional condition defined below, not a grant implied by any action in the table. Fully
qualified labels are defined in §3.5; deployed PDP policy/relationship behavior must be verified
under the upstream integration requirement. Private persistence and maintenance follow §3.5's
bounded internal authority, not a new per-table PDP catalog. Registration alone is not enforcement.

Two rows warrant their reasons being stated. **`amend` is separated from the authoring row**
because PRD §6.6 grants amendment to Partner Admin and withholds it from Direct Customer, whose
PRD grant names create, submit and cancel but never amendment; the draft-edit, administrative-edit
and acceptance rows derive from that authoring grant (D-117, D-130), and amendment does not; collapsing them into one row silently widened a
privilege the PRD withheld. **Orders Workflow holds and resumes** because PRD §6.6, §12 AC-15,
§5.1 and §6.3 all name it as a hold actor and the sibling gear's remediation path for a
permanently failed line is to hold the order pending operator resolution — so denying it made a
`MUST`-level acceptance criterion unbuildable. Both are gated on the same gateway-asserted
service principal the other seam operations require
([`06-workflow-seam`](./06-workflow-seam.md) §3.3).

**Seller scope does not add actions (PRD §6.6).** The seller role grants hold, resume,
reasoned cancel and reads; it grants neither `edit` nor `preview`. The earlier seller cells for
these actions are removed rather than treated as implicit read privileges. In particular a seller
operator cannot use draft `PATCH` to change quantities or pricing references: its request is
denied before field/state guards, in draft and every later state, with
`operation-not-permitted-for-actor` (403): the seller holds `order × read` on the target, so the
§3.6 common read wrapper's denial mapping (step 2, D-114) discloses nothing new. An independently authorized
customer/partner path remains subject to the complete-path rule above; seller authority itself
never supplies buyer authoring rights. Outbox re-drive is already outside the Orders API and
permission matrix; the platform recovery tooling requires its own operational authority, which
the seller role does not confer. Test seller-only PATCH refusal as `operation-not-permitted-for-actor` (403) — and, for a
seller with no read path to the target, as `order-not-found` (404) — and seller-only Preview
refusal as `operation-not-permitted-for-actor`, as well as its permitted reads/hold/resume/cancel. This narrows the design to the PRD, with no additional
seller grant requiring Product approval.

**Must not**, stated positively because these are the constraints that carry weight: a Seller
Operator **MUST NOT** amend commercial content on the buyer's behalf — an operator correcting a
buyer's order would be authoring the buyer's commercial intent. Orders Workflow **MUST NOT**
author commercial content. An actor **MUST NOT** act on or read an order outside their scope.

**The acceptance rule.** On a partner-placed order (`orders_order.sales_path` =
`partner_placed`, fixed at create by `01 §3.7`'s observable rule — a delegation proof reference
on the allowed create — D-106, D-140), and on any order where that actor's recorded
`orders_order_version.actor_tenant_id` differs from `resourceTenantId` (D-146), the actor who
**created, submitted or amended** it (`orders_order_version.actor` of version 1, of the submitted
version, or of `expected_version` where an amendment appended it) **MUST NOT** record the
customer's acceptance instant. The split is fixed (D-130): the PDP pre-guard decides
`resourceTenantId` membership and the `acceptance × record` grant (`operation-not-permitted-for-actor`,
or `order-not-found` per D-114); Lifecycle's own recording-party guard
([`05-preconditions`](./05-preconditions.md) §3.6 *Record Acceptance* step 4) compares the trusted
actor with those stored version actors, which the PDP cannot see (D-111), and refuses
`acceptance-recording-party-barred` (403). Recording requires a principal of the
`resourceTenantId` party. A Seller Operator has no acceptance-recording authority: no verifiable
customer-instruction artifact is specified. Without this, nothing would prevent the placing party
supplying their own customer's consent — the exact
conflation [`05-preconditions`](./05-preconditions.md) §2.1 exists to prevent, and the one review
finding that was a substantive hole rather than a documentation gap
([`../DECISIONS.md`](../DECISIONS.md) D-31).

Scope is evaluated **by relationship**: seller operators against `sellerTenantId`, partner admins
against their delegated set of `resourceTenantId` values, direct customers against their own
orders. A caller's tenant equalling an axis is neither necessary nor sufficient.

**Direct Customer: meaning of "own orders".** Ownership is customer-tenant-based, not
creator-based: the order belongs to the customer's `resourceTenantId`, and the platform PDP
**MUST** grant the authenticated principal the requested action for that resource tenant.
Membership in that tenant alone **MUST NOT** grant access. Different users representing the same
customer tenant may work on its orders only within their respective PDP-granted permissions.
Being the creating user, belonging to the payer tenant, or knowing the order ID **MUST NOT**
independently grant access. Partner delegation and seller access remain separate authorization
paths. This definition does not widen the matrix above: Direct Customers still cannot amend
submitted commercial content or read audit, and the acceptance-specific restrictions still apply.
Authority to select a payer is separate, as defined below. The PDP wiring and bounded internal
authority are defined in §3.5; runtime implementation and deployed policy verification remain
pending, not a further choice of an Orders-owned evaluator.

**Create and mutation authorization: check both (normative).** For an existing order, the
platform PDP **MUST** authorize the requested action against the existing order and its current
tenant relationships. If an otherwise permitted mutation changes any tenant axis, PDP **MUST**
also authorize the proposed resulting relationships for that operation, including authority
to use the proposed payer. Both conditions are required: access to the old order cannot grant
authority over a new payer or resource tenant, and authority over proposed values cannot grant
access to an existing order. Construct the proposed values from the stored order plus the
validated delta; omitted fields retain their stored values and must not disappear from the
authorization context. Do not overwrite the existing authorization properties with proposed
ones before checking the existing-order permission.

Creation has no existing-order side: authorize `order × create` and the complete proposed
tenant arrangement before insertion. For mutations without tenant-axis changes, authorize the
operation against the current order; action-specific requirements still apply, including the
submit-time payer-use check below. Authority to use a tenant in an order is not permission to
edit that tenant's account. Neither check widens the actor matrix or overrides business rules
about immutable axes, allowed states, commercial eligibility or seller-scoped payer rebinding.

The mutation transaction **MUST** enforce the PDP-produced current-order scope and verify that
the authorization-relevant stored properties and expected version still match those used for
the decision. A mismatch **MUST** cause a conflict/refusal, with no automatic reauthorization
or silent rebase within that attempt. A subsequent attempt must read fresh facts and obtain
fresh authorization before mutation. A version mismatch uses the existing `version-conflict`
reason where safely observable; otherwise return `authorization-context-changed` (HTTP 409)
as registered in `01 §3.3`, without new target facts.
If access has been lost, preserve the non-disclosing authorization/not-found response. Audit the
late authorization-fact conflict through the service's authorized path without settling the
idempotency key or altering any existing registry record, as specified in `01 §3.6`. Retries
require fresh authorization and retain the existing fingerprint rules. Locking or equivalent conditional-write
checks must protect those facts
through commit. The proposed arrangement persisted must be the one authorized. Missing or
failed authorization causes no business mutation, even if the other side was allowed.
Foundation's create/transition algorithms carry this contract. The concrete provider request
and SecureORM insertion/update enforcement must still be demonstrated during implementation;
the documented flow is not a claim of implemented enforcement.

Acceptance tests must cover old-side allow/new-side deny, old-side deny/new-side allow,
both sides allowed, prohibited axis changes despite both grants (including a draft
`sellerTenantId` change, refused `tenant-axis-immutable` under D-119), concurrent axis/version
changes, and create with an unauthorized payer. Refusal evidence continues to follow the
private service persistence path of §3.5; the denied caller gains no audit-write permission.

**Authority to select a payer.** `payerTenantId` may differ from `resourceTenantId`, but the
platform PDP **MUST** explicitly authorize the authenticated principal to use the named payer
for the requested operation, based on an approved relationship or valid delegation. Authority
over the resource tenant alone **MUST NOT** confer authority to make another tenant the payer.
This is an additional authorization condition, not an alternative to the operation's existing
permission and resource/seller scope checks. Apply it when creating an order, previewing a
basket, changing its payer through an otherwise permitted edit or amendment, and submitting
the order; an earlier authorization **MUST NOT** substitute for authorization at submit.
Preview authorization permits assessment only and does not authorize a later commitment.
Orders **MUST** authorize use of the requested payer before disclosing payer-related commercial
facts. Knowing the payer's identifier, its eligibility, or the existence of an active contract
**MUST NOT** establish the caller's authority: eligibility and contract validity remain separate
business checks. This rule does not widen actor permissions, permit otherwise prohibited
tenant-axis changes, or override seller-scope restrictions on payer rebinding. Exact PDP action
names, relationship inputs and enforcement wiring remain to be specified in the integration
contract; no Orders-local authorization fallback is implied.

**Payer-reader permission (current payer only).** A principal explicitly granted payer-reader
access by the platform PDP may list and read orders through their **current** `payerTenantId`,
including version lists, version details, lines and acceptance information. Tenant membership
alone is insufficient. This is an additional read authorization path alongside customer,
partner and seller access, not a new audit actor class. It grants no create, Preview, edit,
amend, submit, cancel, hold/resume, acceptance-recording or audit-log permission.

The current aggregate's payer determines access even when reading an older version: a payer
recorded only in historical versions **MUST NOT** grant access. After a payer change commits,
the former payer loses this authorization path; independently granted customer, partner or
seller access remains valid. Each request, including each cursor page, **MUST** enforce the
current payer relationship; a cursor or an earlier authorization cannot preserve former-payer
access. Child-record reads must be scoped through the current parent order, not the child's
historical payer snapshot. Verification must cover successful payer reads, denied writes and
audit reads, and payer reassignment removing former-payer access to both current and historical
versions. The read algorithms above enforce PDP-produced scopes that include this permission
without requiring a new actor class.

The **seller operator's exclusion from amending commercial content** is deliberate and worth
naming: an operator correcting a buyer's order would be authoring the buyer's commercial intent,
which is exactly the evidentiary problem the acceptance instant exists to solve.

**Internal lifecycle stream authorization (D-95).** The root-scoped event contract is defined in
[`01-foundation.md §4.7`](./01-foundation.md#47-gts-types-for-the-cross-gear-contract-surface-normative).
The Orders producer service principal **MUST** have explicit broker authorization to publish the
Orders event family to the Orders topic under platform-root tenancy. Workflow, Subscriptions and
Billing consumer service principals **MUST** each have explicit authorization for their required
Orders event types, topic and consumer group, with root-scoped event access. Merely belonging to
the root tenant **MUST NOT** grant publication or consumption privileges.

Customer, partner and seller user principals **MUST NOT** receive direct access to this internal
stream under their Orders API roles. Their order reads remain subject to the relationship-based
rules above; root-tagged events cannot provide customer isolation through envelope-tenant
filtering. Consuming services **MUST** authorize subsequent business actions against the relevant
resource, seller and payer axes; root event access **MUST NOT** be substituted for business
authorization. Exact deployed identities and grants require platform confirmation and acceptance
tests under `cpt-cf-bss-orders-lifecycle-upreq-event-broker-root-tenancy`
([`UPSTREAM_REQS.md §2.7`](../UPSTREAM_REQS.md#27-event-broker)).

### 4.4 Delegation proof (normative)

**Served-read logging policy (normative).** "Delegated" means exercising authority on
behalf of another tenant; Orders cannot observe which PDP path was used, so for logging a read is
treated as delegated when the caller supplied a delegation proof reference on the allowed request
(D-111). The create's `sales_path` uses the same proxy (`01 §3.7`, D-140). "Cross-tenant disclosure" means reading an order whose current
`resourceTenantId` differs from the authenticated `SecurityContext.subject_tenant_id()`.
That comparison classifies logging only; it never grants or denies access. A direct seller or
current-payer grant needs no delegation proof solely because those tenants differ, but the
served read still logs. All point/child reads use the same authorized current-parent snapshot.

| Request / effective authority | Served log | Delegation evidence |
|-------------------------------|------------|---------------------|
| Point read with no supplied proof, current resource tenant equals subject tenant | No | None |
| Direct seller/payer point read, current resource tenant differs | Yes | None |
| Any allowed read on which a delegation proof reference was supplied | Yes | The reference PDP reports accepting, else the supplied reference |
| Collection with no supplied proof, provably confined to the subject's resource tenant | No, including empty pages | None |
| Collection with supplied proof or potentially foreign-resource scope, including mixed/empty pages | Yes, one row per request | The reference PDP reports accepting, else the supplied reference, if any |
| Access refusal, including no relationship or missing/invalid proof | Refused log required; no payload | Presented reference when available; never fabricate proof |
| Input-validation error before an access decision (`page-size-exceeded`, `filter-invalid`, `cursor-invalid`, `request-invalid`, `expected-version-required`) | No | None |

For a collection, classify the effective PDP-authorized query scope after validated filters,
not just the returned rows or the presence of the caller's tenant in a seller/payer axis.
If confinement to the subject's resource tenant cannot be established, log conservatively;
this is not permission to widen the scope. Retain the proof reference covering every delegated
part of a mixed scope; the existing proof reference may identify the supplied evidence bundle.
An independent direct grant cannot hide delegated authority used by the query, which is why a
supplied proof always logs. An own-tenant
page does not erase the logging obligation of a broader scope, and an empty page still logs.
Where a served log is required, persistence failure returns `read-store-unavailable` with no
payload. A refused-log failure preserves the refusal and emits the infrastructure/security
signal. Apply this table to order, version, line, acceptance and audit reads, including SDK
calls; test each row, mixed/empty pages, and served/refused logging failures.

Any operation relying on a delegated access path—ordinarily a partner acting for another
tenant—**MUST** carry explicit, auditable delegation proof for that path. Direct customer,
seller and current-payer paths do not require resource-tenant delegation solely because the
order spans different tenants. PDP grants, action limits and payer-use checks remain mandatory.
**Evaluation is PDP policy, not Orders code** (D-111): the shared adapter forwards the supplied proof
reference as request context on every PolicyEnforcer call, read and write; PDP policy decides
whether the path needs delegation and verifies the proof against the properties below, and Orders
maps its deny reasons to `delegation-proof-required` (absent) and `delegation-proof-invalid`
(supplied but rejected) on an untargeted request, and to `order-not-found` on a targeted one
(§3.6 common read wrapper item 2, D-141). No Orders step verifies a signature, expiry, scope or revocation.
The required proof has a specified form rather than a name only
([`../DECISIONS.md`](../DECISIONS.md) D-32; requested upstream as
[`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) `…-upreq-delegation-proof-credential`):

| Property | Requirement |
|----------|-------------|
| Issuer | Account Management; PDP policy verifies it against the published issuer key |
| Subject | The delegate principal presenting it |
| Claims | The delegating tenant, the delegated scope, the issue instant, a finite expiry |
| Validity | Evaluated by PDP policy: signature verifies, not expired, and not revoked — revocation is checked at verification time, not cached past it |
| Revocation | By the delegating tenant, observable at the next verification |
| Alignment | BSS manifest §2.1.3, which the PRD names normatively |

PDP policy **MUST** treat absence, expiry, signature failure or revocation on a path requiring
delegation as a **refusal of that path**, never a warning. Another complete PDP-authorized path may
still permit the operation; incomplete paths cannot be combined. The proof reference PDP reports
accepting — or, while the PDP response cannot name one, the reference the caller supplied on the
allowed request, recorded as supplied rather than verified — **MUST** be recorded on the audit entry in
`orders_transition_audit.delegation_proof_ref` for a write, and in the read access log for a read
— which is where the previous claim that a review could establish "under whose authority an order
was **read**" became true rather than aspirational, since reads register no transition and only
the engine writes the audit store.

Where a caller has **no relationship** to an order, the response **MUST** be not-found rather
than forbidden, because distinguishing the two leaks the existence of another tenant's order.

### 4.5 Policy values

**Set by this design**: the **default page size of 50 and maximum of 200** (§2.2; enforced by the common read wrapper, §3.6
common read wrapper item 1), on which the
200 ms per-page budget depends; and the **read-access-log retention of 90 days**, bounded separately
from the commercial audit trail because it grows with traffic rather than with commercial events.

**Set elsewhere and read through this surface**: refusal audit rows carry a **90-day retention**
distinct from committed transitions, specified in [`01-foundation`](./01-foundation.md) §3.7
([`../DECISIONS.md`](../DECISIONS.md) D-49) — it is settled, not open.

**Owned by Product**: the program retention period for completed and cancelled orders, which
governs how far back this surface can read at all. That is **PRD §15 row 5**, tracked as
[`../DECISIONS.md`](../DECISIONS.md) Q-07.

## 5. Traceability

- **PRD**: [`../PRD.md`](../PRD.md) — §6.6 authorization, §9.1 read operations, §11 the order console and customer order view
- **Gear design**: [`../DESIGN.md`](../DESIGN.md) — realises `cpt-cf-bss-orders-lifecycle-component-read-and-authz`
- **Engine**: [`01-foundation`](./01-foundation.md) — the authorization pre-guard, the aggregate row, the audit store
- **Depends on**: [`02-capture`](./02-capture.md) and [`03-gate-and-pin`](./03-gate-and-pin.md) for the content read; [`04-versioning`](./04-versioning.md) for the version reader; [`06-workflow-seam`](./06-workflow-seam.md) for the per-line projection
- **ADRs**: [`ADR/0001`](../ADR/0001-cpt-cf-bss-orders-lifecycle-adr-transition-through-engine.md) transition through the engine; [`ADR/0002`](../ADR/0002-cpt-cf-bss-orders-lifecycle-adr-slice-decomposition.md) the foundation-plus-seven-slices decomposition
