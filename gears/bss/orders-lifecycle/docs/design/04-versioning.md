<!-- CONFLUENCE_TITLE: [BSS]: Orders Lifecycle — Amendment and Version History (Slice 4) -->
<!-- Related: ../DESIGN.md, ../PRD.md, ./README.md, ./01-foundation.md | Owners: BSS Orders team -->

# DESIGN — Amendment and Version History (Slice 4)


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
  - [4.1 Admissibility (normative)](#41-admissibility-normative)
  - [4.2 Carry forward and re-resolve (normative)](#42-carry-forward-and-re-resolve-normative)
  - [4.3 Re-approval is a two-step seam interaction (normative)](#43-re-approval-is-a-two-step-seam-interaction-normative)
  - [4.4 Stale results (normative)](#44-stale-results-normative)
  - [4.5 Version history (normative)](#45-version-history-normative)
  - [4.6 Administrative edits are last-write-wins (normative)](#46-administrative-edits-are-last-write-wins-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-design-versioning`

## 1. Architecture Overview

### 1.1 Architectural Vision

This slice owns what happens when a committed order needs to change. It admits amendments in
`submitted`, `pending_approval` and `approved`, appends a new version rather than editing the
old one, re-runs the whole gate, re-pins every line, and leaves the amended version in
`submitted` for the sibling workflow to reflect its new approval requirement
([`../PRD.md`](../PRD.md) §6.2).

The design rests on one observation: **the version counter is both the audit chain and the
concurrency mechanism**. Appending version N+1 simultaneously records what changed and
invalidates every asynchronous result still in flight against version N. That is why approval
can be a long-running, human-paced process without distributed locking: a decision that arrives
late is refused as stale, and the sibling gear's process instance for the prior version is
superseded by the event this slice publishes. This needs no lock or lease spanning approval;
each short engine transaction still serializes and validates its transition.

The second observation is that an amendment is **not primarily a state change**. From
`submitted` the order's state does not move — the version bumps, `OrderAmended` publishes,
and the process reacts. From `pending_approval` or `approved`, state returns to `submitted`.
Treating amendment as a versioning operation that *sometimes* moves state,
rather than as a state transition that sometimes bumps a version, is what keeps the
transition table honest.

What this slice does not own is whether the amended order needs re-approval. That verdict is
external and arrives through [`06-workflow-seam`](./06-workflow-seam.md); this slice always
returns the amended version to `submitted` and never preserves or infers a prior-version verdict.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|------------------|
| `cpt-cf-bss-orders-lifecycle-fr-order-amendment` | Amendment is a versioning transition on three table rows. It appends, re-runs the gate, re-pins, and publishes `OrderAmended` whether or not state moved. |
| `cpt-cf-bss-orders-lifecycle-fr-order-history` | Every version is retained with actor, timestamp, reason and a `supersedesVersion` back-reference, so consumers reconstruct the commercial trail without inferring the chain from ordering. |
| `cpt-cf-bss-orders-lifecycle-fr-order-tenant-axes` | The payer-change amendment is the one axis mutation the design permits, but refuses cross-seller transfer. This narrows the paired-rebinding MUST and remains the D-62/Q-28 divergence. |
| `cpt-cf-bss-orders-lifecycle-fr-order-idempotency` | Stale results are refused by the engine's version check; this slice owns the caller-facing contract that makes the refusal actionable rather than merely correct. |
| `cpt-cf-bss-orders-lifecycle-fr-orders-boundary-r1-state-sor` | Amendment never asks the sibling gear's permission and never reads its process state. It appends and publishes; the process reacts. |

#### NFR Allocation

| NFR ID | NFR Summary | Allocated To | Design Response | Verification Approach |
|--------|-------------|--------------|-----------------|----------------------|
| `cpt-cf-bss-orders-lifecycle-nfr-order-audit-completeness` | 100 % of amendments audited | Version appender | The version append and the audit entry share the engine transaction; an administrative edit audits without a version | Test asserting every amendment yields both a version row and an audit row, and every administrative edit yields an audit row only |
| `cpt-cf-bss-orders-lifecycle-nfr-order-retention` | All versions retained | Version store | Append-only with no update or delete path; retention is the program policy and archival never removes a version | Test asserting no code path deletes a version row |
| `cpt-cf-bss-orders-lifecycle-nfr-order-snapshot-integrity` | Every submitted line pinned | Re-pin on amendment | The gate re-run and re-pin are part of the amendment commit, so version N+1 is pinned contemporaneously with itself | Test asserting a version's pin catalog-version matches the amendment instant, not the original submit |
| `cpt-cf-bss-orders-lifecycle-nfr-order-read-latency` | Version read p95 < 200 ms | Version reader | A version is addressed by `(order_id, version)` and read directly; no chain walk and no event replay | Benchmark on historical version reads at realistic chain depths |

#### Key ADRs

The seven gear ADRs govern this slice. One decision taken here is recorded in the register: **an amendment carries forward the prior
version's content and re-resolves only what the gate produces**
([`../DECISIONS.md`](../DECISIONS.md) D-77, §4.2), whose alternative was requiring the caller to
resubmit the full document.

### 1.3 Architecture Layers

Inherited from [`01-foundation`](./01-foundation.md) §1.3. This slice adds no layer; it calls
the gate ports of [`03-gate-and-pin`](./03-gate-and-pin.md) through that slice rather than
directly.

## 2. Principles and Constraints

### 2.1 Design Principles

#### The version counter is the concurrency mechanism

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-version-is-concurrency`

Appending a version invalidates every in-flight asynchronous result against the prior one. There
is no separate lock, lease or generation token. A consequence to hold onto: a caller that omits
the expected version is refused rather than served, because an amendment that races an approval
reflection must have a defined loser. The refusal is `expected-version-required`, raised by
boundary input validation before authorization, unaudited and without touching idempotency
(`01 §4.1` *Expected version is validated at the boundary*, D-112); a stale version is still the
engine's `version-conflict`.

#### Amend by append, never by edit

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-amend-by-append`

A prior version is never rewritten, re-pinned, corrected or deleted — not by a repair path, not
by a migration. A mistake in version N is fixed by version N+1. This is what makes a reviewer
able to say what they approved, and it is the reason the aggregate row holds a pointer rather
than the content.

#### Carry forward, re-resolve the gate

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-carry-forward-reresolve`

An amendment supplies only the fields it changes. The new version inherits the prior version's
commercial content for everything else, and the gate output — pin, total, market — is
**re-resolved in full** rather than inherited. Inheriting a pin would silently carry a stale
catalog version into a version the buyer believes is current.

#### An amendment is a versioning operation first

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-amendment-not-state-first`

Amendment always appends a version and always publishes `OrderAmended`; it moves state from
`pending_approval` and `approved` (rows 19, 20); only `submitted` (row 18) produces no state
change. Consumers subscribe to the event rather than to a state change, because one of the three
admitting states produces no state change at all.

### 2.2 Constraints

#### Amendment stops at `in_fulfillment`

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-constraint-no-amendment-in-fulfillment`

There is no amendment row from `in_fulfillment` or from any terminal state — only cancel and
hold remain. The reason is downstream rather than local: a provisioning intent may already be
accepted, and amending the document underneath it would leave the spawned subscriptions
describing a version nobody agreed to. A buyer who needs a change after fulfillment starts
cancels and reorders, or changes the subscription.

#### The re-approval target is not this slice's decision

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-constraint-reapproval-target-external`

An amendment from `pending_approval` or `approved` transitions the order to **`submitted`
unconditionally**. Whether the **new version** requires approval is a verdict owned by the
approval policy owner, and the sibling gear obtains it on consuming `OrderAmended` and reflects
the order onward through rows 7 and 8. This slice **MUST NOT** compute that verdict, cache the
prior version's answer, assume the previous answer still holds, or make an outbound call to
acquire it — no such port is declared, and no verdict exists for a version that has not yet been
created. The two-step shape is what makes rows 19 and 20 reachable at all
([`../DECISIONS.md`](../DECISIONS.md) D-61, Q-12; §4.3).

#### A payer change must not cross seller scope

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-constraint-paired-payer-seller-rebinding`

`payerTenantId` is the only tenant axis with an amendment path, and that path **stops at the
seller boundary**. A payer change that crosses seller scope **MUST** be refused with
`payer-rebinding-requires-seller`, because the paired seller rebinding such a move requires
cannot be performed here at all: `sellerTenantId` is **commercial-frozen** and **MUST NOT** change
after `submitted` by any path, amendment included
([`02-capture`](./02-capture.md) §4.3 *Commercial-frozen*; §4.1;
[`../DECISIONS.md`](../DECISIONS.md) D-62). So the payer is never silently rebound alone across
sellers — that would move billing attribution without moving the selling relationship — and it is
never rebound *together with* the seller either, because this slice owns no operation that can
move the selling party. A genuine cross-seller transfer is a cancel-and-reorder under the new
seller; it is not an amendment. Since the seller is fixed at creation (D-119), no pre-submit
flow rebinds it either — a draft for the wrong seller is voided and re-created.

**What "crosses seller scope" means.** A payer change **crosses seller scope** when the identity
operation ([`03-gate-and-pin`](./03-gate-and-pin.md) §3.6 *Run Gate and Submit* step 2) does not
confirm that the proposed payer tenant has a commercial relationship with the order's
`sellerTenantId`, which is immutable from creation (D-119). The answer is part of the payer's
commercial profile that the identity port already returns from Account Management
(`cpt-cf-bss-orders-lifecycle-upreq-payer-commercial-profile`, `UPSTREAM_REQS.md` §2.4); this
slice holds no port of its own (§3.5), so *Append Amendment* step 2 resolves it through 03 once
per run and the gate run reuses it. A confirmed relationship admits the change; an answer that
does not confirm one is refused with `payer-rebinding-requires-seller`. If the operation is
unavailable or misses its deadline, the input is unresolvable and refuses
`identity-party-unavailable`. No new reason is added
([`../DECISIONS.md`](../DECISIONS.md) D-128).

**Seller rebinding is therefore not possible at all after submit**, and this constraint no longer
claims otherwise. It previously described a paired payer/seller rebinding while §4.1 and
`02 §4.3` prohibited every post-submit `sellerTenantId` change; no implementation could satisfy
both, and a reader could conclude either that the documented payer-transfer path must be rejected
or that an unintended tenant rebinding must be permitted. This deliberately narrows PRD §6.1's
paired-rebinding MUST; it is not evidence of conformance. Product/Architecture reconciliation
remains open under D-62/Q-28. `payer-rebinding-requires-seller` keeps its registered name and its
authorization requirements are those of `amend` itself: a cross-seller payer change needs no extra
authority because it is never admitted.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-entity-amendment-request`

The delta a caller supplies: the changed commercial fields, the amendment reason, the expected
version, and the idempotency key. It is not persisted as itself — it is resolved into a new
version row — but it is the unit the caller-facing contract is written against.

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-entity-administrative-edit`

An in-place change to administrative content, at order or line level, in any non-terminal state:
each changed field with its prior and new value, the actor and the instant. It produces one audit
entry per changed field and no version.

It never appends to `cpt-cf-bss-orders-lifecycle-entity-order-version-chain`; it consumes the field
classification owned by [`02-capture`](./02-capture.md) §4.3.

**Relationships**:
- `Amendment request` → `Order version`: one-to-one; each admitted request produces exactly one version.
- `Order version` → `Order version`: `supersedesVersion` forms a linear chain with no branching, because only one version is ever current.
- `Administrative edit` → `Order transition`: one-to-many, one audit entry per changed field (D-117); those entries are the edit's only durable trace beyond the changed values themselves.

### 3.2 Component Model

This slice realises `cpt-cf-bss-orders-lifecycle-component-versioning`
([`../DESIGN.md`](../DESIGN.md) §3.2) as two internal parts.

#### Version appender

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-component-versioning-appender`

##### Why this component exists

Appending a version correctly means contributing four things — carry forward, apply the delta, re-run
the gate, re-pin — that the engine commits together with its pointer move; getting any one of them wrong produces a
version that misrepresents what was agreed.

##### Responsibility scope

Admissibility per state; the carry-forward of unchanged commercial content; delta application;
the gate re-run and re-pin through [`03-gate-and-pin`](./03-gate-and-pin.md); the
version content contribution; it neither assigns `supersedesVersion` nor moves the pointer.
The engine's transition row determines the target state, including amendment from `approved`.

##### Responsibility boundaries

It decides no re-approval requirement, evaluates no predicate itself, and never edits a prior
version. It does not publish `OrderAmended` — the engine enqueues the typed event through the
platform producer outbox from the transition.

##### Related components (by ID)

- `cpt-cf-bss-orders-lifecycle-component-transition-orchestrator` — depends on
- `cpt-cf-bss-orders-lifecycle-component-gate-and-pin` — calls
- `cpt-cf-bss-orders-lifecycle-component-capture-field-classifier` — depends on

#### Version reader

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-component-versioning-reader`

##### Why this component exists

"What exactly did they agree to, and who changed it" is the question the gear exists to answer,
and answering it must not require reconstructing anything.

##### Responsibility scope

Retrieval of any version by order and version number; the version list with actor, timestamp,
reason and supersession; and the administrative-edit trail alongside it.

##### Responsibility boundaries

It serves no current-state projection — that is
[`08-read-and-authz`](./08-read-and-authz.md) — and it applies no access decision of its own
beyond the engine's pre-guard.

##### Related components (by ID)

- `cpt-cf-bss-orders-lifecycle-component-read-and-authz` — shares model with

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-interface-versioning-ops`

- **Requirement**: `cpt-cf-bss-orders-lifecycle-interface-order-ops`
- **Technology**: REST/OpenAPI via `OperationBuilder`; RFC 9457 problems; ETag carries the expected version

| Method | Path | Description | Stability |
|--------|------|-------------|-----------|
| `POST` | `/bss-orders-lifecycle/v1/orders/{orderId}/amendments` | Append a new version from a delta; re-runs the gate and re-pins | unstable |
| `GET` | `/bss-orders-lifecycle/v1/orders/{orderId}/versions` | List versions with actor, timestamp, reason and supersession — **surface owned by [`08-read-and-authz`](./08-read-and-authz.md)**; this slice owns the version reader behind it | unstable |
| `GET` | `/bss-orders-lifecycle/v1/orders/{orderId}/versions/{version}` | Retrieve one historical version in full — **surface owned by [`08-read-and-authz`](./08-read-and-authz.md)**; this slice owns the reader | unstable |

The administrative edit is `PATCH /orders/{orderId}` for order-level fields and
`PATCH /orders/{orderId}/lines/{lineId}` for line-level fields, both owned by
[`02-capture`](./02-capture.md) (§3.6 *Edit Order* and *Edit or Remove Line*) and admitted here in
every non-terminal state through the field classifier. The named fields' classes alone select
the trigger: an administrative-only request takes this path in every non-terminal state, while
one naming any commercial field is capture's `draft-mutate`, which outside `draft` refuses the
engine's `not-admissible` ([`../DECISIONS.md`](../DECISIONS.md) D-117, D-118, D-145).

**The version reason vocabulary is `{create, submit, amendment}`.** PRD §6.2 lists eight reason
values — submit, amendment, approval reflection, hold, resume, cancel, fulfillment outcome, expiry
— and the design adds `create` because PRD §12 AC-1 requires creation to materialise version 1.
Five transition rows append a version: creation, submit and the three amendment rows. The other six
PRD reasons name state-only transitions, so they live on `orders_transition_audit.reason`, not on
the version chain. That column holds only these machine reasons; what a caller writes — a cancel or
hold reason, the amendment explanation, a failure reason — goes in `caller_reason` (`01 §3.7`, D-143). Two of those five append a version **without** publishing `OrderAmended`,
against PRD §6.5's stated trigger of "on creation of a new order version": creation is event-less
and submit publishes `OrderSubmitted`. `OrderAmended` fires only on rows 18, 19 and 20
(`01 §4.4`), and the §6.5 wording is routed as Q-25. A consumer reconstructing the commercial trail keyed on the PRD's list finds
six of eight values on the audit row rather than the version row; the split is stated here so the
two are not read as one vocabulary ([`../DECISIONS.md`](../DECISIONS.md) D-82).

**Reasons contributed to the registry**: payer-rebinding-requires-seller,
tenant-axis-immutable, **administrative-field-in-amendment**, amendment-empty,
**amendment-reason-invalid**, version-not-found, **amendment-cap-exhausted**,
**administrative-edit-unchanged**. The shared read wrapper raises version-not-found
only after current-parent authorization when a requested historical version is absent; it is
not a mutation guard. Administrative edits pass expected_version unchanged to the engine and
never advance it, so they are last-write-wins per field (§4.6, D-120). `tenant-axis-immutable`
also refuses a draft edit naming `sellerTenantId`, which capture raises (D-119).
`amendment-cap-exhausted` is registered **here and only here**: it is the refusal of rows 18, 19
and 20 when `orders_order.amendment_count` has reached the cap of §4.1, and it names the cap and
the count so a caller learns the order cannot be revised again and must be cancelled and
re-placed. Like `resume-cap-exhausted` it is deliberately **not** folded into the engine's
`not-admissible` — the row *is* admissible and the state *does* permit amendment; what refuses is a
guard on data. `administrative-edit-unchanged` is registered **here and only here** (D-149): it is
the last guard of §3.6 *Apply Administrative Edit*, refusing an edit whose every named field
already holds its new value; it is audited and settled, and names no stored value. `administrative-field-in-amendment` is registered **here and only here**: it
is the amendment path's refusal for a delta naming any administrative field, and it names the
offending field and directs the caller to `PATCH /orders/{orderId}` (§4.1). It is the mirror of
capture's `commercial-field-immutable` — that reason refuses commercial content on the
administrative-edit trigger (a defensive guard, since capture's trigger selection never routes a
commercial field there, D-145), this one refuses administrative content on the commercial surface — and
the two are distinct names because they refuse on opposite operations and point the caller in
opposite directions. `amendment-reason-invalid` is registered **here and only here** (D-129): it refuses an amendment
whose `amendment_reason` is absent or outside 1–4096 characters, and it is a name distinct from
`amendment-empty`, which refuses an empty delta — two conditions, two names (D-38).
`payer-rebinding-requires-seller` refuses a payer change that crosses seller
scope as §2.2 defines it (D-128), which §2.2 and §4.1 establish is never admissible, since `sellerTenantId` cannot be
rebound to accompany it. An amendment attempted from
`in_fulfillment` or a terminal state resolves to the engine's own `not-admissible`, which carries
the current state and trigger — no row exists for those pairs, so a slice-local
`amendment-not-admitted-in-state` was unreachable from every path and is deleted, matching the
treatment [`05-preconditions`](./05-preconditions.md) §3.3 and
[`07-hold-and-expiry`](./07-hold-and-expiry.md) §3.3 give the analogous cases. The
stale-version condition uses the engine's own `version-conflict`, and a commercial field edited
by `PATCH` outside `draft` is the engine's `not-admissible` for `draft-mutate` (D-145) — one name per condition
([`../DECISIONS.md`](../DECISIONS.md) D-38). Gate reasons are passed through unchanged from
[`03-gate-and-pin`](./03-gate-and-pin.md).

### 3.4 Internal Dependencies

| Dependency Gear | Interface Used | Purpose |
|-------------------|----------------|----------|
| `toolkit-db` | Runtime-scoped access, via the engine | Version append and read inside the transition transaction |

### 3.5 External Dependencies

None directly. The gate re-run reaches pricing, rating, account-management, contracts and
subscriptions, but does so **through** [`03-gate-and-pin`](./03-gate-and-pin.md), which owns
those ports. This slice holds no adapter of its own, so an upstream contract change lands in one
place.

### 3.6 Interactions and Sequences

#### Amend an order

**ID**: `cpt-cf-bss-orders-lifecycle-seq-amend-order`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-amendment`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`

**Algorithm: Append Amendment**

Input: order_id, delta, amendment_reason, expected_version, idempotency_key, security_context
Output: the new version, or a registered refusal

1. [ ] - `p1` - Declare the amendment's guards in this one complete registration order; the engine evaluates them in this order and the first failing guard is the refusal (`01 §4.1`): - `inst-am-declare-guards`
   - (1) the amendment cap (`amendment-cap-exhausted`, §4.1);
   - (2) a non-empty delta (`amendment-empty`);
   - (3) a valid explanation: `amendment_reason` present and 1–4096 characters, matching `01 §3.7` `orders_order_version.amendment_reason` (`amendment-reason-invalid`, D-129);
   - (4) no administrative field (`administrative-field-in-amendment`, naming the field and directing the caller to `PATCH /orders/{orderId}`);
   - (5) no commercial-frozen field (`tenant-axis-immutable`);
   - (6) no payer change that crosses seller scope, as §2.2 defines it (`payer-rebinding-requires-seller`, D-128);
   - (7) capture's shared structural guards (`01 §3.6`): `category-not-admitted`, `currency-mixed`, and `line-cap-exceeded` applied to the complete proposed line set, since an amendment can add lines (the declared cap, baseline 200, [`02-capture`](./02-capture.md) §3.7);
   - (8) the shared date guard (`date-cascade-invalid`, capture §4.2);
   - (9) the gate composite owned by [`03-gate-and-pin`](./03-gate-and-pin.md), including its pin outcomes.

   The engine first applies authorization, idempotency resolution, state-table admissibility and expected-version checking (`01 §4.1`); admissibility is not a slice guard. Thus an amendment from `in_fulfillment` returns `not-admissible` even when the amendment cap is exhausted. An unclassified field fails startup ([`02-capture`](./02-capture.md) §4.3); there is no runtime refusal for it, so every delta field has a class at runtime. A delta key naming no authored field is rejected at boundary validation with `request-invalid` (`01 §4.7` *Validation flow at the boundary*, D-142), before authorization and before the engine
2. [ ] - `p1` - Resolve those guards' inputs: classify every delta field through the shared declaration, and determine whether a `payer_tenant_id` change crosses seller scope (§2.2, D-128) from the payer's commercial profile returned by the identity operation, resolved through [`03-gate-and-pin`](./03-gate-and-pin.md) (`03 §3.6` *Run Gate and Submit* step 2) once per run and reused by step 5's gate run rather than read again; that operation's unavailability or deadline contributes `identity-party-unavailable`. Do not treat a paired seller change as permission: `sellerTenantId` is commercial-frozen and its guard has precedence inside the engine. When a local guard already fails on its resolved inputs, skip dependent external work and contribute the failing input to step 11 for the engine to decide and audit (§2.2, §4.1); the skipped inputs are **precluded** as defined in `01 §4.1` *Precluded inputs* (D-113). A precluded input is never unresolvable, so the engine reports the earlier guard's reason — e.g. `amendment-cap-exhausted`, not a 503 — and, even where another input is genuinely unavailable, its step 3.1 settles an earlier-registered failing guard first - `inst-am-resolve-guard-inputs`
3. [ ] - `p1` - Carry forward the current version's commercial content - `inst-am-carry-forward`
4. [ ] - `p1` - Apply the delta over the carried-forward content - `inst-am-apply-delta`
5. [ ] - `p1` - Resolve the full shared gate over the amended content outside the transaction, preparing its policy snapshot and proposed dates before date-dependent calls under capture §4.2, re-deriving the market and fixing one catalog frontier for the entire run — the **seller's** catalog frontier, read for `seller_tenant_id` through the operation taking the catalog tenant explicitly, a PEP denial refusing `catalog-frontier-unavailable` rather than 403 (D-122). The gate's parallel resolve step also re-composes every line's pin at that frontier whatever the other inputs answer (`03 §3.6` *Run Gate and Submit* step 5, D-123). Resolve every proposed line's overlap key through the gate's catalog product-key operation at that fixed frontier (`03 §3.6` *Run Gate and Submit* step 4), and the proposed payer; persist the key with the proposed line and include the complete distinct `(proposed payer_tenant_id, overlap_scope_key)` claim set, including retained lines, in the contribution - `inst-am-rerun-gate`
6. [ ] - `p1` - **IF** the gate refuses or any input is unevaluable: retain its complete outcome — including every line's pin outcome — and failures as declared guard inputs; skip only total assembly that depends on unavailable inputs and continue to step 11. Never return before the engine audits and settles the refusal - `inst-am-if-gate-refuses`
7. [ ] - `p1` - Take every line's catalog price pin re-composed by step 5's gate resolution at the same frontier; its failures are already in the gate outcome carried to step 11, and a line whose reference is unresolvable carries an `unevaluable` pin outcome with that reason, not a second failure - `inst-am-repin`
8. [ ] - `p1` - Re-capture the resolved total and the TCV figure - `inst-am-recapture-total`
9. [ ] - `p1` - **IF** the current state is `pending_approval` or `approved`: the transition target is `submitted`, and no verdict is read - `inst-am-target-submitted`
10. [ ] - `p1` - **ELSE** the current state is `submitted` and the target is the current state - `inst-am-target-unchanged`
11. [ ] - `p1` - Request the amendment transition with all guard inputs and gate outcomes; on success contribute the new version with validated amendment_reason, proposed dates/date basis and policy snapshot, pins, totals and complete proposed overlap claim set. The shared date guard validates transition-date defaults against `UTC-date(t)` before consuming dependent results, refusing a changed basis under capture §4.2. An absent or incomplete claim set is never permission to bypass claim maintenance for an amendment. The engine atomically replaces claims under `01 §3.6`; any refusal retains the old version and entire old claim set - `inst-am-request-transition`
12. [ ] - `p1` - **RETURN** the engine's committed new version or settled refusal, unchanged on idempotent replay - `inst-am-return-version`

**Description**: Steps 3 through 8 are the carry-forward-and-re-resolve contract: content is
inherited, gate output never is. Step 9 is the only place amendment moves state, and the target
is always `submitted` — this slice reads no approval verdict and makes no direct outbound call. Every refusal this
algorithm can produce is a **declared guard**, so none of them returns ahead of the engine and
each one audits and settles (`01 §4.1`,
[`../ADR/0005-cpt-cf-bss-orders-lifecycle-adr-refusals-commit.md`](../ADR/0005-cpt-cf-bss-orders-lifecycle-adr-refusals-commit.md)).

Verification must cover an inadmissible state combined with an exhausted cap (the engine's
`not-admissible` wins); changed payer with the same textual overlap key (a distinct claim tuple);
line addition/removal replacing the complete claim set; and a collision after another missing
claim has been provisionally inserted (all provisional business changes roll back). A failed
gate must commit its outcome, audit and idempotency refusal, leave versions/claims unchanged,
and replay without another upstream fan-out.

#### Amendment supersedes in-flight approval

**Sequence ID**: defined in [`../DESIGN.md`](../DESIGN.md) §3.6 as `cpt-cf-bss-orders-lifecycle-seq-amendment-supersession`; this section specifies its mechanics

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-amendment`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`, `cpt-cf-bss-orders-lifecycle-actor-orders-workflow`

```mermaid
sequenceDiagram
    participant A as Partner Admin
    participant L as Orders Lifecycle
    participant W as Orders Workflow
    A ->> L: amend (expected version 2)
    L ->> L: append version 3, re-run gate, re-pin
    L -->> A: version 3 current
    L ->> W: OrderAmended (version 3)
    W ->> W: cancel the open gate for version 2, open one for version 3
    W ->> L: reflect approval (version 2) - late decision
    L -->> W: refused - version-conflict
```

**Description**: No lock is held across the human approval wait; the engine still serializes each
transition inside its database transaction. The late decision is refused because the version it
names is superseded — as `version-conflict`, not `not-admissible`, although version 3 has moved the
order to `submitted`, because the engine checks a workflow-class trigger's version before its
admissibility (`01 §4.1`, D-110) — and the sibling gear learns of the supersession from the event rather than
from a callback. This is the whole of the concurrency design for a human-paced approval.

#### Administrative edit

**ID**: `cpt-cf-bss-orders-lifecycle-seq-administrative-edit`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-amendment`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`, `cpt-cf-bss-orders-lifecycle-actor-orders-direct-customer`

**Algorithm: Apply Administrative Edit**

Input: order_id, optional line_id (line-level fields, via line `PATCH`), one or more named fields with their new values, expected_version, security_context, idempotency_key
Output: applied, or a registered refusal

1. [ ] - `p1` - Declare the slice guards in this registration order: line membership when line_id is supplied (`line-not-found`, refusing a line_id that is not a member of the current working set — the draft working membership in `draft`, the current version's lines after submit; D-117), then field classification, refusing a commercial field with capture's `commercial-field-immutable` — defensive, since capture's trigger selection sends any commercial field to `draft-mutate` and a `PATCH` never reaches it (D-145) — then **at least one named field changes**, refusing with `administrative-edit-unchanged` an edit whose every named new value equals the stored value (D-142, D-149). The engine checks state-table admissibility before these guards and returns its own `not-admissible` for a terminal state - `inst-ae-declare-guards`
2. [ ] - `p1` - Resolve those guards' inputs: classify every named field through the shared declaration and, for a line edit, read line_id's membership; the change guard compares the named values with the stored ones at the engine's locked read, so a concurrent edit cannot turn a checked change into a no-op - `inst-ae-resolve-guard-inputs`
3. [ ] - `p1` - Pass the new values as the contribution to the administrative-edit transition, which writes them to `orders_order_admin`, or to `orders_order_line_admin` keyed `(order_id, line_id)` when line_id is supplied - `inst-ae-contribute-value`
4. [ ] - `p1` - The engine writes the values and appends **one audit entry per changed field**, each carrying that field — named `lines/<line_id>/<field>` for a line field — with its prior and new value, consecutive in `sequence`; it settles the idempotency record with the last entry and commits - `inst-ae-engine-writes`
5. [ ] - `p1` - **RETURN** applied; no version was appended and no OrderAmended was published - `inst-ae-return-applied`

**A no-op edit is refused, never a success (D-142, D-149).** An edit naming no field is rejected at
boundary validation with `request-invalid` (`01 §4.7` *Validation flow at the boundary*), before
authorization and unaudited. An edit whose named fields all already hold their new values cannot
be recognised there, because validation precedes any state read; it reaches step 1's change guard,
which refuses it `administrative-edit-unchanged` (FailedPrecondition, 400) as an ordinary audited,
settled guard refusal. The request is well formed; what refuses is the stored state, so it is
not the boundary-only `request-invalid`. A named field
whose value is unchanged beside one that does change writes no entry; only the changed fields do.
A committed edit therefore always appends at least one entry, the one step 4's settlement needs.

**Description**: The absence of a version bump and of an event is the point. Correcting a
mistyped purchase-order number must not invalidate an approval or restart a process, and the
per-field audit entries are sufficient trace for fields that carry no commercial meaning. A
concurrent edit to the same field is last-write-wins (§4.6).

### 3.7 Database Schemas and Tables

This slice introduces no table. It owns the content of `orders_order_version`
(`cpt-cf-bss-orders-lifecycle-dbtable-order-version`) specified normatively in
[`01-foundation`](./01-foundation.md) §3.7, and two additions belong here:

The engine alone assigns `supersedes_version` and moves the current-version pointer. Slice
contributions supply content and the validated `amendment_reason` (1–4096 characters), not
those assignments. Store that explanation on the appended version, separately from the closed
machine reason `amendment`; the committed amendment's audit entry carries the same text as its
`caller_reason`, with `amendment` as its `reason` (`01 §3.7`, D-143). An absent explanation, or one outside 1–4096 characters, is refused
by the declared guard `amendment-reason-invalid` (§3.6 *Append Amendment* step 1, D-129).

- The `supersedes_version` invariant is **linear**: it references the immediately prior version, and there is no branching. Enforced by the engine as an invariant with its own verification test (`01 §3.7`: no DDL can express a cross-row rule), not by convention, because a branch would make "the current version" ambiguous.
- An **administrative edit writes no version row** and mutates no append-only row. It writes the mutable administrative tables, and its trace is the audit entry's `changed_field`, `prior_value` and `new_value` columns ([`01-foundation`](./01-foundation.md) §3.7). A reader reconstructing the commercial trail reads versions; a reader auditing every change reads the audit log.

Version retention follows the program retention policy. There is no delete path.

### 3.8 Deployment Topology

Inherited from [`01-foundation`](./01-foundation.md) §3.8. No background worker.

**Observability owned here**: amendments per order and the version-depth distribution, because an
order accumulating versions is either a negotiation or a client defect and the two need
separating; `version-conflict` refusal rate on the workflow-only operations, which is the signal
that the sibling gear is racing an amendment — complete, because the engine checks a
workflow-class trigger's version before admissibility, so a race the amendment also moved out of
the trigger's from-state is counted here rather than as `not-admissible` (D-110); gate re-run outcome on amendment split by
predicate, since an amendment that newly fails the gate is a commercial event, not an error; and
administrative-edit volume by changed field, which is the audit-facing series. Alerts fire on the
`version-conflict` rate crossing its threshold, and on an amended order dwelling in `submitted`
beyond the sibling gear's reflection lead time — which is the signal that the two-step re-approval
of §4.3 has stalled.

## 4. Additional Context

### 4.1 Admissibility (normative)

**The number of amendments per order is capped, and the cap is a commercial value owned here.**
`orders_order.amendment_count` ([`01-foundation`](./01-foundation.md) §3.7) is incremented by rows
18, 19 and 20 and reset by no transition, and all three rows carry a registered guard refusing
`amendment-cap-exhausted` once it reaches the cap. The baseline is **20 amendments per order**.

*Why a cap exists at all.* Rows 19 and 20 target `submitted` from `pending_approval` and
`approved`, so the effective target differs from the outgoing state and `01 §3.6` *Attempt
Transition* step 20.1 **resets `state_entered_at`**. `approved → submitted → approved` therefore restarts the dwell
clock on every cycle, which is the same unbounded-lifetime loop D-90 closed for hold/resume,
available through a second operation. Capping resumes alone left it open
([`../DECISIONS.md`](../DECISIONS.md) D-90).

*Why the cap is separate from the resume cap rather than one shared budget.* A shared counter would
be structurally tidier — one column, and any future clock-resetting row covered by construction —
and it is rejected on commercial grounds: a resume is a **seller-side operational** act (a
compliance hold, a dispute) and an amendment a **buyer-side commercial** one (negotiation). One
budget would let a seller's holds silently consume a buyer's ability to correct their own order,
which is the wrong failure to design in.

*Why 20.* Negotiated orders revise two to five times in practice, so twenty is roughly four times
the plausible upper end and never fires in honest commerce. An amendment is a **re-quote, not an
edit** — it re-runs the full gate — the adopted catalog predicates and the nine Orders delta predicates (`03 §4.2`), re-pins every line and resets the approval clock
(§3.6) — so nobody reaches twenty by accident, and the cap also bounds a cost nothing else bounded:
repeated amendments can invoke all seven submit-path outbound operations. It bounds a **buyer-facing** action, unlike the
resume cap, so it is deliberately generous: an order a buyer cannot correct is a worse outcome than
a long-lived order. A twenty-first revision is a signal to re-place the deal as a new order, and
the refusal names the cap and the count so the caller can tell which. A deployment **MAY** raise or
lower it and **MUST NOT** unset it; there is no unlimited value.

Amendment **MUST** be admitted from `submitted`, `pending_approval` and `approved`, and **MUST
NOT** be admitted from `draft`, `on_hold`, `in_fulfillment` or any terminal state. From `draft`
the content is freely editable and no version is warranted. From `on_hold` the order is paused
and the pre-hold state governs what is permitted, so an amendment resumes first. From
`in_fulfillment` onward only cancel and hold remain.

An amendment **MUST** carry a non-empty delta and an expected version. A missing or unparseable
expected version is rejected at the boundary with `expected-version-required` under `01 §4.1`
(D-112), before authorization and before the engine; an empty delta is the engine-audited
`amendment-empty` guard. It **MUST** also carry an `amendment_reason` of 1–4096 characters; an
absent or out-of-range explanation is the engine-audited `amendment-reason-invalid` guard (D-129),
never `amendment-empty`. An amendment whose delta
names **any** administrative field **MUST** be refused with `administrative-field-in-amendment`,
naming the offending field and directing the caller to `PATCH /orders/{orderId}` — rather than
silently appending a version that changes no commercial content, or writing administrative content
onto an append-only version row where §3.7 says it never lives.

**The rule is "any", not "only".** A **mixed** delta naming both an administrative field and a
commercial one is refused on the same reason: the caller is asking for two operations with
different audit semantics — one appends a version and re-runs the gate, the other does neither —
and splitting it silently would either bump a version for an administrative correction or apply an
administrative change with no `PATCH` audit entry. The caller **MUST** send the two separately.
**Precedence is the one registration order of §3.6 *Append Amendment* step 1.** Guards refuse
in **registration order** (`01 §4.1`), after the engine's authorization, idempotency, state and
expected-version checks: amendment cap, `amendment-empty`, `amendment-reason-invalid`,
`administrative-field-in-amendment`, `tenant-axis-immutable`, `payer-rebinding-requires-seller`,
capture's shared structural guards (`category-not-admitted`, `currency-mixed`, and
`line-cap-exceeded` over the complete proposed line set), `date-cascade-invalid`, then the gate
composite. So where a delta names both an administrative field and a commercial-frozen axis, the
refusal is `administrative-field-in-amendment`; where an empty delta also carries a bad
explanation, it is `amendment-empty` — the outcome is deterministic rather than
implementation-dependent. An unclassified field fails startup (`02 §4.3`); there is no runtime
refusal for it, and a delta key naming no authored field is rejected earlier, at
boundary validation, with `request-invalid` (`01 §4.7`, D-142).

**The tenant axes are not uniformly amendable.** `payerTenantId` **MAY** change through an
amendment **only where the change does not cross seller scope**; a cross-seller payer change
**MUST** be refused with `payer-rebinding-requires-seller`, because the paired seller rebinding it
would require is unavailable — see §2.2 *A payer change must not cross seller scope*.
`resourceTenantId` **MUST NOT** change through any path once the order is `submitted`, and
`sellerTenantId` through any path once the order is created; an amendment delta naming either
**MUST** be refused with `tenant-axis-immutable`, which also refuses a draft edit naming
`sellerTenantId` ([`02-capture`](./02-capture.md) §4.1; [`../DECISIONS.md`](../DECISIONS.md)
D-119). **No seller rebinding is possible at any point**, so nothing in this slice pairs one with
a payer change. This slice previously asserted that payer was the only axis
with an amendment path and registered no guard for it, so the assertion was unenforced and a delta
rebinding the resource recipient or the selling party would have committed
([`02-capture`](./02-capture.md) §4.3 *Commercial-frozen*;
[`../DECISIONS.md`](../DECISIONS.md) D-62).

### 4.2 Carry forward and re-resolve (normative)

The new version **MUST** inherit every commercial field the delta does not name, and **MUST
NOT** inherit any gate output. Specifically, the catalog price pin, the resolved total, the TCV
figure and the order market **MUST** be re-resolved in full as part of the amendment commit.

The rejected alternative was requiring the caller to resubmit the whole document. It was
rejected because a caller reconstructing an unchanged five-line basket to alter one quantity
will eventually reconstruct it wrongly, and because the diff between versions is then the
caller's artefact rather than the store's. The rejected shortcut in the other direction —
inheriting the pin to avoid a catalog round trip — is worse: it would carry a stale catalog
version into a version the buyer believes is current, defeating the pin's only purpose.

An amendment **MUST** publish `OrderAmended` carrying the new version, **even when the order's
state does not change**, so a consumer never has to infer that a version bumped from the absence
of a state event.

### 4.3 Re-approval is a two-step seam interaction (normative)

An amendment from `pending_approval` or `approved` **MUST** transition the order to `submitted`
and publish `OrderAmended`. This slice **MUST NOT** read, derive or request an
approval-requirement verdict for the new version. The sibling gear, on consuming `OrderAmended`,
obtains the verdict for the new version and reflects the order onward through the rows that
already exist: `submitted → pending_approval` where approval is required (row 7) or
`submitted → approved` where it is not (row 8).

**Why the verdict cannot be a guard here.** A verdict for version N+1 is unobtainable at the
moment the amendment commits, because version N+1 does not exist until it does. Verdicts are
stored only as reflections keyed `(order_id, version)`
([`06-workflow-seam`](./06-workflow-seam.md) §3.7), that slice's §4.2 forbids deriving one for an
amended version from the version it superseded, this gear declares no port to the approval policy
owner ([`../DESIGN.md`](../DESIGN.md) §3.5), and PRD §12 AC-11a forbids it to *query* that owner.
A guard on the new version's verdict is therefore not a hard guard to satisfy — it is one nothing
in the specified system can ever satisfy, which made the previous rows 19 and 20 unreachable and
amendment from `approved` impossible.

**The objection this answers.** The earlier design rejected landing in `submitted`
unconditionally on the grounds that it "left row 20's `pending_approval` target unreachable and
silently narrowed a PRD edge". Half of that objection is now moot and half is met head-on. The
earlier two-row split is folded into the `approved → submitted` row, which carries the §3.6 amendment guards but no guard on the approval verdict,
and `pending_approval` is reached by row 7, a real edge the sibling gear already drives. What
remains is a genuine divergence: PRD §6.1's diagram declares a **direct**
`approved → pending_approval` amendment edge, and this design reaches that state in two steps
instead. That divergence is **disclosed, not silent** — recorded as
[`../DECISIONS.md`](../DECISIONS.md) D-61 and routed to Product as Q-12. The same route discloses
that this design's `pending_approval → submitted` amendment transition conflicts with PRD §6.1's
statement that amendments from `pending_approval` do not change order state; §10 UC-002 step 3
and §12 AC-5 then require a return to the pre-approval state, while §5.1 and §6.2 scope that
clause to `approved` only.

**What a caller observes.** An amendment returns the new version with the order in `submitted`.
An order that requires approval is briefly in `submitted` before the sibling gear reflects it to
`pending_approval`; consumers keyed on `OrderAmended` see the version, and consumers keyed on
state see one extra transition. Nothing is lost: the audit trail records both, and the two-step
path is why no amendment can ever be refused for want of a verdict.

### 4.4 Stale results (normative)

Once version N+1 exists, an approval reflection or fulfillment acknowledgement carrying version
N **MUST** be refused with the engine's `version-conflict` reason — the identifier for the
PRD's stale-version condition ([`01-foundation`](./01-foundation.md) §4.2). The engine performs the check, and
for these callers it performs it **before** state-table admissibility: every such result is a
workflow-class trigger, for which `01 §4.1` orders the version check first
([`../DECISIONS.md`](../DECISIONS.md) D-110). That ordering is what makes this rule hold after an
amendment from `pending_approval` or `approved` (`01 §4.3` rows 19 and 20), which moves the order
to `submitted`, where no row exists for the late trigger; checked the other way round, the stale
result would be refused `not-admissible` and never name a version. This slice
owns the caller contract, which has two obligations. The refusal **MUST** name the current
version, so a caller can re-read and retry rather than poll blindly. And a refused stale result
**MUST NOT** be treated as a failure of the operation it reports — the approval that was granted
against version N really was granted, and the sibling gear's correct response is to open a gate
against version N+1, not to record a denial.

### 4.5 Version history (normative)

Every version **MUST** record the full commercial content at that version, the actor who created
it, the timestamp, the version reason (`create`, `submit` or `amendment`), and the
`supersedesVersion` reference. Every version
**MUST** remain retrievable by order identity and version number for the retention period. No
path **MAY** update or delete a version row.

The reason vocabulary is registered rather than free text — `create`, `submit` and `amendment` —
because a consumer reconstructing the commercial trail keys on it.

### 4.6 Administrative edits are last-write-wins (normative)

Administrative fields — order-level and line-level alike ([`02-capture`](./02-capture.md) §4.3
*Administrative*) — are **last-write-wins per field**. An administrative edit is guarded only by
`expected_version`, which it never advances (§3.3), so two concurrent edits against the same
version both commit and the later-committed value stands. This is accepted, not a defect: every
change **MUST** be audited per field with its prior and new value (§3.6 *Apply Administrative
Edit* step 4), so an overwritten value is always reconstructible from the audit trail, and the
fields carry no commercial meaning that a lost intermediate value could corrupt. There is **no
administrative revision token**. The rejected alternative was an `admin_revision` counter on the
aggregate, bumped by `01 §4.3` row 3 and required as `expected_admin_revision`, mirroring
`draft_revision`: it would make every purchase-order correction a read-then-write round trip and
add a second concurrency token to clients for fields whose history is already complete
([`../DECISIONS.md`](../DECISIONS.md) D-120).

## 5. Traceability

- **PRD**: [`../PRD.md`](../PRD.md) — §6.2 amendment and version history
- **Gear design**: [`../DESIGN.md`](../DESIGN.md) — realises `cpt-cf-bss-orders-lifecycle-component-versioning`
- **Engine**: [`01-foundation`](./01-foundation.md) — version chain, version check, audit
- **Depends on**: [`02-capture`](./02-capture.md) field classification; [`03-gate-and-pin`](./03-gate-and-pin.md) for the gate re-run and re-pin
- **Consumers**: [`06-workflow-seam`](./06-workflow-seam.md) reacts to `OrderAmended` and receives the `version-conflict` refusal; [`08-read-and-authz`](./08-read-and-authz.md) serves the version reads
- **ADRs**: [`ADR/0001`](../ADR/0001-cpt-cf-bss-orders-lifecycle-adr-transition-through-engine.md) transition through the engine; [`ADR/0002`](../ADR/0002-cpt-cf-bss-orders-lifecycle-adr-slice-decomposition.md) the foundation-plus-seven-slices decomposition
