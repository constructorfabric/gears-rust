---
description: "Prepare required services, collect readiness evidence and recover activation or cleanup through the selected adapters."
---

# Feature: Initial Readiness

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-featstatus-initial-readiness-implemented`
- [ ] `p1` - `cpt-cf-bss-entitlements-feature-initial-readiness`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Readiness manifest and observation envelope](#readiness-manifest-and-observation-envelope)
  - [Pre-purchase activation configuration](#pre-purchase-activation-configuration)
  - [Readiness profile schema](#readiness-profile-schema)
  - [Readiness SDK results](#readiness-sdk-results)
  - [Readiness change events](#readiness-change-events)
  - [Preparation bounds](#preparation-bounds)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Observation acceptance](#observation-acceptance)
- [4. States (CDSL)](#4-states-cdsl)
  - [Readiness state](#readiness-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Manifest-gated readiness with recoverable observations](#51-manifest-gated-readiness-with-recoverable-observations)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->


**Status:** proposed implementation; provider contracts and runtime conformance remain delivery gates.

**Release scope.** V1 targets Q1 and includes held readiness, security blocks and cleanup. Required-output delivery checks and configured hooks are v2. A v1 profile has no such requirements; it must still prove essential service access. The pre-purchase binding refuses a profile requiring v2 capabilities.

## 1. Feature Context

### 1.1 Overview

Entitlements saves a manifest of the assignments required for initial activation. Capability handlers prepare them through the selected adapters while the subscription is `preparing`. Entitlements accepts observations against that manifest and reports aggregate readiness for the exact operation and revision. Subscriptions performs the guarded activation commit and owns T.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-entitlements-fr-initial-readiness`, `cpt-cf-bss-entitlements-nfr-handoff-latency`.

**Primary NFRs:** `cpt-cf-bss-entitlements-nfr-handoff-latency`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-entitlements-actor-subscriptions` | Persists the activation operation, consumes `ReadinessObservationV1` and commits active with fresh T. |
| `cpt-cf-bss-entitlements-actor-license-adapter` | Selected license adapter; prepares rights assignments and reports readiness. |
| `cpt-cf-bss-entitlements-actor-service-adapter` | Prepares the purchased environment and reports its readiness. |
| `cpt-cf-bss-entitlements-actor-platform-operator` | Inspects pending, failed and withdrawn preparation and its cleanup obligations. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [readiness observation contract](../DESIGN.md#332-readiness-observation-contract).
- **Build prerequisites:** as allocated in [DECOMPOSITION.md](../DECOMPOSITION.md); decisions in [DECISIONS.md](../DECISIONS.md).
- **Provider prerequisites / integration acceptance:** the joint freshness and withdrawal contract with Subscriptions and the per-profile readiness gates ENT-Q09 to ENT-Q13 and ENT-Q15 in [DECISIONS.md](../DECISIONS.md#open-questions).

## 2. Actor Flows (CDSL)

### Readiness manifest and observation envelope

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-flow-initial-readiness`

**Actor:** Subscriptions, through `CommercialPostureV1` for the preparation revision; the selected adapters as producers of readiness observations.

**Success:** every required assignment of the manifest is `ready` for its exact desired revision and Entitlements reports ready to Subscriptions.

**Errors:** manifest cannot be built, unsupported `schemaVersion`, unauthorized producer, refused or quarantined observation, preparation `failed`, operation `withdrawn`.

Entitlements is the provider side of IA-2026-10-18 (ENT-D-02): initial activation completes only after all required initial service capabilities are confirmed usable. Subscriptions keeps the subscription `preparing` from the authorized start of preparation until its guarded activation commit, and only Subscriptions writes that status (SUB-D-58).

Entitlements records preparation observations. Subscriptions tracks the operation and alone changes subscription status. The design is accepted; SDK delivery, owner agreement and runtime proof are still required. Renewals, resume and modifications of an already active subscription keep their existing contracts.

1. [ ] - `p1` - Subscriptions first saves its activation operation, authorized scope, preparation posture revision/digest and outbox. No preparation request or readiness observation may leave before this commit. It publishes `CommercialPostureV1` for that revision through [posture intake](01-posture-intake.md). Entitlements accepts the posture and durably schedules manifest derivation with its intake. The derivation transaction rechecks the source and preparation-generation fences and persists the manifest with the first aggregate result and publication intent. If a non-activation fence already closed the generation, retain that closure and do not initiate preparation or report ready. When the manifest cannot be built, Entitlements reports an aggregate result without a manifest binding: `failed_definitive` with reason `manifest_unbuildable` when the posture or the readiness profile is unsupported or inconsistent, and `pending` with a reason while a dependency is unavailable (ENT-D-09). - `inst-readiness-manifest-and-1`

   The manifest lists every required scoped assignment, desired revision, service profile and target, and the authoritative producer; optional and not-applicable capabilities are explicit. A manifest with no assignments and no readiness hooks is permitted only when the selected accepted service profile authoritatively requires no preparation, never because discovery or an adapter failed.

