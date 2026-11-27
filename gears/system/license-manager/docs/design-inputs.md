# License Manager: inputs for DESIGN

This file collects the execution mechanisms that the 26 November 2026 consolidation pass removed from the [PRD](./PRD.md). The owner decisions D1–D5 of that date moved them out. The PRD keeps the outcome guarantees; DESIGN decides how to meet them. §16 adds the mechanisms behind the PRD edits of 27 November 2026.

- This is input only. It adds no requirement and defines no ID. Where an item and the PRD differ, the PRD applies.
- It is not a CF Studio artifact. DESIGN.md (planned) takes over each item it adopts.
- Each item names the PRD requirement it serves and the decision it came from. Values marked "proposed" are configurable defaults that the owner has decided.
- Source: the PRD before the pass (1,785 lines, SHA-256 `3fb81abd…`) and the deep review of 26 November 2026 (findings DR-01 to DR-30).

Decision ids: D1–D7 are the owner decisions on the deep review (26 November). Z-nnn come from the ninth review (26 November), Y-nnn from the eighth, X-nnn from the seventh, V-nnn from the fifth and U-nnn from the fourth (24 November). Requirement ids are written without the `cpt-cf-license-manager-` prefix.

## 1. Change execution

Serves `nfr-correlated-outcome` (one outcome, fail-closed transitions, liveness). From D1.

- **Validate first.** Validate the whole change against fresh reads from license-enforcement and quota-enforcement before applying any part of it. A validation failure changes nothing (`nfr-correlated-outcome`, `nfr-reduction-safety`; U-006, D1).
- **Durable intent.** Make the intent of a change durable before the first gear call, so a License Manager restart resumes the change instead of losing it. The audit events go in the same transaction (§7) (`nfr-correlated-outcome`, `nfr-recovery`; D1, D3).
- **Removals before additions.** Apply removals and reductions before additions and increases. Each Grant and Quota then holds its prior or new value, and a window in which neither applies fails closed (`nfr-correlated-outcome`; D1).
- **Giving side first.** In split-cap, lower the parent Quota before the child Quota is created or raised. On a decrease or Reclaim, lower the child Quota and confirm it before the parent Quota is raised. In a bulk share change, apply reductions before increases (`fr-split-cap`, `fr-share-bulk`, `nfr-concurrency-safety`; D1).
- **One order per Quota.** Writes to one Quota go in acceptance order (`nfr-concurrency-safety`; X-003).
- **Phase 1 replace.** Validate, apply the Grant and Quota changes, and record the new version as Effective at the instant the last required change is applied. The replaced version stays authoritative in License Manager's records until then (`fr-pack-replace`, `fr-versioning-status`; DR-04). A license-enforcement replace step that applied cannot be undone: license-enforcement has terminated the base Pack, and the replacement Pack stays in force. The next replace or revoke targets that Pack (`fr-versioning-status`; sibling review R-02). Renew, suspend and reinstate need the same rule (re-review 3, R-19).

## 2. Error classification and retry

Serves `nfr-correlated-outcome`, `fr-stuck-steps` and `nfr-dependency-outage`. From D1 and D2; replaces U-005 and the premise of V-003 and Z-016.

- **Transient.** A gear that is unavailable or times out, or answers `ServiceUnavailable`. Retry until the change is applied or a later change preempts it. Never undo steps after a transient failure. DESIGN sets the retry schedule and backoff.
- **Business rejection.** A rejection with a named business code, such as quota-enforcement `CAP_BELOW_CONSUMED` or a version mismatch. Do not retry; the change applies no further forward step. Undo applied additions and increases where the gears allow it; applied removals and reductions stay. A license-enforcement replace step cannot be undone, because its base Pack is terminated. It stays applied and is reported as a partial state. The change ends as failed. Only a named business code ends a change as failed.
- **Other errors.** An error that is neither transient nor a business rejection is treated as a business rejection, except in a revoke or a system cascade.
- **Authorization rejection by a gear** (D2). In a caller's change, it is a rejection and the change fails closed. In a revoke, deletion or cascade step, it is retried and escalated as a stuck termination step (§5).
- **Termination steps** (revoke, expiry, tenant deletion, user cascade, child deletion). A step that the gear reports as already done counts as done. A version conflict or stale base is read again and retried with the current version; it is never a reason to stop. Any other rejection is retried and escalated per §5 (`fr-pack-revoke`, `fr-child-disable`, `fr-data-lifecycle`; U-004, D1).
- **User cascade** (DR-14). The old text let a named business code, for example `CAP_BELOW_CONSUMED` after an in-flight lease commits, end the cascade as failed. Now it never fails: it reads state again and escalates per §5 (`fr-user-lifecycle-cascade`, `fr-stuck-steps`).

