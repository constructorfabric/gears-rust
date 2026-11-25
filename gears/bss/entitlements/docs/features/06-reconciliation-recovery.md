---
description: "Outbox replay, observation journal, stale-action fencing, findings, operator repair and adapter or profile changes."
---

# Feature: Reconciliation and Recovery

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-featstatus-reconciliation-recovery-implemented`
- [ ] `p1` - `cpt-cf-bss-entitlements-feature-reconciliation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Reconcile desired and applied state](#reconcile-desired-and-applied-state)
  - [Inspect and repair a mismatch](#inspect-and-repair-a-mismatch)
  - [Public reads and operator commands](#public-reads-and-operator-commands)
  - [OData collection contract](#odata-collection-contract)
  - [Retention and safe compaction](#retention-and-safe-compaction)
  - [Finding events](#finding-events)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Recovery matrix](#recovery-matrix)
- [4. States (CDSL)](#4-states-cdsl)
  - [Delivery state](#delivery-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Versioned, recoverable handoff with visible findings](#51-versioned-recoverable-handoff-with-visible-findings)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->


**Status:** proposed implementation; provider contracts and runtime conformance remain delivery gates.

**Release scope.** Recovery, findings, audit and safe local retention ship in v1 with each enabled capability. Output reads/retrieval, hook execution reads and service-disposition repair are v2. Local evidence-retention holds and readiness commitments remain v1. OData and retention rules apply to each collection when its capability is enabled.

## 1. Feature Context

### 1.1 Overview

Entitlements recovers work through its original operation and adapter binding. It records observations, compares desired and applied state and exposes unresolved findings. Authorized repair respects the current source authority. An older command cannot undo a newer restriction or revocation.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-entitlements-fr-stale-action-fencing`, `cpt-cf-bss-entitlements-fr-reconciliation`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-entitlements-actor-platform-operator` | Requests reconciliation, retries a desired revision, records audited dispositions. |
| `cpt-cf-bss-entitlements-actor-subscriptions` | Reads desired state, last observed state and findings per revision. |
| `cpt-cf-bss-entitlements-actor-quota-adapter` | Exposes accepted and fully applied source heads and original operation outcomes. |
| `cpt-cf-bss-entitlements-actor-license-adapter` | Exposes applied grant evidence for readback. |
| `cpt-cf-bss-entitlements-actor-service-adapter` | Exposes service-operation outcomes per assignment for recovery. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [adapter capability profile](../DESIGN.md#333-adapter-capability-profile).
- **Build prerequisites:** as allocated in [DECOMPOSITION.md](../DECOMPOSITION.md); decisions in [DECISIONS.md](../DECISIONS.md).
- **Provider prerequisites / integration acceptance:** fences demonstrated per adapter under gates E-U01, E-U02 and E-U05 in [UPSTREAM_REQS.md](../UPSTREAM_REQS.md).

## 2. Actor Flows (CDSL)

### Reconcile desired and applied state

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-flow-reconcile`

**Actor:** the Entitlements outbox and reconciliation workers, selected adapters and Subscriptions. Workers use the gear's service principal and authorized tenant roster. Before reading or writing tenant rows, they obtain that tenant's single-target scope (ENT-D-04). Adapters supply application evidence; Subscriptions reads delivery state.

**Success:** every outstanding desired revision is either confirmed applied by correlated evidence, confirmed refused with a reason, or kept open as a durable finding with an unknown or conflicting outcome.

**Errors:** adapter unavailable, readback denied, evidence for another revision or generation, unexplained higher revision at the provider, adapter or profile changed while work is in flight.

Reconciliation is the same procedure for quotas, grants and service work orders; the fence it relies on is the provider-independent boundary of ENT-D-01. Entitlements keeps the desired state and the correlated outcomes; the adapter supplies evidence from its native versions, operation status, authoritative state reads or a combination backed by its durable journal. An adapter's dispatch log alone is not proof of external application. Vendor-specific mappings belong to the adapter specification.

1. [ ] - `p1` - **Replay the outbox.** Outbox replay uses the same request identity and the saved `AdapterBinding` from [DESIGN §3.3.3](../DESIGN.md#333-adapter-capability-profile). Resolve that exact scoped client and verify contract/profile, stable provider target and observation authority; do not run discovery again to choose a replacement. Retry and backoff change neither identity nor content. A worker that dies after the local commit and before dispatch is recovered by the outbox, which sends the identical desired revision. - `inst-reconcile-desired-and-1`
2. [ ] - `p1` - **Journal every observation.** Every provider observation is persisted against the source key, the desired revision it answers and the operation identity, with the producer, its sequence and the observed time. A result for another revision or generation does not move the current head; it is stored as historical evidence. A stale success cannot mark a newer desired head complete. - `inst-reconcile-desired-and-2`
3. [ ] - `p1` - **Compare complete state.** Reconciliation reads the application state through authorized adapter reads: the quota adapter's accepted and fully applied heads, license grant evidence, and resource outcomes per generation. It compares the complete desired set, including tombstones, with the applied state. Repair touches only records owned by this source. - `inst-reconcile-desired-and-3`
4. [ ] - `p1` - **Decide per situation.** The [recovery matrix](#recovery-matrix) names the required action for each situation. An unknown outcome is marked unknown and read back; it is never guessed as success or failure. - `inst-reconcile-desired-and-4`
5. [ ] - `p1` - **Record findings.** Open a durable finding for missing or mismatched application, refusal, an unexplained higher revision or a blocked metric. Unknown outcomes also open a finding when the profile's recovery horizon expires. Escalate to the platform operator (ENT-D-14). Each finding retains desired and observed state, source revision, observation time and reason. Subscriptions reads these through the delivery state; a finding shows pending or failed explicitly instead of implied access. - `inst-reconcile-desired-and-5`
6. [ ] - `p1` - **Keep the fence through change.** When the adapter or the profile changes, in-flight work stays bound to its original adapter and profile. A new adapter never replays old commands into a different provider. The transition needs a separately supported settlement before the new adapter owns the source identity. - `inst-reconcile-desired-and-6`

### Inspect and repair a mismatch

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-flow-repair`

**Actor:** an authorized platform operator; Subscriptions as the owner of commercial validity.

**Success:** a finding is closed on correlated evidence or an explicit audited disposition.

**Errors:** operator not authorized for the scope, successor revision not authorized, repair refused by the provider fence, evidence does not match the finding.

1. [ ] - `p1` - An authorized operator requests reconciliation for a scope, reading desired and observed state with one consistent tenant scope. Authorization is enforced on recovery reads as well as writes. - `inst-inspect-and-repair-1`
2. [ ] - `p1` - A missing or mismatched application produces a durable finding. Recover pending or unknown quota work under its original identity. A saved definitive refusal remains final. After remediation, Entitlements may create a new operation and higher source revision under the same authorized commercial intent. Only Subscriptions changes that commercial intent. A retry creates no new purchase. - `inst-inspect-and-repair-2`
3. [ ] - `p1` - A commercially invalid desired posture is changed in Subscriptions. The operator cannot edit a materialized grant, quota counter or resource through Entitlements; repair runs only through authorized conditional operations at the provider. - `inst-inspect-and-repair-3`
4. [ ] - `p1` - A finding closes only on correlated evidence of application or on an explicit audited disposition with its author and reason. A consumer outage or a newer desired revision cannot silently erase a historical failed handoff; the superseded revision keeps its findings as history. - `inst-inspect-and-repair-4`

### Public reads and operator commands

This section completes the operations listed in DESIGN §3.3.8. Each SDK method takes current `SecurityContext` and one target owner tenant. Assignment/output methods use the resource tenant; finding methods use the subject owner defined below. REST uses the same authorization and domain service; it never exposes the service-only posture or observation methods.

| Operation | Input and result |
| --- | --- |
| Assignment reads | List by optional `fulfillmentRef`, assignment, capability, generation, delivery state or `applicationOperationId`. Return the retained `fulfillmentRef`. Point read by assignment ID. Return identity, pinned profile/binding, desired revision/digest, application evidence, delivery/materialization and cleanup state. No provider credentials or quota usage. |
| Finding reads | Within one owner tenant, list by subject kind, intake receipt, preparation operation/generation, assignment, hook execution, state, reason or source revision; point read by ID. Return finding ID/revision, owner tenant, typed subject, safe correlated references, observation time, reason, state and disposition evidence. The subject contract below covers failures before any assignment or manifest exists. |
| Hook execution reads | Use the safe DTO and filters in [feature 07](07-extension-hooks.md#stored-evidence-and-operational-reads), with `hook_execution_read` and the same tenant/cursor rules. No public execute, retry or rebind action. |
| Output reads | List by optional `fulfillmentRef`, authorized assignment or `applicationOperationId`, plus optional kind and delivery state. Return the retained `fulfillmentRef`. Point read supports exact `outputVersion`; without it return the current version. Use the safe descriptor in feature 04. |
| Audit reads | List within one tenant, filtered by resource, operation, action and time range. Return immutable audit ID, subject/executor, request/operation, resource revisions, reason, outcome and recorded time. |
| `reconcile_assignment` | `operationId`, assignment ID, `expectedDesiredRevision`, `expectedDesiredDigest`, reason. Return operation ID, assignment, observed head and `scheduled / replay / refused`. Schedule recovery atomically with its audit/outbox; never accept caller-supplied provider commands. |
| `acknowledge_finding` | `operationId`, finding ID, `expectedFindingRevision`, reason. Return the persisted finding revision/state and command result. It changes no application outcome. |
| `close_finding` | Same identity/revision fields and a nonempty closure reason, with optional authorized evidence reference. Return the persisted finding revision/state. Records the audited operator disposition and sets `dispositioned`; it closes only the finding, not the underlying application or recovery work. |

**Purchase component correlation.** Subscriptions creates `fulfillmentRef` once for each logical purchased component, before its first posture. It stores the association with the subscription, component and accepted commercial sources. Its authorized component read exposes the reference to the portal. This is an E-U06 SDK/GTS and read-contract extension; the current owner contract is not assumed to carry it.

The reference stays the same through quantity changes, renewal of the retained component, restriction/resume, output replacement and successor preparation generations for that purchase. A new purchase or a permanently removed and replaced component gets a new reference. Never recycle a reference. A renewal may add accepted commercial sources to the same component; each source still requires its own validated purchase evidence.

Entitlements validates the reference against the authenticated ordering tenant, subscription, component and resource tenant. It retains this association and links all derived assignments, generations and output versions to it. Reject an attempt to rebind it to another component, subscription or tenant. A tenant transfer needs a separate owner contract. Keep the association in retained history/tombstones so replay cannot reassign it. A component may have several capability assignments. The reference changes no source key, assignment identity, generation fence, receipt or readiness rule.

The reference stays inside BSS. Do not send it to adapters or hooks, accept it from provider results, or use it as a native key. Entitlements adds it to public assignment/output DTOs from its saved association. Provider output schemas remain unchanged. The service-only fulfillment read includes the same reference from this association.

**Read filters and authorization.** SDK `fulfillmentRef` maps to REST `fulfillment_ref` on the existing assignment and output collections. SDK `applicationOperationId` maps to `application_operation_id` and means an Entitlements adapter-command operation. It never means the posture delivery `operationId` or the Subscriptions activation operation. These are fields inside `$filter`, not separate query parameters. Document the mapping in the public SDK, GTS DTOs and OpenAPI. Filter expressions follow ToolKit OData; combine conditions with `and` when both must match. The authorized tenant constraint always applies to the whole expression, including `or`.

The portal obtains the reference from an authorized Subscriptions component read, then lists Entitlements results in the resource tenant. `assignment_read` or `output_read` is still required there; content needs the separate `output_retrieve` check. Subscription access and knowledge of the reference grant neither permission. This also applies when ordering and resource tenants differ. A foreign or unknown reference exposes no rows or existence information. Pagination binds the reference with the other tenant/filter/order fields.

By default, correlation reads use current assignment/source/generation pointers and current output versions. They do not select the greatest timestamp or mix superseded generations into current results. Historical selections require the existing audit/history authority and retain exact generation/version identity. Closure may leave no current output. An empty list does not mean successful application or readiness, and intake does not invent an output before derivation. Subscriptions remains the source of the component's commercial status.

Feature delivery includes persisted correlation mappings, owner SDK/GTS adoption, safe public DTOs and scoped query fixtures. E-AC69 covers multiple purchases, renewal, successor generations, cross-tenant reads, conflicting associations and replay.

**Finding subjects and ownership.** `gts.cf.bss.entitlements.finding.v1~` requires `ownerTenantId` and a tagged `subject`. The SDK enum and GTS schema allow exactly these variants; fields of another variant are invalid. Resolve ownership from the retained authorized subject, never from a caller-supplied tenant or a provider payload.

| `subject.kind` | Required identity | Owner and examples |
| --- | --- | --- |
| `posture_intake` | `intakeId`, opaque `streamId`, submitted revision/digest | Ordering tenant. Digest conflict, refused delivery or derivation failure before an assignment exists. The exact receipt exists even when no posture was accepted. |
| `preparation_generation` | Activation `operationId`, generation, preparation revision/digest | Ordering tenant. `manifest_unbuildable`, conflicting activation resolution or `resolution_overdue`. A manifest reference is optional and never invented. |
| `assignment` | `assignmentId`, generation and source revision | Resource tenant. Provider refusal, missing application or unresolved provider effects. |
| `hook_execution` | `executionId`, operation and manifest reference when applicable; optional assignment | Resource tenant. Hook-only work does not require an assignment. Existing feature 07 correlation remains unchanged. |

Create or reuse an intake finding with its scoped receipt, using the transactional deduplication key in [feature 01](01-posture-intake.md#sdk-contract-and-validation). Repeated deliveries retain the original finding subject and disposition. Derivation failure outside preparation, before assignment creation, uses the accepted receipt. Create a preparation finding against the retained, validated preparation identity, even before a manifest exists. If the preparation identity is invalid or missing, use the refused intake receipt instead. An unauthenticated or unscoped request cannot choose a finding owner or fabricate a business subject. Scope/authentication failures without a valid journal target follow the existing security error/audit path.

Owner and subject identity are immutable. Creating an assignment later does not move a finding to another tenant or replace its subject. Correlated evidence may resolve it under the existing lifecycle. Findings are diagnostics: their creation or disposition cannot accept a posture, make readiness pass or change the saved activation decision.

**Scoped reads and events.** Keep the existing `/findings` routes and SDK methods. `finding_read`, `finding_acknowledge` and `finding_close` apply to the finding's owner tenant. Operators need those permissions in the ordering tenant for intake/preparation findings and in the resource tenant for assignment/hook findings. Subscriptions may use `finding_read` in its authorized ordering scope; a resource-tenant role grants no ordering-tenant access. Intake inspection permission alone does not grant finding access. These grants remain an E-U08 adoption requirement.

A list selects one owner tenant and uses its scope throughout filtering and pagination; it never merges ordering and resource tenants. Point reads and commands resolve the same owner under current authorization. Unknown and unauthorized IDs return the same result. A subject reference grants no permission to read its underlying record. Links into another tenant require a separate authorized read; do not copy protected payloads into the finding.

Public DTOs and finding events contain the owner tenant, typed safe subject, reason and finding revision/state. They exclude subscription IDs, raw posture, commercial terms and provider payloads. Finding events, audit records and outbox rows use the same owner as the finding. Event consumers must enforce that tenant boundary. Subject and event fields use the same versioned GTS contract; REST DTOs use snake_case.

Subscriptions can correlate intake findings through receipt IDs and preparation findings through the saved operation/generation. Its existing posture-outcome read still returns at most 16 authorized correlated findings; a reference never widens that read's authority. No new application or readiness status is inferred from a finding.

All public lists follow the [OData collection contract](#odata-collection-contract). These are live operational reads, not complete commercial snapshots. An exact accepted set uses its separate snapshot ID/count/digest. The service-only fulfillment snapshot read keeps its fixed revision and snapshot binding; public filters cannot turn it into a discovery endpoint.

Authenticate and authorize before checking command replay. Within `(tenant, action, operationId)`, identical semantic content returns the saved outcome; changed content conflicts. Check expected revision after replay lookup. If current state changed, refuse without scheduling work. A durable result and its audit/outbox share one transaction. Finding acknowledgment/disposition does not edit provider state. Reasons contain 1 to 1024 characters and must contain no secrets.

A definitive refusal at an adapter stays final. Reconcile may create a new operation and higher source revision only under unchanged current commercial authority, with evidence that the refusal was remediated. Pending or unknown effects always recover under their existing operation. No operator method changes accepted terms or skips an unresolved predecessor.

**Errors and HTTP mapping.** Reads return 200. Reconcile returns 202 after work is saved for dispatch, with its correlation identity. Finding commands return 200 with their recorded result. Metadata output reads return 200; retrieval returns 200 with content or the approved link response. Use RFC 9457 `application/problem+json` for malformed input/cursor (400), unauthenticated caller (401), denied action (403), invisible or unknown object (404 with the same shape), replay/revision conflict (409), unsupported semantic profile (422), rate limit (429), unavailable dependency (503) and internal failure (500). Return no provider diagnostics.

A saved refusal or pending application remains a domain result. HTTP success does not mean the provider applied it.

**Audit and retrieval.** Write one audit outcome for each durable operator command. Protected retrieval rechecks current output version, authority and release policy on every call, including retries. Record no bytes, keys or URLs. A retry may obtain the same version through a new authorized link, but cannot replay prior permission. Listing historical output metadata requires audit authority as well as the relevant read scope.

### OData collection contract

Use the [ToolKit OData contract](../../../../../docs/toolkit_unified_system/07_odata_pagination_select_filter.md) for every public collection: posture intakes, assignments, findings, outputs, hook executions and audit. SDK list methods take `SecurityContext`, one owner tenant and `&ODataQuery`, and return `Page<SafeDto>`. REST maps the same page to safe response DTOs. It returns `items` and `page_info` with `next_cursor`, `prev_cursor` and `limit`; no separate page envelope is defined here.

REST accepts `$filter`, `$orderby`, `$select`, `$top` and `$skiptoken`. Reject other query keys with 400, including plain `state`, `fulfillment_ref`, `limit` or `cursor`, and unsupported OData options. Never silently ignore a filter. `$select` can project only the safe public DTO; it cannot expose raw posture, credentials or provider payloads. Point-read version selectors retain their own contract.

Declare typed filter and order fields using ToolKit's OData metadata and wire them through `OperationBuilderODataExt`. Use the same declarations for SDK validation, storage mapping and OpenAPI. Use `page_to_projected_json()` for list projection. Separate order declarations may restrict sortable fields. Do not write another filter parser or pagination model.

| Collection | Allowed `$filter` fields | Allowed `$orderby` fields; default |
| --- | --- | --- |
| Posture intakes | `id`, `stream_id`, `decision`, `accepted_state`, `created_at` | `created_at`, `id`; `created_at asc, id asc` |
| Assignments | `id`, `fulfillment_ref`, `capability`, `generation`, `delivery_state`, `application_operation_id` | `id`, `generation`; `id asc, generation asc` |
| Findings | `id`, `subject_kind`, `intake_id`, `preparation_operation_id`, `generation`, `assignment_id`, `execution_id`, `state`, `reason`, `source_revision`, `observed_at` | `observed_at`, `id`; `observed_at asc, id asc` |
| Outputs | `id`, `fulfillment_ref`, `assignment_id`, `application_operation_id`, `kind`, `delivery_state` | `id`, `output_version`; `id asc, output_version asc` |
| Hook executions | `id`, `operation_id`, `assignment_id`, `point`, `status` | `id`; `id asc` |
| Audit | `id`, `resource_id`, `operation_id`, `action`, `recorded_at` | `recorded_at`, `id`; `recorded_at asc, id asc` |

These are safe read-model fields. Map them to the retained semantic SDK fields without changing their meaning. Finding subject filters project the tagged subject defined above; inapplicable fields are absent/null and cannot match an equality filter for another subject kind. Declare those safe projection fields in the read DTO and OpenAPI. `generation` means the subject's generation, and `source_revision` is present only for subjects that carry it. No filter scans raw payloads or follows a linked record into another tenant.

IDs use their declared UUID or opaque-reference type. State, reason, capability, kind and point use their declared string/GTS representation; validate supported values and operators. Numeric revisions and generations use their existing codecs without conversion through floating-point numbers. Time fields use ToolKit date/time parsing. Unknown fields, unsupported operators or sort keys fail validation. Do not advertise an operator that the field's storage mapping cannot implement.

Use `LimitCfg { default: 50, max: 200 }` in storage as well as the REST boundary. Omitted `$top` uses 50; values above 200 are clamped to 200. Report the effective limit in `page_info.limit`. Zero returns ToolKit's canonical invalid-argument error (REST 400). Validate it on SDK entry too; storage clamping alone is not validation. SDK callers cannot bypass the cap. A deployment may lower the cap; it applies equally to REST and SDK reads.

Append the collection's stable identity to the requested sort to make it total. Versioned historical selections also bind generation or output version where needed; never skip rows with equal primary sort values. Cursors bind the owner tenant, collection, normalized filter, effective order and current/history selection. A continuation restores the saved order. If `$orderby` is also supplied, validate it against that order before the standard extractor discards it. Refuse a foreign cursor or changed filter/order/selection. Apply the authorized tenant scope before filtering and paging; neither query fields nor cursor content grant access.

### Retention and safe compaction

Entitlements owns retention of its local records. This policy never authorizes deletion of OSS service data. Recovery timeouts, event-topic retention and local record retention are separate settings.

**Policy ownership.** The platform operator supplies `RetentionPolicyV1` in deployment configuration, with immutable ID/version/digest and GTS type `gts.cf.bss.entitlements.retention_policy.v1~`. Entitlements validates and applies it. The platform audit/security owner approves the referenced audit policy. Subscriptions and each selected adapter owner qualify any finite replay bound for their boundary; the operator cannot invent one by choosing a TTL.

| Policy field | Required meaning |
| --- | --- |
| `schemaVersion`, `policyId`, `policyVersion`, `policyDigest` | Supported schema and immutable policy identity. Retain the resolved policy with each expiry decision. |
| `operationalDetailsSeconds` | Positive bounded uint32 seconds after local terminal settlement. Applies to completed diagnostic receipts, observations, attempt history and superseded payload details that meet the protection rules below. |
| `closedFindingDetailsSeconds` | Positive bounded uint32 seconds after the finding's last closure. An operator disposition alone does not make its unresolved operation eligible. |
| `auditPolicyRef`, `auditPolicyVersion`, `auditPolicyDigest` | Exact platform policy for audit expiry, protected identity retention and erasure, hold precedence, authorized erasure requests and backup handling. No implicit period or local replacement policy. |
| `replayPolicies[]` | Exact authenticated boundary/namespace and qualified contract version/digest. `unbounded` retains compact fences. `bounded` requires positive `maxDelayedRequestSeconds` and a recoverable retirement proof that all receivers reject the retired namespace before effect-capable work. |

Identity, version and digest fields use the shared SDK/GTS codecs from feature 01. Durations use checked arithmetic and must fit the supported timestamp range. Deployment configuration must supply finite detail periods and the audit policy; there are no hidden defaults. Unqualified or absent replay bounds mean `unbounded`, never zero. Configuration publication validates these fields before enabling the retention worker. A runtime policy lookup failure stops deletion, not fulfillment or recovery. New deployments must provide the policy; this document does not choose a universal audit period.

| Record class | Expiry and required survivor |
| --- | --- |
| Current desired state, active bindings, manifests, commitments and outputs | Keep while needed by current rights, service reads or linked obligations. Payload age alone makes nothing eligible. |
| Pending, partial, unknown or quarantined effects; open cleanup | Keep the complete recovery record, original request, bindings and evidence until authoritative settlement. Operator acknowledgment and exhausted retries do not start expiry. |
| Completed operational details | After the configured period, compact only unreferenced details. Keep operation identity and namespace, request digest, exact recorded decision/result, source revision/digest and the evidence needed to validate that result. |
| Closed-finding details | After the configured period and release of evidence dependencies, compact diagnostic history. Keep finding identity, immutable owner/subject, revision, final state and closure/disposition proof. Preserve its incident deduplication key while repeat intake remains admissible. |
| Revision heads, closure fences, tombstones, operation/event deduplication and hook budgets | Retain compact protection while the namespace can receive valid old requests. Under a qualified bounded policy, eligibility starts at proven retirement plus the largest applicable delayed-request bound. Keep a durable retirement marker and never reuse that namespace. Without proof, retain the protection. |
| Audit | Expire from `recordedAt` under the referenced audit policy, extended by applicable holds. Identity erasure follows that policy independently of operational-detail expiry. |
| Delivered outbox rows | Remove after the durable journal owns recovery. Undelivered work is protected until delivered or durably cancelled by the existing authority/fence. Event Broker owns topic retention. |

**Retained proof.** Compaction must preserve the actual evidence required to validate a result, not just a digest of deleted evidence. Keep the full shared readiness result and manifest roster, activation resolution, settlement proof and service-fulfillment snapshots wherever the existing read contracts require them. Preserve source-to-assignment and output-version associations. Hook records retain original keys, budgets and the evidence supporting a retained readiness decision. Old duplicate calls return the saved result or fail against a retired namespace; they never dispatch again. Conflicting reuse of an operation ID still conflicts after compaction.

Operational-detail expiry cannot change an existing SDK result to `not_found`. Authorized diagnostic reads return the retained summary and `detailAvailability = retained / purged` (`detail_availability` in REST). This is metadata, not a new application, readiness or finding status. A service read that requires complete evidence prevents that evidence from being purged. Referenced evidence remains protected until its last obligation is released.

**Clocks, holds and transactions.** Save `terminalSettledAt` when effects and cleanup are authoritatively settled, or when durable no-dispatch proof establishes that no effects were possible. Use local time; provider timestamps cannot backdate eligibility. Save finding `closedAt` separately. Record `eligibleAfter`, the resolved policy and applicable hold/retention constraints. Linked commercial or financial evidence may extend retention; unresolved constraints block deletion.

The worker processes limited batches under the record owner's single-target scope. Before deletion, recheck terminal state, current references, policy version, hold version and retirement proof. Save the compact proof and remove details in that same transaction. A hold is acknowledged only after its local protection is durable. A remote policy owner must supply a qualified version/fencing contract before automatic deletion can rely on its hold state.

Concurrent recovery, new evidence references and holds use the same local fence; a stale worker cannot delete protected evidence. Do not hold a database lock during external policy/proof lookup. Defer deletion while external constraints are unknown. Shortening a retention policy requires an explicit versioned rollout. It cannot shorten protected obligations. Restart resumes from saved eligibility and proof, without duplicate effects.

**Audit and personal data.** Retained audit action/outcome fields are immutable to ordinary callers. Store actor identity attributes in a separately protected block or reference, with only the minimum pseudonymous reference in the action core. Do not copy names, emails or raw user input into reason text. Pseudonymous identifiers are still sensitive; removing a lookup alone is not proof of erasure.

The platform audit policy defines which identity fields or entire records may be erased, who authorizes it and how holds affect it. Entitlements applies that policy. It removes approved identity data and mappings, invalidates their caches and search projections, and records a minimal erasure outcome without copying erased values. The platform storage owner handles replicas and backup expiry; restores must reapply recorded erasures before serving data.

Retention/erasure records themselves follow the audit policy. If erasure is held or incomplete, report that state rather than claim completion. Erasure cannot remove business fences needed to reject stale work or authorize external cleanup. E-U08 owns this integration and proof; no new public erase or policy-management API is introduced here.

### Finding events

Finding events announce a saved diagnostic change. They do not command an operator, settle provider effects or change commercial state. Consumers read the finding with their own permission when they need details.

| Item | Contract |
| --- | --- |
| Open event | `gts.cf.core.events.event.v1~cf.bss.entitlements.event.v1~cf.bss.entitlements.finding_opened.v1~` |
| Close event | `gts.cf.core.events.event.v1~cf.bss.entitlements.event.v1~cf.bss.entitlements.finding_closed.v1~` |
| Topic | `gts.cf.core.events.topic.v1~cf.bss.entitlements.findings.v1`; owned and registered by Entitlements. |
| Subject and routing | `subject` is the canonical finding UUID; `subject_type = gts.cf.bss.entitlements.finding.v1~`. Each final event type declares `topic`, `allowed_subject_types` with this subject type, and `partition_key = /subject`. |
| Envelope | Platform event envelope; `source = bss-entitlements`, `tenant_id = ownerTenantId`, a durable UUID `id` and the local change time in `occurred_at`. Retry the saved envelope and payload. Broker sequence is not the finding revision. |
| Required `data` | Exactly `ownerTenantId`, `findingId`, `findingRevision`, `subject`, `reason` and `state`. Copy the finding's typed subject and safe fields defined above, with the same SDK/GTS codecs and bounds. `subject` here is the tagged business subject; the envelope subject identifies the finding itself. No raw posture, subscription ID or provider payload. |
| Retention | Topic default `P7D`, overridable through Event Broker configuration. Retained findings and authorized list/point reads provide recovery after notification loss or expiry. This event stream is not the audit store. |

Register the abstract event family, final schemas and topic in the Entitlements GTS inventory. Concrete payload schemas close `data` to the fields above. Save an opened event with creation of a finding, and a closed event when evidence resolves it or an operator dispositions it. Acknowledgment alone does not close it. Repeated intake failures follow the existing deduplication rule and emit no new opened event. The finding change and its outbox entry share one transaction in the finding owner's scope.

Publish in background work with tenant-scoped authority. A missing broker leaves publication pending; it does not undo the finding or block finding reads. Consumers need broker consume authority for that tenant and separate permission to read the finding. Deduplicate by event ID and finding revision; validate the owner and immutable subject before correlating. Out-of-order notifications cannot overwrite a newer finding. Consumers that maintain a view recover through authorized reads after cursor loss or retention expiry. Event publication failures are delivery diagnostics; do not recursively create finding events for the same broker failure.

## 3. Processes / Business Logic (CDSL)

### Recovery matrix

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-algo-recovery-matrix`

**Input:** one outstanding desired revision, its journal of observations and the adapter readback. **Output:** the next delivery state of that revision and, where required, a finding.

**Versioned recovery policy.** `RecoveryPolicyV1`, registered as `gts.cf.bss.entitlements.recovery_policy.v1~`, defines Entitlements scheduling at the adapter boundary. It does not configure retries inside OSS. Every qualified capability profile contains this policy and the `readinessFailureClasses` map defined in feature 02. The profile reference/version/digest covers both; plugin metadata advertises that exact profile, not mutable retry overrides.

| Policy field | Type and rule |
| --- | --- |
| `schemaVersion` | Explicit supported schema version. Unknown versions or missing fields cannot qualify a profile. |
| `callTimeoutSeconds` | Positive integer; bound for one SDK call. Timeout means an uncertain response, never proof of no effect. |
| `initialRetryDelaySeconds`, `maxRetryDelaySeconds` | Positive integers; maximum is at least initial. Bound backoff for repeated delivery of the same operation. |
| `maxFastDeliveryAttempts` | Positive integer, including the first delivery. Counts durably claimed call attempts, not provider effects. Exhaustion switches to escalated recovery; it does not create another operation. |
| `resultPollIntervalSeconds` | Positive integer; minimum interval between result reads while pending or unknown. Reads do not spend the delivery-attempt counter. |
| `recoveryHorizonSeconds` | Positive integer, at least `callTimeoutSeconds`. Age after which unresolved work raises a finding and uses the slower cadence. This is neither an operation deadline nor a retention limit. |
| `escalatedRetryIntervalSeconds` | Positive integer, at least the maximum retry delay and result-poll interval. Minimum interval between background recovery calls after escalation. |
| `remediableReasons` | Explicit set of registered application-refusal reason IDs supported by the pinned capability profile. Empty disables automatic replacement attempts. Readiness failure classification alone grants no retry permission. |
| `maxAutomaticRemediationAttempts` | Nonnegative integer; maximum replacement operations after the initial operation, shared by one recovery chain. Zero disables automatic replacements. |
| `remediationDelaySeconds` | Positive integer; minimum delay after a confirmed refusal before an eligible replacement may be scheduled. Remediation evidence and current authority are still required. |

All numeric fields are bounded unsigned 32-bit integers in the SDK/GTS schema; duration arithmetic must be checked. Values must also fit the runtime's supported timer range. No omitted, infinite or implicit default budget is allowed. An empty reason set requires a zero automatic replacement limit and conversely. Qualification selects and tests the actual values for the service; this document supplies no universal production timeout.

**Before adapter selection.** Deployment configuration supplies a mandatory `coreRecoveryPolicy` with immutable reference/version/digest and the same schema. It has no remediable reasons and a zero replacement limit. Pin it when a valid intake schedules derivation, and retain it on the preparation generation for Subscriptions resolution reads. It covers missing accepted evidence or unresolved profile lookup before an adapter policy is available. A missing core policy fails configuration validation, not individual purchase recovery. It cannot replace a missing capability policy or make an unsupported adapter eligible.

**Clocks and counters.** Save the exact policy, `recoveryStartedAt`, `nextAttemptAt`, delivery-attempt counter, last read/call time and escalation state with durable work. Start age when that work is first committed, including a queued operation that has not dispatched. A retry, successful pending response, worker restart or live profile edit never resets age. Compute the next delivery delay with capped exponential backoff from the initial delay; use jitter between half and the full delay, rounded up to at least one second. Persist the selected time. Read intervals and the escalated interval are minimums, not readiness promises.

Claim each call under the existing worker lease and operation fence, and advance its counter/time before it can leave the process. A crash may consume a call slot without sending; it cannot produce an unlimited fast retry loop. Use one outstanding call per operation. Before retrying a call that may have had effects, read its original result. Resend the original request only when the adapter's existing idempotency/fencing contract permits it. No missing receipt, read timeout or elapsed budget authorizes a replacement operation.

When the horizon or fast-delivery limit is reached, open the corresponding recovery finding once and keep the original operation at the escalated cadence. `unknown_outcome` describes uncertain effects; never use it to claim that a proven unstarted operation ran. Pending or known-unstarted work uses `recovery_exhausted` with its correlated evidence. Recovery continues until authoritative resolution or an explicit existing lifecycle fence determines which work remains authorized. Escalation never clears rights, releases preparation effects or abandons cleanup.

**New attempts after refusal.** A saved refusal is final for that operation. Automatic replacement requires a listed reason, correlated proof of remediation, current unchanged commercial authority and remaining replacement budget. Commit the new operation, higher source revision, parent/root operation references, incremented chain counter and outbox together. The root identifies the initial attempt at the same authorized target; generated revisions and operation IDs do not create fresh budgets. Replayed postures or unrelated metadata changes do not reset the chain. A genuinely changed authorized desired target starts its own work and must still respect unresolved predecessor fences.

An operator can request an otherwise permitted retry after the automatic limit through the existing audited reconcile command. This does not reset the automatic counter or override commercial, settlement or security guards. Reasons outside the set require that operator path; a wrong commercial posture requires correction by Subscriptions. Pending, partial or unknown effects recover under their original identity, regardless of the replacement budget.

Before a manifest can be built, preparation resolution monitoring uses the pinned core horizon. Resolve every applicable capability policy, including optional capabilities. Set `resolutionRecoveryHorizonSeconds` to the maximum of their horizons and the core horizon. Save this value before any preparation effects can dispatch. An empty manifest uses the core value.

A missing profile blocks dispatch. Never substitute a zero horizon. Retain the resolved policies and horizon through retries and configuration changes. Use this saved horizon with fresh authorized Subscriptions operation progress for `resolution_overdue`, as defined in feature 02. Suppress deadline findings while paused or while progress is unknown. After unpause, use the owner's current deadline. Keep dependency recovery and held-age monitoring active. A diagnostic grants no closure authority. Hook invocation budgets remain the separate contract in feature 07.

The policy, timers and counters belong to the durable work and survive restart, rollback and binding changes. E-U01/E-U02/E-U05 qualification proves original-operation recovery and remediation; E-U06 proves resolution recovery. Feature delivery includes SDK/GTS validation and deterministic clock/race fixtures (E-AC64).


| Recovery situation | Required action |
| --- | --- |
| Remote apply succeeds; local response or write times out | Mark unknown, read the same source key and operation, verify revision and digest and recover the receipt; replay only the same idempotent apply. Do not issue another unkeyed create. |
| License/output response lost, or protected content unavailable | Recover the original operation and output version through the saved binding under [fulfillment outputs](../DESIGN.md#337-fulfillment-outputs). Preserve delivery fences and historical evidence; report missing content and never create another license to satisfy retrieval. |
| Quota result is known partial or uncertain | Keep known partial application `pending`; use `unknown` when a required effect is uncertain. The adapter owns internal sequencing and compensation. Neither retry exhaustion nor a newer desired head proves refusal. |
| Quota operation has a definitive refusal | Replay returns that refusal. A corrected request needs a new operation and a higher source revision under unchanged authorized commercial provenance. |
| Local commit succeeds; worker dies before dispatch | Recover the outbox and send the identical desired revision. |
| Preparation effects or commitment outstanding; activation outcome missing or unavailable | Recover the immutable Subscriptions decision through its authorized operation read, using `resolutionRef` or saved preparation identity; redelivered posture is another input to the same resolution fence. Match authenticated operation/preparation revision and digest; applied requires full manifest/commitment matching, while non-activation may precede either and validates any supplied bindings. Retain preparation and escalate while unknown. Report failure and deadline evidence through aggregate readiness. Subscriptions decides closure and resends its resolution through posture. Entitlements sends no separate closure command and cancels or refunds no purchase. No timeout cleanup or successor while predecessor effects are unsafe. Applied settles the original generation and continues assignments only where the current desired head permits them; it cannot undo a later restriction or cancellation. Proved irreversible non-activation permits authorized cleanup. |
| Delayed old request arrives after revoke | The provider rejects it against the retained tombstone and source head. Local callback filtering is an additional check, not the primary fence. |
| Provider reports a higher accepted revision | Stop effect-capable dispatch for that source and open a conflict finding. Recover through the saved binding and verified original history as specified below. Do not overwrite the provider head or infer application from acceptance. |
| Record missing, drift or operator edit | Use authoritative source ownership and readback. Repair only through authorized conditional operations; report conflict where another owner or unknown state exists. Never create a parallel record or overwrite an operator record blindly. |
| Metric registration or enforcement mode drifts | Report a blocked update or unknown enforcement finding, retain the commercial obligation and obtain owner remediation. A materialized but inert record never marks the purchased hard-limit profile ready; no metric is substituted silently. |
| Service outcome partial or unknown, including after supersession | Record the service-operation result for its assignment generation; the adapter owns recovery of internal resources. Stop stale dispatch separately and keep unresolved effects open. A newer desired head cannot turn unknown or partial execution into refusal. Recover the old operation's actual result without changing the current desired head; timeout is not completion or absence. |
| Hook start or result is lost | Feature 07 recovers the original invocation and exact binding. Never replace an unknown start. Keep hook findings separate from adapter application outcomes; hook-only findings can name the manifest and operation without an assignment. |
| Adapter or profile changes | Keep in-flight work bound to the original adapter and profile. Require a separately supported transition and settlement; a new adapter never replays old commands into a different provider. |
| Plugin removed, or registry instance exists without a client | Retain the binding and outstanding work, report dependency unavailability and recover through the same binding when restored; do not try a lower-priority provider. |
| Same plugin GTS ID points at a different target/namespace or incompatible contract | Block dispatch and record a binding conflict; neither matching ID nor new metadata authorizes effect replay. Keep unknown outcomes open against the old target. |

**Provider head ahead of local history.** Keep read-only recovery on the saved binding while all new effects for that source are blocked, including restrict/revoke. Read the provider's accepted/applied heads and recover known operation results. Match every recovered record to the original authenticated scope, source, generation, binding, operation ID, revision, digest and commercial authority. Provider numbers alone cannot reconstruct a missing desired payload or authorize a new revision.

Restore missing local records only from verified original retained history or an authorized data-recovery procedure. Each owning gear restores its own records; Entitlements cannot repair another gear's database. Preserve operation identities, fences, tombstones and unresolved effects. Acceptance alone cannot prove application. Restore each operation's recorded pending, applied, refused or unknown result from correlated evidence. A confirmed refusal remains failed; missing or uncertain outcome evidence stays pending in the materialization view.

Resume through the existing conditional reconcile flow only after local and provider history agree, the current authorized posture is known, and the saved binding/scope is still valid. Recheck that evidence before dispatch. If the history cannot be verified, keep the source blocked and escalate for data recovery. Do not copy the provider's number into the local head, invent a higher revision, replace the source, or use finding disposition to bypass the block. Independent OSS emergency security controls remain under their own authority; they are not an Entitlements repair command.

Apply the [retention and compaction contract](#retention-and-safe-compaction) to local journals. Keep terminal closures and monotonic heads until stale senders are fenced by qualified retirement. Detail expiry cannot reopen a revoked source or turn a known operation into a new one. OSS owns runtime counter retention. A recovery horizon controls escalation, not deletion.

All four subject kinds use the existing acknowledgment/disposition flow within their owner tenant. Hook findings keep the typed subject `hook_execution`, with operation, manifest when applicable and execution ID; assignment is optional. Adapter findings keep their assignment subject. A hook finding does not change the per-revision posture outcome. Additional readiness evidence affects only the readiness fold. Core recovery can ship without the hook plugin; feature 07 owns its invocation procedures and integration.

## 4. States (CDSL)

### Delivery state

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-state-delivery`

A desired revision has a delivery state `pending`, `acknowledged`, `failed` or `withdrawn` and an observed materialization `pending`, `applied` or `failed`. `withdrawn` is terminal for fresh dispatch of that revision: a cancellation or newer desired revision prevents further stale commands, but does not prove that work already in flight never applied.

Retain unknown and partial effects until authoritative recovery establishes their outcome. Late observations update the original operation and its cleanup evidence; they cannot activate, restore access or replace the current desired head.

Claim non-application only when the provider fence or authoritative outcome proves it. These are operation observations, not subscription statuses and not a replacement for the selected adapter's native state machine. A later desired revision supersedes the target but retains earlier outcomes. A failed handoff never silently broadens commercial access.

| From | Event / to | Mandatory guard |
| --- | --- | --- |
| pending delivery | adapter accepted the dispatch / acknowledged | Acceptance of dispatch is not application; materialization stays `pending`. |
| acknowledged | correlated evidence for the same revision and digest / applied materialization | Evidence carries the same source key, revision and digest; a dispatch log is not evidence. |
| pending or acknowledged | definitive refusal or conflict / failed | Recorded with a structured reason and a finding; distinct from unknown. |
| pending, acknowledged or failed | newer revision or cancellation stops fresh dispatch / withdrawn | Preserve outstanding effects and recover their exact original outcomes. Withdrawal alone does not prove non-application; the newer desired head remains authoritative. |
| applied materialization | higher revision applied / materialization remains applied | Mark the old desired revision superseded separately; retain its observed outcome. The newer revision owns the desired head. |

The capability features keep their own application status and receipts; this read model is derived from them as follows.

| Capability status or receipt | Delivery state | Observed materialization |
| --- | --- | --- |
| `incomplete` (temporary derivation dependency or unresolved evidence) | `pending` | `pending`, with the reason and a finding when required by recovery policy |
| Definitive derivation `refused` | `failed` | `failed`, with the structured refusal reason and a finding; no dispatch |
| `derived` or `requested`, dispatch not yet accepted | `pending` | `pending` |
| Adapter accepted the dispatch; receipt `pending` | `acknowledged` | `pending` |
| Receipt `unknown`, or a lost response | `pending` or `acknowledged`, as reached | `pending`, with reason `unknown` and a finding |
| Resource outcome `partial` | `acknowledged` | `pending`, with service-level partial evidence and a finding |
| Receipt `applied` for the same revision and digest, without an open fulfillment discrepancy | `acknowledged` | `applied` |
| Stored applied receipt with an open fulfillment discrepancy | `acknowledged` | `pending`, retaining the receipt, discrepancy and finding |
| Receipt `refused`, or a conflict | `failed` | `failed`, with the structured reason and a finding |
| Dispatch eligibility `superseded` | `withdrawn` | unchanged; the recorded outcome of the old revision is retained |

This delivery view is separate from initial readiness. [Feature 02](02-initial-readiness.md#readiness-state) maps required refusals to retryable or definitive readiness failure through the pinned profile. Pending or unknown effects stay pending; a delivery failure alone does not authorize closure or cleanup.

A definitive derivation refusal contributes `failed` to the affected required capability and posture outcome, even before an assignment exists. It references the scoped intake/preparation finding. Missing evidence or dependency unavailability stays `pending`; recovery exhaustion cannot turn it into a refusal. Repair follows ENT-D-13 under the current posture fence.

The adapter result keeps `refused` and `unknown` distinct. This delivery view maps refusal to failed materialization and uncertainty to pending materialization. It retains the original result, reason and finding.

A finding is `open`, then `acknowledged`, then `resolved` by correlated evidence or `dispositioned` by an audited operator decision. A disposition closes only the finding. It does not make an operation applied, settle or quarantine a cleanup obligation, change purchased rights or settle OSS accounting (ENT-D-14). Preparation recovery uses the pinned adapter's settlement read and the proof rules in [feature 02](02-initial-readiness.md#readiness-sdk-results). Reconciliation can obtain evidence; it cannot fabricate it. When the head of a provider is above the local head, dispatch for that source stops and nothing is overwritten (ENT-D-13).

A lifecycle event without a posture revision changes nothing here, and an inactive resource tenant receives only restricting commands (ENT-D-16). A reconcile, an acknowledgement and a disposition each write one audit record with their outcome (ENT-D-20).

The posture outcome that Subscriptions observes for one revision is the fold of ENT-D-06 over this read model: `applied` when every required capability of the revision has materialization `applied`, `failed` when at least one required capability is `failed`, otherwise `pending`. Optional failures remain visible but do not change this fold. An explicitly empty required set is applied only after complete derivation proves it is empty.

An unknown outcome leaves delivery `pending` before dispatch acknowledgement or `acknowledged` afterwards, unless fresh dispatch has been `withdrawn`. Materialization remains `pending` with service-level partial/unknown evidence and a finding until readback resolves it. An older revision cannot move the current desired head; authenticated results update only their exact historical operation and cleanup obligations. Each state is readable by source key, operation and revision after restart.

## 5. Definitions of Done

### 5.1 Versioned, recoverable handoff with visible findings

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-dod-reconciliation`

The implementation must:

- Replay the outbox with the original identity and saved routing.
- Journal every observation against its revision and operation.
- Compare complete desired and applied state through authorized adapter readback.
- Apply the recovery matrix without guessing an unknown outcome.
- Record each finding with desired state, last observed state, source revision, observed time and reason.
- Allow authorized operator repair only through conditional operations at the provider. Acknowledgement, applied revision and findings are recorded separately from commercial status.

The stale-action fence is proved end to end for every capability: an older issue, replace, restore or provision cannot undo a newer revoke, restrict or stop at any adapter, including reordered deliveries with different request keys and work already in flight (ENT-D-01). In-flight work stays on its original adapter and profile through a change.

Accept each adapter contract and the joint failure and continuity fixture before it is enabled; no local point-of-use check, counter or reset job is reintroduced.

**Implements:** `cpt-cf-bss-entitlements-flow-reconcile`, `cpt-cf-bss-entitlements-flow-repair`, `cpt-cf-bss-entitlements-algo-recovery-matrix`, `cpt-cf-bss-entitlements-state-delivery`.

## 6. Acceptance Criteria

- [ ] Completed detail expiry retains exact replay/conflict results, high-water marks, tombstones and linked readiness/settlement evidence. Late requests after compaction neither reopen rights nor execute twice (E-AC79).
- [ ] Unknown effects, open cleanup, active rights, retained snapshots and unresolved retention constraints prevent deletion even after a timeout or operator disposition. Compact findings retain their incident deduplication key.
- [ ] Race deletion with a new evidence reference, hold, policy update and recovery. Recheck under the same fence; restart keeps protection and the original eligibility time. A missing policy stops deletion only.
- [ ] A qualified retired namespace waits its full delayed-request bound, rejects old calls at every receiver and is never reused. Without qualification, retain compact fences indefinitely. Namespace retirement is not inferred from silence.
- [ ] Audit expiry and approved identity erasure follow the platform policy. Held erasure stays pending; caches and restore handling do not resurrect erased identity data. Record no erased values in the erasure outcome. Business fences survive (E-AC79).

- [ ] Every public list uses `ODataQuery` and returns the same `Page<T>` shape through SDK and REST. OpenAPI declares the supported fields and query options; there is no custom cursor envelope (E-AC78).
- [ ] Test declared filters and ordering on every collection, including purchase correlation, typed finding subjects, hook-only executions and time ranges. Combined `or` filters remain inside the authorized tenant scope. Wrong fields, operators and plain query parameters fail instead of returning an unfiltered page.
- [ ] Omitted `$top` returns the default limit, values above 200 are capped, zero fails, and direct SDK calls enforce the same bounds. Equal sort values across page boundaries lose or duplicate no unchanged rows. Changed filters, order, selection mode or tenant invalidate the cursor.
- [ ] `$select` returns only declared safe DTO fields. REST, SDK and OpenAPI agree on filter types, field names and page metadata. Projection cannot expose hidden fields or widen linked-record permissions.

- [ ] Finding creation and closure commit with exactly one saved notification intent. Rollback, duplicate intake, broker outage and producer restart preserve finding state and event identity. Duplicate or reordered events cannot overwrite a newer finding. Retention expiry recovers through scoped reads; event access grants no linked-record access.

- [ ] A higher provider head blocks effects until verified original history restores the matching local head and current authority. Accepted is not applied. Missing history, a fabricated revision or finding disposition cannot unblock the source.
- [ ] Definitive derivation refusal before assignment creation reports failed delivery and a failed posture outcome for a required capability. Temporary missing evidence remains pending, including after recovery escalation. The scoped finding never requires a fabricated assignment.

- [ ] Missing or invalid policy values fail qualification; changed profile configuration cannot alter an outstanding operation's saved policy, counters or elapsed age (E-AC64).
- [ ] A response is lost after the provider applies work. Delivery-budget or horizon exhaustion raises a finding and slows recovery of the original operation; no replacement grant or service is created. A crash before a call cannot reset the fast-attempt counter.
- [ ] A registered remediable refusal permits a replacement only after remediation evidence and within the chain budget. Replays and generated source revisions do not reset it. Unknown effects and unlisted reasons never trigger automatic replacement.
- [ ] Core recovery continues before profile lookup succeeds. Empty-manifest resolution uses the pinned core horizon; optional capability effects contribute their saved horizons. No missing profile is treated as a zero horizon.

- [ ] A digest conflict or refused delivery before assignment creation produces an ordering-tenant finding for the exact intake receipt. No assignment is fabricated (E-AC63).
- [ ] Manifest derivation failure or resolution conflict/overdue work produces a preparation-generation finding in the ordering tenant, even without a manifest. Invalid preparation identity instead uses its refused intake receipt.
- [ ] With different ordering and resource tenants, each finding is listed, read and changed only under its owner's permissions. A foreign cursor, guessed ID or linked subject cannot widen access. Events and audit remain in that same tenant.
- [ ] Creating an assignment later leaves the original finding owner and subject unchanged. Assignment and hook findings stay in the resource tenant. Public reads and events expose no subscription ID or raw posture.

- [ ] An independent provider ceiling changes without a commercial revision: recover its ordered actual restriction/evidence, retain the purchased desired cap, and expose the discrepancy. A successful source-state apply cannot clear it or authorize overwriting operator policy; clear only from authoritative satisfied-enforcement evidence or an explicit commercial change (E-AC39).

- [ ] A crash after the local commit and before dispatch recovers the same adapter command from the outbox without losing the desired revision or duplicating materialization.
- [ ] Delivering issue request N after revoke request N+1 with different command keys cannot restore access; the adapter integration recovers evidence that the newer revision is applied without requiring a vendor-native revision field.
- [ ] A newer desired revision withdraws dispatch while an older license or resource result is unknown or partial. Recovery remains open; a later exact result updates the old operation and cleanup evidence without changing the current desired head.
- [ ] A delayed old result cannot restore the old plan after a newer replacement or cancellation; it is journaled as historical evidence for the superseded revision.
- [ ] The read surface distinguishes accepted at intake, delivery pending, materialization failed and observed success without asserting point-of-use access.
- [ ] A numerical quota decrease is refused in v1. Recovery cannot disguise it as another source, a reset or a change to accepted terms.
- [ ] No local point-of-use check endpoint, quota reset job or balance logic is required by this feature.
- [ ] An unavailable required adapter yields an explicit gated capability and a visible finding; a product profile that does not require that capability reports not applicable without fabricated application success.
- [ ] An adapter or profile change keeps in-flight work on the original adapter; the new adapter receives no replay of old commands, and the source identity moves only after a supported settlement.

- [ ] After a provider applies a command but its reply is lost, plugin removal or a priority change never sends the command to a second provider. Recovery uses the original binding and source identity.
- [ ] Reusing a plugin GTS ID for a different external target fails binding verification before dispatch; compatible credential rotation to the same authorized target does not create another assignment or source fence.

- [ ] An optional capability refusal stays visible while all required capabilities yield an applied posture outcome.
- [ ] Operator commands replay before expected-revision checks, after current authorization. Changed content conflicts and creates no second outbox item or audit outcome.
- [ ] A cursor from another tenant or query is refused. Historical metadata access and content retrieval enforce their distinct permissions.
