<!-- CONFLUENCE_TITLE: [BSS]: Orders Lifecycle — Design Set -->
<!-- Related: ../DESIGN.md, ../PRD.md | Owners: BSS Orders team -->

# Orders Lifecycle — Design Set

<!-- toc -->

- [Slice documents](#slice-documents)
- [Slice map (PRD ↔ implementation phase)](#slice-map-prd--implementation-phase)
- [Authoring status](#authoring-status)

<!-- /toc -->

This folder holds the Orders Lifecycle technical design as a **set of slice designs**: a shared
**Order Transition Engine** (`01-foundation.md`) plus per-capability handler designs. Every
slice **transitions through** the Engine — the order aggregate and its append-only version
chain, the state-machine table and its guards, the idempotency registry, the optimistic version
check, the transition audit log, the typed event contract and platform producer-outbox binding,
and the machine-readable reason catalogue.
The Engine owns no commercial policy (it does not know what a sellability gate or a catalog
price pin is); each slice is a handler that declares guard predicates, contributes to the order
document, and calls the Engine's transition API.

**[`../DESIGN.md`](../DESIGN.md) is the canonical index for the architecture overview, the
component model, the cross-cutting posture and the traceability.** The **phased slice map and
dependency order are owned here**, per [`../ADR/0002`](../ADR/0002-cpt-cf-bss-orders-lifecycle-adr-slice-decomposition.md),
which names this table the build-order authority.
Requirements (WHAT/WHY) live in [`../PRD.md`](../PRD.md); decision rationale lives in
[`../ADR/`](../ADR/) and in the register [`../DECISIONS.md`](../DECISIONS.md); asks on gears this
one does not own live in [`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md).

## Slice documents

- [`01-foundation.md`](./01-foundation.md) — **shared engine**: order aggregate, append-only version chain, declarative state-machine table + guard evaluation, idempotency registry and its three non-success outcomes, optimistic version check, append-only transition audit, `event-broker-sdk::DbProducer`/`toolkit_db::outbox` binding and the one-event-per-committed-transition rule, machine-readable reason registry. Carries the gear-wide normative statements (§4.1–§4.5), the extension boundary (§4.6) and the canonical schema (§3.7) (PRD §6.1 state machine + idempotency, §6.5 events, §7.1).
- [`02-capture.md`](./02-capture.md) — order and line authoring in `draft`; the line model (SKU/plan/price references, quantity, contract-effective date, service-activation and acceptance-due dates with cascading defaults governed by the per-tenant `orders_date_policy` table, term duration, billing cycle, external reference); the single-currency basket rule; field-level classification of commercial versus administrative content (PRD §6.1 create + line dates).
- [`03-gate-and-pin.md`](./03-gate-and-pin.md) — the submit gate: published pricing sellability predicates adopted by reference plus the Orders delta (tenant axes, contract-active, purchase-quantity floor, order-market consistency, reference resolution, single currency, overlap uniqueness, one-in-flight-order); catalog price pin capture; non-authoritative resolved-total capture; the read-only Preview operation (PRD §6.1 submit, §9.1 Preview).
- [`04-versioning.md`](./04-versioning.md) — amendment from `submitted` / `pending_approval` / `approved`; new-version append with `supersedesVersion`; gate re-run and re-pin; historical version retrieval; the non-versioned audited administrative edit path; the absence of any amendment row from `in_fulfillment` onward (engine `not-admissible`; `cpt-cf-bss-orders-lifecycle-constraint-no-amendment-in-fulfillment`) (PRD §6.2).
- [`05-preconditions.md`](./05-preconditions.md) — the customer-acceptance instant as a recorded fact with its own transition and event; the acceptance-required election source; begin-fulfillment guard inputs including the payment-authorization outcome, the seller tolerate-failure election and its risk flag (PRD §6.1 acceptance + payment authorization).
- [`06-workflow-seam.md`](./06-workflow-seam.md) — the five workflow-only operations (approval reflection, begin fulfillment, the spawn signal, fulfillment acknowledgement, workflow-mediated cancel with compensation evidence); the recorded spawn signal anchoring the cancel guard; per-line line-to-subscription linkage; the read-only per-line fulfillment projection; the recorded deciding authority on every stored verdict (PRD §6.1 atomic fulfillment + linkage, §6.4 R1–R5).
- [`07-hold-and-expiry.md`](./07-hold-and-expiry.md) — hold and resume with the stored pre-hold state; per-state TTL policy for `submitted` / `pending_approval` / `approved` / `on_hold`; the coordinated expiry scheduler; the transition-table exclusion of `in_fulfillment` and of holds taken from it, with handoff to the sibling gear's operational escalation; caller-initiated cancel and its guards; the abandoned-draft auto-void sweep to `expired` (PRD §6.3).
- [`08-read-and-authz.md`](./08-read-and-authz.md) — the current-version read projection and the paginated tenancy-scoped list with its state, date and contract filters; historical version reads; exposure of expected fulfillment time and per-line deferral; audit-trail retrieval; the exhaustive per-actor permission matrix over all twenty-four endpoints, the shared platform PDP adapter, the recording-party rule for acceptance and the verifiable cross-tenant delegation proof (PRD §6.6, §9.1 reads).

## Slice map (PRD ↔ implementation phase)

The numeric prefix is **implementation order**, not the PRD section number — the two axes
deliberately do not line up. A slice is scoped by PRD decomposition but built when its
dependencies exist.

| Doc | PRD § | Phase | Depends on |
|-----|-------|-------|------------|
| `01-foundation` | 6.1 (state machine, idempotency), 6.5, 7.1 | 0/1 | Event Broker runtime, `event-broker-sdk` with `outbox`, `toolkit_db::outbox` |
| `02-capture` | 6.1 (create, line dates) | 1 | 01 |
| `03-gate-and-pin` | 6.1 (submit), 9.1 (Preview) | 1 | 01, 02 |
| `04-versioning` | 6.2 | 2 | 01, 02, 03 |
| `05-preconditions` | 6.1 (acceptance, payment auth) | 2 | 01, 02, 03 |
| `06-workflow-seam` | 6.1 (atomic fulfillment, linkage), 6.4 | 2 | 01, 03, 05 |
| `07-hold-and-expiry` | 6.3 | 2/3 | 01, 02, 06 |
| `08-read-and-authz` | 6.6, 9.1 (reads) | 2/3 | 01, 02, 03, 04, 06 |

Six of these edges are less obvious than the rest and are stated because the 2026-09-08 review
found them missing. `05` needs `03` because acceptance is a contribution to the submit
transition, which `03` owns. `06` needs `03` because the begin-fulfillment guard re-reads the
pinned snapshot and the market binding `03` captures. `07` needs `02` for the draft content the
auto-void sweep collects and `06` for the spawn signal its cancel guard reads. `08` needs `04`
for the historical version reads and `06` for the per-line fulfillment projection it embeds.

**Phase 1's successful submit path has open upstream prerequisites.** The gate fails closed
on an unevaluable input ([`../ADR/0003`](../ADR/0003-cpt-cf-bss-orders-lifecycle-adr-fail-closed-gate.md)).
Pricing's current [`domain/sellability.rs`](../../../pricing/pricing/src/domain/sellability.rs)
implements four predicate families: active-window coverage/horizon, committed catalog version,
availability dates and plan lifecycle. The GA/prepaid-execution flags and registry sellability
families remain `NotEvaluable`; the registry flag is owned by `products`. The older claim that
active-window coverage is unimplemented is superseded by Pricing's window-linkage implementation.

Implemented predicates are not yet a consumable Orders interface: the public
[`pricing-sdk/src/api.rs`](../../../pricing/pricing-sdk/src/api.rs) exposes only `pin_frontier`.
Batched fixed-version predicates and pin composition still require public SDK publication.
Bundles additionally require frozen component/key composition and wiring of Pricing's component
conjunction into that SDK. Its current own-price path omits component evaluation; publishing
that path unchanged would not satisfy Orders' gate. The dedicated bundle requirement in
`../UPSTREAM_REQS.md §2.2` remains open, with fail-closed bundle assessment until complete
pinned coverage is supplied.
`pin_frontier` also reads the caller's tenant rather than the seller's catalog, so the
explicit catalog-tenant reads (`…-upreq-pricing-catalog-tenant-reads`, D-122) are a prerequisite
too. No Rating SDK crate exists (`…-upreq-rating-evaluation`), Account Management exposes
`get_tenant` for tenant-axis validity but no payer commercial profile
(`…-upreq-payer-commercial-profile`), and Contracts has no implementation for contract status and
party eligibility (`…-upreq-contract-party-eligibility`); until they land the gate refuses
`evaluation-unavailable`, `identity-party-unavailable` and `contract-resolution-unavailable`.
Subscriptions' occupancy/cardinality read (`SUB-O5`) and activation enforcement are separate
open prerequisites. See `03 §2.2` and `../UPSTREAM_REQS.md` for owners and acceptance contracts.
Capture and engine development can proceed against doubles, subject to the authorization
integration prerequisite in `01 §3.6`; this is not a production-readiness claim. Product-visible
fail-closed behavior remains routed as `../DECISIONS.md` Q-15. Runtime implementation, public
interface availability and deployed authorization/integration evidence must be tracked separately.

Phase 0/1 is the correctness core and is a prerequisite for everything else. Its event-producing
runtime is additionally blocked until the Event Broker implementation exists: `docs/GEARS.md`
currently records “SDK landed — impl crate TODO”. Startup must prepare all event types, resolve the
managed chained producer and start the toolkit outbox workers before readiness. Capture and local
transition tests can proceed against an `EventBrokerApi` double; production event traffic cannot.
The four `p1` non-functional guarantees — audit completeness, zero duplicate effects, transition
latency and recoverability — are properties of the Engine, so no slice can be accepted before it
exists.
Phase 1 delivers the capture-to-`submitted` path, which is the smallest set that produces a
pinned, audited order. Phase 2 completes the commercial lifecycle and the sibling-gear seam.

## Authoring status

The [2026-09-23 High-register disposition](../reviews/2026-09-23-high-register-disposition.md)
maps all 33 High findings to design corrections and separates remaining implementation/platform
prerequisites. It is the current handoff for that review, not evidence of runtime test coverage.

The [Medium disposition](../reviews/2026-09-23-medium-register-disposition.md) accounts for all
28 Medium findings, including the local replacement PR-description draft awaiting publication.

**The design set is complete: all eight slices are authored, and five review waves have been
remediated.** Together with [`../DESIGN.md`](../DESIGN.md), this index, **seven ADRs**, the
decisions register and the upstream-requirements register, that is **nineteen artifacts**
covering every design item the PRD's twenty-two functional and seven non-functional requirements
imply. Six review waves are recorded with the team rather than in this set: the 2026-09-08 wave (`R-01`…`R-74`) and the 2026-09-09/10 waves, whose dispositions include the findings that were **declined** and the reasons why.

**No claim is made that the set has converged.** Wave 5 produced two CRITICALs, and a subsequent
sweep found seven further instances of one of them that wave 5 had not reached. Wave 6 reviewed
wave 5's *own fixes* and found five more CRITICALs, **all five introduced by those fixes** — an
Orders-owned outbox scheduling column that was indexed but never declared, monthly partitioning
incompatible with the ordering constraint it sat on (both since superseded by the platform
producer outbox), a savepoint that hid a phantom version rather than
preventing it, an absolute-lifetime backstop rendered inert by a shared idempotency key, and a
timed validity window no declared interface could carry. That is the honest shape of this set's
state: each wave has found real defects in the previous wave's remediation, and the rate is not
yet falling. Editors must review counts, citations, reason-name uniqueness, table/index
references and agreement with the transition table. This change adds `make design-check`
and a docs-workflow CI job for mechanical invariants, with positive/negative checker fixtures.
They do not establish semantic correctness or runtime integration; remote CI execution must
still be observed after publication.

**Structural review is not proof of correctness.** Step ordering, transaction scope,
cardinality coupling and whether each declared `MUST` has an implementable interface require
semantic review and runtime verification. A 2026-09-10 review found five defects of those kinds.

Phase 0/1 is [`01-foundation.md`](./01-foundation.md), the correctness core: the transition
contract, the idempotency semantics with their four exhaustive outcomes, the state machine as a
**twenty-seven-row** table with its normative exclusions, the audit and platform producer-outbox rules, and the
canonical schema for the Orders-owned Foundation tables inventoried in `../DESIGN.md §3.7`.

Two slices carry a dependency on an unagreed upstream ask rather than a gap in their own design.
[`03-gate-and-pin.md`](./03-gate-and-pin.md) needs the occupancy read (`SUB-O5`, amended by D-126) for the
against-existing-subscriptions half of the overlap rule; until it lands that half is unevaluable
and therefore a refusal, which fails closed. [`06-workflow-seam.md`](./06-workflow-seam.md)
depends on six: the compensation cancel reason (`SUB-O1`), the order reference on `create`
(`SUB-O2`), the occupancy read (`SUB-O5`, amended by D-126), correlation propagation (`SUB-O9`) and the
explicit subscription start instant (`SUB-O10`). Each is stated as a constraint naming its ask, so the
design is complete and the boundary is honest, and the **Workflow-side amendment verdict** (`…-upreq-workflow-amendment-verdict`). That last one is not upstream *code* but an upstream **document**: the sibling Workflow PRD restricts verdict acquisition to `OrderSubmitted`, so it needs amending before the two-step re-approval seam of [`04-versioning`](./04-versioning.md) §4.3 can be implemented at all (`p1`; see [`../DECISIONS.md`](../DECISIONS.md) Q-12).

One thing remains outstanding for the gear, and it is not a slice. The **`SUB-O*` register has
forked** — the Subscriptions seam map defines `SUB-O1` through `SUB-O6` while the sibling
Workflow PRD cites `SUB-O5` through `SUB-O9`, with `SUB-O6` carrying different
meanings on the two sides — and reconciling it is a document diff rather than a dependency on
code. It is tracked as Q-04 in [`../DECISIONS.md`](../DECISIONS.md), which treats the seam-map
numbering as canonical in the meantime.

Configuration values are split by owner rather than left uniformly blank. Values this design can
choose — sweep cadence, batch size, page sizes, port budgets — carry working baselines set in
[`07-hold-and-expiry.md`](./07-hold-and-expiry.md) §4.5,
[`08-read-and-authz.md`](./08-read-and-authz.md) §4.5 and
[`03-gate-and-pin.md`](./03-gate-and-pin.md) §2.2. Values that are commercial policy — the
per-state TTLs and the override scope — remain **PRD open questions owned by Product** and have
no code default, so that an unset value is visible as absent behaviour rather than silently
becoming the platform answer. The override scope's mechanism is specified but ships disabled
behind the default-off `ttl_seller_override_enabled` flag, so Product's answer becomes
configuration (`../DECISIONS.md` D-137).

- **ADRs**: [`ADR/0002`](../ADR/0002-cpt-cf-bss-orders-lifecycle-adr-slice-decomposition.md) the foundation-plus-seven-slices decomposition