## 3. Concurrency: locks, priority and limits

Serves `nfr-concurrency-safety` (no over-allocation; revoke comes first), `fr-one-capacity-rule`, `fr-child-disable` and `fr-data-lifecycle`. From D1; carries X-002, X-003, X-021, U-008, V-005 and Z-009, and old §6.3 items 1, 2 and 5.

- **Lock keys and order.** A table of the keys that each write operation and cascade takes, and their order. It covers every write: renew, suspend, reinstate, unassign, bulk changes, and the user and parent cascades too (X-021).
- **Per-tenant serialization.** Every operation that creates a lineage or a version is serialized per tenant, including a delegate for its child (U-008).
- **Delegation order.** Delegate, decrease and Reclaim lock the parent lineage first, then the child tenant (V-005).
- **Counting unapplied changes.** How accepted changes not yet applied count against free capacity and the protected minimum. When a failed or preempted change stops counting, and how its effects that stayed applied keep counting (X-003).
- **Revoke priority.** A revoke is accepted without waiting for locks that a running change or a validation holds, and preempts both. Its latency counts from the request (X-002).
- **Validation limits.** The timeout of each validation read, the total validation limit and the lock-wait limits. A validation past its limit changes nothing and releases its locks. It ends with a retryable result and uses up neither the idempotency key nor the BSS revision. A validation preempted by a revoke behaves the same and ends as preempted, naming the revoke (X-002, Z-009).
- **Deadlock freedom.** Child deletion, the parent revoke cascade and the tenant-deletion cascade never deadlock against each other. The same holds against a running revoke and first issue, against the user cascade (from Phase 2a), and against child deletion (from Phase 2c).
- **Cascade order.** When a parent and its child are deleted in one run, the child's cascade runs before the parent's (U-004).
- **Raise exactly once.** The parent Quota raise from child deletion and the parent revoke cascade are idempotent against each other. The raise is skipped once a parent revoke is recorded. The parent revoke cascade counts a Child Delegation that child deletion already terminated as done (`nfr-correlated-outcome` liveness, `fr-child-disable`; U-004, Z-014).

## 4. Replace and revoke taking over a running change

Serves `fr-pack-replace` and `fr-versioning-status` (a replace takes over a running change), `fr-pack-revoke` (revoke comes first) and `nfr-pack-latency`. From D1; carries U-006, V-001, X-014, Z-002 and Z-017, and old §6.3 item 3.

- **Pause and resume.** A replace on a lineage with a running change first pauses that change. It validates against the state read from the gears, not only against License Manager's records. If the replace is accepted, the paused change ends as preempted, naming the replace. If it is rejected, the paused change resumes (U-006).
- **First issue.** A replace against a lineage whose version 1 is not applied is rejected and does not pause the first issue (V-001).
- **Removing preempted effects.** How and when a revoke or a replace removes the effects of the change it preempted. This includes gear calls still in flight and calls whose reply was lost (X-014, Z-002).
- **Gear-write timeouts** that fit inside the revoke latency bound (`nfr-pack-latency`; Z-017).
- **Revoke completion.** When a revoke counts as complete while gear calls are still in flight. The PRD defines it as "no effect of the lineage is known" (DR-10).
- **Late effects.** How a late effect that reconciliation finds resumes the revoke that owns it (`fr-pack-revoke`, `fr-reconciliation`; DR-10).
- **Revoke latency measure.** How "no license check and no new debit or lease succeeds" is observed, counted from the revoke request (`nfr-pack-latency`; DR-15).

## 5. Stuck termination steps

Serves `fr-stuck-steps`, `fr-child-disable`, `fr-split-cap` and `fr-authorization`. From D1; replaces the park clocks of U-004, V-003, X-006, Z-004 and Z-016. The confirm rule in a deleted tenant is from D4.

