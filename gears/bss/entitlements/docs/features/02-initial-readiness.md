---
description: "Readiness manifest, preparation assignments, observation envelope, withdrawal and cleanup on the provider side of IA-2026-10-18."
---

# Feature: Initial Readiness

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Readiness manifest and observation envelope](#readiness-manifest-and-observation-envelope)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Observation acceptance](#observation-acceptance)
- [4. States (CDSL)](#4-states-cdsl)
  - [Readiness state](#readiness-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Manifest-gated readiness with recoverable observations](#51-manifest-gated-readiness-with-recoverable-observations)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-featstatus-initial-readiness-implemented`
- [ ] `p1` - `cpt-cf-bss-entitlements-feature-initial-readiness`

**Status:** proposed implementation; provider contracts and runtime conformance remain delivery gates.

## 1. Feature Context

### 1.1 Overview

For an initial activation Entitlements persists the manifest of every required assignment, makes the preparation assignments available through the selected adapters while the subscription is still `preparing`, accepts readiness observations against the manifest, and reports an operation-correlated aggregate result to Subscriptions only when every required assignment and producer is ready for the exact desired revision. Subscriptions performs the guarded commit and owns T.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-entitlements-fr-initial-readiness`.

**Primary NFRs:** `cpt-cf-bss-entitlements-nfr-handoff-latency`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-entitlements-actor-subscriptions` | Persists the activation operation, consumes `ReadinessObservationV1` and commits active with fresh T. |
| `cpt-cf-bss-entitlements-actor-license-enforcement` | Selected license adapter (LE, native LM or third party); prepares rights assignments and reports readiness. |
| `cpt-cf-bss-entitlements-actor-resource-provider` | Prepares the purchased environment and reports its readiness. |
| `cpt-cf-bss-entitlements-actor-platform-operator` | Inspects pending, failed and withdrawn preparation and its cleanup obligations. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [readiness observation contract](../DESIGN.md#332-readiness-observation-contract).
- **Build prerequisites:** as allocated in [DECOMPOSITION.md](../DECOMPOSITION.md); decisions in [DECISIONS.md](../DECISIONS.md).
- **Provider prerequisites / integration acceptance:** the joint freshness and withdrawal contract with Subscriptions and the per-profile readiness gates ENT-Q09 to ENT-Q15 in [DECISIONS.md](../DECISIONS.md#open-questions).

## 2. Actor Flows (CDSL)

### Readiness manifest and observation envelope

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-flow-initial-readiness`

**Actor:** Subscriptions, through `CommercialPostureV1` for the preparation revision; the selected adapters as producers of readiness observations.

**Success:** every required assignment of the manifest is `ready` for its exact desired revision and Entitlements reports ready to Subscriptions.

**Errors:** manifest cannot be built, unsupported `schemaVersion`, unauthorized producer, refused or quarantined observation, preparation `failed`, operation `withdrawn`.

Entitlements is the provider side of IA-2026-10-18 (ENT-D-02): initial activation completes only after all required initial service capabilities are confirmed usable. Subscriptions keeps the subscription `draft` while its durable activation operation is pending or approved. Preparation progress is observation state here and operation state in Subscriptions, not a new commercial status. This is an accepted design direction, not delivered SDKs, external-owner ratification or runtime proof. Renewals, resume, trial-to-paid conversion and modifications of an already active subscription keep their existing contracts.

1. Subscriptions persists its activation operation, authorized scope, preparation posture revision/digest and outbox before any preparation request or readiness observation can escape, and publishes `CommercialPostureV1` for the draft revision through [posture intake](01-posture-intake.md). Entitlements accepts the posture and durably schedules manifest derivation with its intake. The derivation transaction rechecks the source and preparation-generation fences and persists the manifest with the first aggregate result and publication intent. If a non-activation fence already closed the generation, retain that closure and do not initiate preparation or report ready.

   The manifest lists every required scoped assignment, desired revision, service profile and target, and the authoritative producer; optional and not-applicable capabilities are explicit. An empty manifest is permitted only when the selected accepted service profile authoritatively requires no preparation, never because discovery or an adapter failed. Its digest and version are bound to the operation and posture input. Persist the first aggregate result and its outbox intent with the manifest before adapter dispatch.

   Subscriptions binds a result through operation/posture identity, not through previously known assignments; composition or logical-target changes require a newer preparation posture and a new manifest binding.
2. The selected license adapter (LE, native LM or third party) makes the authorized preparation assignment available before commercial activation (ENT-D-01). The selected service adapter reconciles that assignment to the required platform state: for PaaS, creating or linking the account, deploying the environment included in the purchase and establishing necessary usable access. This does not wait for the customer's first login or a later VM purchase. Entitlements does not orchestrate each vendor step. It records the preparation authority and waits for the required readiness evidence.

   Preparation rights cannot be derived only from `active`, or the cycle would prevent activation. Reuse an existing assignment identity if it supplies the required scope, lifetime and revision semantics; no new license entity is mandated.
3. The [License Resolver](../../../../system/license-resolver/docs/DESIGN.md) is a read/check boundary, not the owner of issuing/revoking rights or a delivered change feed. The selected adapter's contract must supply authoritative assignment state and recovery. Event delivery, polling or another reliable feed is an integration choice (ENT-Q09); no broker or new issuance API is assumed by this document. Entitlements records an assignment/publication intent and its manifest association durably before dispatch, with recovery for external results. A fast callback must never be irretrievably orphaned by a not-yet-persisted association.
4. Each producer reports readiness per assignment with the envelope below. Entitlements accepts or refuses every observation under [observation acceptance](#observation-acceptance), persists it and advances the producer's high-water mark.
5. Entitlements reports an aggregate `ReadinessObservationV1` under [DESIGN §3.3.2](../DESIGN.md#332-readiness-observation-contract), bound to ordering tenant, scope, operation, posture revision/digest and the immutable complete manifest. It reports `ready` only when every required assignment and producer is ready for `desiredRevision` and the held commitment below has been persisted; Subscriptions performs the guarded commit and owns T (ENT-D-02). Entitlements never assigns T and never treats a ready event, a receipt or an intermediate approved operation as completion. All rights essential for initial usable service are ready before that commit.

   Post-commit reconciliation may reconcile the same satisfied assignment or deliver nonessential or ongoing changes; it cannot be the first action that makes required initial access usable. Initial readiness is not an uptime guarantee, and later service failure does not reopen the completed acquisition.
6. **Uncertainty, revocation and cleanup.** Entitlements persists external effects and durable lookup/reconciliation keys. Lost/unknown results, absence from a read, or a quiet broker are not proof of failure or non-execution; recover the same assignment and operation without allocating a second account/environment. The status/read contract must recover authoritative state and sequence after missed events, restart or retention gaps; dispatch logs are not completion evidence. A `failed` observation describes preparation failure, not proof all partial effects are gone.

   Withdrawal starts in Subscriptions, which fences its local activation first; Entitlements then marks the manifest withdrawn, revokes/supersedes preparation authority at the selected adapters, reconciles already running external actions and records cleanup/remedy obligations. Delayed ready for a cancelled/obsolete revision cannot activate; it still contributes historical effect evidence needed for cleanup. Adapter-side ordering must prevent delayed preparation from recreating a withdrawn environment, including work already in flight. Do not erase pending cleanup or release protected purchase rights because a draft disappeared.

   Cleanup targets only resources owned by this operation under the accepted policy, never an existing customer account merely because it was linked. Operational cleanup completion and refund/financial settlement remain separate outcomes.

**Held readiness procedure (SUB-D-57, ENT-D-02).** Once the complete manifest is ready, durably establish the provider retention guarantees and save a `ReadinessCommitment` with the aggregate ready result/outbox before publishing it. The immutable binding names ordering namespace, scope, operation, preparation revision/digest, manifest identity/version/digest and profile. Empty manifests also receive explicit commitments. Required assignments stay on their saved adapter bindings; a provider that cannot retain the prerequisite through an unresolved decision cannot enable this profile.

Subscriptions is the decision owner. Its activation and ordinary withdrawal serialize on one operation/preparation-generation fence. It publishes `activationResolution` in CommercialPostureV1, keyed by authenticated ordering namespace, scope, operation and preparation revision/digest, with a durable decision version and an authorized immutable recovery read. On `applied`, require the exact manifest identity/version/digest and readiness commitment binding, retain actual T and continue the same prepared rights only where the current desired head still permits them; a later restriction/cancellation remains authoritative. Historical settlement cannot regrant a revoked assignment.

On `closed_without_activation`, verify the exact preparation generation and irreversible local fence, plus any manifest/commitment bindings supplied. Closure is valid before a manifest or readiness commitment exists or has reached Subscriptions. Resolve local effects by their saved operation/preparation input; a later discovered manifest, commitment or partial effect belongs to the same closed generation without rewriting its terminal decision. Persist closure before processing delayed preparation, block new dispatch and ready publication, then fence in-flight provider work and perform only authorized cleanup.

No empty ready manifest or synthetic commitment is needed. For superseded preparation, require the retained non-activation decision for that old generation; a new revision or missing draft alone is not enough. Deduplicate resolution by identity/version/content, quarantine conflicts and never infer a historical outcome from current subscription status alone.

On pending/unreachable/missing outcomes, keep the commitment, retry recovery and escalate. Readiness failure and failure of an execution guard are not release authority. Timers and operator deadlines may request Subscriptions to close the attempt under its local fence; they cannot clean up first. Limit admission of new preparations to contain outstanding obligations. Do not create a second environment or switch providers to resolve uncertainty.

**Preparation deadline and closure.** Subscriptions snapshots the versioned service-profile preparation policy, authorized start and evaluated deadline on the activation attempt. Dispatch retry, polling, restart and profile edits cannot extend it; waiting for the requested future start is separate. Entitlements retains the deadline/evidence for observation and escalation and may request closure with correlated timeout or definitive-failure evidence. The saved profile distinguishes retryable/unknown observations from definitive failure; ordinary recovery retains the same attempt and deadline. Only Subscriptions decides the attempt at the same fence as activation.

After deadline that attempt cannot newly activate, even if a timer runs late or readiness was observed earlier. A prior applied winner is recovered with its original T; otherwise durable `closed_without_activation` names the timeout/failure reason. Lost requests or missing/unreachable results remain unknown.

A timer or failure never authorizes immediate cleanup. Recover the exact resolution first, then settle only that preparation generation under the existing resource, quota and credit contracts. Occupied capacity still requires confirmed release. Orders owns remediation of the retained purchase/payment/claim; Entitlements neither cancels the purchase nor initiates a refund. An authorized successor requires safe predecessor-effect settlement/quarantine and resource-generation fencing so old commands or cleanup cannot affect it. No second environment or provider switch may hide an unknown result. Numeric profile durations, local clock/runtime mapping and executed race cases remain delivery prerequisites.

Emergency security blocking is an independently authorized immediate exception. Deny access and journal/publish incident evidence without waiting for Subscriptions. Known evidence blocks activation; if notification races an already applied commit, preserve T and follow later guarded suspension/cancellation and existing financial policy. Physical faults remain truthful failed observations. Neither is a claim of atomic rollback or universal uptime.

Authorized preparation use before T is platform-borne, also when no activation ever commits. Require action-scoped authorization and applicable financial admission before usable exposure. Enabled profiles define bounded preparation timing, capability/capacity/usage and admission concurrency, with supported retention and fenced settlement. A limit cannot authorize unilateral cleanup of an unknown outcome. Preserve authoritative usage time and operation provenance for Rating/Billing; late pre-T usage cannot become a customer tariff, and unknown boundary evidence is unresolved. Post-T use follows the accepted paid/trial terms.

This creates no bonus grant, zero-price catalog rewrite, purchased-validity extension or bypass of the separate preparation-quota contract. Credit trials also require the Billing commitment contract below.

**Aggregate publication and recovery.** Every changed manifest result commits with a durable Entitlements producer sequence, event identity and outbox intent. Subscriptions can recover the same full result by ordering tenant, activation operation and preparation posture revision after response loss or a sequence gap. A manifest confirmed empty by the authoritative profile gets an explicit result with no-preparation evidence; it does not wait for a nonexistent assignment callback. Duplicate and conflicting manifests and results follow DESIGN §3.3.2.

Physical targets allocated during execution bind internally to the pinned logical roster; they cannot silently change the manifest the consumer accepted.

**Provider restriction discrepancy.** A stored/applied purchased quota is not enough when an independent provider ceiling prevents a required purchased capability. Retain desired rights and the authoritative actual restriction/scope/reason, and do not emit aggregate ready until the requirement is satisfied. Ordinary changes after held ready follow the existing Subscriptions activation/withdrawal fence; authorized emergency security restrictions act immediately with truthful recovery. A prior applied T is not rewritten. Reconciliation cannot clear a discrepancy merely because the source cap matches; see [quota restrictions](03-quota-projection.md#quota-projection).

**Allocation-dependent readiness.** The [allocation preparation contract](03-quota-projection.md#quota-projection) permits only authorized preparation under the frozen purchased cap and allowed resource/actions/schedule. Required applied quota and real prepared resources belong in the readiness manifest. QE counts occupied/reserved resources before T and retains the same source/member/allocation across activation and delayed resolution delivery. Platform-borne preparation cost does not waive occupancy. Unknown outcomes retain protection; fenced non-activation blocks new allocation and releases capacity only after authoritative resource release and settlement. ENT-Q15/E-U01 prove this profile; consumable-limit readiness follows the provider-purpose/evidence contract in quota projection; its runtime profile remains separately gated.

**Consumable-limit evidence.** For a separately enabled consumption profile, the [quota contract](03-quota-projection.md#quota-projection) requires authoritative evidence that the exact limit, scope, window, enforcement and access are ready. Entitlements supplies recognizable check purpose in adapter context; execution and quota/accounting handling belong to that provider. The marker implies neither dry-run nor exemption; it is not readiness proof. Activation retains the same rule/counter/window, and deferred runtime scope is unchanged.

**Credit-trial coverage.** Before preparation, Billing durably commits one reserved grant to the accepted acquisition and exact Subscriptions activation operation/preparation generation. `trialCoverage` in the preparation posture carries the authorized commitment reference/digest, reserved grant identity, accepted scope/terms and selected coverage profile. Entitlements authenticates the Billing evidence through its supported public contract, checks exact operation/scope/profile binding and retains it with the manifest and posture digest. Missing, conflicting or unsupported coverage cannot satisfy readiness. The source commitment is a Billing obligation, not a balance inferred from Pricing policy or a customer wallet.

The selected adapter prepares usable access under that guarantee and must retain it across successful activation without waiting for Billing's later recording. Billing's prior commitment makes the same grant effective at the authoritative Subscriptions T, with original expiry derived from T and frozen duration. Entitlements does not issue the monetary grant, calculate or cache an independent spendable balance, choose T, or allocate usage. Adapter payloads contain authorized scope/assignment/desired revision and only the necessary opaque coverage reference/profile; no subscription ID or unfiltered commercial receipt crosses that boundary.

Authorized pre-T/never-activated use is platform-borne and consumes no customer bonus. Usage from applied T follows the effective grant and accepted liability rules even when Billing records or receives it later. Preserve authoritative usage time and preparation/assignment provenance; an unknown boundary remains unresolved. The profile must demonstrate bounded exposure and enforcement/recovery during delayed Billing recording, without treating an unknown balance as unlimited spending. Unknown activation retains coverage and prepared assignments; only `closed_without_activation` for that exact operation permits release/cleanup.

If applied won, preserve historical T/grant and obey any newer cancellation, expiry or exhaustion. Late recording never regrants access. E-U07/ENT-Q14 gate exact owner mappings and conformance; allocation preparation must also satisfy ENT-Q15/E-U01; consumable-limit check requests carry a purpose marker; their execution and quota treatment follow the provider contract.

**Readiness-check purpose.** Entitlements marks a request whose purpose is checking readiness in the scoped adapter request context, semantically `purpose = readiness_check`. The exact wire field/version belongs to the adapter contract. The marker is bound to the same authenticated scope, assignment, desired revision and operation and is preserved in request/evidence correlation; adapters may map it to their native provider representation. It identifies why the request was made, not how it must execute.

The adapter/provider decides how to perform and account for a recognized check under its declared contract: inspect state, validate without execution or perform a supported real operation. The marker alone guarantees neither dry-run, absence of effects nor zero quota consumption; it grants no extra authorization, accounting exemption or success result. No universal service allowance or special provider-side check API is required. Readiness still needs authoritative evidence satisfying the selected profile, and any real effects retain normal idempotency, usage provenance, recovery and cleanup obligations.

Platform-borne pre-T cost and existing lifecycle guards remain applicable. No subscription identifier crosses this request boundary.

**Adapter observation envelope.** The following fields belong only to adapter-to-Entitlements observations. They are not `ReadinessObservationV1`, which is the aggregate result above, or ratified SDK fields. Adapters never receive or report `subscriptionId`; Entitlements keeps the intake-to-assignment mapping internally.

| Semantic field | Assignment / readiness meaning |
| --- | --- |
| `schemaVersion` | Explicit supported contract version; unsupported versions are quarantined/reported, not interpreted as ready. |
| `scopeId`, `assignmentId` | Authorized tenant scope and stable identity of the concrete rights assignment/service instance. A shared product or customer ID alone is not enough. Native identity must distinguish multiple environments under one broad right. |
| `desiredRevision` | Immutable desired-state generation bound to exact accepted preparation inputs; it is not a transport idempotency key. Retired identities/revisions cannot be silently reused. |
| `serviceProfileRef`, `targetRef` | Pinned versioned readiness requirements and concrete service target, retained in the descriptor/manifest and bound to results. Target allocation discovered during execution is durably associated before it can satisfy readiness. |
| `eventId` | Identity of an emitted observation for delivery deduplication; repeats keep the original identity/content, conflicting content is refused. A reconciled newer observation has its own identity. |
| `producerId`, `statusSequence` | Trusted responsible authority and durable monotonically ordered observation sequence within the agreed scoped assignment/authority stream. Producer replacement or sequence reset needs an explicit fenced generation mapping. |
| `status` | Semantic observation `pending`, `ready`, `failed` or `withdrawn`; these are preparation observations, not subscription states. Pending includes accepted work; ready means the service profile's complete initial readiness criterion. |
| `observedAt`, `evidenceRef`, `reasonCode` | Observation time, recoverable authoritative evidence and structured reason. Timestamp is diagnostic evidence, not ordering, freshness authority or the commercial start. |

**Open integration gates.** ENT-Q11 and ENT-Q13 now have the accepted held-readiness and platform-borne preparation directions above. Owners still agree exact contract versions, authenticated terminal-outcome read and source retention, provider guarantee/emergency mapping, exposure bounds, authoritative time/accounting evidence, feed recovery and cleanup mechanics. ENT-Q09 to ENT-Q15 remain per-profile delivery gates until these contracts and race/recovery fixtures pass; registry metadata or a cached ready event alone cannot close them.

## 3. Processes / Business Logic (CDSL)

### Observation acceptance

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-algo-observation-acceptance`

**Input:** one adapter readiness observation from a producer and the saved manifest. **Output:** accepted and persisted, no-op, or refused/quarantined with a structured reason.

1. Authenticate the producer. Authorize its tenant, assignment and profile authority against the saved manifest; an arbitrary publisher's `producerId` is not proof.
2. Check `schemaVersion`. An unsupported version is quarantined and reported, never interpreted as ready.
3. Match exact `scopeId`, `assignmentId`, `desiredRevision`, `targetRef`, `serviceProfileRef` and the current manifest association. A target allocated during execution must be durably associated before an observation against it can satisfy readiness.
4. Compare `eventId` and content with what is stored. An identical delivery is a no-op. A conflicting duplicate, wrong scope, unexpected producer or ambiguous association is refused or quarantined without information leakage.
5. Compare `statusSequence` with the producer's high-water mark for the assignment stream. An older sequence cannot overwrite a newer `ready`, `withdrawn` or `failed` result. A gap requires authoritative reconciliation unless the agreed full-state snapshot contract proves it safely covered. Producer replacement or a sequence reset needs an explicit fenced generation mapping.
6. Persist the accepted observation, its content identity, the producer sequence high-water mark and provenance durably, then advance the assignment status under [readiness state](#readiness-state).
7. Recompute manifest readiness and persist any changed aggregate result with its sequence and publication intent in the same transaction. Receipts, broker acknowledgement and license existence alone never satisfy readiness. One adapter's ready result does not satisfy another required producer or assignment. The manifest is ready only when the held commitment exists and every required assignment's current accepted observation is `ready` for the exact `desiredRevision`; a ready for a retired, superseded or withdrawn revision is historical evidence only.

## 4. States (CDSL)

### Readiness state

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-state-readiness`

Each required assignment of a manifest carries one current observation status: `pending`, `ready`, `failed` or `withdrawn`. These are preparation observations, not subscription states; the subscription stays `preparing` on the Subscriptions side until its own commit. Order is the producer's `statusSequence`, never `observedAt`.

| From | Observation / to | Mandatory guard |
| --- | --- | --- |
| none | manifest persisted / pending | Durable intake work consumed; current source and open preparation-generation fences checked; manifest, producer authorities, first aggregate result and publication intent committed together before dispatch. |
| pending | newer `ready` / ready | Accepted under observation acceptance; exact `desiredRevision`, target and profile. |
| pending or ready | newer `failed` / failed | Higher sequence from the authorized producer; partial effects stay subject to reconciliation. |
| failed | newer `pending` or `ready` / pending or ready | Higher sequence; the same assignment identity is recovered, no second environment is allocated. |
| pending, ready or failed | withdrawal / withdrawn | Subscriptions has fenced its local activation; Entitlements revokes or supersedes preparation authority and records cleanup obligations. |
| withdrawn | any later observation / withdrawn | Stored as effect evidence for cleanup; never produces ready for that revision. |

The held commitment is separate from these observation states: ready-to-failed does not permit release. Only the exact applied or irrevocable non-activation resolution settles it; immediate security denial does not erase recovery obligations.

The manifest is `ready` only while every required assignment is `ready` for the exact desired revision; any other assignment status makes the manifest `pending`, `failed` or `withdrawn` in that precedence. A lower sequence never moves any row. A new desired revision starts a new manifest row set; it does not reuse retired identities or revisions.

## 5. Definitions of Done

### 5.1 Manifest-gated readiness with recoverable observations

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-dod-initial-readiness`

The implementation must persist the complete manifest before any preparation request or observation can escape, authenticate and authorize every producer against it, keep one durable high-water mark per producer and assignment stream, refuse or quarantine conflicting, stale, wrong-scope and unsupported observations without information leakage, and report ready to Subscriptions only when the whole manifest is ready for the exact desired revision. Readiness, withdrawal and cleanup obligations survive restart and are readable by assignment and operation; dispatch logs are not completion evidence.

The same assignment identity is recovered after a lost or unknown result without a second account or environment. No subscription identifier is accepted in or written to any adapter payload; the manifest association is keyed by assignment within `(namespace, resource tenant)`.

**NFR acceptance evidence:** measure first delivery of each preparation assignment to its required adapter against the ratified `cpt-cf-bss-entitlements-nfr-handoff-latency` targets (p95 < 5 s, p99 < 15 s, auditable exception beyond 60 s). Report time to confirmed usable access separately; it is never hidden inside the delivery figure. Fault injection must show a lost ready event recovered through the assignment status read, a restart resuming pending observations from the high-water mark, and a withdrawal racing a ready reaching the fenced outcome with cleanup obligations recorded.

Credit profiles also prove authenticated commitment binding, readiness before T, continued prepared access during delayed/lost Billing recording, pre-T usage excluded from bonus drawdown, one effective grant at T, and activation/cancellation recovery without release on unknown outcome. E-AC31 to E-AC33 cover these cases; an adapter requiring a later Billing issuance decision to make initial access usable cannot be selected.

**Implements:** `cpt-cf-bss-entitlements-flow-initial-readiness`, `cpt-cf-bss-entitlements-algo-observation-acceptance`, `cpt-cf-bss-entitlements-state-readiness`.

## 6. Acceptance Criteria

- [ ] Subscriptions closes before manifest creation or before any aggregate result reaches it. Entitlements retains the generation fence without requiring readiness metadata; delayed preparation and ready cannot reopen it (E-AC41).
- [ ] Partial preparation fails before aggregate ready and no readiness commitment exists. The generation-bound non-activation resolution authorizes only correlated cleanup; lost closure recovers by the same identity and late effects remain tracked without affecting a successor (E-AC42).

- [ ] Deadline expiry and definitive failure request Subscriptions closure for the exact operation/generation. Recover the applied or terminal non-activation winner before cleanup; delayed timer/ready delivery and restart/profile edits cannot reopen or extend the attempt. A prior applied T remains authoritative (E-AC35).
- [ ] Unknown outcome retains effects; a successor cannot overlap unsafe predecessor allocation or cleanup. Terminal attempt closure preserves the original purchase/payment and does not itself release occupied quota or create a refund (E-AC36).

- [ ] Two concurrent operations in one scope, a fast callback and a lost aggregate result recover by the exact saved operation/posture; no assignment discovery race or cross-operation match is possible.
- [ ] A manifest confirmed empty by the authoritative profile emits and retains aggregate ready. A changed roster at the same manifest version conflicts; a higher posture revision replaces the binding and fences old results.
- [ ] Ready is reported only for the entire pinned assignment and producer manifest; duplicate, wrong-tenant, old-revision and lower-sequence observations cannot produce ready or overwrite a newer `withdrawn`.
- [ ] A lost ready event is recovered through the same assignment status read; an unknown or not-found result does not allocate a second environment. A Subscriptions commit failure after readiness finds the same ready state again; Entitlements assigns no T.
- [ ] Withdrawal racing ready is fenced at external execution; partial and late effects retain cleanup obligations. A delayed ready for a withdrawn revision is stored as evidence and never produces ready.
- [ ] PaaS initial readiness covers the account, the purchased environment and required access, without waiting for first login or a later VM creation. Entitlements reports ready; the active commit and period start are owned by Subscriptions with fresh local T, not provider observed time.
- [ ] An observation with an unsupported `schemaVersion`, an unauthenticated producer or an ambiguous manifest association is quarantined with a structured reason and no readiness effect; an identical redelivery is a no-op.

- [ ] Ready is published only with a durable commitment, including empty manifests. Concurrent activation/withdrawal produces one exact terminal Subscriptions decision; no lost event or read timeout permits release while that decision is unknown.
- [ ] After active commits but its publication is lost, outcome recovery continues the same assignments with the original T; after terminal non-activation, late ready/apply cannot revive that generation and cleanup touches only authorized operation-owned resources.
- [ ] Failure of an execution guard, a failed observation, provider reconfiguration and a missing operation read do not release held preparation. An authorized emergency block acts immediately and reconciles its evidence without erasing an applied activation.
- [ ] Late usage belonging to authorized pre-T or never-activated preparation is platform-borne; unknown boundary evidence stays unresolved. A profile without bounded exposure, retention and Rating/Billing mapping cannot enable preparation.
