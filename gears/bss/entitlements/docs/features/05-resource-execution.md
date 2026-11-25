---
description: "Service fulfillment intents, assignment identity, confirmed outcomes and recovery through a provider adapter without OSS resource inventory."
---

# Feature: Service Fulfillment

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-featstatus-resource-execution-implemented`
- [ ] `p1` - `cpt-cf-bss-entitlements-feature-resource-execution`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Service work order](#service-work-order)
  - [Work-order SDK contract](#work-order-sdk-contract)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Generation fencing](#generation-fencing)
- [4. States (CDSL)](#4-states-cdsl)
  - [Work order state](#work-order-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Truthful service fulfillment](#51-truthful-service-fulfillment)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->


**Status:** proposed implementation; provider contracts and runtime conformance remain delivery gates.

**Release scope.** V1 includes provision, withdraw, pause, restore, service-stop and their recovery where the service profile requires them. Service dispositions, export, holds, retention scheduling and `delete` are v2. V1 accepts only profiles whose promised lifecycle works without those deferred operations. Preparation withdrawal and safe cleanup remain v1; they are not deferred service-data disposal.

## 1. Feature Context

### 1.1 Overview

Entitlements turns committed posture into intents for a tenant and its logical service assignment. The selected adapter reports service outcomes and confirmed timing. OSS owns the affected resources and execution. A tenant-level OSS API can satisfy the contract without either BSS or the adapter enumerating resources.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-entitlements-fr-resource-execution`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-entitlements-actor-service-adapter` | Applies service intents and reports confirmed assignment-level outcomes. |
| `cpt-cf-bss-entitlements-actor-subscriptions` | Owns commercial intent, the confirmed charging cutoff and disposition eligibility. |
| `cpt-cf-bss-entitlements-actor-platform-operator` | Inspects partial and unknown outcomes and hold protection. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [adapter capability profile](../DESIGN.md#333-adapter-capability-profile).
- **Build prerequisites:** as allocated in [DECOMPOSITION.md](../DECOMPOSITION.md); decisions in [DECISIONS.md](../DECISIONS.md).
- **Provider prerequisites / integration acceptance:** gate E-U05 in [UPSTREAM_REQS.md](../UPSTREAM_REQS.md).

## 2. Actor Flows (CDSL)

### Service work order

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-flow-resource-work-order`

**Actor:** Subscriptions supplies committed intent; the selected service adapter reports execution.

**Success:** the requested service effect is confirmed for its assignment, revision and operation. Actual effective time is retained when known.

**Errors:** unsupported capability or kind, missing authority, conflicting identity, pending hold protection, refusal, partial application, unknown outcome or superseded intent.

The `resource` capability covers service fulfillment. It does not make Entitlements an OSS resource manager. Subscriptions owns commercial intent, cutoff and retention/export or disposal authority. Entitlements owns logical assignments, delivery and recovery. OSS owns resource selection and execution. The adapter design defines native APIs, any internal mapping and recovery steps.

1. [ ] - `p1` - **Derive.** Accept posture under [posture intake](01-posture-intake.md). Select service members through their explicit accepted `serviceBindingRef` under E-U06. Resolve the frozen SKU-to-service-type binding, parameters, policy and exact profile before new fulfillment. Do not infer a service from a SKU name or current deployment defaults. Resolve the logical component to its retained assignment and read the complete `serviceDispositions` snapshot. Compare the authorized service target with saved application results and unresolved operations. Carry outstanding authorized work into the new revision; a disposition-only update cannot drop a pending stop. Reconcile through the saved binding without repeating confirmed effects. - `inst-resource-work-order-1`