- **Escalation.** The PRD escalates a step within 24 hours of its first rejection (proposed, configurable). DESIGN sets the retry schedule before escalation. After escalation the step is not retried automatically.
- **Release.** A release returns the step to retry, and the step stays outstanding. The old text restarted the window from the release time (X-006); DESIGN confirms whether the 24-hour window restarts.
- **Verified confirm.** Read the step's Grant or Quota from the gear, and accept the confirm only if the read shows the step done. A Quota raise step is done when the Quota is at or above its target amount.
- **Unverifiable reads.** A denied, refused or not-found read is unverifiable and rejects the confirm; the step stays escalated. This covers a read refused in a deleted tenant (G24).
- **Deleted tenant.** A confirm is accepted without a successful gear read only for a tenant that is gone for good: `TenantNotFound` for a tenant already recorded as deleted (D4, re-review RR-01). A refused read in a soft-deleted tenant keeps the step stuck.
- **Stored read result.** The stored and returned result of a confirm keeps only the step state (done or not done, plus the amounts) or "unverifiable". It holds no user id (X-017).
- **Operator actions.** An action carries an action id or idempotency key; a repeat returns the first result. A new confirm after a rejected one is a new action. When a confirm and a release race on one step, the first accepted one wins. A confirm by the principal that sent the revoke is rejected (X-012; `fr-authorization`).
- **Parent raise past a stuck step.** While a child-side deletion step is outstanding, the delegated amount still counts as used by the parent. If the step is still outstanding 72 hours after its escalation (proposed, configurable), the parent raise goes ahead with an operator signal. The delegated amount then stops counting (X-022, Y-016, Z-013). The old text applied this only to a deleted child; `fr-child-disable` sets the scope.
- **Not carried forward.** The two park clocks (24-hour park, 72-hour cap) and their anchor on alert delivery, acknowledgement or a 24-hour fallback (Z-004). Also the clock pause during a PDP outage (Z-016) and the restart of both clocks on release (X-006).

## 6. Operator signals and alert routing

Serves `nfr-correlated-outcome` (operator signals) and `fr-reconciliation` (visibility). From D3; replaces U-009 (one alert table) and X-011, X-013, Y-006, Z-004, J2 and J3.

- **Signals.** Expose each condition in the signal list of `nfr-correlated-outcome` as a metric or health signal. Platform alerting routes it, as account-management does (its PRD, line 1023).
- **Routing and repeat.** DESIGN sets these with platform alerting: routing, repeat interval (the old proposal was every 24 hours), one signal per trigger and object, and the clear conditions.
- **Old clear conditions,** for reference:
  - a partial state: it ends, or the change is settled;
  - a stuck step: a confirm of it is accepted, or it is released;
  - a tenant-resolver not-found answer: a later read returns the tenant, or the tenant is recorded as `deleted`;
  - a parent raise past a stuck step: the step is done, or a confirm of it is accepted.
- **Optional signals.** DESIGN may add:
  - an account-management not-found answer for a tenant that License Manager holds records for (an old alert row; with D4 this happens after hard deletion);
  - a gear that keeps answering `ServiceUnavailable`.
- **Not carried forward:**
  - the alert lifecycle that License Manager kept itself (open, repeat, clear, close, re-raise);
  - "acknowledged" and "delivered" alerts;
  - the operator acknowledge and close verbs;
  - the notification channel dependency (old G29, OQ-41) and operator-action notices;
  - the PDP probe alert, the delayed-audit alert, and the alert for an authorization refusal older than 15 minutes.

## 7. Audit pipeline

Serves `fr-audit-store`, `fr-idempotency-audit` and `fr-visibility`. From D3, which follows the license-enforcement DESIGN pattern (its DESIGN.md, lines 648-653).

- **Outbox.** Audit events go to a transactional outbox in the same transaction as the change. The platform audit target owns storage, tamper evidence and retention.
- **Parent visibility.** The audit target must support the parent visibility filter (G20).
- **Rejected attempts.** These are audited through the outbox too. If the outbox write fails, the attempt fails with it. The old "reported to the caller as unaudited" case is dropped (owner decision D-02, 27 November).
- **Erasure.** User ids in audit entries must be erasable without breaking tamper evidence (DR-19, OQ-29).
- **Audit completeness.** The §1.3 goal is measured from outbox delivery, no longer by a reconciliation check.
- **Not carried forward:**
  - License Manager's own append-only store;
  - the audit-outage exception for revokes and cascades, and the intent-kept audit entry;
  - delayed-entry marking and the 15-minute delayed-entry alert;
  - the "audit missing or delayed" reconciliation check.

## 8. PDP handling

Serves `nfr-dependency-outage`, `contract-pdp` and `fr-stuck-steps`. From D2; replaces U-005, V-003, X-007, Y-019 and Z-016. The gears already map a PDP failure to `ServiceUnavailable`: quota-enforcement `canonical_mapping.rs:203`, account-management `domain/error.rs:472` and license-enforcement DESIGN.md line 106.