Its digest and version are bound to the operation and posture input. Persist the first aggregate result and its outbox intent with the manifest before adapter dispatch.

   Profile-required [fulfillment outputs](../DESIGN.md#337-fulfillment-outputs) are part of each assignment's readiness evidence: required content and its authorized retrieval path must be prepared and retained under the readiness commitment. Optional output failure is visible but does not block readiness. Customer release follows the accepted policy; essential pre-T delivery cannot wait for active.

   Subscriptions binds a result through operation/posture identity, not through previously known assignments; composition or logical-target changes require a newer preparation posture and a new manifest binding.
2. [ ] - `p1` - Capability handlers prepare the required grants and resources through the selected adapters (ENT-D-01). A deployment purchase may require an account, environment and usable access before first login. Adapters own their provider steps. Entitlements retains preparation authority and waits for the required evidence. - `inst-readiness-manifest-and-2`

   Preparation rights cannot be derived only from `active`, or the cycle would prevent activation. Reuse an existing assignment identity if it supplies the required scope, lifetime and revision semantics; no new license entity is mandated.
3. [ ] - `p1` - A runtime license check does not prove that the requested grant state was applied. The license adapter must supply authoritative assignment state and recovery. Event delivery, polling or another reliable feed is an integration choice (ENT-Q09); no broker or new issuance API is assumed by this document. Entitlements records an assignment/publication intent and its manifest association durably before dispatch, with recovery for external results. Save the association before a callback can arrive, so no fast callback is lost. - `inst-readiness-manifest-and-3`
4. [ ] - `p1` - Each producer reports readiness per assignment with the envelope below. Entitlements accepts or refuses every observation under [observation acceptance](#observation-acceptance), persists it and advances the producer's high-water mark. - `inst-readiness-manifest-and-4`
5. [ ] - `p1` - Entitlements reports an aggregate `ReadinessObservationV1` under [DESIGN §3.3.2](../DESIGN.md#332-readiness-observation-contract), bound to ordering tenant, scope, operation, posture revision/digest and the immutable complete manifest. Report `ready` only after all required assignment criteria and output checks pass for the exact desired revision/digest, all configured readiness hooks pass against current prerequisite evidence, and the held commitment is saved. Subscriptions performs the guarded commit and owns T (ENT-D-02). Entitlements never assigns T and never treats a readiness notification, a receipt or an intermediate approved operation as completion. All rights essential for initial usable service are ready before that commit. - `inst-readiness-manifest-and-5`

   Post-commit reconciliation may reconcile the same satisfied assignment or deliver nonessential or ongoing changes; it cannot be the first action that makes required initial access usable. Initial readiness is not an uptime guarantee, and later service failure does not reopen the completed acquisition.
6. [ ] - `p1` - **Uncertainty, revocation and cleanup.** Entitlements persists external effects and durable lookup/reconciliation keys. Lost/unknown results, absence from a read, or a quiet broker are not proof of failure or non-execution; recover the same assignment and operation without allocating a second account/environment. The status/read contract must recover authoritative state and sequence after missed events, restart or retention gaps; dispatch logs are not completion evidence. A `failed` observation describes preparation failure, not proof all partial effects are gone. - `inst-readiness-manifest-and-6`

   Subscriptions closes activation for the generation before withdrawal. Entitlements then marks the manifest withdrawn and revokes or supersedes its preparation authority at the adapters. It reconciles running actions and records cleanup for each affected assignment. The readiness read reports settlement for the generation (ENT-D-08).

Delayed ready for a cancelled/obsolete revision cannot activate; it still contributes historical effect evidence needed for cleanup. Adapter-side ordering must prevent delayed preparation from recreating a withdrawn environment, including work already in flight. Do not erase pending cleanup or release protected purchase rights because a draft disappeared.

   Entitlements requests cleanup only for the closed service assignment. The adapter/OSS integration must preserve pre-existing customer accounts and data outside that authority. BSS does not select individual cleanup resources. Operational cleanup completion and refund/financial settlement remain separate outcomes.

**Held readiness procedure (SUB-D-57, ENT-D-02).** Once the complete manifest is ready, durably establish the provider retention guarantees and save a `ReadinessCommitment` with the aggregate ready result/outbox before publishing it. The immutable binding names ordering namespace, scope, operation, preparation revision/digest, manifest identity/version/digest and profile.

Empty manifests also receive explicit commitments. Required assignments stay on their saved adapter bindings; a provider that cannot retain the prerequisite through an unresolved decision cannot enable this profile.

Subscriptions is the decision owner. Its activation and ordinary withdrawal serialize on one operation/preparation-generation fence. It publishes `activationResolution` in CommercialPostureV1, keyed by authenticated ordering namespace, scope, operation and preparation revision/digest, with a durable decision version. A newer lifecycle posture may supersede that delivery. Entitlements recovers the same immutable result through the Subscriptions operation read (ENT-D-24).

On `applied`, require the exact manifest identity/version/digest and readiness commitment binding, retain actual T and continue the same prepared rights only where the current desired head still permits them; a later restriction/cancellation remains authoritative. Historical settlement cannot regrant a revoked assignment.

On `closed_without_activation`, verify the exact preparation generation and irreversible local fence, plus any manifest/commitment bindings supplied. Closure is valid before a manifest or readiness commitment exists or has reached Subscriptions. A valid unfamiliar reason does not change that rule or prove that cleanup is settled.

Resolve local effects by their saved operation/preparation input; a later discovered manifest, commitment or partial effect belongs to the same closed generation without rewriting its terminal decision. Persist closure before processing delayed preparation, block new dispatch and ready publication, then fence in-flight provider work and perform only authorized cleanup.

No empty ready manifest or synthetic commitment is needed. For superseded preparation, require the retained non-activation decision for that old generation; a new revision or missing draft alone is not enough. Deduplicate resolution by identity/version/content, quarantine conflicts and never infer a historical outcome from current subscription status alone.

While the resolution is missing, keep the commitment, accept redelivery and retry the authorized operation read with backoff. Save recovery work and its next retry time so restart and posture supersession cannot lose it. Escalation does not stop recovery. Adapter-effect recovery continues through the saved binding. Readiness failure and failure of an execution guard are not release authority.

Report correlated failure or deadline evidence through aggregate readiness. Subscriptions alone decides whether to close the attempt and delivers that resolution in posture. No separate closure command or timer authorizes cleanup. Limit admission of new preparations to contain outstanding obligations. Do not create a second environment or switch providers to resolve uncertainty.

**Resolution recovery contract.** Use the Subscriptions SDK operation read, under the Entitlements service principal and a guarded single-target scope for the ordering tenant. This is a Subscriptions-owned read, not a new Entitlements public endpoint.

| Input or result | Rule |
| --- | --- |
| Lookup | `orderingTenantId` and `activationOperationId` from the authenticated `resolutionRef` or saved preparation. Retain the expected preparation generation, revision and digest. When a reference supplies a decision version or digest, the returned result must match it. Never substitute the current subscription status. |
| Unresolved operation | Pending, absent or unreachable results retain recovery and held obligations. Authorization failure is visible and cannot become non-activation. An unresolved read cannot authorize a replacement generation. |
| Terminal result | The immutable `activationResolution`: operation, scope, generation, preparation revision/digest, decision version and outcome. `applied` includes T and the exact manifest/commitment binding where preparation required it. `closed_without_activation` includes its reason and any known bindings; it is valid before a manifest exists. Use the owner SDK shape and codec. |
| Validation and replay | Check owner authority, tenant, operation, preparation binding, decision version and content digest. Identical results replay. Conflicting results are quarantined and settle nothing. No result for another generation can settle this one. |

A missing `resolutionRef` target queues recovery when a valid posture is journaled, even if that posture is stale. Outstanding preparation also queues recovery by its saved operation identity, so recovery does not depend on a later lifecycle change. Run reads outside the local transaction. On return, recheck the generation and current desired head, then store the resolution and settle its commitment atomically. Close any correlated `resolution_overdue` finding. Preserve current restrictions and cancellation. Cleanup still requires exact non-activation authority and the existing cleanup fences. A concurrent delivered resolution and recovered result follow the same deduplication rule.

**Preparation deadline and closure.** Subscriptions records the versioned preparation policy, authorized start and evaluated deadline on the activation attempt. For purchases using the [pre-purchase activation binding](#pre-purchase-activation-configuration), it copies the bound policy and profile. Only the actual start and deadline are calculated at admission; live Settings cannot replace the bound selection. Dispatch retry, polling, restart and profile edits cannot extend it; waiting for the requested future start is separate.

Entitlements keeps deadline evidence for observation and escalation. It reports correlated failure and timing evidence through the aggregate readiness result, not a separate closure request. Only Subscriptions evaluates the deadline: an Orders hold pauses the attempt and stops its clock without publishing a posture, and `unpause` continues with the remaining time (SUB-D-59).

**Pause-aware deadline diagnostics.** Before raising `resolution_overdue`, read the exact activation operation through the public Subscriptions `get_operation_in(ctx, orderingTenantId, operationId)` SDK method. The [shared contract](../../../subscriptions/docs/features/05-entitlements.md#shared-readiness-result-contract) requires `OperationV1.activation` with `phase`, `paused`, current `deadlineAt` and optional `activationResolution`. Use the existing authorized ordering-tenant operation read under E-U06/E-U08. Verify the returned operation and its saved preparation binding; unsupported or mismatched progress is unknown.

If a terminal resolution is present, process it through the resolution fence. Otherwise, deadline-based escalation requires current progress with `paused = false` and a current deadline exceeded by the pinned resolution recovery horizon. While paused, do not raise this deadline finding. Missing, unsupported or unavailable progress cannot be interpreted as unpaused. Continue operation recovery, ordinary dependency findings and held-age monitoring; a pause does not stop those limits. After unpause, use the owner-returned deadline, not the copied posture deadline.

Keep the observed progress and read time as diagnostic evidence. The read is not atomic with a later pause. A later verified pause or revised deadline resolves an obsolete deadline finding; it changes no activation or cleanup authority. Never retain a stale unpaused observation as a new proof of overdue status.

The copied deadline is not proof of expiry. A readiness report cannot close a paused attempt; Subscriptions applies its own clock and guards. Elapsed held age keeps running during a pause.

The count and age limit of held preparation, including a paused attempt, and the Orders-authorized action at that limit are a joint gate with Subscriptions and Orders (Subscriptions S-U01/S-U04, ENT-Q13); refusing new preparations alone does not satisfy it. The saved profile distinguishes retryable/unknown observations from definitive failure; ordinary recovery retains the same attempt and deadline.

Only Subscriptions decides the attempt at the same fence as activation.

After deadline that attempt cannot newly activate, even if a timer runs late or readiness was observed earlier. A prior applied winner is recovered with its original T; otherwise durable `closed_without_activation` retains the reason supplied by Subscriptions (SUB-D-86). Under the [closure-reason compatibility rule](01-posture-intake.md#sdk-contract-and-validation), an unfamiliar but owner-valid reason does not block a verified closure. Preserve its exact code; derive cleanup authority from the known outcome and exact generation, never from the reason. Unknown outcomes and invalid bindings still cannot authorize cleanup. Lost requests or missing/unreachable results remain unknown.

A timer or failure never authorizes immediate cleanup. Receive the exact resolution first, then settle only that generation under the resource and quota application contracts. OSS owns allocation and release accounting. Entitlements uses the adapter's closure and safe-reuse confirmation.

Orders owns remediation of the retained purchase/payment/claim; Entitlements neither cancels the purchase nor initiates a refund. An authorized successor requires safe predecessor-effect settlement/quarantine and assignment-generation fencing so old commands or cleanup cannot affect it. No second environment or provider switch may hide an unknown result.

Numeric profile durations, local clock/runtime mapping and executed race cases remain delivery prerequisites.

Emergency security blocking is an independently authorized immediate exception. Deny access and journal/publish incident evidence without waiting for Subscriptions. Known evidence blocks activation; if notification races an already applied commit, preserve T and follow later guarded suspension/cancellation and existing financial policy. Physical faults remain truthful failed observations. Neither is a claim of atomic rollback or universal uptime.

Authorized preparation use before T is platform-borne, also when no activation ever commits. Require action-scoped authorization and applicable financial admission before usable exposure. Enabled profiles define bounded preparation timing, capability/capacity/usage and admission concurrency, with supported retention and fenced settlement. A limit cannot authorize unilateral cleanup of an unknown outcome.

Preserve authoritative usage time and operation provenance for Rating/Billing; late pre-T usage cannot become a customer tariff, and unknown boundary evidence is unresolved. Post-T use follows the accepted commercial terms.

Preparation does not change catalog prices or purchased validity, or bypass the preparation-quota contract.

**Aggregate publication and recovery.** Every changed manifest result commits with a durable Entitlements producer sequence, event identity and outbox intent. Subscriptions can recover the same full result by ordering tenant, activation operation and preparation posture revision after response loss or a sequence gap.

A profile confirmed to require neither assignments nor readiness hooks gets an explicit result with no-preparation evidence; it does not wait for a nonexistent assignment callback. Duplicate and conflicting manifests and results follow DESIGN §3.3.2.

Physical targets allocated during execution bind internally to the pinned logical roster; they cannot silently change the manifest the consumer accepted.

**Provider restriction discrepancy.** A stored/applied purchased quota is not enough when an independent provider ceiling prevents a required purchased capability. Retain desired rights and the authoritative actual restriction/scope/reason, and do not emit aggregate ready until the requirement is satisfied.

Ordinary changes after held ready follow the existing Subscriptions activation/withdrawal fence; authorized emergency security restrictions act immediately with truthful recovery. A prior applied T is not rewritten. Reconciliation cannot clear a discrepancy merely because the source cap matches; see [quota restrictions](03-quota-projection.md#quota-projection).

**Quota-dependent readiness.** [Quota projection](03-quota-projection.md#quota-projection) applies the purchased limits under explicit preparation authority. Quota readiness needs confirmation of the complete set for the saved operation and scope. It requires no resource creation by itself. If the purchase includes deployment, the manifest also requires the resource adapter's readiness evidence.

Activation continues the same quota assignment and limits without a second allowance. A confirmed non-activation closes that source and permits only authorized resource cleanup. The adapter and resource owner confirm closure and safe reuse; BSS does not inspect allocations or settle counters. Unknown effects remain recoverable. ENT-Q15/E-U01 qualify this boundary.

Consumption quotas and their readiness rules are outside v1. The general readiness-check purpose below does not enable that profile.

**Readiness-check purpose.** Entitlements sets `purpose = readiness_check` in the scoped v1 adapter request context. The adapter design maps this marker to the provider API.

The marker is bound to the same authenticated scope, assignment, desired revision and operation and is preserved in request/evidence correlation; adapters may map it to their native provider representation. It identifies why the request was made, not how it must execute.

The adapter/provider decides how to perform and account for a recognized check under its declared contract: inspect state, validate without execution or perform a supported real operation. The marker alone guarantees neither dry-run, absence of effects nor zero quota consumption; it grants no extra authorization, accounting exemption or success result.

No universal service allowance or special provider-side check API is required. Readiness still needs authoritative evidence satisfying the selected profile, and any real effects retain normal idempotency, usage provenance, recovery and cleanup obligations.

Platform-borne pre-T cost and existing lifecycle guards remain applicable. No subscription identifier crosses this request boundary.

**Adapter observation envelope.** These adapter-to-Entitlements fields form the v1 SDK contract below. They are separate from the aggregate `ReadinessObservationV1`. Owner adoption remains an integration gate. Adapters never receive or report `subscriptionId`; Entitlements keeps the intake-to-assignment mapping internally.

| Semantic field | Assignment / readiness meaning |
| --- | --- |
| `schemaVersion` | Explicit supported contract version; unsupported versions are quarantined/reported, not interpreted as ready. |
| `scopeId`, `assignmentId` | Authorized tenant scope and stable identity of the concrete rights assignment/service instance. A shared product or customer ID alone is not enough. Native identity must distinguish multiple environments under one broad right. |
| `desiredRevision` | Immutable desired-state generation bound to exact accepted preparation inputs; it is not a transport idempotency key. Retired identities/revisions cannot be silently reused. |
| `serviceProfileRef`, `targetRef` | Pinned versioned readiness requirements and concrete service target, retained in the descriptor/manifest and bound to results. Target allocation discovered during execution is durably associated before it can satisfy readiness. |
| `eventId` | Identity of an emitted observation for delivery deduplication; repeats keep the original identity/content, conflicting content is refused. A reconciled newer observation has its own identity. |
| `producerId`, `statusSequence` | Trusted responsible authority and durable monotonically ordered observation sequence within the agreed scoped assignment/authority stream. Producer replacement or sequence reset needs an explicit fenced generation mapping. |
| `status` | Semantic observation `pending`, `ready`, `failed` or `withdrawn`; these are preparation observations, not subscription states. Pending includes accepted work; ready means the service profile's complete initial readiness criterion. A `failed` observation carries the failure class `definitive` or `retryable`; a `withdrawn` that a producer originates is taken as a definitive failure with reason `producer_withdrawn` (ENT-D-09). |
| `observedAt`, `evidenceRef`, `reasonCode` | Observation time, recoverable authoritative evidence and structured reason. Timestamp is diagnostic evidence, not ordering, freshness authority or the commercial start. |

**Generations, dispatch and profile.** Each preparation generation, one activation operation with one preparation revision, has its own assignment identities; closure without activation closes them for good, and a successor generation gets new ones (ENT-D-07). The capability flows derive and dispatch the desired state of a `prepare_activation` revision as they do for any revision.

This component builds the manifest from that desired state and the readiness profile, aggregates observations and dispatches nothing itself (ENT-D-10). The readiness profile is a versioned Entitlements record that states, for each capability, required, optional or not applicable, the readiness criterion, the target rule and the required outputs.

The activation profile of Subscriptions names it by reference and version (SUB-D-85), and the preparation posture carries that reference.

**Preparation findings.** `manifest_unbuildable`, activation-resolution conflict and `resolution_overdue` use `subject.kind = preparation_generation` in the ordering tenant. Bind them to the retained operation/generation and preparation revision/digest, even before a manifest exists. An invalid preparation identity instead uses its refused intake receipt. [Feature 06](06-reconciliation-recovery.md#public-reads-and-operator-commands) defines safe fields and owner-scoped reads.

**Cleanup and settlement.** A closed generation has one cleanup obligation for each assignment that can hold an effect: `open`, `requested`, then `settled` or `quarantined`. The readiness read of the generation carries its settlement, and Subscriptions admits a successor attempt only when it is `settled` or `quarantined`; a generation without authoritative closure and either local no-dispatch proof or complete adapter settlement evidence stays `open` (ENT-D-08).

BSS does not inspect runtime counters to make this decision. After the resolution the manifest is terminal, `resolved_applied` or `resolved_closed`, and a later observation is evidence only (ENT-D-09).

**Contract with Subscriptions.** The commitment is the `readinessCommitment` member of the `ready` result, created in the transaction that first makes the result `ready`. The resolution arrives through posture or the authorized Subscriptions operation recovery read. Both paths validate and store the same immutable result under its generation fence. Entitlements derives no resolution from silence, a status or a timer.

The pause-aware operation read above controls `resolution_overdue`, using the saved resolution recovery horizon from [feature 06](06-reconciliation-recovery.md#recovery-matrix). The copied deadline alone cannot raise the finding; the finding releases nothing. The duration and the deadline belong to the activation profile of Subscriptions (ENT-D-24). The readiness profile has the schema of [DESIGN §3.3.9](../DESIGN.md#339-readiness-profile) (ENT-D-21).

**Open integration gates.** Each adapter still proves that it retains prepared effects while the decision is open, how an emergency block maps, its cleanup mapping, the exposure bounds, the authoritative time and accounting evidence and the feed recovery. ENT-Q09 to ENT-Q13 and ENT-Q15 remain per-profile delivery gates until these contracts and race/recovery fixtures pass; registry metadata or a cached readiness notification alone cannot close them.

### Pre-purchase activation configuration

This contract applies to all offers that use the activation requirement, including preparation without hooks. This contract checks configuration compatibility, not live readiness. It creates no service, held readiness commitment, capacity reservation or payment. The owner SDK changes below are proposed adoption requirements; Entitlements adds no support-query API.

| Step | Owner and action |
| --- | --- |
| Publish qualification | The service owner qualifies the immutable Entitlements profile and publishes its activation requirement in Products. Products retains the requirement and profile evidence with its qualification version/digest. Any hook-enabled service requires `entitlements`. |
| Accept commercial terms | Pricing validates the complete selected set against Products qualification and freezes its activation requirement in the acceptance receipt. Pricing acceptance records agreed terms; it does not confirm service readiness or complete the purchase. |
| Bind before purchase | Orders calls the Subscriptions configuration-binding SDK method with the exact acquisition line and Pricing acceptance. Subscriptions reads that retained acceptance and resolves effective activation Settings for its seller/resource tenant. It checks the requirement and persists the selected configuration before replying. |
| Commit purchase | Orders records the returned binding against the same order version/line and acceptance digest. The existing purchase/payment guards require that exact binding before commercial commitment or payment capture, whichever comes first. A missing binding, incompatible configuration or unresolved binding request blocks this step. |
| Admit activation | Orders carries the binding reference through subscription creation and activation. Subscriptions validates the purchase/tenant association and uses the saved mode and profile. It does not reselect them from live Settings. Entitlements receives the resulting preparation posture and evaluates its pinned capability criteria and any configured checks. |

| Proposed owner contract | Fields and rules |
| --- | --- |
| Products `ActivationRequirementV1` | Owner schema version, requirement ID/version/digest, qualified service/selection identity, qualification reference/version/digest and `preparation = entitlements_required / no_preparation_allowed`. The qualified readiness profile reference/version/digest is required when preparation is required. Hook code, bindings and payloads stay inside Entitlements. Absence is unsupported evidence, not permission to skip preparation. |
| Pricing accepted requirement | Retain the complete selected requirements and their digest with the immutable acceptance. Any required preparation makes the purchase require `entitlements`. The selected readiness profile must cover the complete qualified set. Reject incompatible profile requirements before acceptance; do not silently compose profiles or drop checks. |
| Subscriptions `bind_activation_configuration(acquisition_ref, pricing_acceptance_ref, requirement_digest, operation_id)` | Service-only call restricted to the named Orders service principal, with current authorization and one target tenant scope. The recovery read uses the same identity and scope checks. The acquisition reference identifies the immutable order version/line or equivalent owner identity. Subscriptions verifies the acceptance, requirement digest and ordering/resource/seller tenant axes through authorized owner reads. It alone resolves effective Settings. A caller-supplied mode or hook list cannot replace those sources. |
| `ActivationConfigurationBindingV1` | Binding ID/version/digest, acquisition reference, Pricing acceptance ID/terms digest, requirement digest, tenant axes, selected activation mode, immutable resolved activation-policy snapshot/version/provenance, and qualified readiness profile reference/version/digest when selected. Subscriptions owns and retains this record; Orders and later activation carry its reference. |
| Binding result and recovery | `bound` returns the immutable binding; `refused` returns an incompatibility reason with no binding. Dependency or transport failure leaves no usable proof. A saved operation replays its exact result; changed content conflicts. `read_activation_configuration_binding(acquisition_ref, pricing_acceptance_ref)` recovers the authoritative result under the same scope. Missing or unknown results cannot authorize purchase; recovery cannot select another configuration for the same bound identity. |

When `preparation = entitlements_required`, `no_preparation` is incompatible. For `no_preparation_allowed`, Subscriptions may select that mode only when the accepted qualification permits it. If it selects `entitlements`, it must bind a qualified complete profile. Unknown Settings, unavailable retained profile evidence or an unsupported owner contract never fall back to no preparation. Products/Pricing check what the service requires; Subscriptions checks what this tenant is configured to use. Orders checks that this exact purchase has passed both.

Subscriptions enforces one binding per acquisition and Pricing acceptance, regardless of operation ID. Concurrent requests reuse the committed binding; conflicting requirements or tenant associations fail. Resolve Settings outside the transaction, then persist the complete candidate under that uniqueness fence. A refused operation keeps its result. A new operation may retry after configuration repair only if no binding exists. Abandoned bindings reserve no provider capacity.

The binding is a durable choice for that purchase, not an expiring observation of live Settings. An ordinary Settings change affects later bindings only. Existing purchases retain their saved mode, policy and profile. Activation still assigns the actual start and deadline at admission, using the frozen duration; binding does not start that clock or bypass the existing activation guards. This extends the owner contract of SUB-D-85 and needs explicit Subscriptions adoption under E-U06.

Binding does not replace current sale eligibility, purchase authorization, quota exclusivity, payment guards or emergency security checks. Orders uses those existing contracts at their normal boundaries. A binding cannot be reused for another acceptance, tenant or order version. Changed terms require a new acceptance and matching binding through the existing amendment flow. Emergency withdrawal or an outage after commitment leaves fulfillment blocked or unresolved under owner recovery/remediation; it cannot silently change the mode, omit hooks or undo a purchase.

Enable sales of affected profiles only after Products, Pricing, Subscriptions and Orders adopt this path and prove its guards. Static qualification alone, a live Settings read or an activation-time refusal does not close the pre-purchase gate. No remote call is required inside the purchase commit; the guarded commit consumes the already durable binding and validates its exact association.

### Readiness profile schema

A readiness profile is an immutable record of the type `gts.cf.bss.entitlements.readiness_profile.v1~` (ENT-D-21). The deployment supplies the profiles with its route configuration, and Entitlements loads them at start. The identity is `(profileRef, version)`. A loaded profile that has the identity of a stored profile and another digest stops the start. No API changes a profile; a changed profile is a new version.

| Field | Rule |
| --- | --- |
| `profileRef` | Stable name of the profile: 1 to 128 characters of `a-z`, `0-9`, `_`, `-` and `.`. |
| `version` | 1 to 128 visible ASCII characters, the form that the activation profile of Subscriptions freezes (SUB-D-85). |
| `digest` | Lowercase hexadecimal SHA-256 of the canonical JSON of the record without this member. |
| `capabilities` | One entry for each of `quota`, `license` and `resource`; all three are present. |
| Entry `requirement` | `required`, `optional` or `not_applicable`. An optional capability is dispatched and reported, and it never blocks `ready`. |
| Entry `capabilityProfile` | `profile_ref`, `version` and `digest` of the adapter capability profile that the route and the plugin instance must match (§3.3.3). Absent for `not_applicable`. |
| Entry `criterion` | `applied`: complete application is confirmed for the exact desired revision/digest, with no open fulfillment discrepancy. Required outputs must also pass. `observed_ready`: it additionally needs a current `ready` observation from the pinned producer. The receipt mapping below applies to both criteria. |
| Entry `targetRule` | `resource_tenant`: the logical target is the resource tenant. `member_scope`: use the explicit Subscriptions member holder scope validated under feature 01 and E-U06. Missing scope blocks derivation; there is no fallback to the caller, payer or seller. |
| Entry `requiredOutputs` | For each output: `outputKey`, the kind, `required` or `optional`, `release` = `before_activation` or `after_activation`, `retrieval` = `stream` or `link`, the link lifetime for `link`, and whether retrieval is allowed under a restricting posture (§3.3.7, ENT-D-23). |

A profile whose three entries are `not_applicable` and which has no readiness hooks gives an empty readiness requirement set, and the identity and the digest of the profile are its no-preparation evidence. An unknown profile or an unsupported schema version makes the manifest unbuildable (ENT-D-09). The profile holds no activation duration or activation deadline; both belong to Subscriptions (ENT-D-24). Hook invocation timeouts and retry bounds are separate execution limits, defined in feature 07.

The optional `hooks` member holds at most 16 immutable bindings under [feature 07](07-extension-hooks.md#binding-and-payload-contracts). It is covered by the profile digest. An omitted member stays omitted when verifying an existing profile. The manifest saves the complete hook roster before dispatch; readiness checks are always required. An empty assignment roster with checks is not an empty readiness requirement set.

Feature 07 schedules checks only after required adapter criteria pass. Hook results have their own authority and journal; adapters cannot submit them through `record_assignment_observation`. A changed prerequisite epoch/digest invalidates previous passes. Core readiness can be implemented and enabled for profiles without hooks before feature 07; a profile containing hooks remains gated by E-U09.

### Readiness SDK results

**Delivery and peer recovery.** Commit the aggregate result and event outbox entry locally. Publish through Event Broker as described below; there is no direct readiness SDK callback. Resolve the Subscriptions client lazily only for operation recovery. Missing clients and transport failures retain the same result, sequence and recovery identity under the policy in feature 06.

Intake, startup and result reads do not wait for broker delivery or a consumer acknowledgment. An unavailable result read stays unavailable; it is not `not_found` or a failed preparation. If the Subscriptions outcome read is unavailable, retain commitments and effects until a verified resolution arrives. Peer outage authorizes no cleanup. The [startup contract](../DESIGN.md#34-internal-dependencies) applies in both directions.

The readiness envelope sent to Subscriptions uses the shared SDK contract linked below. Entitlements-owned adapter observations still use the primitive rules in [posture intake](01-posture-intake.md#sdk-contract-and-validation). `read_preparation_result` accepts the ordering tenant, activation operation and preparation revision. It returns the latest full result, not a new event or a newly allocated manifest. Before any result exists, return `not_found`; callers retry their saved posture. Absence never authorizes activation or cleanup.

The canonical envelope, variants, conditional evidence, roster bounds and integer codecs are defined in the [Subscriptions shared readiness result contract](../../../subscriptions/docs/features/05-entitlements.md#shared-readiness-result-contract). `read_preparation_result` uses that contract. Event notifications carry only the correlation fields below. Entitlements must not define another wire shape or silently fall back to a readiness-only result.

The local manifest keeps the full capability, source digest, profile version/digest, saved adapter target and producer authority, required outputs and hook bindings. Its immutable digest binds that retained evidence to the public roster. Subscriptions authenticates Entitlements as the aggregate producer; it does not validate native adapter producers or receive their credentials. Extra local fields must not leak into `RequiredAssignmentV1`. A change to any digest-covered local binding changes the manifest digest and follows the existing preparation fence.

**E-U06 implementation and qualification.** Subscriptions implements the shared SDK types and consumer validation. Entitlements implements the matching aggregate producer, GTS schema and full-result read. Both owners qualify result encoding, conditional evidence, ordered closure and settlement replay under the same version. E-U06 also owns the event schema, producer, consumer and lost-notification recovery. [UPSTREAM_REQS](../UPSTREAM_REQS.md) tracks the delivery gate; the contract above is the design source, not proof of implementation.

The manifest-unbuildable, preparing and ready cases use `kind = readiness`. Temporary build dependencies return that variant with `status = pending`, a reason and no manifest. Closure uses the separate variant, not a sixth readiness status. Full-result reads return the entire required assignment roster with the saved sequence. They cannot truncate or page it. Hook bindings stay in the local manifest and pinned profile; attempts and evidence stay in the hook journal. Hooks add no roster or hook-specific field to the Subscriptions result. Hook progress changes only the existing aggregate status, reason and evidence reference. Operators inspect detail through hook execution reads.

Store the non-activation fence, closed result, next sequence and publication intent atomically, including closure before manifest creation. The sequence belongs to the exact operation/preparation revision and continues across both variants. A read after that commit returns the closed variant. Duplicate closure delivery reuses the saved result; a settlement change creates a new closed result with a higher sequence and the same resolution binding. Later adapter observations may update settlement evidence but cannot restore the readiness variant. A delayed old readiness notification remains historical: Subscriptions checks its own terminal operation fence as well as the sequence.

An applied resolution keeps the existing historical readiness evidence and continues lifecycle processing. Neither replaying that evidence nor this closure variant starts a new activation. Only verified `closed_without_activation` selects the closed variant; a timeout, failed check or absent result does not.

A closed result is terminal for activation. Settlement covers every assignment that could have preparation effects, including optional assignments. It does not reuse the required-only readiness fold.

**Local proof of no dispatch.** Every call under preparation authority that can change rights or service state must first commit an irreversible `dispatchStarted` marker for its operation under the preparation-generation fence. This also applies to checks that the qualified profile permits to change state. Read-only queries are outside this proof.

Authorized cleanup after closure uses its own operation journal and remains allowed. The marker records that the preparation call may have left the process. It does not prove success. A lease or worker restart never clears it. Closure uses the same fence to block new markers and cancel unstarted outbox work.

Local settlement requires a closed generation, complete retained dispatch history and no such marker. Record that proof with the closure. The same proof may settle an individual unstarted assignment in a mixed generation; it proves nothing about other assignments that may have dispatched. This also covers closure before manifest creation, if no work capable of changing state could have left the process. Missing receipts, missing manifests, timeouts and expired leases are not proof. If closure loses the race to a marker, require adapter confirmation.

**Adapter settlement contract.** Preparation-enabled adapter instances expose the read-only SDK interface `read_preparation_settlement(source_key, assignment_generation, closure_ref)` through the same saved instance and scoped authority. This is an optional preparation interface on the existing adapter, not another provider selection or a change to the quota application methods. A profile requiring preparation stays disabled until its adapter implements and qualifies it. The read grants no cleanup authority; cleanup still follows the exact non-activation resolution and capability command.

| Evidence | Required content and meaning |
| --- | --- |
| `PreparationSettlementEvidenceV1` identity | Schema version, source key, assignment/generation, saved binding/profile version and digest, exact closure reference/version/digest, correlated preparation and cleanup operation IDs, producer, ordered evidence version, observation time and safe evidence reference. No subscription ID or OSS resource roster. |
| `outcome = unresolved` | Prior effects, closure enforcement or safe reuse are not yet confirmed. Retain the obligation and recover the original operations. |
| `outcome = resolved` | Adapter confirms the source is closed against delayed effects, preparation effects are resolved, and the exact authorized logical successor scope is safe to reuse. Include that scope and the confirmation evidence. |
| `outcome = isolated` | Adapter confirms source closure and durable isolation of the old effects from the stated safe successor scope. Keep the old obligation and recovery history; do not claim cleanup completed. |

Authenticate the pinned producer and verify the source, generation, closure and profile bindings. Reject stale or conflicting evidence. Missing proof stays unresolved. A valid isolation scope must fit existing commercial authority and saved routing; it cannot authorize a provider switch, scope migration or extra quota. Reads and pushed `preparation_settlement` observations normalize to the same typed evidence and ordering rule. Qualification proves that delayed old actions and cleanup cannot affect the authorized successor. OSS owns how it achieves this.

| Cleanup obligation | Transition evidence |
| --- | --- |
| `open` to `requested` | Authorized cleanup intent and its operation identity are durably saved. Dispatch acceptance does not settle it. |
| `open` or `requested` to `settled` | Valid adapter `resolved` evidence, including safe reuse and closure enforcement, or complete local no-dispatch proof for that assignment under the closed-generation fence. |
| `open` or `requested` to `quarantined` | Valid adapter `isolated` evidence with the authorized safe successor scope. Operator disposition is insufficient. |
| `quarantined` to `settled` | Later correlated adapter evidence confirms completed resolution and safe reuse. Retain the isolation history. |

The generation returns `open` if any obligation lacks complete proof. It returns `settled` when every obligation is settled, or when the local no-dispatch proof covers the entire generation. Otherwise it returns `quarantined` only if every remaining obligation has valid isolation and safe-reuse evidence. Subscriptions decides whether to admit the successor. An operator may request reconciliation; no operator command sets settlement directly. Recovery may update evidence without rewriting the immutable closure.

`AssignmentObservationV1` carries the shared plugin context, `schemaVersion`, `scopeId`, `assignmentId`, `generation`, `operationId`, `desiredRevision`, `desiredDigest`, pinned `serviceProfileRef`/version/digest, `targetRef`, `eventId`, `producerId`, `statusSequence`, `observedAt`, `evidenceRef` and a typed `result`. Results are `application`, `readiness`, `security_block`, `output`, `resource_hold` or `preparation_settlement`; the corresponding feature defines each payload. A readiness payload uses `pending / ready / failed / withdrawn`; `failed` requires a retryable/definitive class and reason. Application evidence cannot satisfy an `observed_ready` criterion by itself.

`record_assignment_observation` returns `accepted / duplicate / refused / quarantined`, the event ID, accepted sequence and a safe reason. A duplicate does not allocate a new event or advance the aggregate. Conflicting event content or equal-sequence content is quarantined. A full-state recovery observation must name the same authority stream and covered sequence; a missing event range cannot be assumed complete. A changed physical target needs durable association with the pinned logical target before it can satisfy readiness.

**Security block observations.** The `security_block` variant of `AssignmentObservationV1` uses the same authenticated assignment, generation, desired revision and producer sequence. It applies to one assignment, not directly to a whole manifest. The pinned adapter reports the provider's security decision; Entitlements neither makes that decision nor adds a command to lift it.

| Payload field | Rule |
| --- | --- |
| `blockId` | Stable incident identity within the assignment generation and producer authority. Replays retain it. Different incidents remain independent. |
| `state` | `active` or `cleared`. This is separate from ordinary readiness and application outcomes. |
| `reasonCode` | `security_blocked` for active; `security_block_cleared` for cleared, using the registered GTS reason instances. Neither is a definitive preparation failure. |
| `evidenceRef` | Required recoverable evidence of the provider security decision. Clearance includes evidence of its audited lift by the authority that imposed it. |

The SDK uses a typed payload and the assignment-observation GTS schema validates this variant. A clear must identify a known block and carry a higher sequence from the same pinned authority. A missing incident history requires authoritative recovery; it cannot be treated as clearance. Producer replacement must preserve the incident history and authority mapping under the existing fenced replacement contract.

Store block state with its observation and any changed aggregate/outbox in one transaction. An active block on a required assignment prevents ready for both `applied` and `observed_ready` criteria. It yields aggregate `blocked`, unless a required definitive failure already takes priority under ENT-D-09. Optional assignment blocks remain visible but do not block the manifest. A provider-wide incident must be reported for each affected assignment by its responsible adapter; it cannot bypass assignment authority.

An ordinary `ready`, `applied`, retry, timeout, missing observation, hook result or operator finding disposition cannot clear a block. Clearance affects only its incident; any other active block remains. Then recompute readiness from all current criteria and evidence. Clearance alone is not ready, and after terminal resolution it cannot reopen activation or undo historical T. Retain active incidents through restart and observation gaps. Recovery snapshots must explicitly cover their state; omission cannot mean cleared.

Qualification under E-U01/E-U02/E-U05 and E-U06 proves the signal, authenticated clearance and recovery (E-AC61). Aggregate Subscriptions statuses and quota application results remain unchanged.

### Readiness change events

`ReadinessChanged` is a notification that a result was saved. It carries no status, manifest, commitment, resolution or subscription ID. Only `read_preparation_result` returns readiness evidence. The same notification covers pending, ready, failed, blocked and closed results, including closure before a manifest exists.

| Item | Contract |
| --- | --- |
| Event type | `gts.cf.core.events.event.v1~cf.bss.entitlements.event.v1~cf.bss.entitlements.readiness_changed.v1~`; final type under the abstract Entitlements event family. |
| Topic | `gts.cf.core.events.topic.v1~cf.bss.entitlements.readiness.v1`; owned and registered by Entitlements. |
| Subject | ASCII `<orderingTenantId>/<operationId>/<postureRevision>`; UUIDs use canonical lowercase form and the revision uses decimal digits. `subject_type` is `gts.cf.bss.entitlements.readiness_observation.v1~`. |
| Type traits | `topic` is the topic above; `allowed_subject_types` contains only the subject type above; `partition_key = /subject`. These are schema traits, not fields in each event. |
| Envelope | Platform event envelope. `source = bss-entitlements`; `tenant_id = orderingTenantId`; `id` is the saved result's `eventId`; `occurred_at` is its save time. Publication retries keep these values and the payload. The broker assigns its own `sequence` and `partition`. |
| Required `data` | Exactly `orderingTenantId`, `scopeId`, `operationId`, `postureRevision`, `postureDigest` and `statusSequence`, copied from the saved result. Use the shared readiness SDK UUID, digest and lossless signed-64 integer codecs. `statusSequence` starts at 1. The topic and GTS event type identify the notification version. |
| Consumer | Subscriptions provisions one anonymous broker-minted consumer group per ordering tenant and durably saves its returned GTS ID. Replicas reuse that tenant's group, topic and type filter. Never share a cursor across tenant scopes; a broker group is not tenant-keyed. |
| Retention | Topic default `P7D`, with deployment overrides under Event Broker rules. This is a wake-up log, not an audit or result store. Correctness does not depend on retaining every notification. |

Use the [Event Broker envelope](../../../../system/event-broker/docs/ADR/0003-event-schema.md) and [partition rules](../../../../system/event-broker/docs/ADR/0002-partition-selection.md). Register schemas and the topic through the gear's GTS inventory; publish through the broker SDK. Bind the broker client in background work, not as a required peer in `init`. A missing broker retains outbox work and leaves result reads available. Retry the same saved notification; permanent schema errors become visible delivery findings. Never roll back or replace the result because publication failed.

The publisher needs produce authority for the event and subject type in the ordering tenant. The Subscriptions consumer uses its service principal and authorized tenant scopes. Broker authorization must limit publication to the configured Entitlements authority. A matching `source` string or GTS ID alone is not authentication. Validate the event type, subject, envelope tenant and data against the existing operation and its saved preparation binding. A notification cannot create an activation operation, choose another tenant or provide a URL to fetch.

Before acknowledging delivery, Subscriptions atomically saves deduplication state and durable work for that operation. Persist the highest notified `statusSequence` as a read target, not as accepted readiness. Duplicate or stale notifications can be acknowledged once matching work or a validated newer result is durable. Conflicting content under the same event identity, invalid bindings and unsupported versions are quarantined with a safe diagnostic; they never advance readiness. Save that disposition before acknowledgment.

The driver reads the latest full result under the ordering-tenant scope. A result newer than the notification is valid if its binding matches; a lower sequence stays pending and is read again. An event gap does not require intermediate results. Unavailable or absent reads supply no decision evidence. Validate the full result and normal activation guards before acting. Neither broker sequence nor event time orders business state or assigns T.

If a group must be recreated, save the replacement ID and schedule recovery of that tenant's pending operations. Never infer business progress from the new cursor. Polling runs independently of the consumer. After broker outage, cursor loss or retention expiry, recover each pending operation from the local work set through full-result reads. Durable wake-up work survives an acknowledgment followed by a crash. Notification retention never deletes commitments, closed results or unresolved cleanup work.

### Preparation bounds

Subscriptions completes financial admission before requesting preparation and rechecks its guards before activation. Entitlements reports service readiness independently of financial approval. It does not verify Billing commitments or manage monetary grants.

Subscriptions owns admission limits and attempt deadlines. Its frozen preparation policy must identify a versioned exposure authorization with the responsible financial owner, allowed capability/scope, maximum concurrent held preparations, maximum held age and bounded resource exposure. Values must be finite and positive where preparation is allowed. Each bound names an authorized owner action that can contain existing exposure; a profile lacking that action cannot qualify. Held age keeps advancing while the activation deadline is paused. Missing evidence blocks new preparation. Entitlements validates and retains this authorization; it keeps no usage or monetary ledger.

Admission counts include paused attempts and unresolved predecessor effects until their owner confirms settlement. At a limit, refuse new preparation and report the reason. For an already-held assignment, report the breach and retain the obligation until Subscriptions publishes a terminal resolution. Orders owns purchase/payment remediation. A timer, budget breach or operator finding cannot free uncertain work or refund a purchase. Emergency security denial follows its own immediate authority and retains recovery evidence.

The evidence passed to Rating/Billing names the preparation authorization, operation/generation, assignment, resource tenant, capability, authoritative usage-time source and activation resolution reference. Usage before applied T, or in a never-activated generation, stays platform-borne. Unknown time boundaries remain unresolved. After T, accepted charging terms apply. The adapter supplies native attribution; Entitlements never computes consumption or a remaining balance.

## 3. Processes / Business Logic (CDSL)

### Observation acceptance

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-algo-observation-acceptance`

**Input:** one adapter readiness or security-block observation from a producer and the saved manifest. **Output:** accepted and persisted, no-op, or refused/quarantined with a structured reason.

1. [ ] - `p1` - Authenticate the producer. Authorize its tenant, assignment and profile authority against the saved manifest; an arbitrary publisher's `producerId` is not proof. - `inst-observation-acceptance-1`
2. [ ] - `p1` - Check `schemaVersion`. An unsupported version is quarantined and reported, never interpreted as ready. - `inst-observation-acceptance-2`
3. [ ] - `p1` - Match exact `scopeId`, `assignmentId`, `desiredRevision`, `targetRef`, `serviceProfileRef` and the current manifest association. `targetRef` identifies the authorized logical service or holder scope, not a VM or volume. Its association must be saved before readiness evidence is accepted. OSS infrastructure changes do not replace that logical target. - `inst-observation-acceptance-3`
4. [ ] - `p1` - Compare `eventId` and content with what is stored. An identical delivery is a no-op. A conflicting duplicate, wrong scope, unexpected producer or ambiguous association is refused or quarantined without information leakage. - `inst-observation-acceptance-4`
5. [ ] - `p1` - Compare `statusSequence` with the producer's high-water mark for the assignment stream. An older sequence cannot overwrite a newer `ready`, `withdrawn` or `failed` result. A gap requires authoritative reconciliation unless the agreed full-state snapshot contract proves it safely covered. Producer replacement or a sequence reset needs an explicit fenced generation mapping. - `inst-observation-acceptance-5`
6. [ ] - `p1` - Persist the accepted observation, its content identity, the producer sequence high-water mark and provenance durably, then advance the assignment readiness or incident state under [readiness state](#readiness-state). - `inst-observation-acceptance-6`
7. [ ] - `p1` - Recompute manifest readiness and persist any changed aggregate result with its sequence and publication intent in the same transaction. A dispatch receipt, broker acknowledgment or license existence alone never satisfies readiness. Application receipts use the readiness mapping below. One adapter's ready result does not satisfy another required producer or assignment. The manifest is ready only when the held commitment exists and every required assignment satisfies its pinned readiness criterion and required-output checks for the exact desired revision/digest and every required hook passes for current prerequisite evidence; a ready for a retired, superseded or withdrawn revision is historical evidence only. - `inst-observation-acceptance-7`

## 4. States (CDSL)

### Readiness state

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-state-readiness`

For an `observed_ready` criterion, each required assignment of a manifest carries one current observation status: `pending`, `ready`, `failed` or `withdrawn`. These are preparation observations, not subscription states; the subscription stays `preparing` on the Subscriptions side until its own commit. Order is the producer's `statusSequence`, never `observedAt`.

| From | Observation / to | Mandatory guard |
| --- | --- | --- |
| none | manifest persisted / pending | Durable intake work consumed; current source and open preparation-generation fences checked; manifest, producer authorities, first aggregate result and publication intent committed together before dispatch. |
| pending | newer `ready` / ready | Accepted under observation acceptance; exact `desiredRevision`, target and profile. |
| pending or ready | newer `failed` / failed | Higher sequence from the authorized producer; partial effects stay subject to reconciliation. |
| failed | newer `pending` or `ready` / pending or ready | Higher sequence; the same assignment identity is recovered, no second environment is allocated. |
| pending, ready or failed | withdrawal / withdrawn | Subscriptions has fenced its local activation; Entitlements revokes or supersedes preparation authority and records cleanup obligations. |
| withdrawn | any later observation / withdrawn | Stored as effect evidence for cleanup; never produces ready for that revision. |

**Application receipts and readiness.** Evaluate the current required assignment's correlated application result before applying its pinned criterion. This table gives that assignment's contribution to the aggregate, not a new adapter result enum.

| Application evidence | Readiness contribution |
| --- | --- |
| `applied` for the exact desired revision/digest, without an open fulfillment discrepancy | Application criterion satisfied. Required outputs, any `observed_ready` observation and all required hooks still need to pass. |
| `pending`, known `partial`, `unknown` or no confirmed result | `pending`; recover the original operation. No timeout or retry limit proves refusal. |
| `refused`, reason classified `retryable` by the pinned capability profile | `failed_retryable`. Keep the refusal as the final result of that operation. |
| `refused`, reason classified `definitive` by the pinned capability profile | `failed_definitive`. Subscriptions decides closure; Entitlements does not clean up on the refusal alone. |
| `refused`, reason missing from the agreed classification | `failed_retryable` with a visible finding. Never infer definitive failure from an unknown reason or provider text. |
| Open fulfillment discrepancy, even with a stored `applied` receipt | Criterion not satisfied; `pending` unless an authoritative classified failure or emergency security block requires a higher-priority status. |

The pinned capability profile defines `readinessFailureClasses`: a map from registered reason IDs to `retryable` or `definitive`, covered by its version and digest. Qualification must classify every supported refusal reason. A missing map is incomplete profile configuration; an unexpected reason in a valid profile uses the fallback above. A runtime profile edit cannot reclassify an existing attempt.

Retryable readiness does not authorize replaying a final refusal as a new execution. A corrected attempt follows ENT-D-13: allowed remediation, new operation and higher source revision. Unknown effects recover under the original identity. Recovery exhaustion raises a finding and retains uncertainty; only Subscriptions evaluates the activation deadline.

Apply the mapping only to required assignments. Optional failures stay visible and do not block readiness. Pending work cannot hide another required assignment's failure. An open discrepancy is cleared only by newer authoritative evidence that the purchased capability is fulfilled, not by matching configuration or acknowledging a finding. Recompute readiness when application, output, discrepancy, security-block or observation evidence changes. After terminal activation resolution, retain later evidence without reopening the activation attempt.

The held commitment is separate from these observation states: ready-to-failed does not permit release. Only the exact applied or irrevocable non-activation resolution settles it; immediate security denial does not erase recovery obligations.

The manifest is `ready` only when all required assignment criteria and output checks pass for the exact desired revision/digest. Every required readiness hook must also pass for the current prerequisite epoch/digest. `applied` needs complete application evidence without an open fulfillment discrepancy; `observed_ready` also needs a current ready observation. Optional failures remain visible. Closure withdraws the manifest and prevents new ready publication.

Hook `not_ready` or a pending check prevents ready. Failed, invalid or exhausted execution reports `failed_retryable`; no hook can report definitive failure or lift a security block. Notices never enter the readiness or posture-outcome fold.

While the generation is unresolved, evaluate aggregate readiness in this order (ENT-D-09):

1. Return `failed_definitive` if a required assignment has a definitive failure.
2. Return `blocked` while an emergency security block is in force.
3. Return `failed_retryable` if a required assignment or readiness hook has a retryable failure.
4. Return `ready` if every required criterion, output and hook passes.
5. Otherwise, return `pending`.

A lower sequence never changes a state.

A new desired revision starts a new manifest row set; it does not reuse retired identities or revisions.

## 5. Definitions of Done

### 5.1 Manifest-gated readiness with recoverable observations

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-dod-initial-readiness`

The implementation must:

- Qualify the Products/Pricing requirement, Subscriptions binding and Orders pre-purchase guard under E-U03/E-U04/E-U06/E-U10. Run E-AC59 with and without hooks before enabling the affected offers. These are owner SDK adoption requirements, not new Entitlements purchase methods.
- Save the complete manifest before preparation dispatch or observation publication.
- Check every producer and scope against the manifest.
- Keep a durable sequence high-water mark for each producer and assignment stream.
- Reject or quarantine conflicting, stale, wrong-scope and unsupported observations without leaking information.
- Report ready only for the whole manifest and exact desired revision, with its held commitment.
- Retain readiness, withdrawal and cleanup obligations across restart. Expose them by assignment and operation.

A dispatch log is not completion evidence.

The same assignment identity is recovered after a lost or unknown result without a second account or environment. No subscription identifier is accepted in or written to any adapter payload; the manifest association is keyed by assignment within `(namespace, resource tenant)`.

**NFR acceptance evidence:** measure first delivery of each preparation assignment to its required adapter against the proposed `cpt-cf-bss-entitlements-nfr-handoff-latency` targets (p95 < 5 s, p99 < 15 s, auditable exception beyond 60 s). The requirement covers every accepted posture revision, so features 03 to 05 measure first delivery of their posture apply against the same targets as supporting evidence.

Report time to confirmed usable access separately; it is never hidden inside the delivery figure. Fault injection must show a lost readiness notification recovered through the assignment status read, a restart resuming pending observations from the high-water mark, and a withdrawal racing a ready reaching the fenced outcome with cleanup obligations recorded.

**Implements:** `cpt-cf-bss-entitlements-flow-initial-readiness`, `cpt-cf-bss-entitlements-algo-observation-acceptance`, `cpt-cf-bss-entitlements-state-readiness`.

## 6. Acceptance Criteria

- [ ] A rolled-back result change emits no event. A committed change survives producer restart and retries with the same ID, binding and sequence. No event carries a full result or a subscription ID (E-AC77).
- [ ] Crash the consumer before its local commit and after its broker acknowledgment. Redelivery or the saved wake-up resumes the same read. Duplicate, stale and skipped sequences do not create operations or replay every intermediate result (E-AC77).
- [ ] A notification precedes result visibility. Keep its target sequence pending until a matching or newer full result is readable. A newer closed result wins over an older notification; neither a broker timestamp nor offset supplies readiness (E-AC77).
- [ ] Two ordering tenants use separate groups and cursors. Both receive their own notifications under single-target scopes; consuming one tenant cannot skip the other's events. Replica restart reuses the saved group (E-AC77).
- [ ] Missing broker, lost cursor and expired notification retention leave result reads and periodic recovery working. Foreign tenants, mismatched subjects, changed content under one event ID and unsupported versions are quarantined without changing activation state (E-AC77).

- [ ] Drop all readiness notifications while full-result reads work. Subscriptions recovers the exact manifest, sequence and commitment through polling and evaluates its normal activation guards. Later duplicate or stale notifications cannot change the outcome (E-AC77).
- [ ] Subscriptions disappears after Entitlements records a ready commitment. Entitlements restarts, serves the saved result and retains notification/resolution recovery. No timeout grants cleanup. When Subscriptions returns, recovery uses the original operation and respects any later terminal decision (E-AC77).

- [ ] An offer requiring preparation, with or without hooks, has `entitlements_required`, but effective Subscriptions Settings select `no_preparation`. The configuration binding refuses; Orders commits no purchase and captures no payment.
- [ ] Binding succeeds, then Settings switch to `no_preparation` before purchase or activation. The exact purchase still uses its saved Entitlements mode/profile. A later purchase binds against the new Settings and refuses the incompatible mode.
- [ ] A binding response is lost. Orders recovers the original binding; it neither commits with missing proof nor creates a replacement configuration for the same acceptance. Wrong tenant, order version, acceptance or requirement digest cannot pass the guard.
- [ ] Concurrent requests with different operation IDs return the same committed binding, including across a Settings change. A saved refusal replays; a new operation after configuration repair may bind only if none exists.
- [ ] A no-preparation-qualified offer can bind `no_preparation`. Missing requirement evidence or incompatible combined profile requirements cannot be interpreted as that permission.
- [ ] Qualification withdrawal follows the existing new-sale eligibility guard. An outage or emergency disablement after commitment preserves the obligation and recovery; activation cannot skip configured checks or reselect a mode. Binding does not start the preparation deadline (E-AC59).

- [ ] An Orders pause without a new posture suppresses deadline-based overdue findings after an authorized operation read. Unpause uses the updated owner deadline. Unknown progress cannot prove overdue; ordinary dependency recovery and held-age limits remain active. A later verified pause resolves a stale diagnostic without changing authority.

- [ ] Saved results and full-result reads round-trip through the agreed consumer SDK/GTS codec with the same nested manifest, five-field roster and two-field commitment (E-AC71). No local adapter or hook fields leak into the public roster.
- [ ] Integer fixtures include a valid value above 2^53 and the signed 64-bit limit. Strings, fractions, negative revisions, zero sequences and overflow fail without rounding or changing digests. Owner UUID and string constraints remain enforced.
- [ ] A 512-entry required roster is complete and unique; a larger or duplicate roster is refused, never truncated. An empty roster still needs the pinned profile and required hook checks.
- [ ] The agreed decoder accepts both result variants and rejects mixed variants, missing ready commitments and invalid conditional evidence. An older readiness-only decoder rejects the new schema; delivery cannot downgrade it or interpret closure as ready.
- [ ] Loss, replay and settlement updates preserve the exact commitment, manifest and sequence in full-result reads; events carry only the matching correlation and sequence. Both owners prove the terminal fence before E-U06 closes.

- [ ] A required assignment reports an authenticated active security block. The aggregate becomes `blocked` even with `applied` or ready evidence. A required definitive failure still takes priority; an optional block alone does not block the manifest (E-AC61).
- [ ] A higher-sequence clear from the same pinned authority clears only the named incident. Wrong authority, stale sequence, ordinary ready/applied results, a hook or operator disposition cannot clear it. Other active incidents still prevent ready.
- [ ] Restart and observation gaps retain active incidents. Recovery requires explicit incident evidence. Clearance recomputes readiness; it cannot reopen a closed attempt or change a previously applied T.

- [ ] Closure before manifest creation returns `kind = closed_without_activation` with no readiness status, manifest or commitment. Settlement is `settled` only with complete fenced no-dispatch proof; otherwise it remains `open` (E-AC60).
- [ ] Closure after `ready` atomically replaces the current result with the closed variant. A delayed readiness notification or adapter callback cannot reopen the attempt. Restart and lost delivery recover the same closed result.
- [ ] Settlement progresses in closed results with higher sequences and the same resolution binding. Duplicate closure reuses the saved result. Conflicting or unsupported variants cannot authorize activation or a successor.

- [ ] A required quota assignment with criterion `applied` receives a definitive refusal. Aggregate readiness is `failed_definitive`, even if another assignment is pending. Only Subscriptions can close the attempt.
- [ ] A refusal classified retryable produces `failed_retryable`. An unclassified refusal also produces `failed_retryable` with a finding, without a new execution or automatic closure. The saved profile class survives restart and profile edits.
- [ ] Pending, partial, lost or unknown application stays pending while the original operation is recovered. Recovery exhaustion cannot turn uncertainty into a definitive refusal.
- [ ] A stored applied receipt with a purchased 100 GiB cap and an unresolved 60 GiB provider ceiling cannot satisfy readiness. Matching stored configuration or acknowledging the finding does not clear the discrepancy. New authoritative fulfillment evidence can clear it.
- [ ] Applied evidence alone cannot bypass required outputs, an `observed_ready` check or a configured readiness hook. Optional assignment failures do not block readiness. A higher-priority required failure or security block wins over pending work.

- [ ] Subscriptions closes before manifest creation or before any aggregate result reaches it. Entitlements retains the generation fence without requiring readiness metadata; delayed preparation and ready cannot reopen it (E-AC41).
- [ ] Partial preparation fails before aggregate ready and no readiness commitment exists. The generation-bound non-activation resolution authorizes only correlated cleanup; lost closure recovers by the same identity and late effects remain tracked without affecting a successor (E-AC42).

- [ ] Failure and deadline evidence are reported through aggregate readiness for the exact operation/generation. Subscriptions decides closure and delivers the resolution through posture. Wait for that result before cleanup. Delayed observations, restart and profile edits cannot reopen or extend the attempt. A prior applied T remains authoritative (E-AC35).
- [ ] Unknown outcome retains effects; a successor cannot overlap unsafe predecessor allocation or cleanup. Terminal closure preserves the purchase and payment. It does not prove resource cleanup or create a refund (E-AC36).

- [ ] Two concurrent operations in one scope, a fast callback and a lost aggregate result recover by the exact saved operation/posture; no assignment discovery race or cross-operation match is possible.
- [ ] A manifest confirmed to require neither assignments nor readiness hooks by the authoritative profile emits and retains aggregate ready. A changed roster at the same manifest version conflicts; a higher posture revision replaces the binding and fences old results.
- [ ] Ready is reported only for the entire pinned assignment and producer manifest; duplicate, wrong-tenant, old-revision and lower-sequence observations cannot produce ready or overwrite a newer `withdrawn`.
- [ ] A lost aggregate notification is recovered through `read_preparation_result`; missing adapter evidence is recovered through the pinned adapter read; an unknown or not-found result does not allocate a second environment. A Subscriptions commit failure after readiness finds the same ready state again; Entitlements assigns no T.
- [ ] Withdrawal racing ready is fenced at external execution; partial and late effects retain cleanup obligations. A delayed ready for a withdrawn revision is stored as evidence and never produces ready.
- [ ] PaaS initial readiness covers the account, the purchased environment and required access, without waiting for first login or a later VM creation. Entitlements reports ready; the active commit and period start are owned by Subscriptions with fresh local T, not provider observed time.
- [ ] An observation with an unsupported `schemaVersion`, an unauthenticated producer or an ambiguous manifest association is quarantined with a structured reason and no readiness effect; an identical redelivery is a no-op.

- [ ] Ready is published only with a durable commitment, including empty manifests. Concurrent activation/withdrawal produces one exact terminal Subscriptions decision; lost resolution delivery cannot permit release while that decision is unknown.
- [ ] Activation commits, then suspension or cancellation supersedes its undelivered posture. Recover the exact applied resolution through the later `resolutionRef`, settle the original commitment and retain T without replacing the newer desired head or restoring access. A stale copy has the same effect on resolution only (E-AC32).
- [ ] Recovery by saved preparation identity works without a later posture. Restart retains the work. Pending, missing and unauthorized reads settle nothing; mismatched operation, generation, digest or decision version is quarantined. Concurrent delivery and recovery store one result. Terminal non-activation before manifest creation fences late preparation and cleans up only authorized operation-owned effects (E-AC41).
- [ ] Failure of an execution guard, a failed observation, provider reconfiguration and a missing operation read do not release held preparation. An authorized emergency block acts immediately and reconciles its evidence without erasing an applied activation.
- [ ] Late usage belonging to authorized pre-T or never-activated preparation is platform-borne; unknown boundary evidence stays unresolved. A profile without bounded exposure, retention and Rating/Billing mapping cannot enable preparation.

- [ ] An applied-only profile becomes ready from complete application and required-output evidence without a separate ready callback. An observed-ready profile waits for that callback.
- [ ] Closure before manifest creation returns an explicit closed result. With complete local history, no dispatch-start marker and unstarted work fenced, it is settled without an adapter call. Missing manifest alone is insufficient.
- [ ] Closure races a worker: if closure wins, the worker cannot commit a dispatch-start marker or call the adapter. If the marker wins, settlement stays open until adapter proof, even after worker death or lease expiry.
- [ ] An optional assignment has an unknown effect while all required assignments are resolved. Settlement remains open; readiness optionality cannot drop cleanup obligations.
- [ ] Acknowledging or dispositioning a finding cannot change settlement. Valid adapter isolation can mark an obligation quarantined only with closure enforcement and a safe scope within the accepted authority. One unresolved obligation keeps the generation open.
- [ ] Wrong-source, wrong-generation, conflicting or stale settlement evidence is rejected. A cleanup success without safe-reuse proof is insufficient. Late predecessor actions and cleanup cannot affect an admitted successor.
- [ ] Preparation admission at its finite limit refuses new work. A held attempt crossing its age bound raises failure evidence but retains effects until authoritative resolution.
