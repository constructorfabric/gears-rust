---
description: "Feature specification of the Settings Service gear's activation read: the per-setting read of which subscribers have confirmed the current value at a tenant, and the history read attaching each subscriber's activation report to the change it answers."
---

<!-- Created: 2026-10-31 by Virtuozzo International GmbH -->
<!-- Updated: 2026-10-31 by Virtuozzo International GmbH -->

# Feature: Activation Read

- [ ] `p1` - **ID**: `cpt-cf-settings-service-featstatus-activation-read`

- [ ] `p1` - `cpt-cf-settings-service-feature-activation-read`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Read a Setting's Activation](#read-a-settings-activation)
  - [Read the History with Attached Reports](#read-the-history-with-attached-reports)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Report Visibility](#report-visibility)
  - [Attachment](#attachment)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [The Activation Route](#the-activation-route)
  - [Authorization of the Read](#authorization-of-the-read)
  - [The Activation Response](#the-activation-response)
  - [Reports Attached to the History](#reports-attached-to-the-history)
  - [Activation Read Tests](#activation-read-tests)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Shows an administrator whether the consumers of a setting have taken its value up. `GET /settings-service/v1/settings/{key}/activation` answers, for the current value of a key at one tenant, one item per subscriber's activation state — `awaiting`, `succeeded` or `failed`, with the `change_ref` of the change it answers — and the history read of a setting attaches, beside each change record, the `activation` audit records in which the subscribers reported on that change, at every tenant the caller may read the key at. Both are reads over what entry 2.13 records; nothing here writes.

### 1.2 Purpose

"Did the value commit" is answered by the `set` response itself. "Did the consumers activate it" cannot be, because the wait for a consumer's report is unbounded, so it is read later, in two shapes (DESIGN-activation §4.3 *Why activation is read on its own*): as the state of the current value, and as the account of each change. Entry 2.13 keeps both — one activation state per subscription, and one `activation` audit record per report that settles a state — and this feature serves them.

Four properties shape the feature.

**Two reads, one account.** The activation read shows the state of the current value: which subscribers have confirmed it and which are still `awaiting`. The history shows each report under the change it answers. "Not confirmed" is the state of the current value: an expectation that a later change replaces before any confirmation leaves no record, and neither read reconstructs one (DESIGN-activation §4.2 *Activation Outcome Recorder*).

**Visible exactly where a read of the key is.** The activation read is decided as a value read of the key at the tenant: a caller denied the action is refused `403` whatever key and tenant it names, and the four cases an authorized caller can still not read — a tenant outside the caller's subtree (a standalone descendant and the tenants below it included), an unknown key, a declaration outside the caller's administrative domain, and a key hidden from the caller there — all answer the same `404` (DESIGN-activation §4.8). The history keeps a report only where the caller could read the key at the report's tenant.

**A report appears once in its change's history.** A report at the tenant the change was written at is a record of that same history; it is returned attached under its change, on whichever page the change falls, and never listed beside it as an ordinary record. A report at another tenant is an ordinary record of that tenant's history and is attached in the change's history too.

**Nothing here writes.** An administrator reads the account and cannot set, forge or clear a confirmation (PRD §5.5 *Consumer Activation: Notify, Confirm, and Account*).

Three details of the attachment are fixed here as refinements, and DESIGN-activation §4.3 *What each caller sees* records them: each attached item carries the report's `occurred_at`; the items under one record are ordered newest first, the report's `id` breaking a tie; and every listed record carries `activations`, an empty list when nothing answers it.

**Requirements**: `cpt-cf-settings-service-fr-consumer-activation` (extends, listed under entry 2.13: the service shows, for a setting's current value, which registered consumers have confirmed it and which have not), `cpt-cf-settings-service-fr-audit-mutations` (extends, listed under entry 2.6: each report recorded in the history under the change it answers), `cpt-cf-settings-service-nfr-scope-isolation` (extends, listed under entry 2.7: the `404` of the activation read and the confinement of attached reports to the caller's subtree)

**Principles**: None beyond those entry 2.13 lists (`cpt-cf-settings-service-feature-consumer-activation`).

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer` (extends, listed under entry 2.1: the read decision of the activation read), `cpt-cf-settings-service-constraint-audit-and-events` (extends, listed under entry 2.1: the history attachment reads the audit store)

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-platform-admin` | Reads the activation of any setting at any tenant it may read the key at, and the history with every report of the tenants below the root that are not standalone; platform operations read through this surface, since the PRD gives them no actor of their own |
| `cpt-cf-settings-service-actor-tenant-admin` | Reads the activation of a setting at its own tenant or a non-standalone descendant, and sees in a history only the reports of tenants in its own subtree |
| `cpt-cf-settings-service-actor-compliance-reviewer` | Reads the history, in which each report stands under the change it answers |
| `cpt-cf-settings-service-actor-authz-resolver` | Decides, through the `PolicyEnforcer`, the `read` decision the activation read takes exactly as a value read takes it |
| `cpt-cf-settings-service-actor-tenant-resolver` | Answers whether the requested tenant, and each report's tenant, lies inside the caller's subtree and outside any standalone tenant |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.5 Consumer Activation: Notify, Confirm, and Account (the account half and its read-only permissions); §5.7 Audited Mutations and Secret Reveals; §6.1 Scope Isolation; §2.2 System Actors (Consuming Gear: an unavailable consumer is visible as not having confirmed)
- **Design**: [DESIGN.md](../DESIGN.md) — §3.2 (*AuthZ*, *Feature/licence entitlement*, *Audit & events*), §4.3 (*Every endpoint below is one `OperationBuilder` declaration*, the DNA REST contract with its `page_info.total_count`; REST API — Search, History & Preferences, the history and activation rows and *History reads the gear's own audit store*; *Activation tracking*; Error Response Format), §4.7 (Table `audit_records`: `operation = activation`, `change_ref`, `detail`, `idx_audit_change_ref`), §4.8 (Authorization Model, the *Read activation state* row and *Administrative domain*; The Data Path), §7 (Feature Metrics, `settings_rest_request_duration_seconds` and `settings_authz_denial_total`)
- **Design (activation)**: [DESIGN-activation.md](../DESIGN-activation.md) — §4.2 (Component: Activation Outcome Recorder, `activation_status`, *Reports in the history*, *"Not confirmed" is the state of the current value*), §4.3 (whole: the route, *Authorization is the read's own*, *Refusals and failures*, *Licence*, *What each caller sees*), §4.7 (`activation_state` with `idx_activation_key_tenant`; *`audit_records` carries the activation records*), §4.8 (the authorization table and *`403` and `404` answer different questions*), §5.2 (*Free-text `detail`*), §7 (Fan-out bounds; Testing Architecture, the API level, the Level 3 rows, the Level 4 *Smoke without a broker* row and the read side of the *Full loop* and *Value mismatch → failed* rows)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.14
- **Dependencies**: entry 2.13 (`cpt-cf-settings-service-feature-consumer-activation`) for what this feature reads — the `activation_state` rows its record flow writes (`cpt-cf-settings-service-flow-consumer-activation-record`) and its report flow settles (`cpt-cf-settings-service-flow-consumer-activation-report`), whose `activation` audit record carries `change_ref` and `detail` (`inst-ca-report-13`), the states of `cpt-cf-settings-service-state-consumer-activation-row`, and the schema with `idx_activation_key_tenant` and `idx_audit_change_ref` (`cpt-cf-settings-service-dod-consumer-activation-schema`); entry 2.7 (`cpt-cf-settings-service-feature-tenant-access`) for the caller's effective access (`cpt-cf-settings-service-algo-tenant-access-resolve`), the rule that access gates the caller rather than the target (`cpt-cf-settings-service-dod-tenant-access-consumption`) and the standalone seam (`cpt-cf-settings-service-dod-tenant-access-standalone`); entry 2.6 (`cpt-cf-settings-service-feature-audit-store`) for the history read this feature extends (`cpt-cf-settings-service-flow-audit-store-history`, `cpt-cf-settings-service-dod-audit-store-history-read`), its masking (`cpt-cf-settings-service-dod-audit-store-masking-classification`) and the vocabulary contract of `operation` and `outcome`, under which a stored value the service does not know is an integrity error on the read (`cpt-cf-settings-service-dod-audit-store-table`). Touched, not depended on: entry 2.5, whose administrative read (`cpt-cf-settings-service-flow-value-resolution-admin-read`) is the decision the activation read reuses.
- **Design components**: extends `cpt-cf-settings-service-component-change-set-outcome-tracker` (the Activation Outcome Recorder, `activation_status`) and `cpt-cf-settings-service-component-audit-emitter` (the history read's attachment of `activation` records)
- **Sequences**: none — the read closes `cpt-cf-settings-service-seq-activation-change-set-notify`, which entry 2.13 covers
- **Performance**: the activation read is one index read through `idx_activation_key_tenant` returning at most the subscribers of one key at one tenant — at most 20 (DESIGN-activation §7 *Fan-out bounds*) — so it is a single resource within the REST latency target of DESIGN §7 (p95 at most 50 ms). The history keeps its scoped-query target (DESIGN §7, p95 at most 2 s over the online window): the attachment adds one statement per page through `idx_audit_change_ref` and one chain question per distinct report tenant on the page and one standalone question per tenant below the caller on those chains, cached for the page — never one per record. A change carries at most one report per affected subscription, at most the key's active subscriptions (at most 1,000), typically one or two. The worst case of one page is therefore the history's `limit` cap — the maximum page size of the DNA REST contract the history read takes (DESIGN §4.3) — times 1,000 attached items; it is accepted, since the per-key bound is a design anchor that the typical key stays far below, and the attached reports are unpaged by decision (entry 2.14, *Out of scope*).
- **Security**: neither read carries a value, an expected or applied value, or a hash. The `403` is decided before any key or tenant is looked at, so it discloses nothing about which exist; the four `404` cases — a tenant outside the caller's subtree (a standalone descendant and the tenants below it included), an unknown key, a declaration outside the caller's administrative domain, and a key hidden from the caller there — carry one problem document. A report is never shown above a standalone tenant or outside the caller's subtree. The activation read takes the licence seam of the value read and the history: no licence gate applies until the License Resolver lands, and the read joins that seam when it does (DESIGN-activation §4.3 *Licence*, `cpt-cf-settings-service-constraint-licence-entitlement-fail-closed`). The `subscriber` is a gear identity, recorded `public`, and is shown unmasked; the `detail` is consumer-supplied free text, capped at 1 KiB by entry 2.13 and documented by the SDK as free of values and secrets — that contract is its only control, and a value leaked into it is shown by both reads (DESIGN-activation §5.2). The activation read is a routine read and writes no audit record, as no non-secret read does (PRD §5.7).
- **Reliability and data integrity**: both reads are read-only, take no lock, and observe committed data only. The listing rule is part of the page query, so `limit`, the cursor and `page_info.total_count` agree on one predicate and a walk neither skips nor repeats a record. A Tenant Resolver or a database that cannot answer fails either read `503` rather than serving an answer unchecked or reports silently dropped, and a listed or attached report whose stored `outcome` the service does not know fails the history read as an integrity error rather than being shown with a guessed outcome.
- **Rollout, rollback and compile-time gate**: the feature adds no table, no column, no index and no migration — it reads what entry 2.13's migration adds. It adds one route and three additive fields on the history record: `activations` on every record, and `change_ref` and `detail` on an `activation` record. It changes one listing: a report at its change's own tenant leaves that history's ordinary listing and its `total_count`, and appears attached under its change. A rollback to entry 2.13's build removes the route and the fields and lists such a report as an ordinary record again, with no data to restore. The compile-time gate is the gear's own build and its regenerated OpenAPI document; the SDK is unchanged. Review owner: the settings-service maintainers.
- **Not applicable**: State machines, since this feature writes nothing (§4). Configuration, since the feature adds no configuration key. Extension points and health checks, since it adds none: the reads run on the gear's existing ports and the gear's existing health. User experience beyond the REST contract, since the console that renders the reads is not this gear's. Compliance beyond the baselines inherited from entries 2.6 and 2.13, since the reads expose only what those entries record under their classifications. No feature-local deviation from a shared baseline is taken: the `404` the activation read answers for a tenant outside the caller's subtree, where the value read and the history answer `403`, is DESIGN-activation §4.8's rule, and the attached item's `occurred_at`, its newest-first order and the always-present `activations` are refinements DESIGN-activation §4.3 records, not deviations.
- **Out of scope**: any write, set, forge or clearing of a confirmation; a view grouped by request, since activation is tracked per change and subscription; paging the attached reports beyond the pages the history read already serves; an account of an expectation that a later change replaced before any confirmation; the consumer activation SDK and everything that writes an activation state or a report (entry 2.13).

## 2. Actor Flows (CDSL)

Both flows are an administrator's read, named after the tenant administrator; a platform administrator reads the same way, with the root tenant as its own tenant.

**Use cases**: `cpt-cf-settings-service-usecase-review-and-set`, `cpt-cf-settings-service-usecase-review-audit-trail`

Throughout, a tenant is a bare lowercase UUID string, and platform scope is the root tenant's id. "The caller's subtree" is the caller's own tenant and every descendant of it that is neither standalone nor below a standalone tenant under the caller, as the Tenant Resolver marks them (`cpt-cf-settings-service-dod-tenant-access-standalone`). "A report" is an audit record with `operation` `activation`; "its change record" is the audit record whose `id` the report names as `change_ref`.

### Read a Setting's Activation

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-activation-read-status`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- One item per activation state of the key at the tenant — `subscriber`, `status`, `change_ref`, `reported_at` once the state is settled, and `detail` on a `failed` state — ordered by subscriber, with no value of any kind
- Without `tenant`, the read answers for the caller's own tenant
- A key nobody subscribed to at the tenant, or whose subscriptions no change has affected yet, answers `200` with no items
- A retired declaration's activation is read like an active one's: its subscriptions stay registered and its activation states stay as entry 2.13 left them

**Error Scenarios**:
- A malformed `key` or `tenant`, an OData option, or a `cursor` or `limit`: `400`, before any decision
- The caller denied the read, or the decision not obtainable: `403`, the same for every key and tenant, declared or not
- A tenant outside the caller's subtree (a standalone descendant and the tenants below it included), an unknown key, a declaration outside the caller's administrative domain, or a key hidden from the caller there: `404`, the four cases indistinguishable
- The Tenant Resolver or the database unavailable, at any step that asks it: `503`, never a `404`

**Steps**:
1. [x] - `p1` - Actor sends `GET /settings-service/v1/settings/{key}/activation` with an optional `tenant` - `inst-ar-status-1`
2. [x] - `p1` - **IF** `key` is not a setting key the gear's key parsing accepts, **OR** `tenant` is present and not a UUID, **OR** a `$filter`, `$orderby` or `$select` option is present, **OR** a `cursor` or `limit` is present → **RETURN** `400` invalid-argument, naming the parameter, before any decision is taken; every parameter the route does not take is refused by this one rule - `inst-ar-status-2`
3. [x] - `p1` - Take `tenant`, when omitted, as the caller's own tenant — the root tenant for a platform administrator — and normalise it to a bare lowercase UUID string - `inst-ar-status-3`
4. [x] - `p1` - Authorize `read` through the `PolicyEnforcer` — the decision a value read of the key and the history take — and obtain the caller's `AccessScope` - `inst-ar-status-4`
5. [x] - `p1` - **IF** the decision is a denial or cannot be obtained → **RETURN** `403`, before the key or the tenant is looked up, so the answer is the same whatever they name - `inst-ar-status-5`
6. [x] - `p1` - Ask the Tenant Resolver whether `tenant` lies inside the caller's subtree; **IF** it cannot answer → **RETURN** `503`, failing closed, before any `404` is decided - `inst-ar-status-6`
7. [x] - `p1` - **IF** `tenant` does not lie inside the caller's subtree → **RETURN** `404`, the answer an unknown key gets - `inst-ar-status-7`
8. [x] - `p1` - DB: SELECT the declaration by key; **IF** the read fails → **RETURN** `503`; **IF** none exists, **OR** it lies outside the caller's administrative domain — the `domain_affinity` constraint on the caller's `AccessScope` → **RETURN** `404`; a retired declaration passes this step and is read like an active one - `inst-ar-status-8`
9. [x] - `p1` - Evaluate the caller's effective tenant access for the key over the caller's own root-to-self chain (`cpt-cf-settings-service-algo-tenant-access-resolve`); **IF** the evaluation fails because the Tenant Resolver or the database cannot answer → **RETURN** `503`; **IF** it is `hidden` → **RETURN** `404` - `inst-ar-status-9`
10. [x] - `p1` - Invoke the Activation Outcome Recorder's `activation_status`: DB: SELECT the `activation_state` rows of the key and `tenant` through `idx_activation_key_tenant`, in one statement; **IF** the read fails → **RETURN** `503` - `inst-ar-status-10`
11. [x] - `p1` - **FOR EACH** row, in ascending order of `subscriber` → build one item with `subscriber`, `status`, `change_ref`, `reported_at` when the row is `succeeded` or `failed` and absent while it is `awaiting`, and `detail` only when the row is `failed`, leaving out `expected_value`, `applied_value`, `last_notified_at`, `updated_at` and every value or hash - `inst-ar-status-11`
12. [x] - `p1` - **RETURN** `200` with `tenant` as normalised and `items`, an empty list when no row exists; the answer is a single resource with no cursor and no `page_info` - `inst-ar-status-12`

### Read the History with Attached Reports

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-activation-read-history`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- Beside each record of the page, the reports that answer it, whatever tenant they sit at, kept where the caller may read the key at the report's tenant, newest first, each carrying `change_ref`, `actor`, `outcome`, `detail` on a failure, `tenant_id` and `occurred_at`
- A report at the tenant its change was written at appears once in that history, attached under its change on whichever page the change falls, and never also as an ordinary record
- A report at another tenant, or one whose change record retention has removed, is listed as an ordinary record in its tenant's history, carrying its `change_ref` and, on a failure, its `detail`
- `limit`, the cursor and `page_info.total_count` count the listed records only

**Error Scenarios**:
- The refusals of the history read of entry 2.6, unchanged: `400` for a malformed request, `403` for a denied decision or a target outside the caller's subtree, `404` for an unknown, hidden or out-of-domain key
- The Tenant Resolver cannot answer for a report's tenant, or the attachment's database read fails: `503`, and no page is served
- A listed or attached report whose stored `outcome` the service does not know: `500` as an integrity error, and no page is served (`cpt-cf-settings-service-dod-audit-store-table`)

**Steps**:
1. [x] - `p1` - Actor sends `GET /settings-service/v1/settings/{key}/history` with optional `tenant`, `cursor` and `limit`; the history read of entry 2.6 takes its gates unchanged (`inst-as-hist-1` to `inst-as-hist-6`), and this flow continues only for a caller they admitted, at the target tenant `X` they resolved - `inst-ar-history-1`
2. [x] - `p1` - Assemble the page as `inst-as-hist-7` does — the records of the key at `X` and the setting's definition records, newest first, cursor-paginated and bound to the pair — and leave out of it every report whose change record exists among the records of the same key at `X`, tested in the page query itself and never by filtering a page already cut, so a page is never shortened - `inst-ar-history-2`
3. [x] - `p1` - Keep in the listing a report whose change record retention has removed: no record of the key at `X` is its change record, so it is listed as an ordinary record - `inst-ar-history-3`
4. [x] - `p1` - Count `page_info.total_count` under the same predicate, so it counts the listed records and every page of one walk reports the same total - `inst-ar-history-4`
5. [x] - `p1` - Mask each listed record as `inst-as-hist-8` does; a report's actor is a gear identity recorded `public` and is shown as recorded - `inst-ar-history-5`
6. [x] - `p1` - Render on each listed `activation` record, whether its change was written at another tenant or its change record has left by retention, its `change_ref` and, when its `outcome` is `failure`, its `detail`, with no pre-value or post-value - `inst-ar-history-6`
7. [x] - `p1` - Invoke the attachment (`cpt-cf-settings-service-algo-activation-read-attach`) with the key, `X` and the page's listed records - `inst-ar-history-7`
8. [x] - `p1` - **IF** the attachment fails because the Tenant Resolver cannot answer for a report's tenant, **OR** its database read fails → **RETURN** `503`, serving no page - `inst-ar-history-8`
9. [x] - `p1` - **IF** a listed record's stored `outcome` is outside the known vocabulary, **OR** the attachment fails because an attached report's is → **RETURN** `500` as an integrity error, serving no page, as the history read does for any stored value it does not know (`cpt-cf-settings-service-dod-audit-store-table`) - `inst-ar-history-9`
10. [x] - `p1` - Set on every listed record its `activations`: the reports the attachment grouped under its `id`, an empty list when none - `inst-ar-history-10`
11. [x] - `p1` - **RETURN** `200` with the page — the fields of `inst-as-hist-9`, `activations` on every record, and `change_ref` and `detail` on an `activation` record — its cursors and `page_info.total_count`; an empty history is `200` with no items and a total of zero - `inst-ar-history-11`

## 3. Processes / Business Logic (CDSL)

### Report Visibility

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-activation-read-visibility`

**Input**: A report's tenant `T`, the history's target tenant `X`, the caller's tenant, and the key-level gates the history read has already passed for the caller — the `read` decision, the administrative domain and the caller's effective access, which `inst-as-hist-6` evaluates over the caller's own root-to-self chain (`cpt-cf-settings-service-algo-tenant-access-resolve`) and never over the target's, so they hold for the caller at every tenant of the page as they hold at `X`, by analogy with the rule that access gates the caller rather than the target (`cpt-cf-settings-service-dod-tenant-access-consumption`)

**Output**: Keep the report, drop it, or a failure when the Tenant Resolver cannot answer. The key's visibility to the caller at `T` is the decision the history already took, so only the tenant is tested here; a report at a tenant the caller's `AccessScope` does not cover never reaches this algorithm, since the attachment's lookup runs on that scope (`inst-ar-attach-2`)

**Steps**:
1. [x] - `p1` - **IF** `T` is `X` → **RETURN** keep: the history's own gate (`inst-as-hist-4`) admitted `X` and its key gates admitted the key - `inst-ar-visibility-1`
2. [x] - `p1` - Ask the Tenant Resolver for the ancestor chain of `T`, root to self, once per distinct report tenant of the page - `inst-ar-visibility-2`
3. [x] - `p1` - **IF** the Tenant Resolver cannot answer → **RETURN** a failure, so the read is refused rather than served with the report kept unchecked or dropped unseen - `inst-ar-visibility-3`
4. [x] - `p1` - **IF** the caller's tenant is not on the chain → **RETURN** drop: `T` lies outside the caller's subtree, as a sibling's or an ancestor's tenant does - `inst-ar-visibility-4`
5. [x] - `p1` - **IF** a tenant on the chain below the caller's tenant, `T` included, is standalone → **RETURN** drop: nothing of a standalone tenant's own state is shown from above it - `inst-ar-visibility-5`
6. [x] - `p1` - **RETURN** keep: `T` lies inside the caller's subtree and the key-level gates hold for the caller there - `inst-ar-visibility-6`

### Attachment

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-activation-read-attach`

**Input**: The key, the history's target tenant `X`, the page's listed records, and the caller's security context and `AccessScope`

**Output**: For each listed record, the ordered reports attached to it; or a failure — the Tenant Resolver could not answer for a report's tenant, the database read failed, or a report's stored `outcome` is outside the known vocabulary. The attached reports are unpaged and count toward no `limit` and no `total_count`, and a report is never both attached and listed in one history: one at `X` is left out of the listing by the history's page query (`inst-ar-history-2`), and one at another tenant is not a record of the history of `X`

**Steps**:
1. [x] - `p1` - Collect the `id` of every listed record of the page; **IF** the page is empty → **RETURN** no attachments - `inst-ar-attach-1`
2. [x] - `p1` - DB: SELECT `audit_records` with `operation` `activation`, `declaration_key` the key and `change_ref` among the collected ids, whatever their `tenant_id`, through `idx_audit_change_ref`, on the caller's `AccessScope` as every audit read runs — one statement for the page, never one per record; **IF** the read fails → **RETURN** the failure - `inst-ar-attach-2`
3. [x] - `p1` - **IF** a report found carries a stored `outcome` outside the known vocabulary → **RETURN** an integrity failure, never a report with a guessed outcome or one silently dropped - `inst-ar-attach-3`
4. [x] - `p1` - **FOR EACH** report found → apply report visibility (`cpt-cf-settings-service-algo-activation-read-visibility`) at its `tenant_id`; drop the reports it drops, and **IF** it fails → **RETURN** the failure - `inst-ar-attach-4`
5. [x] - `p1` - Group the kept reports by `change_ref` under the listed record whose `id` it is; a definition record, a `secret_use` or `stage` record and a listed report are named by no report and receive none - `inst-ar-attach-5`
6. [x] - `p1` - Order each group newest first by `occurred_at`, ties broken by the report's `id`, descending - `inst-ar-attach-6`
7. [x] - `p1` - Build each attached item with `change_ref`, `actor` (the subscriber, under the actor masking of `inst-as-hist-8`), `outcome`, `detail` only when `outcome` is `failure`, `tenant_id` and `occurred_at`, and with no pre-value, post-value or `request_id` - `inst-ar-attach-7`
8. [x] - `p1` - **RETURN** the groups, keyed by the listed records' ids, every kept report of a change on the page attached - `inst-ar-attach-8`

## 4. States (CDSL)

Not applicable. This feature writes nothing and owns no lifecycle. The activation state it shows moves through `cpt-cf-settings-service-state-consumer-activation-row` of entry 2.13, and an `activation` audit record is appended once and never transitions (entry 2.6).

## 5. Definitions of Done

### The Activation Route

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-activation-read-route`

The system **MUST** declare `GET /settings-service/v1/settings/{key}/activation` as one `OperationBuilder` declaration: `operation_id` `settings_service.get_setting_activation`, a summary and description, the tag of the gear's settings routes, `.authenticated()`, never anonymous, the licence seam of the value read and the history — no licence gate until the License Resolver lands, the read joining that seam when it does (DESIGN-activation §4.3 *Licence*) — the `key` path parameter as a URL-encoded GTS type id, the optional `tenant` query parameter, the activation response schema at `200`, and registered error responses for `400`, `403`, `404`, `500` and `503`, as RFC 9457 problem documents mapped from the gear's canonical error categories. The route **MUST** be declared for `GET` only, so a request with any other method on its path is refused and reaches no read. `OpenApiRegistry` **MUST** collect it with the gear's other routes, and the OpenAPI document **MUST** be regenerated and committed with the route and the history record's `activations`, `change_ref` and `detail`. The route **MUST** add no metric of its own: it is observed by the REST metrics DESIGN §7 names for every route — `settings_rest_request_duration_seconds` and, for its `403`, `settings_authz_denial_total` — and logs nothing at `info` or `warn` beyond what every route of the surface logs. No log line, metric label or span attribute of either read **MUST** carry a `detail`, a value or a hash.

**Implements**:
- `cpt-cf-settings-service-flow-activation-read-status`
- `cpt-cf-settings-service-flow-activation-read-history`
- `cpt-cf-settings-service-component-change-set-outcome-tracker` — `activation_status` behind the route

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`, `cpt-cf-settings-service-constraint-licence-entitlement-fail-closed` (the seam the read joins when the License Resolver lands)

**Touches**:
- API: `GET /settings-service/v1/settings/{key}/activation`; the history record schema of `GET /settings-service/v1/settings/{key}/history`
- Entities: `OperationBuilder`, `OpenApiRegistry`, the regenerated OpenAPI document

### Authorization of the Read

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-activation-read-authorization`

The activation read **MUST** refuse a malformed `key` or `tenant`, any OData option, and a `cursor` or `limit` — one rule for every parameter it does not take — `400` before any decision. It **MUST** then take the `read` decision through the `PolicyEnforcer` that a value read of the key takes, and **MUST** answer a denial or an undecidable decision `403` before the key or the tenant is looked up, so the answer is the same for every key and tenant, declared or not. Only an authorized caller **MUST** reach the second question, answered `404` with one problem document for each of four cases: a tenant outside the caller's subtree, a standalone descendant and the tenants below it included; an unknown key; a declaration outside the caller's administrative domain; and a key whose effective access for the caller is `hidden`. A retired declaration **MUST** be readable. `tenant` **MUST** default to the caller's own tenant and be normalised to a bare lowercase UUID string. A Tenant Resolver that cannot answer — at the subtree test or inside the evaluation of effective access — and a database that cannot answer — at the declaration read, the effective-access read or the `activation_state` read — **MUST** answer `503`, failing closed, and the subtree test's `503` **MUST** be decided before its `404`; an integrity fault of a declaration or restriction row that the read looks at is answered `503` too, the history keeping its own mapping of it. Both reads **MUST** be read-only: neither writes an audit record or an activation state, and no route of this feature sets, forges or clears a confirmation.

**Implements**:
- `cpt-cf-settings-service-flow-activation-read-status`

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- API: `GET /settings-service/v1/settings/{key}/activation`
- Entities: `PolicyEnforcer`, `AccessScope`, the Tenant Resolver's subtree and standalone marking, effective tenant access

### The Activation Response

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-activation-read-response`

The activation read **MUST** answer `200` with `tenant`, as normalised, and `items`: one item per `activation_state` row of the key and the tenant, read in one statement through `idx_activation_key_tenant` and ordered by `subscriber`, each carrying `subscriber`, `status`, `change_ref`, `reported_at` when the row is `succeeded` or `failed` and absent while it is `awaiting`, and `detail` only when the row is `failed`. It **MUST NOT** carry `expected_value`, `applied_value`, `last_notified_at`, `updated_at` or any value or hash. A key with no row at the tenant — nobody subscribed there, or no change has affected its subscriptions — **MUST** answer `200` with no items. The answer **MUST** be a single resource with no cursor and no `page_info`. The `status` spellings `awaiting`, `succeeded` and `failed` **MUST** be the stored and the wire form and permanent once shipped, and the route's description **MUST** tell clients to treat them as an open set.

**Implements**:
- `cpt-cf-settings-service-flow-activation-read-status`
- `cpt-cf-settings-service-component-change-set-outcome-tracker` — `activation_status`

**Constraints**: `cpt-cf-settings-service-constraint-activation-wait-for-all-unbounded` (an `awaiting` item stays listed until a report or a later change, with no deadline)

**Touches**:
- DB: `activation_state` (`idx_activation_key_tenant`)
- Entities: `ActivationState`, `ActivationStatus`, the activation response and its per-subscriber item

### Reports Attached to the History

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-activation-read-history`

The history read **MUST** leave out of its listing, in the page query and its count, every report whose change record exists among the records of the same key at the history's tenant, so `limit`, the cursor and `page_info.total_count` count listed records only and every page of a walk reports the same total; a report whose change record has left by retention **MUST** be listed as an ordinary record. For the records of each page it **MUST** look up, in one statement through `idx_audit_change_ref` on the caller's `AccessScope`, the reports whose `change_ref` is a listed record's `id`, whatever their tenant, and **MUST** keep a report only where its tenant lies inside the caller's subtree and outside any standalone tenant, the key's visibility to the caller being the decision the history already took for the caller. Every listed record **MUST** carry `activations` — its kept reports, newest first by `occurred_at` with the report's `id` breaking a tie, each with `change_ref`, `actor`, `outcome`, `detail` on a failure, `tenant_id` and `occurred_at`, an empty list when none — and a listed report, whether its change was written at another tenant or its change record has left by retention, **MUST** carry its `change_ref` and, on a failure, its `detail`. A report **MUST NOT** be both attached and listed in one history, and attached reports **MUST NOT** be paged or counted. A Tenant Resolver that cannot answer for a report's tenant, or a database that cannot answer the attachment's lookup, **MUST** fail the read `503`, and a listed or attached report whose stored `outcome` is outside the known vocabulary **MUST** fail it as an integrity error (`cpt-cf-settings-service-dod-audit-store-table`); in either case no page is served.

**Implements**:
- `cpt-cf-settings-service-flow-activation-read-history`
- `cpt-cf-settings-service-algo-activation-read-attach`
- `cpt-cf-settings-service-algo-activation-read-visibility`
- `cpt-cf-settings-service-component-audit-emitter` — the history read's attachment of `activation` records

**Constraints**: `cpt-cf-settings-service-constraint-audit-and-events`, `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- API: `GET /settings-service/v1/settings/{key}/history`
- DB: `audit_records` (`idx_audit_scoped`, `idx_audit_change_ref`)
- Entities: `AuditRecord`, the attached activation item

### Activation Read Tests

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-activation-read-tests`

The feature **MUST** ship with these tests, each implementing a row of DESIGN-activation §7 or a rule of this document where no row exists:

- API, through `Router::oneshot` over the gear's in-memory SQLite harness as the gear's other REST tests run, with the `PolicyEnforcer` allowing or denying and no broker: every Level 3 row of DESIGN-activation §7 — *Activation read* (one item per subscriber, no value, without `tenant` the caller's own, ordered by subscriber, `reported_at` absent while `awaiting`, `detail` only on `failed`, `200` with no items for a key nobody subscribed to, a retired declaration readable); *Activation read — out of subtree or hidden key*, extended to the four cases of the authorization definition of done — a tenant outside the caller's subtree, at a sibling, at a standalone descendant and at a tenant below it, an unknown key, a declaration outside the caller's administrative domain (at the level of the read itself, since the value resource declares no PEP property a policy could constrain a request on) and a key hidden from the caller — every case answering one problem document; *AuthZ deny — the action*, for a declared key, an undeclared key and a tenant outside the subtree alike; *History shows reports under the change*; *History attaches descendant reports*, including the same report listed in the descendant's history with its `change_ref` and `detail`; *History at the written tenant*, including the page-size-1 walk and a `total_count` that does not count the attached report; and *RFC 9457 errors* for `400`, `403`, `404`, `500` and `503`. The history rows pass their target `X` as `tenant`, the history read's parameter — the scope's tenant id, the root tenant's id for platform scope — wherever the designs name it the scope. Where no row exists: a malformed `key` or `tenant`, an OData option, and a `cursor` or `limit` answering `400`; a Tenant Resolver failing at the subtree test, inside the effective-access evaluation and for a report's tenant, and a database failing at the declaration read and the `activation_state` read, each answering `503` and never `404`; a listed and an attached report whose stored `outcome` is outside the known vocabulary each failing the history read as an integrity error with no page served; a report whose change record has been pruned listed as an ordinary record; a `secret`-trait key's states read with no hash; attached reports ordered newest first, the report's `id` breaking a tie of `occurred_at`, with `detail` on a failure only and the subscriber unmasked; the history's own `403` and `404` unchanged; a method other than `GET` on the activation path refused; no `detail`, value or hash in a log line, metric label or span attribute; and the gear's statement recorder showing one read of `activation_state` per activation read, one attachment lookup per history page whatever the page's size, and no write — no audit record, no activation state — by either read.
- Gate, on the regenerated OpenAPI document: the route under its `operation_id`, `.authenticated()` with no licence requirement and its registered errors, a description telling clients to treat `status` as an open set, and no metric declared for the route of its own.
- Integration, on real PostgreSQL through testcontainers: the listing rule and the attachment lookup over the partial `idx_audit_change_ref`, a change at `X` with reports at `X` and at a descendant, walked with page size 1, giving the same pages and totals as the API rows.
- E2E, environment-bound and not part of what the gear's unit and API suites ship: the *Smoke without a broker* row of DESIGN-activation §7 Level 4, in the gear's smoke suite when such a suite exists — the activation read of a declared key answering `200` with no items, of an undeclared key `404` as a problem document, and of an anonymous caller `401`; and, in an environment that runs a broker and a sample consumer gear, the read side of the Level 4 rows entry 2.13 leaves here — *Full loop*, the activation read showing `succeeded` and the history showing the report under the change; *Value mismatch → failed*, the activation read showing `failed` with its `detail` and the history recording the failure under the change.

**Implements**:
- `cpt-cf-settings-service-flow-activation-read-status`
- `cpt-cf-settings-service-flow-activation-read-history`
- `cpt-cf-settings-service-algo-activation-read-visibility`
- `cpt-cf-settings-service-algo-activation-read-attach`

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- Entities: `Router::oneshot`, the gear's in-memory SQLite harness and statement recorder, the `PolicyEnforcer` test decision point, the testcontainers PostgreSQL, the gear's E2E smoke suite, the broker environment with a sample consumer gear

## 6. Acceptance Criteria

- [x] With subscribers `A` `awaiting`, `B` `succeeded` and `C` `failed` on a key at a tenant, the activation read answers `200` with three items ordered `A`, `B`, `C`, each carrying `subscriber`, `status` and `change_ref`, `reported_at` on `B` and `C` only, `detail` on `C` only, and no value, expected value, applied value or hash (API)
- [x] Without `tenant`, the activation read answers for the caller's own tenant, and the `tenant` it returns is a bare lowercase UUID string whatever case the request used (API)
- [x] A key nobody subscribed to at the tenant, and a key whose subscriptions no change has affected, answer `200` with no items and no `page_info` (API)
- [x] The activation read of a retired declaration answers `200` with its states (API)
- [x] A malformed `key`, a `tenant` that is not a UUID, a `$filter`, `$orderby` or `$select` option, and a `cursor` or `limit` answer `400` problem documents before any decision is taken (API)
- [x] A caller the `PolicyEnforcer` denies receives `403` for a declared key, an undeclared key and a tenant outside its subtree alike (API)
- [x] Each of the four `404` cases answers `404` — a tenant outside the caller's subtree, tested at a sibling, at a standalone descendant and at a tenant below it; an unknown key; a declaration outside the caller's administrative domain; and a key `hidden` for the caller — and the problem documents differ in nothing but `trace_id` (API; the administrative-domain case at the level of the read itself, since the value resource declares no PEP property a policy could constrain a request on)
- [x] A Tenant Resolver that cannot answer, at the subtree test or inside the effective-access evaluation, and a database that cannot answer, at the declaration read or the `activation_state` read, each fail the activation read `503`, never `404` (API)
- [x] For a `secret`-trait key with a `succeeded` and a `failed` state, the activation read carries neither the expected hash nor the applied hash, and the gear's statement recorder shows one read of `activation_state` per request (API)
- [x] Neither read writes: across an activation read and a history read the gear's statement recorder shows no write of an audit record or an activation state; and a `PUT`, `POST`, `PATCH` or `DELETE` on the activation path is refused without reaching the read (API)
- [x] The route is collected by `OpenApiRegistry` under `operation_id` `settings_service.get_setting_activation`, declared `.authenticated()` with no licence requirement and its registered errors, its description tells clients to treat `status` as an open set and the route declares no metric of its own, and the regenerated OpenAPI document carries it and the history record's `activations`, `change_ref` and `detail` (gate)
- [x] After a change at `X` with subscribers at `X` and at a descendant `D`, both reporting, the administrator of `X` reading the history of `X` sees both reports under the change, while the administrator of a sibling of `D` sees neither of `D`'s reports in any history it may read (API)
- [x] The history of `X` returns the report at `D` beside the change record whose `id` it names, with `change_ref`, `actor`, `outcome`, `detail` on a failure, `tenant_id` and `occurred_at`, and the history of `D` lists the same report as an ordinary record carrying its `change_ref` and `detail` (API)
- [x] After a change at `X` with a subscriber at `X` that reported, the history of `X` returns the report once, attached under its change and not listed; with page size 1 and a newer record of the key at `X`, page 1 holds the newer record alone, page 2 holds the change with the report attached, and `total_count` is the same on every page and does not count the report (API, integration)
- [x] A report at a standalone descendant of the caller is attached in no history the caller reads, and a Tenant Resolver failing for a report's tenant fails the history read `503` (API)
- [x] A report whose change record retention has removed is listed as an ordinary record in its tenant's history (API)
- [x] A listed report and an attached report whose stored `outcome` is outside the known vocabulary each fail the history read as an integrity error, `500`, with no page served (API)
- [x] Two reports under one change are attached newest first, and two with the same `occurred_at` in the order of their `id`; a `success` report carries no `detail` and a `failure` report its `detail`; no attached item carries a pre-value, a post-value or a `request_id`; the subscriber is shown unmasked to a caller without the PII entitlement (API)
- [x] The history read keeps its own refusals: a target outside the caller's subtree or standalone answers `403` and a hidden key `404`, as entry 2.6 defines them (API)
- [x] A history page of any size makes one attachment lookup, and every record of a page carries `activations`, an empty list when nothing answers it (API)
- [ ] In the gear's smoke suite without a broker, the activation read of a declared key answers `200` with no items, of an undeclared key `404`, and of an anonymous caller `401` (E2E)
- [ ] In an environment that runs a broker and a sample consumer gear, after a set and the consumer's report, the activation read shows `succeeded` and the history shows the report under the change; after a wrong value reported on the current `change_ref`, the activation read shows `failed` with its `detail` and the history records the failure under the change (E2E)
- [x] No log line, metric label or span attribute of either read carries a `detail`, a value or a hash (API)
- [x] `cfs validate --artifact` reports zero errors on this document, and every `@cpt-*` marker the implementation adds resolves to an id defined here (gate)