- **ServiceUnavailable.** A gear's `ServiceUnavailable` is transient and retried (§2). There is no PDP health probe, no outage state and no clock pause.
- **Authorization rejection.** A caller's change fails closed. A termination step escalates (§5).
- **System work.** System work takes its tenant scope from its trigger, with no PDP call. A cascade start is not deferred. Each gear still authorizes License Manager's calls by its own rules. quota-enforcement accepts the Quota Manager role only in the original caller's tenant, with the caller's SecurityContext forwarded, and needs operator-level scope for cross-tenant operations. Which scope each flow uses is open (`contract-pdp`, G9, OQ-14).
- **Not carried forward.** The probe's call, start-up state, thresholds and error classes (old §6.3 item 6, X-007). Also the recording of trusted events during an outage, with their cascades deferred until it ends (Z-016).

## 9. Tenant-deletion detection

Serves `fr-data-lifecycle`, `contract-account-management` and `contract-tenant-resolver`. From D4; replaces Z-005, X-001, Y-003, Y-018 and J4.

- **Status polling.** Until account-management publishes a tenant-deleted event (G22), each reconciliation run reads the status of every tenant that has a lineage that is not terminated or a change still running. Status `deleted` starts the cascade.
- **Recorded deletion.** Record the tenant as deleted when account-management reports `deleted`. account-management removes a deleted tenant for good after a retention, 90 days by default. After that, a tenant-resolver not-found answer counts as deleted only for a tenant already recorded as `deleted`.
- **Other not-found answers.** A not-found answer for a tenant not recorded as `deleted` raises an operator signal and starts nothing.
- **Plugin semantics.** The needed behaviour is the account-management tenant-resolver plugin (`cpt-cf-tr-plugin-fr-get-tenant`). It returns deleted tenants with their status, and `TenantNotFound` only for absent or provisioning tenants. rg-tr-plugin maps any Resource Group not-found, including one hidden by scope, to `TenantNotFound` (`service.rs:302-309`). Which plugin is deployed is OQ-42.
- **Refused reads in a deleted tenant.** These are reported as unverifiable, not as drift or a match (G24, OQ-36).
- **Not carried forward.** The operator "confirm tenant gone" verb, its fresh-read check, the interim rules list, the two not-found alerts and the operator close of the not-found alert.

## 10. BSS adapter: reason codes and result mapping

Serves `fr-desired-grant-apply`, `fr-source-fencing` and `contract-bss-license-adapter`. From D5 and DR-12; carries U-007, V-004, X-008, X-010, X-020, Y-007, Z-009 and Z-011. The PRD keeps the result classes; the codes and fields are here.

**Proposed reason codes**, registered with BSS under OQ-39 (G27):

| Code | Class | Meaning |
|---|---|---|
| `first_version_running` | retryable | A changed desired set while the source's first issue runs |
| `first_version_failed` | definitive | A changed desired set after the first issue failed; BSS recovers by Revoked, then a new revision |
| `validation_incomplete` | retryable | Validation could not finish; the revision is not used up (Z-009; G31, OQ-43) |
| `preempted` | definitive | A revoke, expiry, tenant deletion or replace took over; names it |
| `partial` | open (OQ-39) | Effects stayed applied; lists every addition, increase, removal and reduction still applied |
| (stuck step; was `parked`) | open (OQ-39) | A termination step of the change is escalated |

BSS treats an unknown reason as retryable with a finding (D5).

**Result mapping:**

| Change end state | Result |
|---|---|
| applied | `applied` once the effective time has passed. Before that, `pending` with the effective time. `refused` with reason `preempted` if the lineage ends or the version is replaced first |
| failed, no effect remains | `refused` |
| failed, effects remain | `pending` with reason `partial`. It ends only when a corrective revision arrives or BSS's own reconcile closes it. It never maps to `refused` (D5) |
| preempted | `refused` with reason `preempted`, naming the change that took over |
| a step escalated | `pending` with the stuck-step reason. `applied` only after a confirm of the step is accepted and the effective time has passed (V-004) |

