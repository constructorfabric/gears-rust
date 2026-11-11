---
description: "Feature specification of the Settings Service gear's activation read: the per-setting read of how the instances of each subscriber stand on the current value at a tenant — the derived outcome, the N of M counts and the per-instance list — and the history read attaching each instance's activation report, and each call of silence, to the change it answers."
---

<!-- Created: 2026-10-31 by Virtuozzo International GmbH -->
<!-- Updated: 2026-11-09 by Virtuozzo International GmbH -->

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
  - [Outcome Derivation](#outcome-derivation)
  - [Identity Disclosure](#identity-disclosure)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [The Activation Route](#the-activation-route)
  - [Authorization of the Read](#authorization-of-the-read)
  - [The Activation Response](#the-activation-response)
  - [Reports Attached to the History](#reports-attached-to-the-history)
  - [The Instances of an Item](#the-instances-of-an-item)
  - [Instance Identities](#instance-identities)
  - [Activation Read Tests](#activation-read-tests)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Shows an administrator whether the consumers of a setting have taken its value up, instance by instance. `GET /settings-service/v1/settings/{key}/activation` answers, for the current value of a key at one tenant, one item per subscriber: the `change_ref` of the change it answers, the derived outcome of that change — `awaiting`, `succeeded`, `failed` or `incomplete` — the counts "N of M" (`confirmed` of `expected`, with `failed`, `silent` and `silent_excluded`), and the subscriber's instances, each with its status — `awaiting`, `succeeded`, `failed` or `silent` — including the silent instances the change left out, flagged `excluded`. The history read of a setting attaches, beside each change record, the `activation` audit records in which instances reported on that change or were called silent for it, at every tenant the caller may read the key at. The instances' identities (`instance_id`, `instance_label`) are shown to a root-tenant reader only; every other reader sees the same counts and states without them. Both are reads over what entry 2.13 records; nothing here writes.

### 1.2 Purpose

"Did the value commit" is answered by the `set` response itself. "Did the consumers activate it" cannot be, because every instance answers asynchronously and the wait is bounded only by the report timeout, counted in deliveries the broker accepted ([ADR-004](../ADR/ADR-004-per-instance-acknowledgement-bounded-wait.md)), so it is read later, in two shapes (DESIGN-activation §4.3 *Why activation is read on its own*): as the state of the current value, and as the account of each change. Entry 2.13 keeps both — one activation state per instance's subscription, and one `activation` audit record per report that settles a state and per call of silence — and this feature serves them.

Five properties shape the feature.

**Two reads, one account.** The activation read shows the state of the current value: per subscriber, the outcome derived over its instances, how many of the expected instances have confirmed it ("N of M"), and which are still `awaiting`, which are `silent` and which the change left out because they were silent. The history shows each instance's report, and each call of silence, under the change it answers. "Not confirmed" is the state of the current value: an expectation that a later change replaces before any confirmation or call of silence leaves no record, and neither read reconstructs one (DESIGN-activation §4.2 *Activation Outcome Recorder*).

**Counts for every reader, identities for the platform.** An instance's `instance_id` and `instance_label` name a process and a host of a platform gear, which a tenant administrator can neither act on nor explain. They are shown only to a caller whose reader tenant is the root and are absent — not masked — for every other caller, in both reads; the outcome, the counts, each entry's status, `excluded` flag, times and failure detail, and the order of the entries are the same for every reader (DESIGN-activation §4.3 *What each caller sees*, the reader-tenant rule of the upward disclosure boundary).

**Visible exactly where a read of the key is.** The activation read is decided as a value read of the key at the tenant: a caller denied the action is refused `403` whatever key and tenant it names, and the four cases an authorized caller can still not read — a tenant outside the caller's subtree (a standalone descendant and the tenants below it included), an unknown key, a declaration outside the caller's administrative domain, and a key hidden from the caller there — all answer the same `404` (DESIGN-activation §4.8). The history keeps a report only where the caller could read the key at the report's tenant.

**A report appears once in its change's history.** A report at the tenant the change was written at is a record of that same history; it is returned attached under its change, on whichever page the change falls, and never listed beside it as an ordinary record. A report at another tenant is an ordinary record of that tenant's history and is attached in the change's history too.

**Nothing here writes.** An administrator reads the account and cannot set, forge or clear a confirmation (PRD §5.5 *Consumer Activation: Notify, Confirm, and Account*).

Three details of the attachment are fixed here as refinements, and DESIGN-activation §4.3 *What each caller sees* records them: each attached item carries the report's `occurred_at`; the items under one record are ordered newest first, the report's `id` breaking a tie; and every listed record carries `activations`, an empty list when nothing answers it. A call of silence is such an item too, with `outcome` `silent` and no `detail`, and it stays under its change after a later change supersedes it or the instance is forgotten.

**Requirements**: `cpt-cf-settings-service-fr-consumer-activation` (extends, listed under entry 2.13: the service shows, for a setting's current value, how many of the expected instances have confirmed it and which instance confirmed, failed or stayed silent, the tenant administrator seeing the counts and states and not the identities), `cpt-cf-settings-service-fr-audit-mutations` (extends, listed under entry 2.6: each report and each call of silence recorded in the history under the change it answers), `cpt-cf-settings-service-nfr-scope-isolation` (extends, listed under entry 2.7: the `404` of the activation read and the confinement of attached reports to the caller's subtree)

**Principles**: None beyond those entry 2.13 lists (`cpt-cf-settings-service-feature-consumer-activation`), `cpt-cf-settings-service-principle-unbounded-wait` among them, superseded by ADR-004: the wait the read shows is bounded per instance by the report timeout.

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer` (extends, listed under entry 2.1: the read decision of the activation read), `cpt-cf-settings-service-constraint-audit-and-events` (extends, listed under entry 2.1: the history attachment reads the audit store)

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-platform-admin` | Reads, with the instances' identities (`instance_id`, `instance_label`) as the reader whose tenant is the root, the activation of any setting at any tenant it may read the key at, and the history with every report of the tenants below the root that are not standalone; platform operations read through this surface, since the PRD gives them no actor of their own |
| `cpt-cf-settings-service-actor-tenant-admin` | Reads the activation of a setting at its own tenant or a non-standalone descendant — the outcome, the counts and each instance's state, never the instances' identities — and sees in a history only the reports of tenants in its own subtree |
| `cpt-cf-settings-service-actor-compliance-reviewer` | Reads the history, in which each report, and each call of silence, stands under the change it answers |
| `cpt-cf-settings-service-actor-authz-resolver` | Decides, through the `PolicyEnforcer`, the `read` decision the activation read takes exactly as a value read takes it |
| `cpt-cf-settings-service-actor-tenant-resolver` | Answers whether the requested tenant, and each report's tenant, lies inside the caller's subtree and outside any standalone tenant |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.5 Consumer Activation: Notify, Confirm, and Account (the account half, the per-instance counting and outcome precedence, and the read visibility: counts and states for a tenant administrator, identities for the platform administrator only); §5.7 Audited Mutations and Secret Reveals; §6.1 Scope Isolation; §2.2 System Actors (Consuming Gear: an unavailable consumer is visible as not having confirmed)
- **Design**: [DESIGN.md](../DESIGN.md) — §3.2 (*AuthZ*, *Feature/licence entitlement*, *Audit & events*), §4.3 (*Every endpoint below is one `OperationBuilder` declaration*, the DNA REST contract with its `page_info.total_count`; REST API — Search, History & Preferences, the history and activation rows and *History reads the gear's own audit store*; *Activation tracking*; Error Response Format), §4.7 (Table `audit_records`: `operation = activation`, `change_ref`, `detail`, `idx_audit_change_ref`), §4.8 (Authorization Model, the *Read activation state* row and *Administrative domain*; The Data Path), §7 (Feature Metrics, `settings_rest_request_duration_seconds` and `settings_authz_denial_total`)
- **Design (activation)**: [DESIGN-activation.md](../DESIGN-activation.md) — §4.1 (`ActivationState`, `Subscription`, *Enums*: `ActivationStatus` and `ChangeOutcome` with its precedence), §4.2 (Component: Activation Outcome Recorder, `activation_status`, *Reports in the history*, *"Not confirmed" is the state of the current value*), §4.3 (whole: the route, the response shape and its example,  *Authorization is the read's own*, *Refusals and failures*, *Licence*, *What each caller sees*), §4.7 (`activation_state` with `idx_activation_key_tenant`; `subscriptions` with the partial `idx_sub_silent`; *`audit_records` carries the activation records*), §4.8 (the authorization table and *`403` and `404` answer different questions*), §5.2 (*Free-text `detail`*), §7 (Fan-out bounds; Testing Architecture, the API level, the Level 3 rows, the Level 4 *Smoke without a broker* row and the read side of the *Full loop* and *Value mismatch → failed* rows)
- **ADR**: [ADR-004](../ADR/ADR-004-per-instance-acknowledgement-bounded-wait.md) (`cpt-cf-settings-service-adr-per-instance-acknowledgement-bounded-wait`) — the account is per instance and the wait is bounded by the report timeout; it supersedes `cpt-cf-settings-service-principle-unbounded-wait` and `cpt-cf-settings-service-constraint-activation-wait-for-all-unbounded`
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.14
- **Dependencies**: entry 2.13 (`cpt-cf-settings-service-feature-consumer-activation`) for what this feature reads — the per-instance `activation_state` rows and `subscriptions` its record flow writes (`cpt-cf-settings-service-flow-consumer-activation-record`) and its report flow settles (`cpt-cf-settings-service-flow-consumer-activation-report`), whose `activation` audit record carries `change_ref` and `detail` (`inst-ca-report-13`), the states of `cpt-cf-settings-service-state-consumer-activation-row`, and the schema with `idx_activation_key_tenant` and `idx_audit_change_ref` (`cpt-cf-settings-service-dod-consumer-activation-schema`); entry 2.7 (`cpt-cf-settings-service-feature-tenant-access`) for the caller's effective access (`cpt-cf-settings-service-algo-tenant-access-resolve`), the rule that access gates the caller rather than the target (`cpt-cf-settings-service-dod-tenant-access-consumption`) and the standalone seam (`cpt-cf-settings-service-dod-tenant-access-standalone`); entry 2.6 (`cpt-cf-settings-service-feature-audit-store`) for the history read this feature extends (`cpt-cf-settings-service-flow-audit-store-history`, `cpt-cf-settings-service-dod-audit-store-history-read`), its masking (`cpt-cf-settings-service-dod-audit-store-masking-classification`) and the vocabulary contract of `operation` and `outcome`, under which a stored value the service does not know is an integrity error on the read (`cpt-cf-settings-service-dod-audit-store-table`). Touched, not depended on: entry 2.5, whose administrative read (`cpt-cf-settings-service-flow-value-resolution-admin-read`) is the decision the activation read reuses.
- **Design components**: extends `cpt-cf-settings-service-component-change-set-outcome-tracker` (the Activation Outcome Recorder, `activation_status`) and `cpt-cf-settings-service-component-audit-emitter` (the history read's attachment of `activation` records)
- **Sequences**: none — the read closes `cpt-cf-settings-service-seq-activation-change-set-notify` and shows the account `cpt-cf-settings-service-seq-activation-report-timeout` produces (the silent instances), both of which entry 2.13 covers
- **Performance**: the activation read is two indexed reads — the `activation_state` rows of `(key, tenant)` joined to their subscriptions for `instance_label` through `idx_activation_key_tenant`, and the silent subscriptions of `(key, tenant)` through the partial `idx_sub_silent` — returning the instances of the key's subscribers at one tenant: the live subscriber instances of one key at one tenant (DESIGN-activation §7 *Fan-out bounds*) plus the ghosts not yet forgotten, and at most `MAX_INSTANCES_PER_SUBSCRIPTION` (64) entries per item, ghosts included (DESIGN-activation §4.3 *Size*); outcome, counts and order are computed in memory, so it is a single resource within the REST latency target of DESIGN §7 (p95 at most 50 ms). The history keeps its scoped-query target (DESIGN §7, p95 at most 2 s over the online window): the attachment adds one statement per page through `idx_audit_change_ref` and one chain question per distinct report tenant on the page and one standalone question per tenant below the caller on those chains, cached for the page — never one per record. A change carries at most 1,000 settling reports (one per instance subscription it expected, at most the key's active subscriptions) plus one `silent` record per call of silence; re-arms are not bounded by design, so the figure is typical, not a cap. The worst case of one page is therefore the history's `limit` cap — the maximum page size of the DNA REST contract the history read takes (DESIGN §4.3) — times 1,000 attached items; it is accepted, since the per-key bound is a design anchor that the typical key stays far below, and the attached reports are unpaged by decision (entry 2.14, *Out of scope*).
- **Security**: neither read carries a value, an expected or applied value, or a hash, and neither shows an instance's identity to a reader whose tenant is not the root: `instance_id` and `instance_label` are absent — not masked — from every entry of the activation read, the excluded entries included, and from every attached item and listed `activation` record of the history, while the counts and states stay (DESIGN-activation §4.3 *What each caller sees*). The `403` is decided before any key or tenant is looked at, so it discloses nothing about which exist; the four `404` cases — a tenant outside the caller's subtree (a standalone descendant and the tenants below it included), an unknown key, a declaration outside the caller's administrative domain, and a key hidden from the caller there — carry one problem document. A report is never shown above a standalone tenant or outside the caller's subtree. The activation read takes the licence seam of the value read and the history: no licence gate applies until the License Resolver lands, and the read joins that seam when it does (DESIGN-activation §4.3 *Licence*, `cpt-cf-settings-service-constraint-licence-entitlement-fail-closed`). The `subscriber` is a gear identity, recorded `public`, so it is never masked for want of the PII entitlement, but it follows the upward disclosure boundary of the history like any actor: it is shown only when its recorded actor tenant lies on the path from the reader down to the report's tenant, else it is the mask token with `actor_withheld`, and a root-tenant reader is shown every subscriber; the `detail` is consumer-supplied free text, capped at 1 KiB by entry 2.13 and documented by the SDK as free of values and secrets — that contract is its only control, and a value leaked into it is shown by both reads (DESIGN-activation §5.2). The activation read is a routine read and writes no audit record, as no non-secret read does (PRD §5.7).
- **Reliability and data integrity**: both reads are read-only, take no lock, and observe committed data only. The listing rule is part of the page query, so `limit`, the cursor and `page_info.total_count` agree on one predicate and a walk neither skips nor repeats a record. A Tenant Resolver or a database that cannot answer fails either read `503` rather than serving an answer unchecked or reports silently dropped, and a listed or attached report whose stored `outcome` the service does not know — outside `success`, `failure` and `silent` — fails the history read as an integrity error rather than being shown with a guessed outcome. The outcome of a change is derived at read time over the rows that still exist and is never stored, so forgetting an instance re-derives it and never rewrites the history.
- **Rollout, rollback and compile-time gate**: the feature adds no table, no column, no index and no migration — it reads what entry 2.13's migration adds, `idx_sub_silent` and the instance columns of `audit_records` included. It adds one route, whose item is the per-instance shape of ADR-004 — outcome, counts and `instances` — in place of one flat state per subscriber, so a client of the earlier flat item follows the new shape; and these additive fields on the history record: `activations` on every record, `change_ref` and `detail` on an `activation` record, `instance_id` and `instance_label` on an `activation` record for a root-tenant reader, and `silent` as a value of `outcome`. It changes one listing: a report at its change's own tenant leaves that history's ordinary listing and its `total_count`, and appears attached under its change. A rollback to entry 2.13's build removes the route and the fields and lists such a report as an ordinary record again, with no data to restore. The compile-time gate is the gear's own build and its regenerated OpenAPI document; the SDK is not touched by this feature (its 0.3.0 contract is entry 2.13's). Review owner: the settings-service maintainers.
- **Not applicable**: State machines, since this feature writes nothing (§4). Configuration, since the feature adds no configuration key. Extension points and health checks, since it adds none: the reads run on the gear's existing ports and the gear's existing health. User experience beyond the REST contract, since the console that renders the reads is not this gear's. Compliance beyond the baselines inherited from entries 2.6 and 2.13, since the reads expose only what those entries record under their classifications. No feature-local deviation from a shared baseline is taken: the `404` the activation read answers for a tenant outside the caller's subtree, where the value read and the history answer `403`, is DESIGN-activation §4.8's rule, and the attached item's `occurred_at`, its newest-first order and the always-present `activations` are refinements DESIGN-activation §4.3 records, not deviations.
- **Out of scope**: any write, set, forge or clearing of a confirmation; a view grouped by request, since activation is tracked per change and subscription; paging the attached reports beyond the pages the history read already serves; an account of an expectation that a later change replaced before any confirmation; the consumer activation SDK and everything that writes an activation state or a report (entry 2.13), the report timeout, the calls of silence and the forgetting of a silent instance included.

## 2. Actor Flows (CDSL)

Both flows are an administrator's read, named after the tenant administrator; a platform administrator reads the same way, with the root tenant as its own tenant.

**Use cases**: `cpt-cf-settings-service-usecase-review-and-set`, `cpt-cf-settings-service-usecase-review-audit-trail`

Throughout, a tenant is a bare lowercase UUID string, and platform scope is the root tenant's id. "The caller's subtree" is the caller's own tenant and every descendant of it that is neither standalone nor below a standalone tenant under the caller, as the Tenant Resolver marks them (`cpt-cf-settings-service-dod-tenant-access-standalone`). "A report" is an audit record with `operation` `activation`; "its change record" is the audit record whose `id` the report names as `change_ref`.

### Read a Setting's Activation

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-activation-read-status`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- One item per subscriber with activation rows of the key at the tenant — `subscriber`, `change_ref`, the derived `outcome` (`awaiting`, `succeeded`, `failed` or `incomplete`), the counts `expected` (`M`), `confirmed` (`N`), `failed`, `silent` and `silent_excluded`, and `instances` — ordered by subscriber, with no value of any kind
- Each item lists one entry per row of the change — `status`, `excluded: false`, `reported_at` once settled, `silent_since` whenever the row carries it (on a silent one and on one a late report settled), `detail` on a failed one — followed by one entry per silent instance the change left out (`status` `silent`, `excluded: true`, `silent_since`), the entries ordered by status and then by `instance_id`, so two reads of an unchanged account list them alike for every caller
- A reader whose tenant is the root sees `instance_id` and `instance_label` on every entry; every other reader sees the same document without them
- Without `tenant`, the read answers for the caller's own tenant
- A key nobody subscribed to at the tenant, whose subscriptions no change has affected yet, or whose subscribers have registered only since the last change, answers `200` with no items
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
10. [x] - `p1` - Invoke the Activation Outcome Recorder's `activation_status`: DB: SELECT the `activation_state` rows of the key and `tenant` through `idx_activation_key_tenant`, joined to their subscriptions for `instance_label` by an outer join — a row is kept without a label when its subscription is absent — in one statement; **IF** the read fails → **RETURN** `503` - `inst-ar-status-10`
11. [x] - `p1` - **FOR EACH** subscriber that has rows, in ascending order of `subscriber` → build one item with `subscriber`, `change_ref` — the one its rows name; should rows ever name different changes, the newest by `change_ref`, a UUIDv7, is the current one and only its rows are counted — and one entry per row with `instance_id`, `instance_label`, `status`, `excluded` as `false`, `reported_at` when the row is `succeeded` or `failed`, `silent_since` whenever the row carries it — on a `silent` row and on a row a late report settled — and `detail` only when the row is `failed`, leaving out `expected_value`, `applied_value`, `deliveries`, `last_notified_at`, `updated_at` and every value or hash - `inst-ar-status-11`
12. [x] - `p1` - DB: SELECT the subscriptions of the key and `tenant` whose `silent_since` is set, through the partial `idx_sub_silent`, in one statement; **IF** the read fails → **RETURN** `503`; **FOR EACH** such subscription of a subscriber that has an item and has no row of the current change — an instance the change left out because it was silent — add to that item an entry with `status` `silent`, its `silent_since`, `excluded` as `true`, and its `instance_id` and `instance_label`; a subscriber with no item gets none, and an instance with a row is listed once, as its row - `inst-ar-status-13`
13. [x] - `p1` - **FOR EACH** item → derive its outcome and counts by `cpt-cf-settings-service-algo-activation-read-outcome`: `outcome`, `expected` (the number of rows), `confirmed`, `failed`, `silent`, and `silent_excluded` (the number of entries with `excluded: true`) - `inst-ar-status-14`
14. [x] - `p1` - **FOR EACH** item → order its entries: the rows of the change first, then the excluded entries, each part by status in the order `awaiting`, `succeeded`, `failed`, `silent`, then by `instance_id` compared lexicographically as its lowercase string; the order is decided on the identity and never depends on whether the caller is shown it - `inst-ar-status-15`
15. [x] - `p1` - Apply identity disclosure (`cpt-cf-settings-service-algo-activation-read-identity`) to the entries of every item with the caller's own tenant - `inst-ar-status-16`
16. [x] - `p1` - **RETURN** `200` with `tenant` as normalised and `items`, an empty list when no row exists; the answer is a single resource with no cursor and no `page_info`, bounded by the live instances of the key at the tenant plus the ghosts not yet forgotten, at most 64 entries per item - `inst-ar-status-12`

### Read the History with Attached Reports

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-activation-read-history`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- Beside each record of the page, the reports — one item per instance that reported on it and one per call of silence for it — whatever tenant they sit at, kept where the caller may read the key at the report's tenant, newest first, each carrying `change_ref`, `actor` (withheld, with `actor_withheld`, when its recorded tenant is not on the path from the reader down to the report's tenant), `outcome` (`success`, `failure` or `silent`), `detail` on a failure, `tenant_id` and `occurred_at`, and `instance_id` and `instance_label` for a root-tenant reader only
- A `silent` item stands under its change beside the confirmation of a sibling instance, and stays there after a later change replaces the account and after the silent instance is forgotten
- A report at the tenant its change was written at appears once in that history, attached under its change on whichever page the change falls, and never also as an ordinary record
- A report at another tenant, or one whose change record retention has removed, is listed as an ordinary record in its tenant's history, carrying its `change_ref`, its `detail` on a failure and, for a root-tenant reader only, its instance identity
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
5. [x] - `p1` - Mask each listed record as `inst-as-hist-8` does and take the actor of a listed `activation` record through the position rule of `inst-as-hist-10`, over the path from the reader tenant down to the record's own tenant: a report's actor is a gear identity recorded `public`, so no PII masking applies, and it is shown as recorded only when its recorded actor tenant lies on that path, else it is the mask token with `actor_withheld` true; a platform administrator is shown every actor - `inst-ar-history-5`
6. [x] - `p1` - Render on each listed `activation` record, whether its change was written at another tenant or its change record has left by retention, its `change_ref`, the `instance_id` and `instance_label` the record holds (removed for a non-root reader by the disclosure step below) and, when its `outcome` is `failure`, its `detail` — a `silent` record has none —, with no pre-value or post-value - `inst-ar-history-6`
7. [x] - `p1` - Invoke the attachment (`cpt-cf-settings-service-algo-activation-read-attach`) with the key, `X` and the page's listed records - `inst-ar-history-7`
8. [x] - `p1` - **IF** the attachment fails because the Tenant Resolver cannot answer for a report's tenant, **OR** its database read fails → **RETURN** `503`, serving no page - `inst-ar-history-8`
9. [x] - `p1` - **IF** a listed record's stored `outcome` is outside the known vocabulary (`success`, `failure`, `silent`), **OR** the attachment fails because an attached report's is → **RETURN** `500` as an integrity error, serving no page, as the history read does for any stored value it does not know (`cpt-cf-settings-service-dod-audit-store-table`) - `inst-ar-history-9`
10. [x] - `p1` - Set on every listed record its `activations`: the reports the attachment grouped under its `id`, an empty list when none - `inst-ar-history-10`
11. [x] - `p1` - Apply identity disclosure (`cpt-cf-settings-service-algo-activation-read-identity`) to every listed `activation` record, with the caller's own tenant: the attached items were handled by the attachment (`inst-ar-attach-9`) - `inst-ar-history-12`
12. [x] - `p1` - **RETURN** `200` with the page — the fields of `inst-as-hist-9`, `activations` on every record, and `change_ref` and `detail` on an `activation` record, and `instance_id` and `instance_label` on an `activation` record or attached item for a root-tenant reader — its cursors and `page_info.total_count`; an empty history is `200` with no items and a total of zero - `inst-ar-history-11`

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

**Output**: For each listed record, the ordered reports attached to it; or a failure — the Tenant Resolver could not answer for a report's tenant, the database read failed, or a report's stored `outcome` is outside the known vocabulary (`success`, `failure`, `silent`). The attached reports are unpaged and count toward no `limit` and no `total_count`, and a report is never both attached and listed in one history: one at `X` is left out of the listing by the history's page query (`inst-ar-history-2`), and one at another tenant is not a record of the history of `X`

**Steps**:
1. [x] - `p1` - Collect the `id` of every listed record of the page; **IF** the page is empty → **RETURN** no attachments - `inst-ar-attach-1`
2. [x] - `p1` - DB: SELECT `audit_records` with `operation` `activation`, `declaration_key` the key and `change_ref` among the collected ids, whatever their `tenant_id`, through `idx_audit_change_ref`, on the caller's `AccessScope` as every audit read runs — one statement for the page, never one per record; **IF** the read fails → **RETURN** the failure - `inst-ar-attach-2`
3. [x] - `p1` - **IF** a report found carries a stored `outcome` outside the known vocabulary (`success`, `failure`, `silent`) → **RETURN** an integrity failure, never a report with a guessed outcome or one silently dropped - `inst-ar-attach-3`
4. [x] - `p1` - **FOR EACH** report found → apply report visibility (`cpt-cf-settings-service-algo-activation-read-visibility`) at its `tenant_id`; drop the reports it drops, and **IF** it fails → **RETURN** the failure - `inst-ar-attach-4`
5. [x] - `p1` - Group the kept reports by `change_ref` under the listed record whose `id` it is; a definition record, a `secret_use` or `stage` record and a listed report are named by no report and receive none - `inst-ar-attach-5`
6. [x] - `p1` - Order each group newest first by `occurred_at`, ties broken by the report's `id`, descending - `inst-ar-attach-6`
7. [x] - `p1` - Build each attached item with `change_ref`, `actor` (the subscriber, under the actor masking of `inst-as-hist-8` and the position rule of `inst-as-hist-10` over the path from the reader tenant down to the report's tenant, with `actor_withheld` true when the actor is the mask token because the rule withheld it), `outcome` — `success`, `failure` or `silent`, the last being a call of silence — `detail` only when `outcome` is `failure`, `tenant_id`, `occurred_at`, and the `instance_id` and `instance_label` the record holds, and with no pre-value, post-value or `request_id`; one item per record, so one per instance that reported and one per call of silence - `inst-ar-attach-7`
8. [x] - `p1` - Apply identity disclosure (`cpt-cf-settings-service-algo-activation-read-identity`) to the attached items with the caller's own tenant - `inst-ar-attach-9`
9. [x] - `p1` - **RETURN** the groups, keyed by the listed records' ids, every kept report of a change on the page attached - `inst-ar-attach-8`

### Outcome Derivation

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-activation-read-outcome`

**Input**: One subscriber's entries at `(key, tenant)`: its rows of the current change, each with a status, and the excluded silent entries

**Output**: The derived `outcome`, and the counts `expected`, `confirmed`, `failed`, `silent` and `silent_excluded`. The outcome is derived at the read, over the rows that exist when the read runs, and is never stored; it is the `ChangeOutcome` of DESIGN-activation §4.1 *Enums*

**Steps**:
1. [x] - `p1` - Count `expected` (`M`) as the number of rows of the change, `confirmed` (`N`) as the rows `succeeded`, `failed` as the rows `failed` and `silent` as the rows `silent`; count `silent_excluded` as the entries with `excluded: true`, which enter none of the other counts - `inst-ar-outcome-1`
2. [x] - `p1` - **IF** any row is `failed` → **RETURN** `failed`, whatever the others are - `inst-ar-outcome-2`
3. [x] - `p1` - **IF** any row is `silent` → **RETURN** `incomplete`; a subscriber whose instances all stay silent reads `incomplete`, "0 of M", since `M` is never `0` for a subscriber the change affected - `inst-ar-outcome-3`
4. [x] - `p1` - **IF** any row is `awaiting` → **RETURN** `awaiting`; **OTHERWISE** **RETURN** `succeeded`, every expected instance having confirmed - `inst-ar-outcome-4`

The precedence is `failed` over `incomplete` over `awaiting` over `succeeded`. A late confirmation of the last silent instance turns `incomplete` into `succeeded` and a late failure into `failed`; an instance forgotten, unsubscribed, evicted or purged leaves the rows, so the next read derives over the instances that remain.

### Identity Disclosure

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-activation-read-identity`

**Input**: Entries or records carrying `instance_id` and `instance_label`, and the caller's own tenant

**Output**: The same entries or records, with both fields present for a root-tenant reader and absent for every other reader; nothing else changes

**Steps**:
1. [x] - `p1` - **IF** the caller's own tenant is the root tenant — the platform administrator, who has nothing above it → **RETURN** the entries unchanged, `instance_id` and `instance_label` present on every one, the excluded entries included - `inst-ar-identity-1`
2. [x] - `p1` - **OTHERWISE** remove `instance_id` and `instance_label` from every entry or record — absent, never masked or blanked — and keep the status, `excluded` flag, times, `detail`, counts, outcome and order; the tenant administrator who made the change is withheld them too - `inst-ar-identity-2`
3. [x] - `p1` - **RETURN** the entries; the rule is the reader-tenant rule of the upward disclosure boundary, applied to an identity that always sits above every tenant reader - `inst-ar-identity-3`

## 4. States (CDSL)

Not applicable. This feature writes nothing and owns no lifecycle. The activation state it shows moves through `cpt-cf-settings-service-state-consumer-activation-row` of entry 2.13, and an `activation` audit record is appended once and never transitions (entry 2.6).

## 5. Definitions of Done

### The Activation Route

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-activation-read-route`

The system **MUST** declare `GET /settings-service/v1/settings/{key}/activation` as one `OperationBuilder` declaration: `operation_id` `settings_service.get_setting_activation`, a summary and description, the tag of the gear's settings routes, `.authenticated()`, never anonymous, the licence seam of the value read and the history — no licence gate until the License Resolver lands, the read joining that seam when it does (DESIGN-activation §4.3 *Licence*) — the `key` path parameter as a URL-encoded GTS type id, the optional `tenant` query parameter, the activation response schema at `200` — an item per subscriber with its outcome, counts and instance entries —, and registered error responses for `400`, `403`, `404`, `500` and `503`, as RFC 9457 problem documents mapped from the gear's canonical error categories. The route **MUST** be declared for `GET` only, so a request with any other method on its path is refused and reaches no read. `OpenApiRegistry` **MUST** collect it with the gear's other routes, and the OpenAPI document **MUST** be regenerated and committed with the route and the history record's `activations`, `change_ref`, `detail`, `instance_id` and `instance_label`, and `silent` as a value of `outcome`; the route's description **MUST** tell clients to treat `status` and `outcome` as open sets. The route **MUST** add no metric of its own: it is observed by the REST metrics DESIGN §7 names for every route — `settings_rest_request_duration_seconds` and, for its `403`, `settings_authz_denial_total` — and logs nothing at `info` or `warn` beyond what every route of the surface logs. No log line, metric label or span attribute of either read **MUST** carry a `detail`, a value or a hash.

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

The activation read **MUST** refuse a malformed `key` or `tenant`, any OData option, and a `cursor` or `limit` — one rule for every parameter it does not take — `400` before any decision. It **MUST** then take the `read` decision through the `PolicyEnforcer` that a value read of the key takes, and **MUST** answer a denial or an undecidable decision `403` before the key or the tenant is looked up, so the answer is the same for every key and tenant, declared or not. Only an authorized caller **MUST** reach the second question, answered `404` with one problem document for each of four cases: a tenant outside the caller's subtree, a standalone descendant and the tenants below it included; an unknown key; a declaration outside the caller's administrative domain; and a key whose effective access for the caller is `hidden`. A retired declaration **MUST** be readable. `tenant` **MUST** default to the caller's own tenant and be normalised to a bare lowercase UUID string. A Tenant Resolver that cannot answer — at the subtree test or inside the evaluation of effective access — and a database that cannot answer — at the declaration read, the effective-access read or the `activation_state` read or the silent-subscription read — **MUST** answer `503`, failing closed, and the subtree test's `503` **MUST** be decided before its `404`; an integrity fault of a declaration or restriction row that the read looks at is answered `503` too, the history keeping its own mapping of it. Both reads **MUST** be read-only: neither writes an audit record or an activation state, and no route of this feature sets, forges or clears a confirmation.

**Implements**:
- `cpt-cf-settings-service-flow-activation-read-status`

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- API: `GET /settings-service/v1/settings/{key}/activation`
- Entities: `PolicyEnforcer`, `AccessScope`, the Tenant Resolver's subtree and standalone marking, effective tenant access

### The Activation Response

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-activation-read-response`

The activation read **MUST** answer `200` with `tenant`, as normalised, and `items`: one item per subscriber that has `activation_state` rows of the key and the tenant, read through `idx_activation_key_tenant` joined to the subscriptions for `instance_label` and completed by one read of the silent subscriptions through `idx_sub_silent`, ordered by `subscriber`, each carrying `subscriber`, `change_ref`, `outcome`, `expected`, `confirmed`, `failed`, `silent`, `silent_excluded` and `instances` as `cpt-cf-settings-service-dod-activation-read-instances` fixes. It **MUST NOT** carry `expected_value`, `applied_value`, `deliveries`, `last_notified_at`, `updated_at` or any value or hash. A key with no row at the tenant — nobody subscribed there, no change has affected its subscriptions, or its subscribers registered after the last change — **MUST** answer `200` with no items. The answer **MUST** be a single resource with no cursor and no `page_info`. The `status` spellings `awaiting`, `succeeded`, `failed` and `silent` and the `outcome` spellings `awaiting`, `succeeded`, `failed` and `incomplete` **MUST** be the stored and the wire form and permanent once shipped, and the route's description **MUST** tell clients to treat both as open sets.

**Implements**:
- `cpt-cf-settings-service-flow-activation-read-status`
- `cpt-cf-settings-service-component-change-set-outcome-tracker` — `activation_status`

**Constraints**: `cpt-cf-settings-service-constraint-activation-wait-for-all-unbounded` (superseded by ADR-004: the wait is bounded per instance by the report timeout, counted in accepted deliveries; an instance still outstanding then is `silent` and the item's outcome `incomplete`, and a silent instance stays listed — as a row until a late report, a re-arm or a later change replaces it, then as an excluded entry — until its silence ends or it is forgotten a day after it was first called silent)

**Touches**:
- DB: `activation_state` (`idx_activation_key_tenant`), `subscriptions` (`idx_sub_silent`)
- Entities: `ActivationState`, `ActivationStatus`, `ChangeOutcome`, the activation response and its per-subscriber item

### Reports Attached to the History

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-activation-read-history`

The history read **MUST** leave out of its listing, in the page query and its count, every report whose change record exists among the records of the same key at the history's tenant, so `limit`, the cursor and `page_info.total_count` count listed records only and every page of a walk reports the same total; a report whose change record has left by retention **MUST** be listed as an ordinary record. For the records of each page it **MUST** look up, in one statement through `idx_audit_change_ref` on the caller's `AccessScope`, the reports whose `change_ref` is a listed record's `id`, whatever their tenant, and **MUST** keep a report only where its tenant lies inside the caller's subtree and outside any standalone tenant, the key's visibility to the caller being the decision the history already took for the caller. Every listed record **MUST** carry `activations` — its kept reports, newest first by `occurred_at` with the report's `id` breaking a tie, each with `change_ref`, `actor`, `outcome` (`success`, `failure` or `silent`), `detail` on a failure, `tenant_id` and `occurred_at`, and `instance_id` and `instance_label` for a root-tenant reader only, an empty list when none — one item per instance that reported and one per call of silence, a `silent` item staying under its change after a later change and after the instance is forgotten — and a listed report, whether its change was written at another tenant or its change record has left by retention, **MUST** carry its `change_ref`, on a failure its `detail`, and `instance_id` and `instance_label` for a root-tenant reader only (`cpt-cf-settings-service-dod-activation-read-identities` governs who sees them). A report **MUST NOT** be both attached and listed in one history, and attached reports **MUST NOT** be paged or counted. A Tenant Resolver that cannot answer for a report's tenant, or a database that cannot answer the attachment's lookup, **MUST** fail the read `503`, and a listed or attached report whose stored `outcome` is outside the known vocabulary (`success`, `failure`, `silent`) **MUST** fail it as an integrity error (`cpt-cf-settings-service-dod-audit-store-table`); in either case no page is served.

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

### The Instances of an Item

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-activation-read-instances`

Each item of the activation read **MUST** carry the `change_ref` its rows name (the newest by `change_ref` when rows ever name different changes, only its rows counted), the outcome derived at the read over its rows — `failed` if any row is `failed`, else `incomplete` if any is `silent`, else `awaiting` if any is `awaiting`, else `succeeded` — never stored, and the counts `expected` (`M`, the number of rows), `confirmed` (`N`, the rows `succeeded`), `failed`, `silent` and `silent_excluded`. It **MUST** list one entry per row — `status`, `excluded: false`, `reported_at` once settled, `silent_since` whenever the row carries it — when `silent` and when a late report settled it, so the read shows the confirmation came late — `detail` only when `failed` — followed by one entry per silent instance of the subscriber the change left out, found through `idx_sub_silent` as a subscription with `silent_since` set and no row of the change, with `status` `silent`, `excluded: true` and `silent_since`, counted in `silent_excluded` only and in none of `expected`, `confirmed` and `silent`; `excluded` **MUST** be present on every entry. A subscriber with a registration but no row has no item, its excluded silent instances included. The entries **MUST** be ordered rows first, then excluded entries, each part by status (`awaiting`, `succeeded`, `failed`, `silent`) and then by `instance_id` as its lowercase string, whoever reads and whether or not the id is shown. An item **MUST NOT** list more than `MAX_INSTANCES_PER_SUBSCRIPTION` (64) entries, ghosts included (held by the registration cap of entry 2.13, not by truncation in the read), so the answer stays one resource and is never paged. Forgetting, unsubscribing, evicting or purging an instance **MUST** change the next read through the rows that remain — forgetting the one silent instance of an `incomplete` change whose other instances confirmed makes it `succeeded`, "N of N" — and **MUST NOT** alter the history.

**Implements**:
- `cpt-cf-settings-service-flow-activation-read-status`
- `cpt-cf-settings-service-algo-activation-read-outcome`
- `cpt-cf-settings-service-component-change-set-outcome-tracker` — `activation_status`

**Constraints**: `cpt-cf-settings-service-constraint-activation-wait-for-all-unbounded` (superseded by ADR-004)

**Touches**:
- DB: `activation_state` (`idx_activation_key_tenant`), `subscriptions` (`idx_sub_silent`)
- Entities: `ChangeOutcome`, `ActivationStatus`, the per-instance entry of the activation item

### Instance Identities

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-activation-read-identities`

Both reads **MUST** show `instance_id` and `instance_label` only to a caller whose own tenant is the root tenant, on every entry of the activation read — excluded entries included — and on every attached item and listed `activation` record of the history; for every other caller they **MUST** be absent, never masked or blanked, the administrator who made the change included when it is a tenant administrator. The outcome, the counts, each entry's `status`, `excluded` flag, `reported_at`, `silent_since` and `detail`, and the order of the entries **MUST** be the same for both callers, so that "6 of 7, one silent since 10:02" is readable without the identity. No log line, metric label or span attribute **MUST** carry an instance identity of a reader who may not see it.

**Implements**:
- `cpt-cf-settings-service-algo-activation-read-identity`
- `cpt-cf-settings-service-flow-activation-read-status`
- `cpt-cf-settings-service-flow-activation-read-history`

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- API: `GET /settings-service/v1/settings/{key}/activation`; `GET /settings-service/v1/settings/{key}/history`
- Entities: the reader's own tenant, the upward disclosure boundary

### Activation Read Tests

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-activation-read-tests`

The feature **MUST** ship with these tests, each implementing a row of DESIGN-activation §7 or a rule of this document where no row exists:

- Unit, with `InMemoryNotificationRepo`: the Level 1 rows of DESIGN-activation §7 *Change outcome derivation* — `failed` over `incomplete` over `awaiting` over `succeeded`, `expected` the rows and `confirmed` the `succeeded` ones, re-derived over the rest when a row goes with its registration; entry 2.13 references the same row for the judgement its call of silence makes on `settings_activation_incomplete_total`, the derivation the read renders being this feature's — and *Excluded silent entries* — each silent subscription of the subscriber at `(key, tenant)` with no row of the current change listed after the rows as `status = silent` with its `silent_since` and `excluded: true`, `excluded` on every entry, the excluded entries counted in `silent_excluded` only, the entry order stable, and a subscriber with no row having no item.
- API, through `Router::oneshot` over the gear's in-memory SQLite harness as the gear's other REST tests run, with the `PolicyEnforcer` allowing or denying and no broker: every Level 3 row of DESIGN-activation §7 — *Activation read* (one item per subscriber with `change_ref`, `outcome`, `expected`, `confirmed`, `failed`, `silent`, `silent_excluded` and an `instances` entry per row with `excluded: false`, then one entry per excluded silent instance with `excluded: true`; `excluded` on every entry and `silent_excluded` equal to their number; no value; without `tenant` the caller's own, ordered by subscriber, `reported_at` absent while `awaiting`, `detail` only on `failed`, `200` with no items for a key nobody subscribed to, a retired declaration readable, the document matching the example of DESIGN-activation §4.3); *Activation read — identities* (the platform administrator sees `instance_id` and `instance_label` on every entry, excluded entries included; a tenant administrator sees the same items, counts, statuses and `excluded` flags in the same order with both fields absent); *Activation read — out of subtree or hidden key*, extended to the four cases of the authorization definition of done — a tenant outside the caller's subtree, at a sibling, at a standalone descendant and at a tenant below it, an unknown key, a declaration outside the caller's administrative domain (at the level of the read itself, since the value resource declares no PEP property a policy could constrain a request on) and a key hidden from the caller — every case answering one problem document; *AuthZ deny — the action*, for a declared key, an undeclared key and a tenant outside the subtree alike; *History shows reports under the change*; *History attaches descendant reports*, including the same report listed in the descendant's history with its `change_ref` and `detail`; *History lists per-instance reports* (two instances of one subscriber reporting on one change give two attached items, identities for the platform administrator only); *History shows calls of silence* (an item with `outcome` `silent` under the change beside a sibling's confirmation, still there after a later change and after the silent instance is forgotten, identities for the platform administrator only); *History at the written tenant*, including the page-size-1 walk and a `total_count` that does not count the attached report; and *RFC 9457 errors* for `400`, `403`, `404`, `500` and `503`. The history rows pass their target `X` as `tenant`, the history read's parameter — the scope's tenant id, the root tenant's id for platform scope — wherever the designs name it the scope. Where no row exists: the outcome precedence over rows in every combination — `failed` over `incomplete` over `awaiting` over `succeeded`; the all-silent subscriber reading `incomplete`, "0 of M", and a late confirmation, a late failure, a re-arm and the forgetting of the one silent instance each re-deriving the next read; a subscriber with a registration and no row having no item; the 64-entry bound per item, ghosts included; the entry order stable between two reads and between two callers; an instance identity of a reader who may not see it absent from every log line and span; a malformed `key` or `tenant`, an OData option, and a `cursor` or `limit` answering `400`; a Tenant Resolver failing at the subtree test, inside the effective-access evaluation and for a report's tenant, and a database failing at the declaration read, the `activation_state` read and the silent-subscription read, each answering `503` and never `404`; a listed and an attached report whose stored `outcome` is outside the known vocabulary (`success`, `failure`, `silent`) each failing the history read as an integrity error with no page served; a report whose change record has been pruned listed as an ordinary record; a `secret`-trait key's states read with no hash; attached reports ordered newest first, the report's `id` breaking a tie of `occurred_at`, with `detail` on a failure only and the subscriber unmasked; the history's own `403` and `404` unchanged; a method other than `GET` on the activation path refused; no `detail`, value or hash in a log line, metric label or span attribute; and the gear's statement recorder showing one read of `activation_state` (joined to the subscriptions) and one of the silent subscriptions per activation read, one attachment lookup per history page whatever the page's size, and no write — no audit record, no activation state — by either read.
- Gate, on the regenerated OpenAPI document: the route under its `operation_id`, `.authenticated()` with no licence requirement and its registered errors, a description telling clients to treat `status` and `outcome` as open sets, the per-instance item schema with `instance_id` and `instance_label` optional, and no metric declared for the route of its own.
- Integration, on real PostgreSQL through testcontainers: the listing rule and the attachment lookup over the partial `idx_audit_change_ref`, a change at `X` with reports at `X` and at a descendant, walked with page size 1, giving the same pages and totals as the API rows; and the Level 2 row *Excluded silent instance in the read* — two instances A and B, change 1 leaving B silent, change 2 committed: the change 2 item reads `1` expected and lists A with `excluded: false` and B with `status = silent`, its `silent_since` and `excluded: true`, `silent_excluded` `1`, B's identity shown to the platform administrator only, and the read making one lookup of the silent subscriptions.
- E2E, environment-bound and not part of what the gear's unit and API suites ship: the *Smoke without a broker* row of DESIGN-activation §7 Level 4, in the gear's smoke suite when such a suite exists — the activation read of a declared key answering `200` with no items, of an undeclared key `404` as a problem document, and of an anonymous caller `401`; and, in an environment that runs a broker and a sample consumer gear, the read side of the Level 4 rows entry 2.13 leaves here — *Full loop*, the activation read showing `succeeded` and the history showing the report under the change; *Value mismatch → failed*, the activation read showing `failed` with its `detail` and the history recording the failure under the change.

**Implements**:
- `cpt-cf-settings-service-flow-activation-read-status`
- `cpt-cf-settings-service-flow-activation-read-history`
- `cpt-cf-settings-service-algo-activation-read-visibility`
- `cpt-cf-settings-service-algo-activation-read-attach`
- `cpt-cf-settings-service-algo-activation-read-outcome`
- `cpt-cf-settings-service-algo-activation-read-identity`

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- Entities: `Router::oneshot`, the gear's in-memory SQLite harness and statement recorder, the `PolicyEnforcer` test decision point, the testcontainers PostgreSQL, the gear's E2E smoke suite, the broker environment with a sample consumer gear

## 6. Acceptance Criteria

- [x] With one subscriber on a key at a tenant whose instances `A` is `awaiting`, `B` `succeeded` and `C` `failed`, the activation read answers `200` with one item carrying `subscriber`, `change_ref`, `outcome` `failed`, `expected` 3, `confirmed` 1, `failed` 1, `silent` 0, `silent_excluded` 0 and three `instances` entries, each with `status` and `excluded: false`, `reported_at` on `B` and `C` only, `detail` on `C` only, and no value, expected value, applied value or hash; with three subscribers the items are ordered by `subscriber` (API)
- [x] Without `tenant`, the activation read answers for the caller's own tenant, and the `tenant` it returns is a bare lowercase UUID string whatever case the request used (API)
- [x] A key nobody subscribed to at the tenant, and a key whose subscriptions no change has affected, answer `200` with no items and no `page_info` (API)
- [x] The activation read of a retired declaration answers `200` with its states (API)
- [x] A malformed `key`, a `tenant` that is not a UUID, a `$filter`, `$orderby` or `$select` option, and a `cursor` or `limit` answer `400` problem documents before any decision is taken (API)
- [x] A caller the `PolicyEnforcer` denies receives `403` for a declared key, an undeclared key and a tenant outside its subtree alike (API)
- [x] Each of the four `404` cases answers `404` — a tenant outside the caller's subtree, tested at a sibling, at a standalone descendant and at a tenant below it; an unknown key; a declaration outside the caller's administrative domain; and a key `hidden` for the caller — and the problem documents differ in nothing but `trace_id` (API; the administrative-domain case at the level of the read itself, since the value resource declares no PEP property a policy could constrain a request on)
- [x] A Tenant Resolver that cannot answer, at the subtree test or inside the effective-access evaluation, and a database that cannot answer, at the declaration read, the `activation_state` read or the silent-subscription read, each fail the activation read `503`, never `404` (API)
- [x] For a `secret`-trait key with a `succeeded` and a `failed` state, the activation read carries neither the expected hash nor the applied hash, and the gear's statement recorder shows one read of `activation_state` and one of the silent subscriptions per request (API)
- [x] Neither read writes: across an activation read and a history read the gear's statement recorder shows no write of an audit record or an activation state; and a `PUT`, `POST`, `PATCH` or `DELETE` on the activation path is refused without reaching the read (API)
- [x] The route is collected by `OpenApiRegistry` under `operation_id` `settings_service.get_setting_activation`, declared `.authenticated()` with no licence requirement and its registered errors, its description tells clients to treat `status` and `outcome` as open sets and the route declares no metric of its own, and the regenerated OpenAPI document carries it, the per-instance item, and the history record's `activations`, `change_ref`, `detail`, `instance_id` and `instance_label` (gate)
- [x] After a change at `X` with subscribers at `X` and at a descendant `D`, both reporting, the administrator of `X` reading the history of `X` sees both reports under the change, while the administrator of a sibling of `D` sees neither of `D`'s reports in any history it may read (API)
- [x] The history of `X` returns the report at `D` beside the change record whose `id` it names, with `change_ref`, `actor`, `outcome`, `detail` on a failure, `tenant_id` and `occurred_at`, and the history of `D` lists the same report as an ordinary record carrying its `change_ref` and `detail` (API)
- [x] After a change at `X` with a subscriber at `X` that reported, the history of `X` returns the report once, attached under its change and not listed; with page size 1 and a newer record of the key at `X`, page 1 holds the newer record alone, page 2 holds the change with the report attached, and `total_count` is the same on every page and does not count the report (API, integration)
- [x] A report at a standalone descendant of the caller is attached in no history the caller reads, and a Tenant Resolver failing for a report's tenant fails the history read `503` (API)
- [x] A report whose change record retention has removed is listed as an ordinary record in its tenant's history (API)
- [x] A listed report and an attached report whose stored `outcome` is outside the known vocabulary each fail the history read as an integrity error, `500`, with no page served (API)
- [x] Two reports under one change are attached newest first, and two with the same `occurred_at` in the order of their `id`; a `success` report carries no `detail` and a `failure` report its `detail`; no attached item carries a pre-value, a post-value or a `request_id`; the subscriber, recorded `public`, is shown unmasked to a caller without the PII entitlement when its actor tenant lies on the path from the reader to the report's tenant (API)
- [x] The history read keeps its own refusals: a target outside the caller's subtree or standalone answers `403` and a hidden key `404`, as entry 2.6 defines them (API)
- [x] A history page of any size makes one attachment lookup, and every record of a page carries `activations`, an empty list when nothing answers it (API)
- [x] A subscriber with seven instances, six `succeeded` and one `silent`, reads `outcome` `incomplete`, `expected` 7, `confirmed` 6, `silent` 1; the late confirmation of the silent instance makes the next read `succeeded`, "7 of 7", with its `silent_since` still present beside `reported_at`, and its late failure `failed` (API)
- [x] A subscriber whose instances are all `silent` reads `incomplete`, "0 of M" with `M` at least 1, never `M` of 0 (API)
- [x] The outcome follows `failed` over `incomplete` over `awaiting` over `succeeded`: rows `failed` and `silent` read `failed`; `silent` and `awaiting` read `incomplete`; `awaiting` and `succeeded` read `awaiting`; all `succeeded` read `succeeded`; the outcome is derived at the read and no stored value or write supports it (API)
- [x] An instance the change left out because it was silent while a sibling was expected appears in its subscriber's item as an entry with `status` `silent`, `silent_since` and `excluded: true`, counted in `silent_excluded` and in none of `expected`, `confirmed` and `silent`; `excluded` is present on every entry, `false` on each entry of a row (API)
- [x] A subscriber with a registration and no row of any change has no item, its silent excluded instances included, and a key with such subscribers only answers `200` with no items (API)
- [x] Forgetting the one silent instance of an `incomplete` change whose other instances confirmed makes the next read `succeeded`, "N of N", and removes no record of the history (API)
- [x] The entries of an item are listed rows first, then excluded entries, each part by status (`awaiting`, `succeeded`, `failed`, `silent`) and then by `instance_id`, and two reads of an unchanged account, by the platform administrator and by a tenant administrator, list them in the same order (API)
- [x] An item lists at most 64 entries, ghosts included, and the answer stays a single resource with no cursor and no `page_info` (API)
- [x] The platform administrator reads `instance_id` and `instance_label` on every entry, excluded entries included, and in the history on every attached item and listed `activation` record; a tenant administrator reading the same account sees the same outcome, counts, statuses, `excluded` flags, times and `detail`, with both fields absent, including when it made the change (API)
- [x] A report whose recorded actor tenant is not on the path from the reader tenant down to the report's tenant, read by a tenant administrator, carries the mask token as `actor` with `actor_withheld` true both as an attached item and as a listed `activation` record, whatever the reader's PII entitlement, while a platform administrator reads the subscriber as recorded (API)
- [x] After two instances of one subscriber reported on one change, the history shows two attached items under it, one per instance (API)
- [x] After one instance confirmed a change and its sibling was called silent for it, the history shows an attached item with `outcome` `silent`, no `detail`, under the change beside the confirmation, and shows it still after a later change and after the silent instance is forgotten (API)
- [ ] In the gear's smoke suite without a broker, the activation read of a declared key answers `200` with no items, of an undeclared key `404`, and of an anonymous caller `401` (E2E)
- [ ] In an environment that runs a broker and a sample consumer gear, after a set and the consumer's report, the activation read shows `succeeded` and the history shows the report under the change; after a wrong value reported on the current `change_ref`, the activation read shows `failed` with its `detail` and the history records the failure under the change (E2E)
- [x] No log line, metric label or span attribute of either read carries a `detail`, a value or a hash (API)
- [x] No log line, metric label or span attribute of either read carries an instance identity of a reader who may not see it (API)
- [x] The re-arm of a silent instance re-derives the next read from the rows that then exist: the instance's row is `awaiting` again with no `silent_since`, and the outcome follows the precedence over the rows (API)
- [x] `cfs validate --artifact` reports zero errors on this document, and every `@cpt-*` marker the implementation adds resolves to an id defined here (gate)
