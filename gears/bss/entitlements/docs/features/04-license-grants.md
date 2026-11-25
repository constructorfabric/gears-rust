---
description: "Grant definitions, DesiredGrantSet derivation, application through the selected license adapter and the seat-assignment boundary."
---

# Feature: License Grants

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-featstatus-license-grants-implemented`
- [ ] `p1` - `cpt-cf-bss-entitlements-feature-license-grants`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Grant application](#grant-application)
  - [Grant purchase and application contracts](#grant-purchase-and-application-contracts)
  - [Shared fulfillment output contract](#shared-fulfillment-output-contract)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Grant derivation](#grant-derivation)
  - [Posture by transition](#posture-by-transition)
- [4. States (CDSL)](#4-states-cdsl)
  - [Desired grant state](#desired-grant-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Fenced grant application through the selected adapter](#51-fenced-grant-application-through-the-selected-adapter)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->


**Status:** proposed implementation; provider contracts and runtime conformance remain delivery gates.

**Release scope.** License grants, application evidence and readiness are v1. The shared fulfillment-output contract, descriptor/delivery state and stream/link retrieval are v2. A v1 grant profile cannot require output delivery through Entitlements; it still proves provider-confirmed usable rights. Output clauses in result/read DTOs apply when that v2 capability is enabled.

## 1. Feature Context

### 1.1 Overview

Entitlements derives license grants from committed posture and frozen grant rules. It applies them through the selected license adapter using scope, assignment and generation identity. Preparation grants are available while the subscription is `preparing`. The licensing service owns seat assignment and runtime checks.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-entitlements-fr-license-grants`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-entitlements-actor-license-adapter` | Selected license adapter; applies fenced grant state and reports evidence. |
| `cpt-cf-bss-entitlements-actor-subscriptions` | Publishes the posture that selects grants, quantity, validity and holder scope. |
| `cpt-cf-bss-entitlements-actor-platform-operator` | Inspects pending, refused and unknown grant application. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [adapter capability profile](../DESIGN.md#333-adapter-capability-profile).
- **Build prerequisites:** as allocated in [DECOMPOSITION.md](../DECOMPOSITION.md); decisions in [DECISIONS.md](../DECISIONS.md).
- **Provider prerequisites / integration acceptance:** gate E-U02 in [UPSTREAM_REQS.md](../UPSTREAM_REQS.md).

## 2. Actor Flows (CDSL)

### Grant application

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-flow-grant-application`

**Actor:** Subscriptions, through `CommercialPostureV1`; the selected license adapter as the effect owner.

**Success:** the `DesiredGrantSet` for the accepted revision is applied and a correlated receipt with status `applied` is stored for every member.

**Errors:** no frozen grant definition for a license member, unsupported capability profile, fence refused, adapter refused, unknown outcome after a lost response, revision superseded before application.

The license adapter implements the selected capability profile (ENT-D-01). Its design defines the receiving system, APIs, permissions and application mechanism. Entitlements keeps the desired state, the correlated outcomes and the end-to-end idempotency, stale-action and recovery guarantees; the adapter maps them to its native verbs.

The deployment selects an adapter for each capability profile. A license capability marked `not_applicable` needs no adapter. A missing required adapter never counts as success.

