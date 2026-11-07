# Feature: Administrator Fact Management


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Read a Subject's Facts](#read-a-subjects-facts)
  - [Add a Fact](#add-a-fact)
  - [Edit a Fact](#edit-a-fact)
  - [Delete a Fact](#delete-a-fact)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Administrator Only](#administrator-only)
  - [Log the Administrator's Events](#log-the-administrators-events)
  - [Wire Administrator Fact Management](#wire-administrator-fact-management)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [The Administrator's Read](#the-administrators-read)
  - [The Administrator's Add and Edit](#the-administrators-add-and-edit)
  - [The Administrator's Delete](#the-administrators-delete)
  - [Review Requests and the Administrator](#review-requests-and-the-administrator)
  - [The Administrator's Events](#the-administrators-events)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p2` - **ID**: `cpt-cf-construct-featstatus-admin-facts-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p2` - `cpt-cf-construct-feature-admin-facts`

## 1. Feature Context

### 1.1 Overview

Administrator Fact Management lets a tenant administrator read all facts of a subject of the tenant, and add, edit or delete one directly, to check and fix a profile when no connector or agent can, and to set up profiles for tests. An add or an edit goes through the Review Queue's one-step plan path with the administrator as origin; a delete works like the subject's delete. An edit or a delete of a fact under an open review request resolves the request as the administrator.

### 1.2 Purpose

The PRD requires that a tenant administrator can read all facts of any subject in their tenant, with origin, and add, edit and delete a fact directly; that an added or edited fact passes the same guardrails as any fact and names the administrator as origin; that an add for a subject with no profile creates it; that a delete works like the subject's delete; that while personalization is off an add or an edit stores nothing, while a read and a delete still work; that an edit of a fact that no longer exists stores nothing; that the administrator is told when nothing was stored; that a stored edit, or a delete, of a fact under an open review request closes it as corrected or deleted, while a blocked or dropped edit leaves it open; and that each read is an audit event, and each add, edit and delete an audit event without content (`cpt-cf-construct-fr-admin-facts`).

**The routes.** The DESIGN writes the paths with the gateway's `/api` prefix; the code registers them without it.

| Route | Answer |
|-------|--------|
| `GET /construct/v1/admin/subjects/{subject_id}/facts` | `200`, a page of the subject's facts |
| `POST /construct/v1/admin/subjects/{subject_id}/facts` | `201`, the fact as stored |
| `PUT /construct/v1/admin/subjects/{subject_id}/facts/{fact_id}` | `200`, the fact as stored, under its new id |
| `DELETE /construct/v1/admin/subjects/{subject_id}/facts/{fact_id}` | `204` |

**Who may.** Every route needs the permission to administer a subject's facts: `administer` on `gts.cf.construct.subject.fact.v1~` in the caller's tenant, which Construct declares as a GTS permission instance. Only a constraint that names the caller's tenant, and nothing narrower, makes an administrator: the administrator does not act for the subject, so the AuthZ resolver is not asked about the subject. A caller without it gets `403`. Everything is scoped to the caller's tenant, so a subject of another tenant answers as one with no profile: its read is empty, and an edit or a delete of one of its facts answers `404`. The gateway rule on `/api/construct/v1/admin/**` is the deployment's to turn on.

**The read.** The read is the subject's own view seen by the administrator: every live fact with its category, its origin and its latest review request, and the profile's version. It is paged, filtered and ordered as the subject's own list (`limit` 50 by default and at most 200, a larger one cut to 200; `$filter` and `$orderby` over `fact_id`, `entity_kind`, `origin` and `stored_at`; `stored_at desc` by default, `fact_id` breaks ties). Personalization off and an erasure under way hide nothing here.

**The add.** The body is `{property, value}`: one property of the person types, as a fact in the read and a reviewer's correction name it, and its value; the category comes from the property. A property name longer than 64 characters or of other characters than lower-case letters, digits and `_`, or a property no person type has, answers `400` naming `property`; a value its schema does not take answers `400` naming `value`; the answer and the log never repeat the name or the value. A value the subject holds already, or a second value of a one-valued property, answers `409` `ALREADY_HELD`, naming the fact that holds it, so the administrator edits it instead. The value then goes through the one-step plan path: a plan with one add step under a new node key, the administrator as origin, the sensitive-data checks and Admission like any plan, and the profile writer against the profile's version. A subject without a profile gets one: the plan is written against no version, as a first record is. The answer is `201` with `{fact_id, entity_kind, property, value, origin}`, the value as Admission let it through.

**The edit.** The body is the add's, with the fact's own property; another property answers `400`. The fact gets a new id, as a reviewer's correction does. A fact the subject does not have, also one that goes away before the edit is written, answers `404` and stores nothing. The answer is `200` with the fact as stored, under its new id.

**The delete.** A delete is the subject's delete with the administrator as origin: a soft delete written against the profile's version, read and written again on a conflict. A fact the subject does not have answers `404`. When the fact was deleted but a review request on it could not be closed, the answer is `503`, and a repeat closes it.

**When nothing is stored.** When the checks fail or block the value, or Admission drops the plan, personalization off and an erasure under way included, nothing is stored and the answer is `422` with the reason's name, as a refused correction's: `value_blocked`, `check_failed`, `personalization_off`, `erasure_in_progress` and the other drop reasons. No sensitive-data check exists yet, so an add and an edit, like a correction, run only where the deployment opted in (`processing.run_without_sensitive_data_checks`); elsewhere they answer `501`. A read and a delete need no opt-in. Without a processing identity every route answers `501`. An add or an edit whose writer failed answers `503`; it may have landed, so the administrator reads the facts before sending it again. A delete whose writer failed is sent again as it is: its repeat deletes the fact or finds it gone.

**Review requests.** An edit or a delete of a fact under an open review request resolves the request as the administrator, through the Review Queue's resolution: the request is claimed, what is about to be written is recorded on the claim, the fact is written, and the request closes as corrected, with the new fact's id, or as deleted, closed by the administrator. While a reviewer holds the request, the answer is `409` `BEING_RESOLVED`; an edit that the checks or Admission refuse leaves the request open; a resolution whose write landed but whose request did not close answers as a reviewer's does (`WRITTEN_NOT_CLOSED`). A stored edit or delete is recorded also when its request did not close. When an earlier resolution's write is in the profile, the request closes as that one, and the administrator's delete stores nothing and answers `409` `CLOSED`; an edit then finds its fact replaced and answers `404`. An edit whose fact goes away while its resolution writes closes the request as deleted, with the reason that nothing was written, and answers `404`. A delete of a fact already gone closes its open request and answers `204`, recording no delete. A request flagged while an edit outside a resolution is written closes after it as corrected, unless a reviewer holds it, whose resolution then finds the fact gone, or a resolution recorded a write on it, which the next resolution settles; when that close fails, it is logged and its claim given back; one flagged while a delete is written closes as deleted, as the subject's delete closes it. The Review Queue's resolved event names the administrator; a refused edit is recorded once, as the administrator's.

**Events.** Each read records the administrator, the subject and the categories it served; each add, edit and delete the administrator, the subject, the fact and its category, and for an edit the new fact's id; each refused add or edit the reason's name. None holds a value. They go to the structured log until the Audit gear ships.

**Out of scope**:

- The one-step plan path itself, which belongs to the Review Queue (`cpt-cf-construct-feature-review-queue`).
- The gateway scope rule on `/api/construct/v1/admin/**`, which a deployment turns on.
- In-process client methods for these operations.

**Requirements**: `cpt-cf-construct-fr-admin-facts`; shared: `cpt-cf-construct-fr-fact-origin` (the administrator as origin), `cpt-cf-construct-fr-sensitive-data-guardrails` (an added or edited value passes the checks and Admission), `cpt-cf-construct-fr-subject-delete` (the administrator's delete works like the subject's).

**Principles**: None. **Constraints**: `cpt-cf-construct-constraint-graph-soft-delete-only`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-tenant-admin` | Reads a subject's facts, and adds, edits and deletes one. |
| `cpt-cf-construct-actor-subject` | Whose facts they are; sees the administrator as the origin of an added or edited fact. |
| `cpt-cf-construct-actor-audit` | Receives the read and change events, through the structured log until the Audit gear ships. |
| `cpt-cf-construct-actor-platform-auth` | Authenticates the caller and decides, through the AuthZ resolver, whether it may administer the tenant's subjects' facts. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), the subject control component and the admin operations
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.12
- **Dependencies**: `cpt-cf-construct-feature-subject-control`, `cpt-cf-construct-feature-profile-writer`, `cpt-cf-construct-feature-admission`, `cpt-cf-construct-feature-review-queue`

## 2. Actor Flows (CDSL)

### Read a Subject's Facts

- [x] `p2` - **ID**: `cpt-cf-construct-flow-admin-facts-read`

**Actor**: `cpt-cf-construct-actor-tenant-admin`

**Success Scenarios**:
- The administrator gets 200 with a page of the subject's facts, each with its origin and latest review request.
- A subject of another tenant, or without a profile, gives an empty page.

**Error Scenarios**:
- The platform refuses: 403.
- A `$filter`, `$orderby` or cursor the list does not know: 400.
- No processing identity: 501. A dependency does not answer: 503.

**Steps**:
1. [x] - `p2` - Caller sends `GET /construct/v1/admin/subjects/{subject_id}/facts`, authenticated, with optional `limit`, `cursor`, `$filter` and `$orderby` - `inst-read-send`
2. [x] - `p2` - API: the extractors take the security context, the service, the subject id and the query - `inst-read-parse`
3. [x] - `p2` - Run `cpt-cf-construct-algo-admin-facts-admin-only` - `inst-read-authorize`
4. [x] - `p2` - Read the subject's facts as Construct, cut the page the query asks for, and add each fact's latest review request - `inst-read-facts`
5. [x] - `p2` - Record a read event with the categories served - `inst-read-event`
6. [x] - `p2` - **RETURN** 200 with the page and the profile's version - `inst-read-return`

### Add a Fact

- [x] `p2` - **ID**: `cpt-cf-construct-flow-admin-facts-add`

**Actor**: `cpt-cf-construct-actor-tenant-admin`

**Success Scenarios**:
- The administrator gets 201 with the fact as stored, the administrator as its origin; a subject without a profile gets one.

**Error Scenarios**:
- The platform refuses: 403.
- A body without exactly one property, a property no person type has, or a value its schema does not take: 400.
- The value, or a value of the one-valued property, is held already: 409 `ALREADY_HELD`.
- The checks or Admission refuse it, personalization off included: 422 with the reason.
- Without the opt-in, or without a processing identity: 501. The profile kept changing, or the writer failed: 503.

**Steps**:
1. [x] - `p2` - Caller sends `POST /construct/v1/admin/subjects/{subject_id}/facts` with `{property, value}`, authenticated - `inst-add-send`
2. [x] - `p2` - API: the extractors take the security context, the service, the subject id and the body; a body without exactly one property answers 400 - `inst-add-parse`
3. [x] - `p2` - Start the budget (20 seconds); it bounds every step before the write, from the permission check on, and past it the answer is unavailable with nothing written. Run `cpt-cf-construct-algo-admin-facts-admin-only`. **IF** the deployment did not opt in, not configured. Check the property and the value against the person types - `inst-add-authorize`
4. [x] - `p2` - Run `cpt-cf-construct-algo-review-queue-correct` for an add, with the administrator as origin: **IF** the subject holds the value, or a value of the one-valued property, already, **RETURN** held with that fact - `inst-add-write`
5. [x] - `p2` - **IF** stored, record an added event and **RETURN** 201 with the fact; **IF** refused, record a refused event and answer with the reason - `inst-add-return`

### Edit a Fact

- [x] `p2` - **ID**: `cpt-cf-construct-flow-admin-facts-edit`

**Actor**: `cpt-cf-construct-actor-tenant-admin`

**Success Scenarios**:
- The administrator gets 200 with the fact as stored under its new id; an open review request on it closes as corrected.

**Error Scenarios**:
- The platform refuses: 403.
- A fact the subject does not have, also one gone before the edit is written: 404.
- Another property than the fact's, or a value its schema does not take: 400.
- A reviewer holds the fact's request: 409 `BEING_RESOLVED`; an earlier resolution closed it: 409 `CLOSED`.
- The checks or Admission refuse it: 422 with the reason, and a review request stays open.
- Without the opt-in, or without a processing identity: 501. The profile kept changing, or the writer failed: 503.

**Steps**:
1. [x] - `p2` - Caller sends `PUT /construct/v1/admin/subjects/{subject_id}/facts/{fact_id}` with `{property, value}`, authenticated - `inst-edit-send`
2. [x] - `p2` - API: the extractors take the security context, the service, the ids and the body; a fact id not shaped like a node key answers 404, a body without exactly one property 400 - `inst-edit-parse`
3. [x] - `p2` - Start the budget. Run `cpt-cf-construct-algo-admin-facts-admin-only`. **IF** the deployment did not opt in, not configured. Read the subject's facts; **IF** no live fact has the id, no such fact; **IF** the property is not the fact's, or its schema does not take the value, invalid - `inst-edit-read`
4. [x] - `p2` - **IF** the fact has an open review request, resolve it as the administrator as `cpt-cf-construct-flow-review-queue-resolve` does from its claim on, within this edit's budget, with the new value as the correction: **IF** this edit stored the value, record an edited event and **RETURN** 200 with the fact, or the conflict when the request did not close; **IF** this edit found the fact gone, no such fact; **IF** an earlier resolution closed the request, conflict; **IF** refused, record a refused event and answer with the reason - `inst-edit-review`
5. [x] - `p2` - Otherwise run `cpt-cf-construct-algo-review-queue-correct` for the fact, with the administrator as origin. **IF** the fact is gone, no such fact; **IF** refused, record a refused event and answer with the reason. Close a request flagged on the fact meanwhile as corrected, unless a resolution holds it or recorded a write on it; a failed close is logged and gives its claim back. Record an edited event. **RETURN** 200 with the fact - `inst-edit-write`

### Delete a Fact

- [x] `p2` - **ID**: `cpt-cf-construct-flow-admin-facts-delete`

**Actor**: `cpt-cf-construct-actor-tenant-admin`

**Success Scenarios**:
- The administrator gets 204; the fact is not served from the next read on, and an open review request on it closes as deleted.

**Error Scenarios**:
- The platform refuses: 403.
- A fact the subject does not have, with no open request: 404.
- A reviewer holds the fact's request: 409 `BEING_RESOLVED`; an earlier resolution corrected the fact: 409 `CLOSED`.
- No processing identity: 501. The profile kept changing, the writer failed, or the request could not be closed: 503.

**Steps**:
1. [x] - `p2` - Caller sends `DELETE /construct/v1/admin/subjects/{subject_id}/facts/{fact_id}`, authenticated - `inst-delete-send`
2. [x] - `p2` - API: the extractors take the security context, the service and the ids; a fact id not shaped like a node key answers 404 - `inst-delete-parse`
3. [x] - `p2` - Start the budget. Run `cpt-cf-construct-algo-admin-facts-admin-only` - `inst-delete-authorize`
4. [x] - `p2` - **IF** the fact has an open review request, resolve it as the administrator as `cpt-cf-construct-flow-review-queue-resolve` does from its claim on, within this delete's budget, as deleted: **IF** this delete removed the fact, record a deleted event and **RETURN** 204, or the conflict when the request did not close; **IF** it found the fact gone and closed the request, **RETURN** 204; else conflict - `inst-delete-review`
5. [x] - `p2` - Otherwise delete the fact as `cpt-cf-construct-algo-subject-control-delete` does, with the administrator as origin, and record a deleted event. Close a request flagged on the fact meanwhile, or left by an earlier delete, as deleted; **IF** that close fails, unavailable. **IF** the fact was gone and no request closed, no such fact. **RETURN** 204 - `inst-delete-write`

## 3. Processes / Business Logic (CDSL)

### Administrator Only

- [x] `p2` - **ID**: `cpt-cf-construct-algo-admin-facts-admin-only`

**Input**: The security context and the subject id.

**Output**: The subject's key in the caller's tenant and the scope of that tenant, or a forbidden error.

**Steps**:
1. [x] - `p2` - Ask the AuthZ resolver for `administer` on `gts.cf.construct.subject.fact.v1~`, with the caller's tenant as the owner tenant and no subject. **IF** it refuses, or no constraint of its scope reaches the whole tenant, refuse. **RETURN** the subject's key in the caller's tenant and the scope - `inst-admin-permission`

### Log the Administrator's Events

- [x] `p2` - **ID**: `cpt-cf-construct-algo-admin-facts-log`

**Input**: A read, added, edited, deleted or refused event.

**Steps**:
1. [x] - `p2` - Write it to the structured log under the target `construct::audit`, with the event's name, the tenant, the subject and the administrator as caller; for a read the categories, for a change the fact, its category and an edit's new fact, for a refusal the reason's name; never a value - `inst-log-event`

### Wire Administrator Fact Management

- [x] `p2` - **ID**: `cpt-cf-construct-algo-admin-facts-wire`

**Steps**:
1. [x] - `p2` - At start, build Administrator Fact Management over the profile writes, the review request store, the Review Queue and its one-step plan path, which exists only with the opt-in; register its routes - `inst-wire-admin-facts`

## 4. States (CDSL)

None. The administrator changes a review request only through the Review Queue's transitions (`cpt-cf-construct-state-review-queue-request`).

## 5. Definitions of Done

### The Administrator's Read

- [x] `p2` - **ID**: `cpt-cf-construct-dod-admin-facts-read`

The system **MUST** serve a caller with the permission to administer a subject's facts in its tenant every live fact of a subject of that tenant, with origin and latest review request, paged as the subject's own list, and **MUST** refuse a caller without it with 403. A subject of another tenant **MUST** read as one with no profile.

**Implements**:
- `cpt-cf-construct-flow-admin-facts-read`
- `cpt-cf-construct-algo-admin-facts-admin-only`

**Touches**:
- API: `GET /construct/v1/admin/subjects/{subject_id}/facts`

**Verified by**: `an_administrator_reads_all_facts_with_their_requests_and_the_read_is_recorded`, `a_caller_without_the_permission_or_with_a_narrower_scope_is_no_administrator`, `each_operation_asks_for_the_permission_to_administer_facts_in_the_callers_tenant`, `a_subject_of_another_tenant_answers_as_one_without_a_profile` (service), `an_administrator_lists_adds_edits_and_deletes_a_subjects_facts`, `a_caller_without_the_permission_gets_403_and_a_fact_the_subject_lacks_404` (routes), `every_enforced_permission_is_declared_once_on_its_resource_and_action` (permissions).

The route tests read a profile that only the path's subject in the caller's tenant has.

### The Administrator's Add and Edit

- [x] `p2` - **ID**: `cpt-cf-construct-dod-admin-facts-change`

The system **MUST** store an added or edited value through the one-step plan path, with the sensitive-data checks and Admission, and the administrator as origin; an add for a subject with no profile **MUST** create it. When nothing is stored, the answer **MUST** say why: 422 with the reason for a refusal, personalization off included, 404 for an edit of a fact that is gone, and 409 naming the fact for a value held already. An add and an edit **MUST** run only where the deployment opted in to storing values without sensitive-data checks.

**Implements**:
- `cpt-cf-construct-flow-admin-facts-add`
- `cpt-cf-construct-flow-admin-facts-edit`
- `cpt-cf-construct-algo-admin-facts-wire`

**Touches**:
- API: `POST /construct/v1/admin/subjects/{subject_id}/facts`, `PUT /construct/v1/admin/subjects/{subject_id}/facts/{fact_id}`
- Graph storage: a new fact's node, a replaced fact's node, the root's version
- Config: `processing.run_without_sensitive_data_checks`, `processing.max_reruns`

**Verified by**: `an_add_stores_a_new_fact_as_the_administrator`, `an_add_for_a_subject_without_a_profile_creates_it`, `an_add_of_a_held_value_or_a_second_one_valued_value_names_the_held_fact`, `an_unknown_property_or_a_value_its_schema_does_not_take_is_invalid`, `an_add_or_edit_while_personalization_is_off_stores_nothing_and_says_why`, `a_blocked_value_stores_nothing`, `without_the_opt_in_an_add_or_edit_is_not_configured_and_read_and_delete_work`, `an_edit_replaces_the_fact_under_a_new_key_as_the_administrator`, `an_edit_of_another_property_or_a_missing_fact_stores_nothing`, `an_edit_whose_fact_goes_away_before_the_write_stores_nothing`, `an_add_after_a_conflict_is_built_again_on_the_new_version`, `an_add_whose_value_lands_meanwhile_is_held_on_the_new_version`, `a_writer_failure_is_unavailable_and_records_nothing`, `a_policy_service_that_outlasts_the_budget_cuts_off_an_add_edit_or_delete` (service), `a_refused_value_is_422_naming_the_reason_and_a_held_one_409`, `an_unknown_property_or_a_value_its_schema_does_not_take_is_400_naming_the_field`, `a_body_without_a_property_or_a_value_or_with_another_field_stores_nothing` (routes), `an_administrators_add_needs_the_opt_in_to_store_values_unchecked` (pipeline).

### The Administrator's Delete

- [x] `p2` - **ID**: `cpt-cf-construct-dod-admin-facts-delete`

The system **MUST** soft-delete a fact for the administrator as the subject's delete does, with the administrator as origin, also while personalization is off, and **MUST** answer 404 for a fact the subject does not have.

**Implements**:
- `cpt-cf-construct-flow-admin-facts-delete`

**Touches**:
- API: `DELETE /construct/v1/admin/subjects/{subject_id}/facts/{fact_id}`
- Graph storage: the fact's node (soft-deleted), the root's version

**Verified by**: `a_delete_removes_the_fact_as_the_administrator_also_while_personalization_is_off`, `without_the_opt_in_an_add_or_edit_is_not_configured_and_read_and_delete_work`, `a_delete_closes_a_request_flagged_after_its_check_and_a_failed_close_is_repeated`, `a_policy_service_that_outlasts_the_budget_cuts_off_an_add_edit_or_delete` (service), `an_administrator_lists_adds_edits_and_deletes_a_subjects_facts`, `a_caller_without_the_permission_gets_403_and_a_fact_the_subject_lacks_404` (routes).

### Review Requests and the Administrator

- [x] `p2` - **ID**: `cpt-cf-construct-dod-admin-facts-review`

A stored edit, or a delete, of a fact under an open review request **MUST** close the request as corrected, with the new fact's id, or as deleted, closed by the administrator, through the Review Queue's resolution, so a reviewer and the administrator never both resolve it. An edit the checks or Admission refuse **MUST** leave the request open; while a reviewer holds it, the answer **MUST** be 409. Only what the administrator wrote **MUST** be recorded as theirs, also when the request did not close.

**Implements**:
- `cpt-cf-construct-flow-admin-facts-edit`
- `cpt-cf-construct-flow-admin-facts-delete`

**Touches**:
- DB Table: `construct__review_requests` (`cpt-cf-construct-dbtable-review-requests`)

**Verified by**: `an_edit_of_a_fact_under_review_closes_the_request_as_corrected_by_the_administrator`, `a_refused_edit_of_a_fact_under_review_leaves_the_request_open`, `an_edit_or_delete_while_a_reviewer_holds_the_request_is_a_conflict`, `a_delete_of_a_fact_under_review_closes_the_request_as_deleted_by_the_administrator`, `a_delete_of_a_fact_already_gone_closes_its_open_request`, `an_edit_or_delete_of_a_fact_an_earlier_resolution_replaced_writes_nothing`, `an_edit_or_delete_stored_but_not_closed_is_recorded_and_says_so`, `an_edit_closes_a_request_flagged_after_its_check_unless_a_reviewer_holds_it`, `an_edit_whose_close_of_a_request_flagged_after_its_check_fails_gives_the_claim_back`, `an_edit_leaves_a_request_flagged_after_its_check_with_a_recorded_write_to_its_resolution`, `an_edit_of_a_fact_under_review_that_goes_away_meanwhile_closes_the_request_and_is_404`, `an_edit_of_a_fact_under_review_resolves_it_within_the_edits_budget` (service), `an_edit_closes_the_facts_review_request_and_while_a_reviewer_holds_it_is_409` (routes).

### The Administrator's Events

- [x] `p2` - **ID**: `cpt-cf-construct-dod-admin-facts-events`

The system **MUST** log each read with the categories served, each add, edit and delete with the fact and its category, and each refused add or edit with the reason's name, each with the tenant, the subject and the administrator, and never a value.

**Implements**:
- `cpt-cf-construct-algo-admin-facts-log`

**Touches**:
- Log target: `construct::audit`

**Verified by**: `an_edit_names_the_administrator_the_old_and_the_new_fact`, `a_read_names_each_category_it_served`, `an_add_a_delete_and_a_refused_edit_name_their_fact`, and the recorded events in the service tests.

## 6. Acceptance Criteria

- [ ] An administrator reads a subject's facts with their origin, and adds, edits and deletes a fact (`an_administrator_lists_adds_edits_and_deletes_a_subjects_facts`).
- [ ] A value the checks block, or Admission drops, is not stored, and the call says so (`a_blocked_value_stores_nothing`, `an_add_or_edit_while_personalization_is_off_stores_nothing_and_says_why`, `a_refused_value_is_422_naming_the_reason_and_a_held_one_409`).
- [ ] A stored edit or delete of a fact under an open request closes that request (`an_edit_of_a_fact_under_review_closes_the_request_as_corrected_by_the_administrator`, `a_delete_of_a_fact_under_review_closes_the_request_as_deleted_by_the_administrator`).
- [ ] The operations are refused without their permission and see only the caller's tenant; a subject of another tenant answers as if it had no profile (`a_caller_without_the_permission_or_with_a_narrower_scope_is_no_administrator`, `a_subject_of_another_tenant_answers_as_one_without_a_profile`).
- [ ] Every answer is `no-store` with the security headers (`every_answer_is_no_store_with_the_security_headers`).
- [ ] The tests named in this document pass.

## 7. Non-Functional Considerations

- **Security**: The caller and the tenant come only from the platform. Every route needs the permission to administer a subject's facts with a scope that reaches the whole caller's tenant; a refusal answers 403. Only the caller's tenant is read and written, so another tenant's subject reads as one with no profile. Graph storage is read and written as Construct's processing identity; the review request table under the administrator's scope. Every answer is `no-store` with the security headers of the subject routes.
- **Privacy**: No event holds a value. An added or edited value passes the same checks and Admission as any value, and names the administrator as its origin.
- **Data integrity**: An add and an edit are written against the profile's version, a held value is refused on the version the plan is built on, and an edit replaces the fact under a new key, keeping the old value until the new one is stored. An edit or a delete of a fact under review claims its request first, so it never runs together with a reviewer's resolution.
- **Reliability**: The rerun limit and the 20-second budget bound an add, an edit and a delete, from the permission check on and a review request's resolution included; past either the answer is 503 and nothing is stored. A write that started before the budget ran out is not cut off by it and is awaited for at most 90 seconds; one that may have landed also answers 503, and the administrator reads the facts before sending it again. A write whose writer failed answers 503 too; it may have landed.
- **Performance**: The read cuts its page in memory from the subject's facts, as the subject's own view does, and reads the latest review requests of the page's facts in one query.
- **Observability**: Each operation has a tracing span with the tenant, the subject and the administrator. Events go to `construct::audit`.
- **Not done in this part**: see the out-of-scope list in 1.2.