- **Future start, new source.** A desired set for a new source with a future start is materialized at once. It stays `pending` until its effective time, then reads `applied` with no further call.
- **Future start, started source.** In Phase 1, a changed set with a future start for a source whose pack has started is refused.
- **Capacity rights.** A capacity right is refused until OQ-12 closes.
- **Aggregate read fields** (DR-12, SIB-6). These depend on OQ-22 and G16. Every row carries pack id and version, Entitlement id, effective window, evaluation timestamp and freshness label.
  - Seats, per Entitlement: Resource, licensed, assigned, available, delegated and unusable counts.
  - Shares, per Entitlement: metric, unit, quota type and period (or none), licensed quantity, distributed amount, protected consumed and in-flight amount, and allocatable remainder.
  - Child Delegations, per child and Entitlement: parent and child pack ids and versions, Child Delegation id and version, delegated quantity, and the child's assigned or distributed, protected and available amounts.
- **Sibling identifiers** that the old PRD text used, for the mechanisms above:
  - license-enforcement: `not_before`, `expires_at`, `grace_until` (unset), `effective_at` (Phase 2), `recall_grant` with `expected_version` (G12 interim), `recall_delegation`;
  - quota-enforcement: `CAP_BELOW_CONSUMED`, `if_match_version` (OQ-25);
  - tenant-resolver: `get_tenant`, `TenantNotFound`;
  - GTS: the old namespace proposal `gts.cf.core.license_management.*` (left to OQ-8; DR-30).

## 11. Settle

Serves `fr-desired-grant-apply`, `fr-authorization` and `fr-reconciliation`. From D5; replaces the BSS mapping of Z-001 and J1.

- **What it is.** An internal platform-operator action for a failed change whose effects partly stayed applied. It names the change by its correlation id, carries a reason and is audited with the effects that stayed applied.
- **Effect.** It ends the change inside License Manager and hands the remaining effects to reconciliation as drift.
- **BSS result.** It does not change the BSS result, which stays `pending` with reason `partial`.

## 12. Source fences, retention and recovery

Serves `fr-source-fencing`, `fr-data-lifecycle`, `nfr-recovery` and `fr-reconciliation`. Carries Z-015 and DR-16, and old §6.3 item 4.

- **Source-key retention.** How long the source-key record is kept, so that a source key never accepts a revision older than one it accepted, even after any purge (Z-015).
- **Fence recovery.** A restore to an earlier point is outside RPO 0. DESIGN defines how source fences and version checks are recovered before any BSS revision is accepted again (DR-16).
- **Reconciliation sizing.** Each run reads tenant status plus Grants and Quotas for every tenant with records, so the work grows with tenant count. `nfr-recovery` and `fr-reconciliation` bound a run at 15 minutes (proposed).
  - **Open number for the owner:** the tenant and record volume that this bound holds for. The source PRD gives none; the pack size of `nfr-pack-latency` (50 Entitlements, 1,000 Grants and Quotas) is per pack, not per deployment. When the owner gives it (DR-16), the number goes into `nfr-recovery`, which `fr-reconciliation` already cites, and this item refers to it.

## 13. Pack state transition table

Serves `fr-versioning-status`, `fr-pack-issue`, `fr-pack-replace`, `fr-pack-revoke`, `fr-child-delegation` and `fr-parent-cascade`. Carries Y-012, Z-003 and DR-23, and old §6.3 item 7. Updated for the DR-04 version rule: every new version is "not applied" until its change is applied.

| From | To | Trigger | Requirement |
|---|---|---|---|
| (none), or the last applied version | New version: not applied (version 1 is also active) | An issue, a first delegate or a replace is accepted | `fr-pack-issue`, `fr-child-delegation`, `fr-pack-replace` |
| Version 1, not applied | Scheduled or Effective, by its dates | Every step of the first issue or first delegate is applied | `fr-pack-issue`, `fr-child-delegation` |
| Replacement, not applied | Effective, or Scheduled if its start is later; the base becomes Superseded | The replacement is applied | `fr-pack-replace`, `fr-versioning-status` |
| Replacement, not applied | Failed or preempted; never in force in License Manager's records; the base stays authoritative. A license-enforcement replace step that applied stays in force, and the next replace or revoke targets that Pack | The replacement fails, or a later replace or a revoke takes over | `fr-versioning-status` |
| Scheduled | Effective | `effective_from` is reached | `fr-versioning-status` |
| Effective | Expired; the lineage ends | `effective_until` is reached | `fr-versioning-status`, `fr-data-lifecycle` |
| Effective | Superseded at the new start | A staged replacement reaches its start (Phase 2) | `fr-scheduled-replace` |
| Effective | Effective, with a later end date | Renew | `fr-pack-renew` |
| Active | Suspended | Suspend, or a suspend of the parent pack | `fr-pack-suspend-reinstate`, `fr-parent-cascade` |
| Suspended | Active | Reinstate, or a reinstate of the parent pack | `fr-pack-suspend-reinstate`, `fr-parent-cascade` |
| Active or suspended, any version, a not-applied one included | Terminated, for the whole lineage | Revoke, tenant or child deletion, or a revoke of the parent pack | `fr-pack-revoke`, `fr-data-lifecycle`, `fr-parent-cascade` |
| Child version 1, not applied | Terminated, with its Child Delegation; the parent Quota is restored | The parent ends an unapplied first delegate, also while it runs | `fr-child-delegation` |