| Intent | Kind | Required meaning |
| --- | --- | --- |
| Authorized preparation or new component | `provision` | Make the accepted service available. Initial activation uses preparation authority and readiness; active additions use their committed posture. |
| Confirmed non-activation | `withdraw` | Settle only the closed preparation assignment under its exact resolution. Timeout alone is insufficient. |
| Suspension | `pause` | Restrict service while preserving data and restoration identity. |
| Authorized resume | `restore` | Restore the same logical assignment. No new service purchase or duplicate entitlement. |
| Termination | `service-stop` | Stop service use under the authorized boundary. Actual execution time is distinct from the charging cutoff. |
| Accepted export entitlement (v2) | `export` | Provide the allowed export access or output. OSS owns how it reads and packages data. |
| Eligible, explicitly authorized disposal (v2) | `delete` | Dispose of data held for this service under the accepted terms. This is not a BSS-selected VM or volume deletion. |

A capability marked `not_applicable` requires no work. A missing required adapter leaves fulfillment unresolved. Activation continues the prepared assignment. An infrastructure replacement inside OSS does not create a new BSS generation.

**V2 service dispositions.** `serviceDispositions` identify the logical component, disposition ID/version, accepted retention/export policy and digest, confirmed cutoff where applicable, disposal schedule version/deadline, hold version/state, notice evidence when required and `deleteAuthorized`. They contain no resource IDs or physical generations. Entitlements retains the component-to-assignment association from the accepted commercial source. It saves each disposition against that exact assignment generation before dispatch. Subscriptions supplies the logical component, not an assignment or physical resource generation. Unknown or ambiguous associations block destructive work. A matching name cannot redirect an old disposition to a successor assignment. The adapter must preserve this generation boundary when mapping work to provider execution, including delayed commands and cleanup.

A disposition-only revision updates authority even when access is unchanged. Missing disposition, missing required evidence, elapsed time or `Revoked` alone never authorizes disposal. A hold blocks new destructive dispatch immediately; protection is confirmed only after the provider acknowledges enforcement. Explicit withdrawal preserves unresolved operation history.

2. [ ] - `p1` - **Persist.** Save the service target, assignment generation, source revision/digest, work orders, request digests and delivery intents in one local transaction. Each order has its own operation ID and saved adapter binding. Recheck preparation, source and disposition authority before commit. The adapter receives tenant scope, logical assignment and operation, not a subscription ID or infrastructure inventory. - `inst-resource-work-order-2`
3. [ ] - `p1` - **Submit.** Dispatch outside the transaction under the saved binding. Recheck current authority. Preparation calls require the durable dispatch-start marker under feature 02's closure fence; cleanup retains its separate non-activation authority. Retries keep identity and content. A service profile without proven retention/hold/disposal guarantees cannot enable those operations; no fallback drops the obligation. - `inst-resource-work-order-3`
4. [ ] - `p1` - **Record.** Store the adapter's `pending`, `applied`, `partial`, `refused` or `unknown` result for the exact service operation. Include actual effective time only when confirmed. A stop at 10:05 after a 10:00 cutoff stays 10:05. Dispatch acceptance does not prove application. Return the service-level evidence through the authorized BSS read defined below. Subscriptions and Billing own charging decisions. - `inst-resource-work-order-4`
5. [ ] - `p1` - **Retention and export.** Pass the accepted terms and authority to the adapter. Record confirmed retention, export availability and hold protection at service scope. OSS decides which data and resources implement that service. Data presence, export or delayed disposal alone creates no fee. - `inst-resource-work-order-5`
6. [ ] - `p1` - **Recover.** A timeout remains unknown. Partial application retains the original operation and unresolved obligation. The adapter recovers its internal effects; Entitlements polls or replays the service operation under the same identity. Neither side must expose a per-resource roster to BSS. Missing service state alone does not prove that this operation completed disposal. - `inst-resource-work-order-6`

Only committed posture changes authorize new work. Renewal, scheduled cancellation and grace alone publish no new intent. Known-source restrictions use retained bindings under ENT-D-13. An inactive resource tenant receives only restricting actions and authorized cleanup.

### Work-order SDK contract