1. [ ] - `p1` - Entitlements accepts a posture revision under [posture intake](01-posture-intake.md). It selects the members whose capability references name a license capability. The other members are handled by [quota projection](03-quota-projection.md) and [resource execution](05-resource-execution.md). - `inst-grant-application-1`
2. [ ] - `p1` - For each selected member Entitlements derives one `DesiredGrantSet` member under [grant derivation](#grant-derivation): grant definition reference from the accepted rule snapshot named in the posture provenance, quantity, validity interval, holder scope and posture. Each logical right has its own `assignmentId` and source key `(namespace, resource tenant, assignmentId)`. The local set collects these independently fenced assignments and explicit revocations for removed rights; it has no shared assignment identity. No subscription identifier is written into the set. - `inst-grant-application-2`
3. [ ] - `p1` - Entitlements fences each assignment by its own source key, revision and digest. A lower revision is stale and refused for dispatch. An equal revision with the same digest recovers the assignment, delivery intent and result without another grant. An equal revision with a different digest conflicts and opens a finding. The derivation transaction saves the complete local set and each changed assignment's desired revision, saved adapter binding and delivery intent. Unchanged assignments keep their existing work. Each intent has its own operation identity. Call the adapter only after commit. - `inst-grant-application-3`

   Recheck the current source and preparation-generation fences before committing; withdrawn or superseded work cannot initiate stale dispatch. Before a preparation call, commit its dispatch-start marker under the same generation fence as closure (feature 02). Adapter calls occur only after commit.
4. [ ] - `p1` - The outbox dispatches each changed assignment's complete target to its saved license adapter with scope, assignment identity, desired revision and generation under [grant apply](../DESIGN.md#335-grant-apply). The adapter returns applied, pending, refused or unknown evidence with authoritative effective time where known. Adapter acceptance of the dispatch is not application. - `inst-grant-application-4`
5. [ ] - `p1` - Entitlements records an `AppliedReceipt` per member and advances the member under [desired grant state](#desired-grant-state). A pending receipt is recovered by reading or replaying the same application operation; no retry creates another grant. A refused or unknown outcome opens a finding under [reconciliation and recovery](06-reconciliation-recovery.md); it never broadens access. - `inst-grant-application-5`
6. [ ] - `p1` - For an initial activation the preparation grants are issued while the subscription is still `preparing` and reported as readiness observations under [initial readiness](02-initial-readiness.md) (ENT-D-02). `active` is not the trigger; preparation rights cannot be derived from `active`, or the cycle would prevent activation. Entitlements owns the assignment manifest; Subscriptions binds its aggregate result to the saved activation operation and posture revision/digest. - `inst-grant-application-6`

**Receipt content.** A receipt carries the same source key, revision and digest, the operation id, the adapter and profile identity, the adapter's own record identity for the grant, the status, the applied effective time where the adapter is authoritative, the highest accepted and last fully applied revision/digest, and a structured reason. The receipt is the only evidence of application; a dispatch log is not.

The adapter establishes durable ordering before acknowledging a higher revision. Older work cannot take effect after that acceptance, even if the newer application later fails. An invalid or conflicting call cannot advance the accepted head. Accepted and fully applied heads stay separate.

A saved definitive refusal remains final for its operation. Replay returns that refusal. Authorized repair uses a new operation and a higher source revision while preserving commercial authority and the same grant ownership. It does not issue another purchase or duplicate grant. Recover unknown work under its original identity before starting a corrected attempt.

**License delivery to BSS.** For profiles that declare outputs, the adapter also returns or exposes recoverable descriptors under [fulfillment outputs](../DESIGN.md#337-fulfillment-outputs). Entitlements validates their scope, member, operation/revision and saved binding, then commits output versions and ordered availability observations with the correlated receipt.

Protected files/keys remain in the provider or approved store. BSS lists the operation's outputs through the public Entitlements client and requests an exact version's content under current authorization and release policy; it never calls the plugin directly.

A required initial output is prepared and its retrieval path verified before aggregate ready. Customer release follows the accepted preparation/activation policy without a post-active dependency for essential pre-T access. Optional output failures remain visible. Lost responses recover the same output/version; downloads do not issue grants.

Replacement creates an explicit successor version, and late observations cannot make a withdrawn, superseded, revoked or expired output current. Content-delivery status and actual provider revocation remain separate facts.

**Lifecycle rows (ENT-D-16).** Only a posture revision changes a grant. A renewal publishes no posture, and grants continue. Under suspension, the member restriction uses the frozen `restrictionPolicy` below. Renewal alone never extends the effective interval in Entitlements; an authorized interval change must arrive in a higher posture revision. An added component gets new assignments. An inactive resource tenant receives only restricting commands.

### Grant purchase and application contracts

These are the field contracts for the logical types in DESIGN §3.1. All listed fields are required unless marked conditional. Use the primitive and digest rules in [posture intake](01-posture-intake.md#sdk-contract-and-validation). Pricing owns rule storage, authoring APIs, publication and acceptance. This feature defines what Entitlements consumes; [VHP-4377](https://virtuozzo.atlassian.net/browse/VHP-4377) tracks Pricing adoption.

| Type | Fields and validation |
| --- | --- |
| `GrantDefinitionV1` | `schemaId`, `definitionId`, `version`, `digest`, immutable `productRef` and `capabilityRef`, `quantityRule`, `holderKinds`, `validityPolicy`, `restrictionPolicy`, `revocationPolicy`, and qualified `capabilityProfile` reference/version/digest. References resolve retained content, not a live catalog value. |
| `quantityRule` | `mode = Fixed / PerPlanUnit`, exact nonnegative integer `coefficient`, `minQuantity`, `maxQuantity`, positive `step`. Represent integers as decimal strings. The inclusive bounds and step apply to the resulting quantity. No fractional seat, rounding, overflow or implicit unlimited value. |
| `GrantBindingV1` | `bindingId`, exact definition ID/version/digest, `planId`, `planRevision`, nonempty unique `itemIds`, accepted `dimensions` and `holderPolicy`. Dimensions may be empty when the profile needs none. One fixed binding applies once even when several selected items reference it. |
| `AcceptedGrantSnapshotV1` | `acceptanceId`, `receiptRef`, `orderId`/version/line, `planId`/revision, original exact `planQuantity`, ordering/resource/payer/seller tenant axes, accepted validity and terms evidence, `snapshotId`, `itemCount`, `digest`, complete selected bindings and definitions. Inline content or durable references are allowed; every reference pins version and digest. |
| `DesiredGrantSetV1` | Local derivation result for one accepted posture revision: complete `members` and `removals`, member count and complete-set digest. Every entry identifies its own assignment and target. It has no shared `source_key`, source revision or operation ID and is not sent as one adapter source. Purchase references stay local. |
| `DesiredGrantStateV1` | Complete target of one assignment: shared plugin context with `assignmentId` and source key, `source_revision`, `desired_digest`, preparation `generation`, exact definition ID/version/digest, integer `quantity`, authorized `holder_scope`/generation, validity interval, frozen `applicationPolicy` with restriction/revocation policies, `posture = Granted / Restricted / Revoked`, effective boundary and conditional preparation authority. Distinct logical bindings stay distinct. |
| Removal | The existing assignment's complete target with its retained definition, holder scope and generation, a higher source revision and `posture = Revoked`. No absence-only delete. Revocation closes only that assignment; a new right after terminal closure needs a new identity. |

**Closed v1 policy values.** These fields are typed SDK values and closed objects in the grant-definition/binding GTS schemas. Unknown modes, extra behavior fields or unsupported combinations fail publication and acceptance. Entitlements also rejects them before new grants or increases; it never selects a default.

| Field | Shape and allowed values | Meaning |
| --- | --- | --- |
| `holderKinds` | Nonempty unique array of `tenant` and/or `logical_scope`. | Holder kinds the definition permits. No physical-resource identity or per-seat roster. |
| `holderPolicy` | Object with only `kind = tenant / logical_scope`; the selected kind must occur in `holderKinds`. | The binding selects the allowed holder kind. Subscriptions supplies the actual holder reference and scope generation in every member. |
| `validityPolicy` | Object with only `mode = finite / unbounded_allowed`. | `finite` requires a supplied start and finite exclusive end. `unbounded_allowed` accepts a finite interval or an explicitly unbounded end. A start is always required; omission never means unlimited. |
| `restrictionPolicy` | Object with only `mode = suspend_access`. | While Restricted, suspend use of this logical right. Keep its assignment identity, accepted quantity, holder and validity for a possible authorized resume. No revocation, new allowance or implicit grace period. |
| `revocationPolicy` | Object with only `mode = revoke_access`. | Revoke this source-owned right and close its assignment against later grant/resume commands. Do not revoke other sources, delete service data or infer a refund. |

For `tenant`, the supplied holder reference must equal the member's resource tenant. For `logical_scope`, Subscriptions must supply an authorized logical holder reference and generation within that resource tenant. The binding and qualified capability profile must support that scope kind. Neither mode selects the caller, payer, seller, a VM or an individual seat. Native holder mapping and seat assignment remain with the licensing service/adapter. An incompatible holder change cannot silently retarget an existing assignment.

Validity is the effective interval supplied by Subscriptions, checked against the frozen policy and accepted terms. Times are UTC; a finite end must be after the start. Entitlements does not calculate a subscription term, extend an interval on renewal or replace it with the provider's current expiry. An unbounded end permits no access after explicit revocation. Preparation authority and effective boundaries still require the separate generation contract; these policies do not assign activation T.

The complete adapter target includes only the validated holder, quantity, interval, posture and frozen `applicationPolicy = {restrictionPolicy, revocationPolicy}`, with definition/profile identities and existing authority fields. Include application policies in `desired_digest`. The adapter does not fetch Pricing terms or receive the full commercial receipt. It maps these meanings to its own APIs and returns correlated outcome and effective-time evidence.

`Granted` applies the exact authorized right and interval. `Restricted` applies `suspend_access` from the supplied effective boundary and remains restricted until a higher authorized member posture changes it. A timer or restriction-end hint cannot resume it. Resume keeps the same assignment and cannot exceed its authorized validity. `Revoked` applies `revoke_access`; it cannot be undone by replay or a later grant on that closed source. A new authorized right uses a new assignment under the existing lifecycle rules.

The adapter must prove the selected suspension and revocation behavior under E-U02. An unsupported policy cannot qualify for sale. After purchase, inability or uncertainty stays refused/pending/unknown as the actual evidence shows; it cannot become applied by hiding an output or changing only a BSS record. Offline license retrieval does not prove that use was suspended or revoked. Output release and retention remain the separate output contract.

Pricing freezes all policy fields in the accepted grant snapshot under E-U03. E-U06 agrees the matching holder and effective-interval semantics in member authority. SDK/GTS generation and validation fixtures belong to this feature (E-AC65). New policy modes require an explicit future contract and qualification; v1 has no silent fallback.


`GrantDefinitionV1` uses `gts.cf.bss.entitlements.grant_definition.v1~`. Bindings, accepted snapshots and desired sets are typed SDK records, not extra registry entities. Exact foreign definitions retain their owner's schema IDs. A complete snapshot is checked before deriving any new or enlarged grant. Restriction and revocation of a known source use its retained target, binding and current commercial authority; missing historical purchase data cannot prevent those source controls (ENT-D-13). An empty selected grant set needs explicit accepted evidence; missing history is not an empty set.

Canonicalize the local collection by `assignmentId`; reject duplicates or membership in both arrays. Its complete-set digest proves derivation completeness, not a shared adapter fence. Hash each assignment's complete target separately, including source, generation, profile, scope, posture, quantity and validity. Keep transport operation ID and receipt state out of `desired_digest`. `request_digest` additionally binds operation ID, target, effective boundary and authority. Replaying an operation with changed content conflicts even if its desired digest is unchanged.

`apply_license_grant` takes one `DesiredGrantStateV1` target with its own `operation_id`, `request_digest`, effective boundary and authority. `GrantApplicationResultV1` returns source/binding/operation identity, request digest, accepted revision/digest, last fully applied revision/digest when present, ordered observation identity/time, `pending / applied / refused / unknown`, safe reason and the outcome of that assignment.

The result names the assignment, requested posture and known outcome. Local collection reads retain one result for every member and removal; they do not replace per-assignment recovery. Include the provider record and actual effective time only when authoritative evidence confirms them. A refusal before creation or an uncertain create may have no record ID. Required outputs use the shared output contract below. An incomplete assignment result cannot be `applied`. A local collection is fully applied only after every member and removal is confirmed. A definitive refusal requires proof that no effects remain unresolved.

`read_license_grant` returns those separate heads, source closure, the complete state of that assignment and its current output descriptors. `read_operation_result` returns `found` with the saved or recovered result, or `not_found` with no result. `found` plus `unknown` means effects remain uncertain. Neither case authorizes a new grant identity. `not_found` permits only the identical original request under current fences. A closed source still supports reads and recovery.

### Shared fulfillment output contract

**Release: v2 / p2.** Retained here as the shared license/service output specification. Output-specific acceptance cases below apply to v2.

This section also applies to service outputs. The descriptor identity, content ownership and release rules are defined in DESIGN §3.3.7. Required SDK fields are `outputId`, `outputVersion`, `outputKey`, `kind`, `schemaId`, `memberId`, source/assignment and generation, origin operation/revision/digest, saved binding, producer, per-slot sequence, observation time, delivery state, safe metadata and validity when applicable. Protected outputs also carry an opaque content reference internally. Public descriptors omit that reference, producer credentials and provider payloads. Entitlements adds `fulfillmentRef` from its retained purchase-component association, as defined in [feature 06](06-reconciliation-recovery.md#public-reads-and-operator-commands). The provider descriptor does not carry this field.

The metadata is a discriminated union: license identifier (`displayName`, `licenseId`, `validFrom`, `validUntil`); document/key (`displayName`, `fileName`, `mediaType`, nonnegative `sizeBytes`); endpoint (`displayName`, credential-free `url`, `protocol`, `region` when applicable). A key is never an identifier or metadata value. Immutable origin/content changes require a new output version; an availability observation does not.

`get_output_descriptor` accepts an exact version; without it, it returns the current version explicitly. `retrieve_output` always requires `outputVersion` and operation identity. Its response is either `stream` (content, media type, safe filename, optional exact byte size) or `link` (URL, expiration instant, exact output ID/version). The profile chooses one mode. Streamed bytes and signed links never enter audit, events, receipts or durable request results.

**Adapter retrieval policy.** The public caller supplies the output version and operation identity. It cannot choose the provider mode or lifetime. Entitlements calls the saved adapter's `retrieve_output(source_key, output_id, output_version, retrieval_policy)` after authorization. The shared SDK defines `OutputRetrievalPolicyV1` as a closed union: `stream`, or `link` with `maxLifetimeSeconds` and `expiresNotAfter`. Stream forbids link fields. The host derives the mode from the pinned output profile. For links, the profile sets a maximum lifetime from 1 to 900 seconds. Expiry is the earliest of request time plus that maximum and any current output or release-authority end. An expired bound refuses retrieval.

The adapter must honor the requested mode, exact version and both expiry bounds. It returns a stream, or a link with exact output ID/version and expiry no later than `expiresNotAfter`. The link's lifetime from issuance cannot exceed `maxLifetimeSeconds`. It cannot substitute a link for a stream. Entitlements checks mode, version and expiry again before release; an expired, mismatched or overlong result is refused with a visible contract finding. A delayed request cannot extend authority. Both the license and resource adapter contracts adopt this policy under E-U02/E-U05. No protected link is saved for replay.

A retrieval retry rechecks current authority and release fences. It resolves the same content version and may mint a fresh short-lived link; it cannot replay expired authority or issue another license. Audit records retain identity and outcome only. Historical metadata requires audit authority; historical versions never grant secret retrieval. A missing content object yields unavailability and recovery of the original operation.

## 3. Processes / Business Logic (CDSL)

### Grant derivation

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-algo-grant-derivation`

**Input:** one accepted `CommercialPostureV1` revision and the frozen grant definitions referenced by its provenance. **Output:** a `DesiredGrantSet` for the revision, or a refusal with a structured reason and no dispatch.

1. [ ] - `p1` - Require the grant-rule branch of E-U03, including the typed definitions/bindings and Pricing receipt freeze; quota-only acceptance does not close it. Read the grant definition for each license member from the acceptance receipt referenced by the posture provenance, never from the live plan. A member whose receipt holds no grant definition for its capability is refused; absence is not "no grant". - `inst-grant-derivation-1`
2. [ ] - `p1` - Read the frozen definition's quantity mode: `Fixed` applies once per logical grant binding; `PerPlanUnit` multiplies its exact coefficient by the posture member quantity. That quantity is the sum of effective lots computed by Subscriptions; lots are provenance only and Entitlements does not recompute it. Use exact integer conversion within the accepted profile bounds; a fractional, negative or overflowing result refuses the whole set. Retrying or adding a same-binding lot never duplicates a fixed grant. - `inst-grant-derivation-2`
3. [ ] - `p1` - Require the E-U06 member inputs under [posture intake](01-posture-intake.md#sdk-contract-and-validation). Take effective validity, holder scope and access posture from Subscriptions. Validate them against the frozen definition and binding from Pricing. Never substitute document-wide access, caller identity or live plan terms. Missing or incompatible values refuse new grants and increases; restriction or revocation of a known source uses its retained binding under ENT-D-13. A `Restricted` member keeps its assignment and applies the frozen suspension policy from the supplied effective boundary. A `Revoked` member produces an explicit revoke member. A member present in the previous revision and absent in this one produces an explicit tombstone. - `inst-grant-derivation-3`
4. [ ] - `p1` - Derive the assignment identity within `(namespace, resource tenant)` for each member. Within the same saved commercial source and preparation generation, reuse the assignment for the same logical binding, capability, grant definition and holder scope. Quantity changes, restriction and resume keep it. Distinct purchases or bindings never share an assignment merely because definition and holder match; retired identities are not reused for a new grant. A successor activation attempt after a closed generation gets new assignment identities, because the identity includes the preparation generation (ENT-D-07). - `inst-grant-derivation-4`
5. [ ] - `p1` - Select and retain the capability profile and adapter route for each assignment. An unknown profile version or an unknown enum variant refuses before application. - `inst-grant-derivation-5`
6. [ ] - `p1` - Compute the local complete-set digest and each assignment's desired digest. Allocate a higher source revision and operation identity only for a changed target. Two derivations of the same accepted input reuse the saved identities, revisions and digests; conflicting content is a finding. - `inst-grant-derivation-6`

### Posture by transition

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-algo-grant-by-posture`

| Commercial event | Desired grant posture and boundary |
| --- | --- |
| Initial activation | Preparation grants are issued while the subscription is `preparing` and confirmed usable before Subscriptions commits active; see [initial readiness](02-initial-readiness.md). Entitlements does not issue initial prerequisites for the first time after the commit. Import and resource adoption are deferred. |
| Composition/quantity change | Increases and additions while suspended are refused by Subscriptions before any posture reaches Entitlements. For an applied active change the complete target replaces the previous set at its committed boundary; no transient union of old and new grants. |
| Suspend | Restricted posture that preserves data and the restoration identity of every grant; grants are suspended, not revoked. |
| Resume | The accepted set is restored only after Subscriptions has passed its lifecycle and occupancy guards; the restore carries a higher source revision and keeps the same assignment and preparation generation. A closed generation cannot resume. |
| Cancel | Revoke normal service; a separate narrow export grant may exist during permitted retention when the posture names it. |

**Purchase-state rule.** Subscriptions requires a separate authorized guarded resume before seats are purchased, quantity is increased or components are added to a suspended subscription. The purchase itself never resumes or queues a deferred grant. If an active purchase committed before the suspension, Entitlements reconciles its pending delivery under the newer restricted revision; the adapter fence must prevent a stale replacement or issue from restoring access.

**Continuity.** Where the product requires no access gap, the selected license adapter must demonstrate that continuity; its replacement or readiness mechanism belongs to the adapter specification. Entitlements cannot promise issue-before-revoke continuity by sending two asynchronous commands.

Initial usable readiness is mandatory before initial commercial activation under the joint freshness and withdrawal contract; no synchronous gate is added to resume or to active changes, and circular waiting for a post-activation command is prohibited.

**Seat-assignment boundary.** The licensing service owns seat binding and runtime license checks. OSS owns any numerical quota accounting. Entitlements sends accepted grant definitions, committed quantity and validity, then tracks application. It calculates no remaining capacity and resets no counter.

A named grant must not issue the same numerical allowance again. Numerical quota limits use the quota adapter contract. Decreases are outside the current quota profile. Any future seat-reduction guard needs authoritative licensing or OSS evidence, not a cached count in Entitlements.

## 4. States (CDSL)

### Desired grant state

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-state-desired-grant`

Each member of a `DesiredGrantSet` carries one application status: `incomplete`, `derived`, `applied` or `refused`. Dispatch eligibility is separate, `current` or `superseded`, as for [work orders](05-resource-execution.md#work-order-state); the [delivery state](06-reconciliation-recovery.md#delivery-state) read model reports a superseded revision as `withdrawn`.

These are application observations of one desired revision, not license lifecycle states and not subscription statuses. The adapter's own grant state machine is read through receipts and never copied as a status here.

| From | Event / to | Mandatory guard |
| --- | --- | --- |
| none | derivation refused / incomplete | A missing frozen grant definition, an unsupported profile or an inexact quantity refuses the whole set under [grant derivation](#grant-derivation); nothing is dispatched and a finding is opened. |
| none | derivation accepted / derived | Local set, assignment targets and per-assignment outbox entries persisted in one transaction; each changed target has a higher revision for its own source key. |
| derived | receipt `applied` / applied | Receipt matches source key, revision, digest and generation; adapter record identity stored. |
| derived | receipt `refused` / refused | Structured reason stored; finding opened; access is not broadened. |
| incomplete, derived, applied or refused | newer revision accepted / status unchanged, dispatch eligibility `superseded` | The newer set fences this one; fresh dispatch of this revision stops and an `incomplete` set is not derived for dispatch later. |
| refused | authorized corrected request / new derived revision | Keep the saved refusal unchanged. A new attempt uses a new operation and higher source revision under the current commercial authority. It updates only that source-owned logical right and creates no new purchase or duplicate allowance. |
| any status, eligibility `superseded` | later receipt for this revision / status as the receipt proves | Updates only this revision's historical evidence and cleanup obligations; it cannot change access or the current desired head. |

A `pending` or `unknown` receipt keeps the member `derived` and schedules recovery by the same operation identity. An older generation cannot move the current desired head. An authenticated result for the exact older operation updates its own historical evidence and cleanup obligations, without authorizing stale dispatch or restoring access. The read model shows the superseding revision with earlier outcomes retained.

## 5. Definitions of Done

### 5.1 Fenced grant application through the selected adapter

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-dod-license-grants`

The implementation must:

- Derive a `DesiredGrantSet` only from the frozen grant definitions and accepted posture.
- Refuse any member without a definition.
- Implement the closed policy SDK enums and GTS validation above, with Pricing acceptance and Subscriptions member-authority fixtures. Preserve application policies in target digests.
- Persist the set, its generation and outbox entry in one transaction before dispatch.
- Apply it through the selected license adapter with scope, assignment identity, desired revision and generation.
- Store a correlated receipt for each member.
- Apply, read and recover through the plugin operations `apply_license_grant`, `read_license_grant` and `read_operation_result` (ENT-D-19).
- Recover pending and unknown outcomes by the same operation identity without creating another grant.
- Persist and expose profile-declared license outputs under the public scoped read/retrieval contract; enforce required-output readiness, release policy and version fences without leaking protected content.

A delayed issue or resume cannot undo a newer revoke or suspend at the adapter's effect boundary. Preparation grants for an initial activation are issued while `preparing` and reported as readiness observations. No subscription identifier is accepted in or written to any adapter payload; each logical right is keyed by its own assignment within `(namespace, resource tenant)`. Seat binding, counting and leases stay with enforcement and are not read from a local counter.

**Implements:** `cpt-cf-bss-entitlements-flow-grant-application`, `cpt-cf-bss-entitlements-algo-grant-derivation`, `cpt-cf-bss-entitlements-algo-grant-by-posture`, `cpt-cf-bss-entitlements-state-desired-grant`.

When hooks are configured, save an assignment-outcome notice intent in the same transaction as each meaningful outcome change. License and resource handlers also save output-state notice intents with output changes. Reuse the saved bindings and safe payload contract in [feature 07](07-extension-hooks.md). Hook execution is asynchronous and cannot change the source outcome, gate ordinary capability work or delay restriction, revocation or cleanup. No-hook profiles create no deliveries.

## 6. Acceptance Criteria

- [ ] Both output-capable adapters receive the pinned retrieval mode and exact expiry bounds. Stream/link substitution, wrong versions, expired links and excessive lifetime are refused before release. Delayed calls and retries cannot extend authority or persist a bearer link.

- [ ] Definition and binding holder kinds agree. A tenant holder matches the resource tenant; a logical holder has explicit authorized scope and generation. Missing, foreign or physical-resource holders cannot default to another identity (E-AC65).
- [ ] Finite validity requires a valid exclusive end. An explicit unbounded end is accepted only under `unbounded_allowed`. Renewal or provider expiry changes cannot extend the member interval locally.
- [ ] Restriction preserves the assignment and purchased quantity while the adapter confirms suspended access. Only a higher authorized member posture resumes it. Revocation closes that source without affecting unrelated grants, data or payments.
- [ ] Unknown policy modes, extra behavior fields and unsupported adapter combinations fail qualification/acceptance. Historical receipt replay keeps its original policies and target digest. Hiding a downloaded license never proves provider revocation.

- [ ] After a changed grant definition is published, the original receipt still resolves the original capability, quantity semantics, scope, validity and digest. No live-plan fallback.
- [ ] A profile requiring a grant refuses missing, unsupported or mismatched definition evidence. A quota-only receipt cannot satisfy it.
- [ ] Two members with different effective intervals and holders keep the exact Subscriptions values allowed by their frozen bindings. Missing validity cannot become unlimited; a missing holder cannot default to caller, payer or seller. Unsupported or out-of-policy values block new grants and increases.
- [ ] A cancellation revokes the normal service member while an explicitly authorized export member remains granted for its accepted retention interval. Document-wide `revoke` cannot erase that distinction, and absent export authority creates no export right. Replay keeps the same member values and digest.
- [ ] A changed effective interval or member access requires a higher Subscriptions revision. Entitlements never extends validity or restores a restricted member by rereading current plan terms.
- [ ] Replaying one acceptance returns the same price, quota and grant snapshots. Mixed old/new snapshots cannot commit, and a grant-semantic change requires a new accepted revision.

- [ ] A posture with editor and analytics rights plus one quota member produces a local `DesiredGrantSet` with two distinct assignments. Each has its own source, operation, fence and receipt. A license member without a frozen grant definition refuses derivation before dispatch.
- [ ] Increasing editor seats from 10 to 13 keeps its assignment and advances only its source revision. Revoking the five-seat analytics right closes only the analytics assignment. Replaying either operation creates no additional seats or provider records.
- [ ] Identical definitions and holders in distinct purchases or bindings keep separate assignments. A successor after terminal closure has a new assignment, and old requests cannot affect it.
- [ ] A revision 7 issue delivered after a revision 8 revoke for the same assignment and preparation generation is refused or reconciled at the adapter; its late success callback is stored as evidence and does not restore access.
- [ ] Equal revision and digest recover the same set and result after a crash or lost response; no second grant or delivery identity is created. Equal revision with a different digest conflicts without changing the saved target.
- [ ] A lost adapter response is recovered by reading or replaying the same application operation; the adapter holds one grant for the assignment, not two.
- [ ] A definitively refused operation replays the same refusal. An authorized corrected attempt uses a new operation and higher source revision. It keeps the same commercial authority and cannot duplicate a grant.
- [ ] A higher accepted revision prevents older effects even if its own application is later refused. Accepted and fully applied heads remain distinct.
- [ ] Preparation grants for an initial activation exist while the subscription is `preparing` and appear as readiness observations; no grant is issued for the first time after the active commit.
- [ ] Suspend keeps the grant and its restoration identity; a purchase committed before the suspension is reconciled under the newer restricted revision and its late delivery cannot restore access.
- [ ] A requested numerical quota decrease is refused in v1. Grant processing maintains no local occupied-seat counter and cannot bypass a future licensing reduction guard.
- [ ] No adapter payload, receipt or stored set contains a subscription identifier.
- [ ] A profile requires a license document with `retrieval = link` and a lifetime of 300 seconds. The output is `pending` until the adapter reports the content retrievable, then `available`. A retrieval returns a link for that version only and writes an audit record. After the source is revoked the output is `revoked` and no link is issued (ENT-D-23).
- [ ] An output whose metadata holds a property outside the closed list of its kind is quarantined with a finding and is never shown. A second retrieval of one version returns the same content and issues no second license.
- [ ] A lost receipt or content-retrieval response recovers the original license/output version; repeated download never issues another grant. Provider content loss is visible as unavailable, not repaired by an unkeyed create.
- [ ] A BSS consumer sees only permitted metadata and retrieves protected content with current scope/action authority. Knowing another tenant's output ID does not authorize either operation; no key or bearer URL appears in ordinary events, receipts or logs.
- [ ] A grant applied without its required retrievable output cannot satisfy initial readiness; an optional missing output does not block it. Pre-T delivery requires explicit preparation authority and never waits for the active commit when essential access needs it.
- [ ] Rotation/replacement, cancellation, expiry and stale callbacks preserve version fences. Withdrawal or an unresolved activation cannot release a new key without authority; hiding a downloaded key is not reported as provider revocation.

- [ ] Two plan items sharing one Fixed binding derive one grant amount. Fractional PerPlanUnit results, duplicate members and incomplete accepted snapshots refuse the complete set.
- [ ] A reordered set has the same desired digest. A changed boundary or authority under an existing operation ID conflicts. Unknown operation and unknown effects take distinct recovery paths.
- [ ] Resume advances source revision and preserves assignment/generation. Repeated output retrieval returns the same version, rechecks current rights and never issues another license.
