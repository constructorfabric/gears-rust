---
description: "Typed ToolKit plugin hooks for additional readiness checks and post-commit actions in Entitlements."
---

# Feature: Extension Hooks

- [ ] `p2` - **ID**: `cpt-cf-bss-entitlements-featstatus-extension-hooks-implemented`
- [ ] `p2` - `cpt-cf-bss-entitlements-feature-extension-hooks`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Additional readiness check](#additional-readiness-check)
  - [Post-commit action](#post-commit-action)
  - [Binding and payload contracts](#binding-and-payload-contracts)
  - [GTS input and result contracts](#gts-input-and-result-contracts)
  - [Hook plugin SDK contract](#hook-plugin-sdk-contract)
  - [Authorization and execution policy](#authorization-and-execution-policy)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Invocation recovery and result acceptance](#invocation-recovery-and-result-acceptance)
  - [Result and reason mapping](#result-and-reason-mapping)
  - [Stored evidence and operational reads](#stored-evidence-and-operational-reads)
- [4. States (CDSL)](#4-states-cdsl)
  - [Hook execution state](#hook-execution-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Optional hooks with bounded authority](#51-optional-hooks-with-bounded-authority)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

**Status:** v2 / p2, proposed implementation. Enable configured hooks only after E-U09 is met and the pre-purchase path is adopted under E-U03/E-U04/E-U06/E-U10. Services without hooks need no hook plugin. Configured hooks require no Serverless runtime in v2.

## 1. Feature Context

### 1.1 Overview

A service can add a readiness check or run an action after an assignment or output changes. Entitlements calls deployment-selected ToolKit hook plugins through scoped ClientHub clients. It reuses the plugin registration, typed contract and outbox pattern used by Subscriptions. Plugins return evidence or perform a separate integration action. They cannot change purchased rights, adapter commands, activation authority or cleanup rules.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-entitlements-fr-extension-hooks`.

**Principles:** `cpt-cf-bss-entitlements-principle-bounded-hooks`, `cpt-cf-bss-entitlements-principle-fenced-application`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-entitlements-actor-hook-plugin` | Validates bindings, starts handlers and returns correlated execution results. |
| `cpt-cf-bss-entitlements-actor-subscriptions` | Consumes aggregate readiness and owns activation and its deadline. |
| `cpt-cf-bss-entitlements-actor-platform-operator` | Configures approved bindings and inspects failures. |

### 1.4 References

- [PRD](../PRD.md), [DESIGN §3.3.10](../DESIGN.md#3310-extension-hooks) and [ADR 0002](../ADR/0002-hook-plugins.md).
- [Initial readiness](02-initial-readiness.md) owns profiles and the aggregate result. [Recovery](06-reconciliation-recovery.md) owns findings and operator reads.
- [E-U09](../UPSTREAM_REQS.md#26-hook-plugin-execution) records the hook plugin contracts and qualification evidence.

## 2. Actor Flows (CDSL)

### Additional readiness check

- [ ] `p2` - **ID**: `cpt-cf-bss-entitlements-flow-hook-readiness`

**Actor:** Subscriptions, through an accepted preparation posture.

**Success:** all required adapter criteria and all configured readiness checks pass for the current manifest.

**Errors:** unavailable binding, execution failure, invalid result, changed evidence or closed generation. None grants readiness or authorizes cleanup.

1. [ ] - `p2` - Save the complete hook bindings in the manifest with the pinned readiness profile before preparation dispatch. Reject a locally malformed profile before use. A plugin binding rejection or unavailable plugin leaves readiness pending with a finding; it cannot produce definitive manifest failure. - `inst-hook-bind`
2. [ ] - `p2` - Wait until every required adapter criterion and required output is satisfied. For a profile with no required assignments, start from its explicit empty assignment roster. - `inst-hook-prerequisites`
3. [ ] - `p2` - Save a readiness attempt, reservations in both check budgets and its dispatch intent in one transaction. Bind it to the manifest, current prerequisite evidence, exact plugin implementation version and execution policy. Start after commit through the saved plugin. - `inst-hook-start`
4. [ ] - `p2` - Recover the invocation and validate its result under the algorithm below. Record hook evidence separately from adapter observations. - `inst-hook-evidence`
5. [ ] - `p2` - Recompute aggregate readiness with the current adapter criteria and hook evidence in one transaction. Publish ready only with the held commitment. Subscriptions still owns the activation commit and T. - `inst-hook-fold`
6. [ ] - `p2` - Stop scheduling checks when the generation resolves. Retain late results as history, request cancellation where supported and preserve all adapter cleanup obligations. A hook cannot delay restriction, revocation or cleanup. - `inst-hook-resolution`

### Post-commit action

- [ ] `p2` - **ID**: `cpt-cf-bss-entitlements-flow-hook-notice`

**Actor:** an authenticated application or output observation accepted by Entitlements.

**Success:** the matching handler completes and its result remains recoverable by delivery ID.

**Errors:** failed or unknown execution produces a finding. It never rolls back the recorded assignment or output state.

1. [ ] - `p2` - On a meaningful assignment outcome or output-state change, save the new state, safe notice, subject sequence and one delivery per matching binding in the same transaction. Identical observations create no new notice. - `inst-hook-notice-save`
2. [ ] - `p2` - Dispatch the notice after commit with its saved identity, payload and handler binding. Lifecycle processing continues without waiting for the handler. - `inst-hook-notice-send`
3. [ ] - `p2` - Recover execution until a terminal result is known or the recovery horizon is reached. Record confirmed completion, failure or uncertainty separately from assignment delivery. - `inst-hook-notice-result`
4. [ ] - `p2` - Expose failures through findings and safe execution reads. A finding disposition does not claim successful execution and does not authorize a second invocation. - `inst-hook-notice-finding`

### Binding and payload contracts

`entitlements-sdk` will register these types. Use the primitive and digest rules of [feature 01](01-posture-intake.md#sdk-contract-and-validation). Input and result objects are closed: reject unknown fields, unsupported versions and invalid discriminators. No inline code, arbitrary URL, provider payload or secret is accepted.

The readiness profile has an optional `hooks` list, ordered canonically by unique `bindingId`, with at most 16 entries. An absent list and an empty list both mean no hooks. Their serialized forms differ. Never add an empty list when verifying a stored profile digest. Each entry is a `HookBindingV1`:

| Field | Required meaning and bounds |
| --- | --- |
| `bindingId`, `point` | Stable profile-local ID, 1 to 128 visible ASCII characters; point is `readiness_check`, `after_assignment_outcome` or `after_output_state_changed`. All readiness checks are required. Notices are nonblocking. |
| `pluginInstanceId`, `pluginContractVersion` | Exact registered plugin instance and supported SDK contract version. Save its verified target identity and namespace; the same instance ID with a changed target is a conflict. |
| `implementationVersion`, `implementationDigest` | Exact plugin implementation version, 1 to 128 visible ASCII characters, and lowercase SHA-256 digest of its code, resolved dependencies, effective configuration and internal retry policy. A version cannot change its content. Each instance implements the bound hook point and input/output types. No `latest`, version range or automatic replacement. |
| `executionPolicyRef` / `version` / `digest` | Immutable approved policy identifying the plugin's restricted service principal, allowed external targets and credential bindings. Values come from deployment configuration, not customer input. Credential values remain in the platform credential store. |
| `inputTypeId`, `outputTypeId` | Exact Entitlements GTS types for the selected point, as listed below. Handler schemas must match these contracts. |
| `timeoutSeconds` | Integer 1 to 3600. Bounds one invocation including plugin retries. It is not the Subscriptions activation deadline. |
| `startRecoverySeconds` | Integer 1 to 900. Bounds retries of an uncertain start. Qualification requires a deduplication window strictly greater than this horizon plus the allowed clock skew. |
| `recoveryHorizonSeconds` | Integer from the sum of `startRecoverySeconds` and `timeoutSeconds` to 86400. After this horizon, unresolved execution stays unknown with a finding; no replacement invocation is allowed. |
| `recheckIntervalSeconds`, `maxChecks`, `maxTotalChecks` | Readiness only: interval 30 to 3600 seconds between admitted checks; `maxChecks` is 1 to 10 admitted invocations per prerequisite epoch; `maxTotalChecks` is an integer from `maxChecks` to 100 over the binding and manifest. Failures or cancellation after admission consume one check. Proven `not_started` consumes neither budget. Epoch changes reset only the per-epoch allowance; restart resets neither ledger. Forbidden for notices. |
| `startDeliveryRetrySeconds` | Integer 1 to 60. Minimum delay between validation/start delivery retries, separate from readiness rechecks. Saved admission and recovery deadlines bound each execution's delivery. |

Before the first plugin call, including plugin binding validation, save the times, final payload and request digest in one transaction:

- `firstDispatchAt`: the saved start time.
- `admissionDeadlineAt`: `firstDispatchAt` plus `startRecoverySeconds`.
- `deadlineAt`: `admissionDeadlineAt` plus `timeoutSeconds`.

Retries reuse these times and payload bytes. Validation and start delivery use backoff no shorter than `startDeliveryRetrySeconds` and stop at the saved admission deadline; transport retries cannot extend it or consume extra checks. Recovery time starts at `firstDispatchAt`. The plugin rejects starts after the admission deadline. Each admitted invocation, including its retries, also has a `timeoutSeconds` limit. At that limit or `deadlineAt`, whichever comes first, the plugin stops new downstream dispatch and requests cancellation of unfinished work. A timeout cannot prove that an external effect stopped; retain uncertain outcomes for recovery. These limits do not change the activation deadline.

**Check budget accounting.** A readiness attempt reserves one slot in both the saved prerequisite epoch and the binding/manifest total before it can dispatch. Allocate the attempt and reservation atomically. Count consumed slots plus outstanding reservations when admitting work; allow at most one nonterminal readiness execution per binding/manifest, even if the prerequisite epoch changes.

| Execution evidence | Budget action |
| --- | --- |
| Queued, plugin validation unavailable, or admission unknown | Keep the reservation. No check is consumed and no replacement execution is allowed while admission may still occur. |
| Confirmed plugin admission, from `start_hook_execution` or recovery | Convert the reservation to one consumed check in both ledgers. Plugin retries, duplicate replies and later failure/cancellation add no charge. Admission counts even if the handler fails before returning a result. |
| Proven `not_started` | Release the reservation once. Local proof requires that no worker ever claimed `start_hook_execution`; otherwise require the plugin's authoritative non-admission fence for the original key. |
| Timeout, lost reply, expired index, cancellation request or retry exhaustion without terminal proof | Retain the reservation and original identity. None proves non-admission. |

Persist a per-execution `checkBudgetState = reserved / consumed / released` with its epoch and manifest. Admission and release are idempotent transactions over that execution ID. Conflicting evidence is quarantined. A non-admission fence must also prevent a delayed start under the old key. Budget accounting follows the actual invocation even when its result is too late or belongs to a retired prerequisite epoch; such a result cannot supply a current pass.

After proven non-admission, a delivery retry may use a new execution ID/key and new bounded deadlines. It must keep the binding and current prerequisite evidence. Wait at least `startDeliveryRetrySeconds` and recheck generation authority and budget availability. This is a delivery retry, not a consumed readiness recheck. Notice deliveries keep their existing no-replay rule. After an admitted check ends, a permitted new check waits `recheckIntervalSeconds`. The increasing `checkNumber` identifies attempts, not consumed checks. It may exceed either check budget and is never reused. A prerequisite change or restart cannot reset the manifest total or discard a reservation for unknown work.

Profiles are immutable deployment configuration. Publishing a new plugin version or changing a route never changes an existing binding. New purchases may bind a newer qualified profile. Retries and successor preparation generations for a bound purchase keep its selected profile; changing it requires an explicitly accepted amendment. Existing assignments, later operations and notices keep their saved version. V2 has no binding migration API. An emergency security action may disable a handler, but it cannot silently select replacement code or count missing checks as passed.

A preparation manifest retains the full binding list and its digest. Assignment creation also saves the notice bindings for its lifecycle, so later restriction, revocation and output changes use the same handlers. V2 hooks require the `entitlements` activation profile. A `no_preparation` service has no hooks; adding them needs a future contract. Products publishes the preparation requirement, Pricing freezes it in accepted terms, and Orders obtains a durable configuration binding from Subscriptions before purchase commitment or capture. The [pre-purchase activation contract](02-initial-readiness.md#pre-purchase-activation-configuration) owns this check under E-U03/E-U04/E-U06/E-U10, including services without hooks.

An assignment added after activation inherits the profile and notice bindings of its source's applied preparation generation. Find that generation through the authenticated posture's `resolutionRef` and saved intake association. If the resolution is missing, recover it through the Subscriptions operation read under feature 02 before inheriting bindings. Save the inherited binding before first dispatch. Do not read a live route for hooks. If the saved profile evidence is missing, leave application unresolved and open a finding. Do not substitute an empty or newer profile. No plugin fallback drops a configured check.

Notice plugin implementations stay pinned for the assignment's lifetime, which may span years. Keep those deployments until the source closes and final deliveries settle. V2 cannot patch or migrate a binding. Retiring its version can stop its notices.

Keep one open binding-unavailable finding per assignment/binding. Link affected executions to it. Save later undeliverable notices as `not_started` with a failure reason and reuse the finding. They are not successful deliveries. Restoring the exact binding allows future notices. Recovery of old records cannot create a second execution.

### GTS input and result contracts

| Type identifier | Fields |
| --- | --- |
| `gts.cf.bss.entitlements.hook_binding.v1~` | `HookBindingV1` above. |
| `gts.cf.bss.entitlements.hook_readiness_input.v1~` | `schemaVersion`, `hookAttemptId`, `bindingId`, `bindingDigest`, `resourceTenantId`, `scopeId`, `activationOperationId`, `generation`, `manifestId`, `manifestVersion`, `manifestDigest`, `prerequisiteEpoch`, `prerequisiteDigest`, `profileRef`, `profileVersion`, `profileDigest`, `checkNumber`, `requestedAt`, `deadlineAt`, `assignmentCount`, and `assignments` as defined below. |
| `gts.cf.bss.entitlements.hook_readiness_result.v1~` | `schemaVersion`, `hookAttemptId`, `bindingDigest`, `manifestDigest`, `prerequisiteEpoch`, `prerequisiteDigest`, `status` = `passed` or `not_ready`, registered safe `reasonCode` when not ready, and optional opaque `evidenceRef` of at most 1024 characters. No definitive commercial rejection or state mutation. |
| `gts.cf.bss.entitlements.hook_notice.v1~` | `schemaVersion`, `noticeId`, `point`, `resourceTenantId`, `scopeId`, `assignmentId`, `generation`, `applicationOperationId`, `desiredRevision`, `desiredDigest`, `profileRef`, `profileVersion`, `profileDigest`, `subjectId`, `subjectSequence`, `occurredAt`, and one typed payload below. |
| `gts.cf.bss.entitlements.hook_notice_result.v1~` | `schemaVersion`, `noticeId`, `status` = `completed`, optional opaque safe `evidenceRef`. Plugin success without this exact acknowledgment is an invalid result. |

A returned `reasonCode` must be a registered instance of `gts.cf.bss.entitlements.reason.v1~` allowed by the qualified contract. An unknown code makes the result invalid. Additional diagnostic prose is not accepted.

Readiness `assignments` is the complete required roster: assignment ID, capability, desired revision/digest, logical target reference, pinned criterion, and safe application/readiness/output evidence identities and sequences. It excludes optional assignments and all protected content. The canonical roster digest includes these fields.

The host increments `prerequisiteEpoch` whenever meaningful prerequisite evidence changes, even if values later return to their previous state. Unchanged polls and sequence-only heartbeats leave the epoch unchanged. The digest uses the saved evidence that changed the state. A previous pass cannot become valid again after a failure.

Store and verify the roster count and digest. Never truncate it. Qualification must prove the maximum profile payload fits the plugin's accepted size. Oversize input leaves readiness unresolved with a finding.

An assignment notice payload contains capability, operation outcome, reason code when present, and accepted/applied source revision and digest where the capability supplies them. Emit on the first persisted outcome and a later change in outcome, reason or confirmed applied head. It describes application, not subscription activation.

An output notice payload contains output ID/version, kind, delivery state and the closed safe metadata of [feature 04](04-license-grants.md). It excludes opaque content-store references, keys, signed URLs and retrieval links. Emit on initial persisted output state and each delivery-state change; unchanged polling creates no notice.

`subjectSequence` increases per resource-tenant assignment for assignment notices and per resource-tenant output ID for output notices. Store it with the state change. Delivery and completion may be out of order. Consumers deduplicate by `noticeId` and ignore lower subject sequences when updating a current projection. A sequence is not a global order across assignments or outputs.

Handlers receive no subscription ID, payer, seller, price, full commercial posture or Entitlements credentials. An opaque evidence reference is not a URL to fetch automatically. Validate it as data and expose it only under the evidence owner's authorization. Raw handler logs and arbitrary error text do not enter BSS reads.

### Hook plugin SDK contract

`EntitlementsHookPluginV1` is a scoped ClientHub plugin contract in `entitlements-sdk`, separate from the three capability plugins. Its specification is `gts.cf.toolkit.plugins.plugin.v1~cf.bss.entitlements.hook_plugin.v1~`. Metadata advertises supported hook/schema versions and stable execution target identity. Discovery metadata never grants execution authority.

Every method takes the current service `SecurityContext` and saved binding. The host and plugin authorize the source resource tenant under ENT-D-04. Downstream calls use the restricted identity in the approved execution policy, not caller credentials.

| Method | Request and result |
| --- | --- |
| `validate_hook_binding` | Binding and exact input/output type IDs. Returns `qualified` with verified implementation and policy digests, owner and plugin recovery capabilities, or `unavailable / incompatible / forbidden` with a registered reason. It executes no handler. |
| `start_hook_execution` | `hookExecutionId`, `idempotencyKey`, `requestDigest`, exact binding, typed input, `admissionDeadlineAt` and `deadlineAt`. The first is the immutable end of the start-recovery horizon; the second bounds execution. Both are saved before dispatch and enforced by the plugin. Returns `accepted` with confirmed admission, invocation identity and verified implementation identity, `refused` with proof no invocation was admitted, or `unknown`. Identical replay returns the same invocation; changed content conflicts. |
| `read_hook_execution` | Execution ID and original key, plus invocation ID when known. Returns `running`, `succeeded` with typed result, `failed` with safe error class, `cancelled`, `unknown`, or `not_started` with authoritative admission-fence evidence. Admitted states include recoverable admission evidence; `not_started` proves the old key cannot admit later. Every found result identifies the original execution, request digest and exact implementation binding. |
| `request_hook_cancellation` | Execution identity, original key and invocation ID if known. Returns `requested / terminal / unknown`. Requesting cancellation proves neither that effects are absent nor that a new invocation is safe. |

A `start_hook_execution` refusal with valid non-admission proof maps to `not_started` and releases a readiness reservation. A refusal without that proof cannot release it.

Canonical validation, conflict, permission and unavailable errors follow feature 01. A timeout after a possible dispatch returns `unknown`, never `refused`. `not_found` in an expired index is `unknown`, not `not_started`. Plugin failure can follow external effects and does not prove they were undone.

The host registers the hook plugin schema and resolves the configured instance through scoped ClientHub. Each integration plugin registers its instance and implements this SDK contract. The host imports no concrete plugin crate. This follows [Subscriptions lifecycle hooks](../../../subscriptions/docs/DESIGN.md#337-adapter-plugins-and-profiles); it does not call `LifecycleHookPluginV1` or add a dependency on Subscriptions for execution.

A plugin may call a connector or implement trusted integration code directly. It owns its durable admission/result journal and downstream retry policy. Even an in-process implementation must retain admission and results across restart; returning a successful call is not enough. Multiple integrations can implement the same contract. They may share connector code with Subscriptions plugins while retaining each host's payload and authority rules.

A Serverless-backed plugin is future scope. It would use the same host contract and own runtime selection, identity, payload mapping and recovery. No Serverless SDK, function registry or bridge is required by the v2 hook contract.

### Authorization and execution policy

V2 permits only deployment-approved integration plugins and deployment-managed bindings. Entitlements authorizes each dispatch and read in the source resource tenant under ENT-D-04. The plugin validates that caller scope and uses its own restricted service identity and credential bindings for downstream calls. It cannot copy the caller's credentials or use a tenant ID in the payload as permission.

Readiness handlers have read-only access to approved external systems. They cannot provision service resources or supply missing essential access. That work belongs to a capability adapter. Notice handlers may perform approved external actions such as CRM updates or notifications. Their configured identity grants no Entitlements mutation, output retrieval, Subscriptions activation or provider grant/revoke authority.

In-process plugins are trusted platform code, not a sandbox for customer code. Qualification includes code review and tests of policy checks, scoped credentials and denied actions. Process isolation, a handler editor and tenant-uploaded code are not supplied by the hook interface. Credential storage and egress controls use the existing platform facilities. E-U09 must prove these controls for the selected implementation.

## 3. Processes / Business Logic (CDSL)

### Invocation recovery and result acceptance

- [ ] `p2` - **ID**: `cpt-cf-bss-entitlements-algo-hook-recovery`

**Input:** a durable hook execution and saved binding. **Output:** recovered state, accepted evidence or a finding.

1. [ ] - `p2` - Save one execution ID and deterministic start key. Before the plugin call, finalize the canonical request digest with the first-dispatch times. Derive the key from resource tenant, binding digest and execution ID; store it and reuse it byte for byte. Never derive a new key from the retry count. - `inst-hook-key`
2. [ ] - `p2` - Authorize the source scope and recheck the saved binding. Claim work with a local lease, then call the plugin outside the transaction. A lease prevents duplicate local work. It cannot prove whether plugin execution started. - `inst-hook-dispatch`
3. [ ] - `p2` - Recover an ambiguous start by its original key. Resend only within the saved start-recovery horizon and the proven deduplication window. After either expires, stop start requests and retain unknown state with a finding. A late worker cannot restart an expired request. - `inst-hook-ambiguous`
4. [ ] - `p2` - Poll the public result read using the saved invocation or key. Plugin retries stay inside that invocation and keep the exact implementation binding. Entitlements does not start a replacement for running or unknown work. Completion notifications, if added later, only wake the same read. - `inst-hook-poll`
5. [ ] - `p2` - Validate the execution identity, request digest, implementation version/digest, schema and result correlation. Quarantine a mismatch. For readiness, also check the open generation, current manifest, current attempt and prerequisite epoch/digest under the transaction fence. - `inst-hook-validate`
6. [ ] - `p2` - Save valid evidence, the idempotent budget transition and any changed aggregate with its publication intent atomically. If prerequisites changed or the generation resolved, retain the result as history and update only its original budget ledger; it cannot supply a current readiness pass. - `inst-hook-commit`
7. [ ] - `p2` - After an admitted check ends not-ready, failed or cancelled, schedule a new check only within both remaining budgets and after `recheckIntervalSeconds`. Proven non-admission instead releases its reservation and allows a delivery retry after `startDeliveryRetrySeconds`. Recheck open-generation authority, prerequisites and the saved binding before allocating a new identity. Increment `checkNumber` for every new attempt; do not use it as the consumed-check count. Running or unknown execution never permits a replacement. - `inst-hook-recheck`
8. [ ] - `p2` - At the invocation deadline, stop accepting its readiness result and request cancellation. Retain unknown execution until terminal evidence is recovered. At consumed-check budget or recovery exhaustion, open a finding and report retryable readiness failure; retain uncertain reservations and do not fabricate a completed check; only Subscriptions may close activation. - `inst-hook-deadline`

For an invalid result, save its safe digest and reason. Do not copy the raw payload into findings. Conflicting identity or deployment evidence stays quarantined and cannot authorize another start.

Readiness `not_ready` maps to aggregate `pending` while checks remain. Plugin binding validation failures remain pending and consume no checks. Proven non-admission due to unavailability also stays pending while delivery retry is allowed. Other configuration or result errors keep their failure mapping; releasing a reservation does not make them successful. Plugin execution failure, invalid results or exhausted checks map to `failed_retryable`. Definitive failures from required adapters and security blocks keep their existing priority. A hook cannot produce `failed_definitive`, lift a block or hide an adapter failure. Missing configuration cannot be interpreted as an empty hook list.

Prerequisite changes invalidate passes for the current epoch. Recheck only after the required adapter roster is satisfied again and any previous invocation is terminal. A pass remains evidence for that epoch until a correlated prerequisite change or generation resolution. It is not a continuous availability guarantee. After an applied resolution, no new readiness checks run and late results cannot undo T.

The hook plugin retries execution within one admitted invocation. Entitlements handles start delivery, result recovery and readiness rechecks, each with separate limits. Start delivery uses its saved admission deadline and retry interval; admitted-check budgets bound rechecks. Retries after non-admission cannot extend the Subscriptions attempt or override its pause or closure.

The plugin may execute code more than once. Handler authors must deduplicate notice effects by `noticeId`. The contract does not guarantee exactly-once effects. A failed notice is final for that delivery in v2. Recovery can find its actual outcome but cannot start a second execution. A new business state change creates a new notice. An operator replay/migration API is future scope.

### Result and reason mapping

The six reason codes below are the registered instances listed in DESIGN §4.3. Their full prefix is `gts.cf.bss.entitlements.reason.v1~cf.bss.entitlements.` and suffix is `.v1`. The aggregate uses the existing reason/evidence envelope. It sends no hook fields to Subscriptions.

| Situation | Execution record | Reason | Readiness contribution | Finding |
| --- | --- | --- | --- | --- |
| Plugin `validate_hook_binding` returns `unavailable` | Queued, not dispatched | `hook_binding_unavailable` | `pending` | One per binding and manifest or assignment |
| Plugin `incompatible` or `forbidden`, or saved target/implementation differs | Queued, not dispatched | `hook_binding_conflict` | `pending` | One per binding and manifest or assignment |
| Valid `not_ready`, checks remain | `succeeded`; check did not pass | `hook_not_ready` | `pending` | None required |
| Proven non-admission during delivery unavailability | `not_started` | `hook_binding_unavailable` | `pending`; permit a bounded delivery retry | Release the readiness reservation; retain the binding finding |
| All current checks passed | `succeeded` | No failure reason | Ready only with required adapter criteria and commitment | Resolve correlated execution findings |
| Invalid schema/correlation/reason or payload too large | `failed` after a known terminal execution; `not_started` if never claimed for `start_hook_execution`; otherwise `unknown` | `hook_result_invalid` | `failed_retryable` | Quarantine or configuration finding |
| Terminal plugin failure, cancellation, or exhausted check budget | Actual terminal state retained; no fabricated invocation for budget exhaustion | `hook_execution_failed` | `failed_retryable` | Check failure finding |
| Invocation deadline elapsed, outcome still uncertain; start/result not recoverable | `unknown` | `hook_unknown` | `failed_retryable` | Retain original identity and recovery obligation |
| Terminal execution recovered after its deadline | Actual terminal state, result expired for readiness | `hook_execution_failed` | `failed_retryable` until a permitted recheck passes | Retain timeout evidence |

A local profile syntax error or unsupported profile schema can still make the manifest unbuildable under feature 02. A plugin validation answer cannot. Notice results never change readiness or posture outcomes.

An unavailable binding keeps delivery queued until the recovery horizon. If no worker ever claimed it for `start_hook_execution`, it may then become `not_started`. The binding finding is deduplicated across later notices. No unknown execution is converted to `not_started`.

### Stored evidence and operational reads

`HookAttempt` stores readiness correlation, check number, prerequisite epoch/digest, deadline and per-execution check budget state. Durable ledgers hold consumed checks and reservations per binding/epoch and binding/manifest; they survive restart. `HookDelivery` stores notice identity, subject sequence and immutable payload. Both reference a retained `HookExecution` with the binding, start key/request digest, invocation ID when known, execution state, first-dispatch time, horizons, safe reason and result digest. `HookEvidence` stores the accepted readiness result and whether it still contributes to the aggregate. These records belong to the resource tenant. Plugin invocation records belong to the execution tenant.

Unique constraints cover readiness `(manifestId, bindingId, checkNumber)`, delivery `(noticeId, bindingId)` and execution ID. Deduplicate under those keys before scheduling. Hook evidence never enters `record_assignment_observation` and has no adapter producer authority. The [retention contract](06-reconciliation-recovery.md#retention-and-safe-compaction) preserves active evidence, unresolved starts, deduplication keys and budget ledgers. Completed diagnostic details may expire; replay cannot reset a check budget or execute a notice again. Delivered outbox rows can be removed only after the durable execution record holds recovery responsibility.

`list_hook_executions` and `get_hook_execution` return execution ID, point, binding ID, safe implementation version, operation/assignment correlation, check or notice identity, subject sequence, status, reason, deadline, check budget state, consumed/reserved counts and last recovery time. The shared [OData collection contract](06-reconciliation-recovery.md#odata-collection-contract) defines `operation_id`, `assignment_id`, `point` and `status` filters, stable `id` ordering and the page envelope. No payload, credential, plugin logs or protected output reference is exposed. Use one resource tenant, the feature 06 OData and cursor rules, and `hook_execution_read`. The point read requires the same authorization. Operator inspection works for a hook-only manifest with no assignment. There is no public execute, cancel, rebind or retry endpoint.

Record metrics for queue age, invocation latency, unknown executions, invalid results and exhausted checks. Do not label metrics with tenant IDs or raw handler payloads. A missing plugin affects only configured bindings. Disabled deployments stop new starts but continue recovery where possible. Rollback must retain journals and pinned dependencies needed for unfinished work; no fallback handler is selected.

## 4. States (CDSL)

### Hook execution state

- [ ] `p2` - **ID**: `cpt-cf-bss-entitlements-state-hook-execution`

Execution state and readiness contribution are separate. An obsolete or expired pass may be a successfully executed handler while contributing no readiness.

1. [ ] - `p2` - **FROM** absent **TO** `queued` **WHEN** the source transaction saves the execution and dispatch intent. - `inst-hook-state-queued`
2. [ ] - `p2` - **FROM** `queued` or `unknown` **TO** `running` **WHEN** the original invocation is authoritatively identified. - `inst-hook-state-running`
3. [ ] - `p2` - **FROM** `queued` or `running` **TO** `unknown` **WHEN** dispatch or execution cannot be determined. Preserve the same identity. - `inst-hook-state-unknown`
4. [ ] - `p2` - **FROM** `queued`, `running` or `unknown` **TO** `succeeded`, `failed`, `cancelled` or `not_started` **WHEN** the original operation supplies terminal evidence. Invalid success payloads become `failed` with a finding. - `inst-hook-state-terminal`
5. [ ] - `p2` - **FROM** any terminal state **TO** the same state **WHEN** identical evidence replays. Conflicting terminal evidence is quarantined. - `inst-hook-state-replay`

Record cancellation requests and elapsed deadlines as flags. Neither proves that execution ended. A queued execution may become `not_started` locally only if it has never been claimed for `start_hook_execution`; commit that terminal state and release its check reservation atomically. Otherwise remote admission-fence evidence is required. An expired attempt cannot later contribute a pass even when its execution completes.

## 5. Definitions of Done

### 5.1 Optional hooks with bounded authority

- [ ] `p2` - **ID**: `cpt-cf-bss-entitlements-dod-extension-hooks`

1. [ ] - `p2` - Implement the GTS payloads, plugin SDK, immutable binding validation, journal, outbox handoff and authorized safe reads. Store generated JSON schemas and fixtures in a separate `schemas/` folder. - `inst-hook-dod-contracts`
2. [ ] - `p2` - Implement and qualify the selected integration plugin against the typed hook SDK under E-U09. The host supports configured hooks without Serverless and serves no-hook profiles without loading a hook plugin. - `inst-hook-dod-plugin`
3. [ ] - `p2` - Verify readiness folding, atomic notice creation, recovery and execution-policy isolation with deterministic transaction and contract tests. Run enabled-profile end-to-end tests against a real qualified plugin. - `inst-hook-dod-tests`
4. [ ] - `p2` - Expose findings and latency metrics, retain evidence through rollback, and prove that no unavailable hook can delay restriction, revocation or cleanup. - `inst-hook-dod-operations`

**Implements:** `cpt-cf-bss-entitlements-flow-hook-readiness`, `cpt-cf-bss-entitlements-flow-hook-notice`, `cpt-cf-bss-entitlements-algo-hook-recovery`, `cpt-cf-bss-entitlements-state-hook-execution`.

## 6. Acceptance Criteria

- [ ] With `maxChecks = 3`, three delivery attempts end in proven non-admission during a plugin outage. All reservations are released; zero checks are consumed. Once the exact binding is available, the next admitted invocation consumes the first check.
- [ ] A start response is lost. The original attempt keeps its reservation and identity, including across restart or prerequisite change. Recovering admission charges once; recovering authoritative non-admission releases once. Duplicate results cannot charge or release twice.
- [ ] One admitted invocation fails, is cancelled or retries internally. It consumes one check. An expired successful result still consumes its original check but cannot satisfy current readiness.
- [ ] Concurrent workers cannot reserve beyond either budget or run two readiness executions for one binding/manifest. Delivery retries keep their saved deadlines; fresh attempts require terminal evidence and preserve monotonic `checkNumber` without treating it as a budget counter.

- [ ] No hook bindings and no hook plugin: normal preparation and lifecycle behavior are unchanged. Configured integration plugins also run and recover without Serverless. Configured but unavailable checks cannot pass (E-AC50).
- [ ] Every adapter is ready but one check is not ready: aggregate readiness waits. A manifest with no assignments but a required hook also waits (E-AC51).
- [ ] Publish another plugin version, change its configuration or change the registry target during execution. The saved binding is preserved or rejected; no replacement code runs (E-AC52).
- [ ] Lose a start reply, restart two workers and pass the deduplication horizon. Recover the original invocation or keep the execution unknown; never start another under a new key (E-AC53).
- [ ] Change prerequisite evidence, then restore its old values. The epoch changes, so a delayed pass cannot restore readiness. Closure or applied T also makes late results historical (E-AC54).
- [ ] Exhaust rechecks, timeout, return an invalid payload or lose the result read. Readiness remains unresolved; no purchase cancellation, cleanup or definitive failure is inferred (E-AC55).
- [ ] Commit an output change and crash before delivery. Recover one notice per binding with the same payload. A handler that deduplicates by `noticeId` produces one effect despite plugin retries. Consumers use sequences to prevent older results from replacing newer state (E-AC56).
- [ ] A notice fails after assignment restriction or revocation. The source outcome stays committed and cleanup proceeds; a failed or unknown hook remains visible (E-AC57).
- [ ] An unapproved plugin, forged resource tenant, excessive permission or attempt to retrieve protected output is rejected. No caller credential, key, payer or subscription ID reaches a handler (E-AC58).
- [ ] Read execution status for a hook-only manifest, page another tenant's cursor, and probe unknown IDs. Authorized inspection works without an assignment; cross-tenant reads disclose nothing.
- [ ] Observe plugin success with a wrong implementation digest or changed request under the same key. Quarantine it; neither readiness nor notice completion advances.
- [ ] A read-only check tries to provision resources or call an Entitlements mutation. The execution policy denies it. A plugin that cannot enforce this cannot close E-U09.