- Superseded and terminated are final. An Expired lineage has ended and is not renewed; a new pack needs a new issue.
- A replace never changes the admin state, so it never un-suspends a pack.
- A version that is not applied is never the lineage head. A replace names the last applied version. A replace against a lineage whose version 1 is not applied is rejected.

## 14. Not carried forward (summary)

DESIGN does not build these:

- a PDP health probe or a "known PDP outage" state (D2);
- its own audit store (D3);
- an alert lifecycle, or the acknowledge and close verbs (D3);
- a dependency on a platform notification channel (D3);
- an operator "tenant gone" confirm (D4);
- a mapping from settle to `refused` (D5);
- park clocks (D1).

## 15. Items added from the use-case and acceptance writer

- How a validation that cannot finish is detected (timeouts and limits), and how a later replace avoids waiting for it (`cpt-cf-license-manager-nfr-concurrency-safety`; Y-005).
- How a revoke finds and removes late effects of its lineage once reconciliation reports them (`cpt-cf-license-manager-fr-pack-revoke`; DR-10).
- The metric or health-signal name and the threshold for each operator condition (`cpt-cf-license-manager-nfr-correlated-outcome`; D3).

## 16. Items added after the PRD edits of 27 November 2026

These items follow the licensing catalog decision, the owner's R-07 decision and the sibling-review fixes R-01 to R-09 (`outputs/license-manager-prd-review.md`). Open points from re-review 3 (`outputs/license-manager-prd-rereview-3.md`) are marked as open.

- **Licensing catalog** (`fr-mapping-capacity-rule`, `fr-pinned-contracts`; catalog decision, 27 November).
  - For each application, the catalog holds its licensable features, their dependencies, and their Resource, Subject and Metric mappings. Each catalog version has an id, a version and a checksum.
  - The App Manager licensing section plugin and the licensing seed write through one write contract (G21, OQ-1).
  - Feature dependencies are resolved server-side. The pack version records the catalog version it used.
  - An uninstall or re-seed marks entries inactive and never deletes a version that a pack references.
- **Adoption of Phase 1 seat binds** (`fr-seat-assign`, `fr-reconciliation`; R-07).
  - At the Phase 2a rollout, one idempotent run adopts each binding of a License Manager seat Grant as a seat assignment, with one audit entry each.
  - A binding that breaks an assignment rule is reported and not adopted.
  - Until that run, reconciliation shows Tenant Administrator binds without drift or an operator signal.
- **Quota attribution and late creates** (`fr-idempotency-audit`, `fr-pack-revoke`, `nfr-pack-latency`, G3; R-03).
  - Read before create. The interim matching rule uses tenant, subject, metric, quota type, period and window, and a creation after the recorded intent.
  - An ambiguous match is reported and never adopted.
  - A Quota create still in flight when a revoke reads is removed within one reconciliation interval.
  - Open (re-review 3, R-22): how a revoke treats an ambiguous late Quota. DESIGN proposes how the revoke stays incomplete and escalates (`fr-stuck-steps`).
- **Quotas from other sources** (`fr-pack-issue`, `fr-reconciliation`, OQ-5; R-04). Report every active Quota from another `source` on a licensed tenant and metric in materialization health. Never adopt or remove it. It does not block an issue.
- **Grant settings in license-enforcement** (`fr-grant-materialization`, OQ-30; R-06).
  - Root Grants and sub-grants carry no acceptance requirement.
  - Seat Grants give the holder bind permission, with maximum depth zero in Phase 1.
- **Self-managed tenants** (`fr-pack-issue`, G33, OQ-45; R-08).
  - Until OQ-45 closes, reject an issue to a self-managed tenant up front, with a typed reason. DESIGN needs the tenant's barrier status at issue.
  - Open (re-review 3, R-20): the same check at replace.