Use the shared context and primitives from [posture intake](01-posture-intake.md#sdk-contract-and-validation). These types implement [DESIGN §3.3.6](../DESIGN.md#336-resource-work-orders). Native resources, jobs, locks, sequencing and compensation stay outside this contract.

**Quota association.** A service-specific quota metric may point to this descriptor through its typed `service_type_ref`. [Feature 03](03-quota-projection.md#sdk-ownership-and-conformance) defines that catalog extension. The quota binding and canonical scope still identify its target; the type reference creates no service assignment or extra allowance. Shared metrics use qualified mappings without a single service reference.

**Service identity and accepted binding.** Products has SKUs and immutable `SkuVersion` snapshots, not a separate Service entity. A SKU identifies a commercial position. The adapter publishes the execution service type it understands. A qualified capability profile defines the supported behavior; it is not an implicit substitute for that service type. Pricing freezes the explicit link below in the same acceptance as prices, quotas and grants.

| Contract | Fields and rules |
| --- | --- |
| `ServiceTypeDescriptorV1` | Adapter-owned `descriptorId` as a GTS instance ID, immutable `version`/`digest`, supported capability-profile references/versions/digests, parameter schema reference/version/digest, and the meaning of commercial service units. Register against `gts.cf.bss.entitlements.service_type.v1~`. This descriptor defines execution meaning, not prices or OSS inventory. |
| `ServiceBindingV1` | Schema version, `bindingId`, content digest, Pricing plan/revision, unique selected item IDs and their exact SKU references (catalog tenant, SKU ID, published version and snapshot digest). Include `capability = resource`, exact `serviceTypeRef`, qualified capability-profile reference/version/digest and qualification evidence. Include quantity rule, target rule, fully resolved accepted parameters and execution-policy evidence below. |
| `serviceTypeRef` | Exact descriptor ID/version/digest supplied by the adapter and retained with qualification. A display name, generic `resource` capability or SKU ID cannot replace it. |
| Quantity rule | `Fixed` or `PerPlanUnit`, exact nonnegative integer coefficient, inclusive minimum/maximum and positive step, using decimal strings as in grant rules. Fixed applies once per binding even if several selected items reference it. PerPlanUnit uses the member's committed quantity from Subscriptions; Entitlements does not sum lots. The result is commercial service units, never a VM count or topology. |
| Target rule | `resource_tenant` or `member_scope`, matching the qualified profile. The latter requires explicit authorized logical member scope/generation from Subscriptions. No physical target IDs. |
| Accepted parameters | Exact parameter schema reference/version/digest and complete validated values. Resolve defaults before acceptance. Unknown fields, invalid units or unsupported choices fail validation. No scripts, native resource IDs or credentials. |
| Execution-policy evidence | Fixed slots `access`, `retention`, `export`, `disposal`. Each names immutable typed policy content or a reference with schema/ID/version/digest; non-access slots may explicitly be `not_applicable`. Missing evidence is not that declaration. `not_applicable` authorizes no export or disposal. Actual cutoff, hold, schedule and disposal authority still come from Subscriptions' `serviceDispositions`. |
| `AcceptedServiceSnapshotV1` | Acceptance ID/terms digest, original plan quantity and purchase/plan provenance, tenant axes, complete bindings and retained descriptor/schema/profile/policy content or immutable resolvable references. Include snapshot ID, item count and whole-set digest. These are a branch of the existing Pricing acceptance, not a second purchase receipt. |
| Member `serviceBindingRef` | Proposed E-U06 fields: exact acceptance reference, binding ID and binding digest for the logical component. Subscriptions also supplies committed member quantity, holder scope, access and lifecycle authority. The receipt resolves the binding's plan/item/SKU evidence. These fields are an owner SDK/GTS adoption requirement, not assumed present in today's wire type. |

Products qualification records the exact SKU-to-service-type/profile mapping and validates supported parameter and policy schemas. It retains descriptor evidence supplied by the adapter. Pricing authoring selects from that qualification and validates the complete binding at publication and acceptance. It freezes all resolved execution inputs, including defaults. Commercial bindings and accepted snapshots are typed SDK/GTS records; they add no Product or Service catalog entity.

Entitlements validates the member reference against the complete accepted snapshot and saves the resulting target with its assignment and adapter binding before dispatch. Missing service evidence leaves new fulfillment incomplete, not not-applicable. An empty service branch requires explicit complete accepted evidence consistent with qualification. A known-source restriction or stop uses its retained target and current authority; a missing catalog read cannot prevent those controls.

The adapter request carries `serviceTypeRef`, the exact profile, service units, validated parameters, logical scope, access and the execution-policy content needed for that intent. Purchase IDs, plan/item/SKU references and full Pricing terms stay in BSS. The adapter verifies support for that exact service-type/profile pair; it does not fetch Pricing or infer a service from a commercial code.

For example, a Backup SKU can bind to the adapter's managed-backup descriptor. On provision, the adapter maps that descriptor to its native offering or template ID. It uses the scoped source/assignment identity to create or recover the intended OSS service. On pause, restore or stop, that same identity resolves its saved native association. Mapping storage, native instance IDs and recovery belong to the adapter; BSS requires no inventory or native instance ID.

One binding under one accepted commercial source maps to a logical assignment, not one assignment per service unit. Matching SKU, tenant or service type cannot merge different purchases or bindings. Active quantity changes use the saved quantity rule and unchanged compatible binding. Changing service type, parameters or policy requires new acceptance and an explicitly supported transition. A new profile version or deployment route cannot reinterpret old accepted work. OSS may change its internal implementation while preserving the accepted contract and existing fences.

Feature delivery includes the descriptor, binding and accepted-snapshot SDK/GTS models, validation and replay fixtures. E-U03/E-U04/E-U05/E-U06 jointly gate adoption (E-AC66). The adapter advertises exact supported descriptor/profile pairs; registry advertisement alone does not prove qualification.

| Type | Required fields and rules |
| --- | --- |
| `ServiceWorkOrderV1` | `operation_id`, `request_digest`, source/binding context including resource tenant and `assignmentId`, `assignment_generation`, `source_revision`, `desired_digest`, `kind`, complete logical `service_target`, requested boundary, authority and conditional disposition evidence. No `resource_ref`, physical generation or target resource roster. |
| `service_target` | Exact `serviceTypeRef`, capability and authorized logical scope, derived service units, validated typed parameters, desired service access, qualified profile reference/version/digest and retained execution-policy content/references. Commercial binding and SKU provenance stay local. The profile may target the whole tenant service. It need not identify a provisioned infrastructure object. |
| `ServiceWorkOrderResultV1` | Source, assignment generation, operation/request digest, source revision/desired digest, producer, observation sequence/time, `pending / applied / partial / refused / unknown`, safe reason, recoverable evidence reference and confirmed `actualEffectiveAt` when known. Conditional retention/export and acknowledged hold evidence describe the service scope. Required output descriptors follow the shared contract. |
| `ServiceStateV1` | Source and assignment generation, accepted and fully applied heads, confirmed service fulfillment state, retention/export evidence, current acknowledged hold version/value and unresolved operation references. Unknown evidence is explicit; no inventory or native resource state. |
| `HoldRequestV1` | Shared context, operation/request digest, assignment generation, source revision/desired digest, `hold_policy_version`, `held` and commercial authority. |
| `HoldResultV1` | Echoed identity, `acknowledged / pending / refused / unknown`, confirmed version/value and safe protection evidence when acknowledged. An older result cannot lower the current version. |

`provision` during preparation requires its authorized operation/generation, pinned profile and exposure bounds. `withdraw` requires exact non-activation resolution. `pause` and `restore` require current restricting or restoring authority. `service-stop` carries its authorized boundary. `export` carries accepted export/output authority. `delete` carries the disposition version, accepted terms, current schedule and hold versions, explicit disposal authority and required notice evidence.

Hash the complete logical target for `desired_digest`. Hash each request, including authority and kind, for `request_digest`. Separate service operations may share one desired revision. Changed content under a saved operation conflicts. `read_service_operation` returns `found` with the saved or recovered result, or `not_found`; only an identical replay under current authority is allowed after `not_found`.

`applied` confirms the entire requested service effect. `partial` confirms known incomplete application and retains recovery; the adapter keeps its internal task details. `unknown` means effects cannot be established. A definitive refusal requires no unresolved effects. A late result updates only its original operation and cannot complete a newer target.

**Subscriptions read.** `read_service_fulfillment(orderingTenantId, subscriptionId, postureRevision, cursor)` is a service-only Entitlements SDK read. Authorize the Subscriptions principal for the ordering tenant and guard each resource-tenant scope before reading results. Use the retained intake-to-assignment mapping; callers cannot substitute another assignment. Return the exact posture identity/digest and a stable paged snapshot. Each entry includes the logical component, retained `fulfillmentRef`, assignment/generation and optional disposition reference.

It also includes operation/kind, requested revision, result, known actual effective time, confirmed hold/retention/export evidence and a safe evidence reference. A cursor pins that snapshot; later progress requires a fresh read. Page size defaults to 50 and is capped at 200. No physical-resource details or credentials are returned. Missing or unresolved results cannot become successful empty fulfillment. E-U06 must adopt this read.

Service output descriptors and retrieval use the [shared fulfillment output contract](04-license-grants.md#shared-fulfillment-output-contract). Cancellation revokes the normal service member's outputs with that source. A separately authorized export member has its own output and release policy during retention. Retention does not revive an old service endpoint or credential. An endpoint is bound to its assignment and output version. Output availability and service readiness are separate checks.

## 3. Processes / Business Logic (CDSL)

### Generation fencing

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-algo-generation-fencing`

**Input:** service intent, operation, assignment generation, source revision/digest and current commercial authority. **Output:** submitted, deferred or refused work with recoverable results.

1. [ ] - `p1` - **Bind the assignment.** Resolve the commercial component through the retained association. A new purchase after terminal closure gets a distinct assignment generation. A tenant name, service label or native resource name cannot retarget old work. OSS may replace its infrastructure without changing the logical assignment. - `inst-generation-fencing-1`
2. [ ] - `p1` - **Order.** Recover known operations before stale filtering. Refuse unseen lower revisions and conflicting content. A higher accepted target fences old actions that would restore access or contradict it. Already applied effects remain in history. Terminal service disposal closes the assignment generation; new provision needs new authority and identity. - `inst-generation-fencing-2`
3. [ ] - `p1` - **Preserve suspension.** A pause preserves service data. Restore uses the same logical assignment and cannot override a newer pause or stop. OSS chooses how to implement this guarantee. - `inst-generation-fencing-3`
4. [ ] - `p1` - **Confirm protection.** Entitlements blocks new disposal dispatch when a hold arrives and forwards its service-level authority. Only provider-enforced protection can be acknowledged. The adapter/OSS design owns arbitration with in-flight disposal. If disposal already won, report its actual service result; do not claim that a later hold restored data. Partial or uncertain protection is not full acknowledgment. - `inst-generation-fencing-4`
5. [ ] - `p1` - **Require new disposal authority.** Hold release does not authorize disposal by itself. Subscriptions supplies a new eligible schedule and required notice evidence. Do not reuse an old expired schedule. The adapter must enforce the current authority without exposing its locking mechanism to BSS. - `inst-generation-fencing-5`
6. [ ] - `p1` - **Recover the same operation.** Retry preserves assignment, generation and operation. A lost provision result cannot create a second service. A late stop result remains historical evidence and cannot undo newer access state. - `inst-generation-fencing-6`

Assignment generation identifies the BSS service incarnation. Source revision orders desired state. Disposition and hold versions order commercial authority. None is a physical resource generation.

## 4. States (CDSL)

### Work order state

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-state-work-order`

Execution outcome is `requested`, `applied`, `partial`, `refused` or `unknown`. Dispatch eligibility is separately `current` or `superseded`. A new desired head stops stale dispatch but cannot erase uncertain effects.

| From | Event / to | Mandatory guard |
| --- | --- | --- |
| none | order persisted / requested | Commit target and delivery intent with the saved binding. |
| requested or unknown | confirmed complete effect / applied | Match assignment, generation, operation and request digest. Retain actual time when known. |
| requested or unknown | confirmed incomplete effect / partial | Retain service-level evidence and recovery of the same operation. |
| requested or unknown | definitive refusal / refused | Prove no unresolved effects. Supersession alone is insufficient. |
| partial | remaining work refused / partial | Keep known effects and unresolved service obligation; do not report whole-operation refusal. |
| requested or partial | response lost / unknown | Preserve identity and existing evidence. |
| partial or unknown | recovery / actual outcome | Update only the original operation from authoritative evidence. |
| any outcome | higher revision / same outcome | Mark old dispatch superseded; preserve effects and recovery. |

## 5. Definitions of Done

### 5.1 Truthful service fulfillment

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-dod-resource-execution`

- Derive complete service intent and preserve outstanding authorized effects across revisions.
- Address each request by tenant and logical assignment, with no required resource inventory.
- Retain operation identity, assignment generation, authority and saved adapter binding.
- Record confirmed service outcomes and truthful effective times; keep partial and unknown work recoverable.
- Expose scoped fulfillment evidence to Subscriptions through the agreed SDK read.
- Prove data-preserving suspension, current hold/disposal authority and stale-command protection at the integration boundary. The native mechanisms stay in the adapter/OSS design.

## 6. Acceptance Criteria

- [ ] A SKU binds to an adapter-owned service descriptor and exact profile. The adapter receives that pair and scoped assignment identity, not a SKU as an implicit execution selector (E-AC66).
- [ ] After purchase, SKU publication, descriptor/profile updates and changed defaults leave the saved service type, parameters and policies unchanged. Changed execution terms require new acceptance and a supported transition.
- [ ] Fixed and per-plan-unit bindings derive exact commercial service units without creating a BSS resource roster. Multiple selected items do not duplicate a fixed binding; distinct purchases retain independent assignments.
- [ ] A missing binding, descriptor, parameter schema or policy blocks new fulfillment. Wrong receipt/binding digest and unsupported service-type/profile pairs fail before dispatch. Existing restriction/stop uses retained target evidence.
- [ ] A lost provision response recovers the same scoped assignment/native association through the adapter. The subsequent pause/stop targets that association without BSS knowing its native ID.

- [ ] An OSS API accepts a tenant and service reference, applies suspension and returns a service-operation result. BSS and the adapter complete delivery without enumerating VMs or volumes.
- [ ] OSS replaces infrastructure while the service assignment stays active. BSS needs no new physical generation or resource mapping.
- [ ] A new purchase reuses a service name after the previous assignment closes. An old disposition, delete retry or delayed cleanup remains bound to its original assignment generation. The adapter prevents it from affecting the successor. Missing or ambiguous component associations block destructive dispatch.
- [ ] A disposition-only revision arrives while stop is pending. The new target retains stop intent and recovery; it cannot silently drop or duplicate the effect.
- [ ] A superseded pause/stop with a lost result remains recoverable. Its eventual result cannot replace the newer desired head (E-AC43).
- [ ] A charging cutoff of 10:00 and confirmed service stop at 10:05 remain distinct in the Subscriptions read. Unknown actual time is absent, not copied from requested time.
- [ ] A hold confirmed before disposal protects the service data. If disposal already won, the result reports that fact without claiming restored data. Unknown or partial protection is not acknowledged as complete.
- [ ] A delayed command for closed assignment A cannot affect successor B for the same tenant and service name. No physical resource identity is needed to express this fence.
- [ ] Lost or partial service disposal recovers under the original operation. BSS receives a safe incomplete result, not per-resource tasks or false completion.
- [ ] Missing, stale or incomplete commercial disposal authority dispatches no delete. Releasing a hold cannot revive an old schedule.
- [ ] Endpoint outputs remain bound to their assignment generation. Closing it cannot expose those outputs as belonging to a successor.
- [ ] A service-only fulfillment read is scoped, paged under one snapshot and correlated to the exact posture revision. Another tenant cannot read it, and missing results cannot appear applied.
