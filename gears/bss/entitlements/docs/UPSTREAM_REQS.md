---
description: "Inbound requirements from Subscriptions and the provider prerequisites of the Entitlements application layer, with ownership and acceptance evidence."
---

# UPSTREAM_REQS: Entitlements

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Requesting gears](#12-requesting-gears)
- [2. Requirements](#2-requirements)
  - [2.1 Subscriptions](#21-subscriptions)
  - [2.2 Pricing and Products](#22-pricing-and-products)
  - [2.3 Enforcement providers](#23-enforcement-providers)
  - [2.4 Resource provider and operators](#24-resource-provider-and-operators)
  - [2.5 Provider prerequisites and bilateral acceptance](#25-provider-prerequisites-and-bilateral-acceptance)
- [3. Priorities](#3-priorities)
- [4. Traceability](#4-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

This register names the requirements consumers place on Entitlements and the provider work those requirements depend on. Every incoming requirement below is covered by the [PRD](PRD.md); implementation belongs to [DESIGN](DESIGN.md) and [features](DECOMPOSITION.md). TARGET means proposed documentation, not delivered code or another owner's approval.

### 1.2 Requesting gears

| Consumer | Needs from Entitlements |
| --- | --- |
| Subscriptions | Fenced intake of `CommercialPostureV1`; application of every committed revision to the enabled providers; aggregate `ReadinessObservationV1` for the complete manifest of an activation operation/posture revision; materialization state and findings readable per posture revision |
| Pricing | The versioned semantic schema of quota and grant rules it stores on PlanRevision and freezes in acceptance |
| Products | The metric, unit and subject semantics the schema needs and the requirement for a versioned mapping to enforcement metrics |
| Quota Enforcement, License Enforcement or the selected license adapter, resource provider | A trusted, authorized projector of committed accepted state with stable source keys, monotonic revisions, digests and recovery by original identity |
| Platform operator | Desired versus applied state, findings and authorized repair without direct edits of provider records |

## 2. Requirements

### 2.1 Subscriptions

#### Commercial posture

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-upreq-commercial-posture`

Entitlements MUST accept `CommercialPostureV1` published by Subscriptions from the same transaction as its lifecycle change, through the Subscriptions outbox and through its recovery reads. Preparation context carries the snapshotted attempt policy/version, authorized start and deadline; terminal resolution retains its timeout/failure reason. The payload carries lifecycleAction and the complete resourceDispositions snapshot under DESIGN §3.3.1, including versioned cutoff, hold, schedule and deletion-authorization evidence; updates advance the aggregate revision even without a status change.

For credit trials, the payload also carries `trialCoverage` under DESIGN §3.3.1, binding the authenticated Billing commitment reference/digest and reserved grant to the exact activation operation/preparation generation and accepted scope/terms. The same reference is retained through activation resolution. The payload carries identity `(orderingTenantId, subscriptionId, revision)`, a payload digest and an operation id; the resource, payer and seller tenant axes; one member per logical component with capability/product references, quantity as the sum of effective lots, validity interval, posture `Granted | Restricted | Revoked` and holder scope; and provenance as accepted snapshot references (acceptance id, rule/binding snapshot digests) and contributing lot ids.

Entitlements MUST authenticate the producer and the tenant scope before acceptance.

Entitlements MUST fence every delivery against its intake journal: a lower revision is stale and refused; equal revision and equal digest is a replay and returns the existing result; equal revision and different digest is a conflict, refused and visible; a higher revision supersedes the target while earlier outcomes are retained. Entitlements MUST apply each accepted revision to every enabled capability as one complete desired state per provider source key, with explicit tombstones, under the fenced apply contract of [DESIGN §3.3.4](DESIGN.md#334-quota-apply-and-read).

Lots are provenance only; Entitlements MUST NOT re-derive quantity from lots, and MUST read rules from the accepted snapshot the provenance names, never from a live plan. Entitlements MUST report applied, pending, refused or unknown materialization per revision and MUST NOT report success because an adapter accepted a dispatch.

- **Source**: Subscriptions [UPSTREAM_REQS S-U04](../../subscriptions/docs/UPSTREAM_REQS.md#25-provider-prerequisites-and-bilateral-acceptance) and [DESIGN §3.3.5](../../subscriptions/docs/DESIGN.md#335-entitlement-and-retention-contracts).
- **Status / owner**: TARGET, Entitlements. Contract version JOINT OPEN with Subscriptions under E-U06.
- **Acceptance**: A replayed posture is a no-op; a conflicting digest is refused and visible; a posture lost in transit is recovered by revision from the Subscriptions recovery read; a revision 7 apply arriving after revision 8 revoked cannot restore access at any provider.

#### Readiness gate

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-upreq-readiness-gate`

For an activation revision Entitlements MUST build and persist the complete assignment/producer manifest before dispatch and prepare while the subscription is `preparing`. It MUST publish aggregate `ReadinessObservationV1` under [DESIGN §3.3.2](DESIGN.md#332-readiness-observation-contract), correlated by ordering tenant, scope, operation and posture revision/digest, with immutable manifest identity/version/digest, complete roster, Entitlements producer sequence and evidence. Subscriptions persists the operation and posture before dispatch, then binds the matching manifest result; no assignment-allocation handshake or vendor callback is required. Empty manifests require explicit no-preparation evidence and a retained aggregate result.

Publication and sequence changes MUST commit with the outbox; authorized recovery by operation/posture returns the full authoritative result after a gap or lost event. Adapter observations stay internal to Entitlements, which authenticates every producer and reports aggregate ready only when all required assignments/producers are ready. No subscription identifier crosses either readiness envelope.

Subscriptions waits: its activation operation stays pending or approved and the subscription stays `preparing` until the exact operation-bound aggregate manifest result reports `ready`; the commit then rechecks fences and writes active, T, receipt and outbox atomically. Entitlements MUST NOT treat receipts, broker acknowledgement or license existence as readiness, MUST NOT let one producer's ready satisfy another required producer or assignment, and MUST honor withdrawal: a delayed `ready` for a withdrawn revision cannot activate and still contributes historical evidence for cleanup.

- **Source**: Subscriptions [initial activation readiness contract](../../subscriptions/docs/features/05-entitlements.md#initial-activation-readiness-contract) (IA-2026-10-18, SUB-D-55).
- **Status / owner**: TARGET, Entitlements. Envelope JOINT OPEN with Subscriptions under E-U06; SUB-D-57 defines held-readiness arbitration and platform-borne preparation cost; attempt timeout/failure closes only through the Subscriptions fence with purchase/payment preserved for Orders remediation; exact resolution/recovery, clock/profile, cleanup and bounded exposure/accounting contracts remain `ENT-Q11` to `ENT-Q13` in [DECISIONS](DECISIONS.md).
- **Acceptance**: A lost ready event is recovered from the producer's authoritative read; a ready racing a withdrawal cannot activate; a wrong-tenant observation is refused without information leakage; an older sequence cannot overwrite a newer ready, withdrawn or failed result.

### 2.2 Pricing and Products

Pricing stores the rule block Entitlements defines and freezes it in the acceptance receipt the posture provenance names; Products supplies the metric, unit and subject semantics that block references. Neither places a MUST on Entitlements beyond publishing a versioned semantic schema and keeping its meaning stable per version; their own obligations are the E-U03 and E-U04 gates below.

### 2.3 Enforcement providers

Quota Enforcement and the selected license adapter require Entitlements to be a trusted, authorized projector of committed accepted state: commands bound to the resource tenant, subject membership proved through the resource authority, source ownership enforced by the provider, credentials that do not authorize uncontrolled customer writes, and authorization on recovery reads as well as writes. Entitlements never adopts or deactivates an operator record or another source's record by matching subject and metric. Their reciprocal obligations are E-U01 and E-U02.

### 2.4 Resource provider and operators

The resource provider requires work orders with operation identity and immutable resource generation, and retry by original identity. Platform operators require desired versus applied state, findings and an authorized repair route that uses only conditional provider operations; Entitlements never edits a license, counter or resource directly.

### 2.5 Provider prerequisites and bilateral acceptance

These are requests to other owners, not assertions of their implementation. The incoming requirements above remain the traceable product obligations. For each owner integration record the accepted version, producer and consumer, adapter mapping, refusal for unsupported capabilities and the executed fixture reference.

| ID | Owner | Required contract / status | Closure signal | Blocks |
| --- | --- | --- | --- | --- |
| E-U01 | Quota Enforcement | Versioned `apply_source_state(source_key, source_revision, digest, complete_target, effective_boundary, authority)` and authorized `read_source_state(source_key)` with durable operation recovery: durable authenticated source key `(namespace, resource tenant, assignmentId)`, uniqueness/ownership constraints, revision/digest compare-and-apply, terminal tombstones and recoverable results (existing create has no idempotency key, update has no expected version); recovery of applied outcomes by the stable key; demonstrated hard enforcement, suspension/revocation with fallback, counter preservation and isolation from operator and other-source records; explicit exclusive-end windows, immutable continuing counter identities, cap-below-used behavior and no reset on replay/update/renewal; scoped allocation preparation under frozen purchased capacity, same-source/member/allocation continuity across T and delayed resolution, denial of unrelated allocations and confirmed resource-release settlement without early counter reset (ENT-Q15); for a separately enabled consumption profile, recognizable readiness-check purpose in scoped adapter context and authoritative evidence under ENT-Q15, with provider-owned execution/quota treatment and no automatic dry-run, exemption or counter reset; authoritative ordered provider-restriction observations independent of source delivery/commercial revisions, preserving purchased desired caps and held-readiness/emergency precedence under ENT-Q03; the required storage/client/outbox runtime with end-to-end tests. EXTERNAL OPEN. | A delayed revision 7 apply is rejected against the applied revision 8 head after timeout, restart or lease loss; a lost-response apply is recovered by source key without a second record; readback exposes the applied head. | Quota projection; the hard-limit profile; allocation preparation under ENT-Q15 |
| E-U02 | License Enforcement, native License Manager or third-party adapter | Accept desired grant state by scope, assignment, desired revision and generation without a subscription identifier; stale-action suppression at the effect boundary; idempotent recovery of the same operation after a lost response; preparation grants while `preparing` reported as readiness observations. EXTERNAL OPEN. | An old issue cannot undo a newer revoke; pending versus actual application is recoverable; the readiness envelope is produced for every required assignment. | License grants; license-dependent initial readiness profiles |
| E-U03 | Pricing | Optional typed rule block on PlanRevision beside the trial block, validated at publication against Products evidence, frozen in `AcceptanceReceipt` with the bindings; replay reads the receipt, never the live plan. EXTERNAL OPEN. | Quota and grant rule branches are verified separately: changed terms require a new accepted revision; receipts resolve every selected rule/binding by digest; grant cases prove frozen replay, refusal of missing required evidence and one atomic price/quota/grant acceptance. | Any sale carrying quota or grant rules |
| E-U04 | Products | Metric, unit and subject-kind semantics; registration of enforcement metrics as GTS instances; versioned mapping from Products references to QE `MetricId` and `projection_type`. A `usage_type_ref` is not automatically a `MetricId`. EXTERNAL OPEN. | Every rule in an accepted snapshot resolves to a registered enforcement metric with unit and subject semantics. | Quota members for the affected metrics |
| E-U05 | Resource provider | Physical outcomes correlated to work order and resource generation with actual effective time and explicit pending/unknown/partial; non-destructive suspension; retention/export under frozen disposition terms; hold/delete arbitration at the execution boundary; retry by original identity, per-resource partial completion, no action on a recycled resource name; preparation cleanup reports actual resource release for QE occupancy settlement, with late-allocation and lost-release recovery under ENT-Q15. EXTERNAL OPEN. | Adapter mapping ratified and the late stop, hold racing deletion and reused name cases pass; E-AC43 preserve unresolved physical outcomes after supersession; automatic deletion stays disabled without the proven hold/execution fence. | Resource execution |
| E-U06 | Subscriptions | Subscriptions publishes `CommercialPostureV1` from its outbox and recovery reads and consumes `ReadinessObservationV1`; versions agreed bilaterally. JOINT OPEN. | Both gears cite the same contract versions, snapshot preparation policy/deadline and return authoritative applied/non-activation reasons for timeout/failure; E-AC35/E-AC36 prove closure, preserved purchase and safe successor cleanup; E-AC41/E-AC42 prove closure before manifest/commitment existence or delivery, delayed preparation fencing and partial-effect recovery. Replay/conflict, concurrent and empty manifests, lost aggregate results, withdrawal, and reordered disposition/hold/schedule snapshots pass on both sides. | Posture intake; readiness reporting; every activation profile that depends on initial readiness |
| E-U07 | Billing / Subscriptions / selected access adapter | Durable promotional-grant commitment bound to accepted terms, acquisition and exact activation operation/preparation generation; authenticated reference/digest in `trialCoverage`; grant effective at Subscriptions T and later recording without new approval or initial-access delay. Conditional coverage must survive unknown outcome and settle only against authoritative resolution. Accepted direction; SDK/provider mappings and execution remain OPEN under ENT-Q14. See the [trial feature](../../subscriptions/docs/features/06-trials.md#credit-preparation-and-recovery). | E-AC31 to E-AC33 prove exact binding, readiness before T, delayed recording without access delay, no pre-T bonus drawdown, bounded exposure/enforcement and cancellation/lost-outcome recovery without double grant or revival. | Credit-trial initial readiness; free-period and ordinary paid profiles do not require this provider. |

An alternative provider may satisfy E-U01, E-U02 or E-U05 without the native system, but it must demonstrate the same selected-profile guarantees. Each selected integration also proves the [SDK/plugin and durable binding contract](DESIGN.md#333-adapter-capability-profile): correct schema/scoped-client registration, accepted profile/version, stable target identity, authorized observation producer, and recovery without rediscovery or fallback. E-AC25 to E-AC27 cover concurrent binding, restart, missing clients and same-ID target drift.

Registry metadata does not close a gate, and a concrete native adapter uses the provider's public management API rather than internal plugins or tables. No direct database writes to any provider are allowed. Broker transport is optional for recovery correctness; recovery reads are not.

**Held readiness (SUB-D-57).** E-U06 includes the durable commitment and immutable Subscriptions applied/non-activation resolution carried through posture and authorized operation recovery. E-U02/E-U05 adapters must retain prepared assignments through unknown outcomes and prove emergency/cleanup behavior. ENT-Q13 also requires Subscriptions and Rating/Billing to classify authorized pre-T or never-activated preparation use as platform-borne with authoritative boundary evidence. E-AC28 to E-AC30 exercise lost applied publication, withdrawal races, missing outcomes, emergency denial and late preparation usage. Exact owner SDK, profile-bound and runtime gates remain open.

## 3. Priorities

Both incoming requirements are `p1`: they gate the first activation profile that depends on initial readiness and every posture application. An implementation can develop against contract fixtures, but mocks never close an external production gate. [DECOMPOSITION](DECOMPOSITION.md) allocates each active PRD requirement to one primary feature and names its prerequisites.

## 4. Traceability

- [PRD](PRD.md): `Covers` links map the two incoming requirement ids to product requirements.
- [DESIGN](DESIGN.md): architecture and contract authority; [§4.1](DESIGN.md#41-capability-gates) lists what each open gate blocks.
- [DECISIONS](DECISIONS.md): `ENT-D-01` to `ENT-D-03` and open questions `ENT-Q01` to `ENT-Q15`.
