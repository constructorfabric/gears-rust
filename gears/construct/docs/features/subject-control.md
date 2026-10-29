# Feature: Subject Control


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [View One's Own Facts](#view-ones-own-facts)
  - [Read One of One's Own Facts](#read-one-of-ones-own-facts)
  - [Delete One of One's Own Facts](#delete-one-of-ones-own-facts)
  - [Export a Subject's Data](#export-a-subjects-data)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Owner Only](#owner-only)
  - [Read the Subject's Facts](#read-the-subjects-facts)
  - [Delete One Fact](#delete-one-fact)
  - [Cut a Page](#cut-a-page)
  - [Log Subject Control's Events](#log-subject-controls-events)
  - [Wire Subject Control](#wire-subject-control)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [The Subject's View](#the-subjects-view)
  - [The Subject's Delete](#the-subjects-delete)
  - [The Export](#the-export)
  - [Shared Pagination](#shared-pagination)
  - [Subject Control's Events](#subject-controls-events)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-subject-control-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-subject-control`

## 1. Feature Context

### 1.1 Overview

Subject Control lets the subject see every fact Construct holds about it, read one, and delete any of them, and lets the data protection officer export all of a subject's data. It owns the subject control component (`cpt-cf-construct-component-subject-control`), which Subject Settings, the Review Queue and Administrator Fact Management extend, and the shared pagination that the Review Queue and Administrator Fact Management reuse.

### 1.2 Purpose

The PRD requires that the subject sees all of its facts with their origin, in every category (`cpt-cf-construct-fr-subject-access`), that it can delete any fact, which is not served from the next read (`cpt-cf-construct-fr-subject-delete`, `cpt-cf-construct-nfr-deletion-time`), and that the data protection officer can give a subject all of its data as a machine-readable file. Personalization off and an erasure under way never block the subject's own view, delete or export.

**The routes.** The DESIGN writes the paths with the gateway's `/api` prefix; the code registers them without it.

| Route | Who | Answer |
|-------|-----|--------|
| `GET /construct/v1/subjects/{subject_id}/facts` | the subject | `200`, a page of its facts |
| `GET /construct/v1/subjects/{subject_id}/facts/{fact_id}` | the subject | `200`, one fact |
| `DELETE /construct/v1/subjects/{subject_id}/facts/{fact_id}` | the subject | `204` |
| `GET /construct/v1/subjects/{subject_id}/export` | the data protection officer | `200`, a JSON file |

The DESIGN lists the view, the delete and the export. The single-fact read is added here, because the subject's screens in the host application open one fact.

**Who may.** The view, the single fact and the delete are the subject's own actions, under the permission to control the profiles one owns (`cpt-cf-construct-fr-access-control`): the path must name the caller, and the AuthZ resolver must allow the action (`read` or `delete`) on the resource type `gts.cf.construct.subject.facts.v1~`, for the subject in the caller's tenant. Another subject, a caller the platform refuses, and a fact the subject does not have all answer `404`, like a subject that does not exist. An application that shows the subject its facts forwards the subject's own token. The export is not the subject's own action: the data protection officer exports a subject on its request, under the permission to export a subject's data (`export` on the same resource type); a caller without it gets `403`. The export covers the caller's tenant only, so a subject of another tenant exports empty, like a subject with no profile. Construct declares the three permissions as GTS permission instances; the tenant grants them in its policy engine.

**The facts.** Every live fact of the subject is listed: the ones the root lists for removal are left out, as in the profile read. The facts are read from graph storage as Construct's processing identity, not as the caller; without an identity every route answers `501`. Each fact has the profile read's shape plus its category: `{entity_kind, fact_id, properties, origin: {kind}, stored_at, produced_at}`. `fact_id` is the fact's graph node key, the id the profile read serves.

**The page.** `{items, page_info: {next_cursor, prev_cursor, limit}, version}`. `version` is the profile's version, none for a subject without a profile, so a client can poll for a change. `limit` is 50 by default and at most 200; a larger value is cut to 200, not refused. `$filter` and `$orderby` work over `fact_id`, `entity_kind`, `origin` and `stored_at`. The default order is `stored_at desc`, and `fact_id` breaks every tie. The cursor is opaque and remembers the order; a request that sends a cursor with another order, or a filter, field or cursor the list does not know, gets `400`.

**The delete.** One fact is soft-deleted, as the soft-delete constraint requires (`cpt-cf-construct-constraint-graph-soft-delete-only`): the delete is a plan with one removal, whose origin is the subject, written through the profile writer against the profile's version. The writer lists the fact in the root, so no read serves it from then on, and soft-deletes its node. On a write conflict the delete reads the profile again and writes again, at most as often as processing reruns a plan (`processing.max_reruns`, 2 by default); past that it answers `503`. A delete needs no model: the writer is built wherever graph storage and an identity are.

**The export.** `{tenant_id, subject_id, exported_at, settings: {subject_id, personalization_enabled, erasure_in_progress}, facts}`, as an attachment named `subject-{subject_id}.json`. The settings are read under the export's scope.

**The settings in the view.** The DESIGN shows the settings with the subject's view. The subject reads them at its settings route (Subject Settings); the export holds both.

**Events.** Each view and each read of one fact writes a read event with the categories served, each delete a delete event with the fact's category, and each export a read event with the caller and the categories exported. None holds a value. They go to the structured log until the Audit gear ships.

**Out of scope**:

- Review requests in the view and the export, and closing an open request when its fact is deleted; the Review Queue adds them.
- Erasing everything (Erasure).
- Refusing a repeat of a deleted fact's record; Record Intake's repeat check does that.
- Administrator operations.
- In-process client methods for these operations.
- The audit gear's read events in the export; the DESIGN has the data protection officer add them from the Audit gear.

**Requirements**: `cpt-cf-construct-fr-subject-access` (the view and the export), `cpt-cf-construct-fr-subject-delete` (the single-fact delete), `cpt-cf-construct-fr-fact-origin` (each fact with its origin), `cpt-cf-construct-nfr-deletion-time` (not served from the next read).

**Principles**: None. **Constraints**: `cpt-cf-construct-constraint-graph-soft-delete-only`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-subject` | Views its facts, reads one, deletes one, through an application that forwards its token. |
| `cpt-cf-construct-actor-dpo` | Exports a subject's data on the subject's request. |
| `cpt-cf-construct-actor-platform-auth` | Authenticates the caller and decides, through the AuthZ resolver, whether it may read, delete or export. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), the subject control component and the REST API
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.10
- **Dependencies**: `cpt-cf-construct-feature-profile-reader`, `cpt-cf-construct-feature-subject-settings`

## 2. Actor Flows (CDSL)

### View One's Own Facts

- [x] `p1` - **ID**: `cpt-cf-construct-flow-subject-control-view`

**Actor**: `cpt-cf-construct-actor-subject`

**Success Scenarios**:
- The subject gets 200 with a page of its live facts and the profile's version, also while personalization is off or an erasure is under way.

**Error Scenarios**:
- The path names another subject, or the platform refuses: 404, and nothing is read.
- A `$filter`, `$orderby` or cursor the list does not know: 400.
- No processing identity: 501. Graph storage or the policy service does not answer: 503.

**Steps**:
1. [x] - `p1` - Caller sends `GET /construct/v1/subjects/{subject_id}/facts`, authenticated, with optional `limit`, `cursor`, `$filter` and `$orderby` - `inst-view-send`
2. [x] - `p1` - API: the extractors take the security context, the service, the subject id and the `OData` query - `inst-view-parse`
3. [x] - `p1` - Run `cpt-cf-construct-algo-subject-control-owner-only` for `read`, then `cpt-cf-construct-algo-subject-control-read-facts` - `inst-view-read`
4. [x] - `p1` - Record a read event with the categories of the facts - `inst-view-event`
5. [x] - `p1` - Run `cpt-cf-construct-algo-subject-control-paginate` on the facts. **RETURN** 200 with the page and the version - `inst-view-return`

### Read One of One's Own Facts

- [x] `p1` - **ID**: `cpt-cf-construct-flow-subject-control-fact`

**Actor**: `cpt-cf-construct-actor-subject`

**Success Scenarios**:
- The subject gets 200 with one live fact.

**Error Scenarios**:
- Another subject, a refusal, or a fact the subject does not have: 404.
- No processing identity: 501. A dependency does not answer: 503.

**Steps**:
1. [x] - `p1` - Caller sends `GET /construct/v1/subjects/{subject_id}/facts/{fact_id}`, authenticated - `inst-fact-send`
2. [x] - `p1` - API: the extractors take the security context, the service, the subject id and the fact id - `inst-fact-parse`
3. [x] - `p1` - Run `cpt-cf-construct-algo-subject-control-owner-only` for `read` and `cpt-cf-construct-algo-subject-control-read-facts`. **IF** no live fact has the id, the fact is not visible - `inst-fact-read`
4. [x] - `p1` - Record a read event with the fact's category - `inst-fact-event`
5. [x] - `p1` - **RETURN** 200 with the fact - `inst-fact-return`

### Delete One of One's Own Facts

- [x] `p1` - **ID**: `cpt-cf-construct-flow-subject-control-delete`

**Actor**: `cpt-cf-construct-actor-subject`

**Success Scenarios**:
- The subject gets 204; the fact is not served from the next read.

**Error Scenarios**:
- Another subject, a refusal, or a fact the subject does not have: 404, and nothing is written.
- The profile kept changing past the reruns: 503, and the fact stays.
- No processing identity: 501.

**Steps**:
1. [x] - `p1` - Caller sends `DELETE /construct/v1/subjects/{subject_id}/facts/{fact_id}`, authenticated - `inst-delete-send`
2. [x] - `p1` - API: the extractors take the security context, the service, the subject id and the fact id - `inst-delete-parse`
3. [x] - `p1` - Run `cpt-cf-construct-algo-subject-control-delete`. **RETURN** 204 - `inst-delete-return`

### Export a Subject's Data

- [x] `p1` - **ID**: `cpt-cf-construct-flow-subject-control-export`

**Actor**: `cpt-cf-construct-actor-dpo`

**Success Scenarios**:
- The data protection officer gets 200 with the subject's settings and every live fact, as a JSON attachment. A subject without data in the tenant exports its default settings and no facts.

**Error Scenarios**:
- The platform refuses the export: 403 on the subject facts resource.
- No processing identity: 501. A dependency does not answer: 503.

**Steps**:
1. [x] - `p1` - Caller sends `GET /construct/v1/subjects/{subject_id}/export`, authenticated - `inst-export-send`
2. [x] - `p1` - API: the extractors take the security context, the service and the subject id - `inst-export-parse`
3. [x] - `p1` - Ask the AuthZ resolver for `export` on `gts.cf.construct.subject.facts.v1~`, with the subject as the resource id and the caller's tenant as the owner tenant. **IF** it refuses, or its scope does not reach the subject, refuse - `inst-export-authorize`
4. [x] - `p1` - Read the subject's settings under the scope's tenant constraints, and its facts as in `cpt-cf-construct-algo-subject-control-read-facts` - `inst-export-read`
5. [x] - `p1` - Record a read event with the caller and the categories exported - `inst-export-event`
6. [x] - `p1` - **RETURN** 200 with the export and `Content-Disposition: attachment; filename="subject-{subject_id}.json"` - `inst-export-return`

## 3. Processes / Business Logic (CDSL)

### Owner Only

- [x] `p1` - **ID**: `cpt-cf-construct-algo-subject-control-owner-only`

**Input**: The security context, the subject id in the path, and the action.

**Output**: The subject's key in the caller's tenant, or a forbidden error.

**Steps**:
1. [x] - `p1` - **IF** the subject id is not the caller's, refuse before the policy service is asked or anything is read; the message names the caller, its tenant and the subject, for the log - `inst-owner-compare`
2. [x] - `p1` - Ask the AuthZ resolver for the action on `gts.cf.construct.subject.facts.v1~`, with the subject as the resource id and the caller's tenant as the owner tenant. **IF** it refuses, or no constraint of its scope reaches the subject in the tenant, refuse. **RETURN** the key - `inst-owner-permission`

### Read the Subject's Facts

- [x] `p1` - **ID**: `cpt-cf-construct-algo-subject-control-read-facts`

**Input**: Construct's security context for the tenant and the subject's key.

**Output**: The profile's version and the live facts.

**Steps**:
1. [x] - `p1` - Build Construct's context for the tenant; without a processing identity, not configured. Read the root and its facts from graph storage as the profile read does, and leave out each fact the root lists for removal and each that cannot be decoded, logged at error. **RETURN** the root's version and the facts - `inst-facts-read`

### Delete One Fact

- [x] `p1` - **ID**: `cpt-cf-construct-algo-subject-control-delete`

**Input**: The security context, the subject id and the fact id.

**Output**: Done, or an error.

**Steps**:
1. [x] - `p1` - Run `cpt-cf-construct-algo-subject-control-owner-only` for `delete`, and build Construct's context for the tenant - `inst-del-own`
2. [x] - `p1` - Read the subject's facts as in `cpt-cf-construct-algo-subject-control-read-facts`. **IF** no live fact has the id, the fact is not visible, and nothing is written - `inst-del-read`
3. [x] - `p1` - Build a plan with one step that removes the fact, of its category, with the subject as origin and the version just read, and write it through the profile writer - `inst-del-write`
4. [x] - `p1` - **IF** the writer stored it, record a delete event with the fact's category. **RETURN** done. On a conflict, go back to step 2, at most `processing.max_reruns` times - `inst-del-stored`
5. [x] - `p1` - **RETURN** unavailable: the profile kept changing - `inst-del-give-up`

### Cut a Page

- [x] `p1` - **ID**: `cpt-cf-construct-algo-subject-control-paginate`

**Input**: The whole list, the `OData` query, and the list's default order and tiebreaker.

**Output**: A page with the next page's cursor, or a mistake of the caller's.

**Steps**:
1. [x] - `p1` - Take `limit`, 50 when absent, at least 1 and at most 200 - `inst-page-limit`
2. [x] - `p1` - **IF** the query has a `$filter`, check it against the list's fields and keep the items it matches - `inst-page-filter`
3. [x] - `p1` - Take the order: the query's `$orderby`, else the one the cursor was issued under, else the list's default; add the tiebreaker last. **IF** a field is not the list's, refuse. Sort - `inst-page-order`
4. [x] - `p1` - **IF** the query has a cursor, check that it was issued for this order and filter, and skip every item up to its position - `inst-page-cursor`
5. [x] - `p1` - Take `limit` items. **IF** more follow, the next cursor holds the last item's keys, the order and the filter. **RETURN** the page - `inst-page-cut`

### Log Subject Control's Events

- [x] `p1` - **ID**: `cpt-cf-construct-algo-subject-control-log`

**Input**: A view, delete or export event.

**Steps**:
1. [x] - `p1` - Write it to the structured log under the target `construct::audit`, with the event's name, the tenant, the subject, the caller and the categories; never a value - `inst-log-event`

### Wire Subject Control

- [x] `p1` - **ID**: `cpt-cf-construct-algo-subject-control-wire`

**Steps**:
1. [x] - `p1` - At start, build the profile writer over graph storage whether or not a model is configured, and Subject Control over the profile read's graph source, that writer, the processing identity, the settings and the policy enforcer, with processing's rerun limit. Record processing uses the same writer - `inst-wire-subject-control`

## 4. States (CDSL)

None. Subject Control keeps no state of its own.

## 5. Definitions of Done

### The Subject's View

- [x] `p1` - **ID**: `cpt-cf-construct-dod-subject-control-view`

The system **MUST** serve the subject, and only the subject, every live fact of its own with its category, origin and times, paged, and one fact by its id, under the `read` permission on the subject facts resource, whether personalization is on or not. Another subject, a refusal and an unknown fact **MUST** answer 404, before anything is read for another subject.

**Implements**:
- `cpt-cf-construct-flow-subject-control-view`
- `cpt-cf-construct-flow-subject-control-fact`
- `cpt-cf-construct-algo-subject-control-owner-only`
- `cpt-cf-construct-algo-subject-control-read-facts`

**Touches**:
- API: `GET /construct/v1/subjects/{subject_id}/facts`, `GET /construct/v1/subjects/{subject_id}/facts/{fact_id}`
- Graph storage: the subject's root and facts, read as Construct

**Verified by**: `the_owner_sees_every_fact_with_the_version_also_while_off_and_erasing`, `another_subject_is_refused_before_the_policy_service_is_asked_or_anything_is_read`, `the_owner_the_platform_refuses_gets_nothing`, `each_operation_asks_for_its_own_action_on_the_subject_facts`, `one_fact_is_served_and_a_fact_the_subject_does_not_have_is_not_visible`, `without_a_processing_identity_every_operation_is_not_configured` (service), `the_owner_lists_its_facts_newest_first_with_kind_origin_and_version`, `another_subject_or_a_refused_owner_gets_404_on_the_facts_resource`, `the_owner_reads_one_fact_deletes_it_and_then_it_is_not_found` (routes), `a_record_is_planned_checked_admitted_written_read_back_and_deleted_by_its_subject` (over graph storage's in-memory store).

### The Subject's Delete

- [x] `p1` - **ID**: `cpt-cf-construct-dod-subject-control-delete`

The system **MUST** soft-delete one live fact of the subject as a one-removal plan with the subject as origin, written against the profile's version, and **MUST** read and write again on a conflict up to the rerun limit, then answer unavailable. The fact **MUST NOT** be served from the next read, and each delete **MUST** write an event without content.

**Implements**:
- `cpt-cf-construct-flow-subject-control-delete`
- `cpt-cf-construct-algo-subject-control-delete`
- `cpt-cf-construct-algo-subject-control-wire`

**Touches**:
- API: `DELETE /construct/v1/subjects/{subject_id}/facts/{fact_id}`
- Graph storage: the root's removal list and version, the fact's node (soft delete)
- Config: `processing.max_reruns`

**Verified by**: `a_delete_writes_one_removal_as_the_subject_against_the_version_and_the_fact_is_gone`, `a_delete_reads_and_writes_again_on_a_conflict`, `a_delete_gives_up_as_unavailable_once_the_reruns_are_spent`, `deleting_a_fact_the_subject_does_not_have_writes_nothing` (service), `the_owner_reads_one_fact_deletes_it_and_then_it_is_not_found` (routes), `a_record_is_planned_checked_admitted_written_read_back_and_deleted_by_its_subject` (the real writer and soft delete over graph storage's in-memory store; the profile read no longer serves the fact and the version moved on).

### The Export

- [x] `p1` - **ID**: `cpt-cf-construct-dod-subject-control-export`

The system **MUST** give the caller with the export permission all of a subject's data in the caller's tenant, the settings and every live fact, as a JSON attachment, and **MUST** answer 403 without the permission. Each export **MUST** write a read event without content.

**Implements**:
- `cpt-cf-construct-flow-subject-control-export`

**Touches**:
- API: `GET /construct/v1/subjects/{subject_id}/export`
- DB Table: `construct__subject_settings` (read)

**Verified by**: `the_export_needs_its_permission_not_the_owner_and_holds_the_settings_and_every_fact` (service), `the_export_is_a_json_attachment_and_403_without_its_permission` (routes).

### Shared Pagination

- [x] `p1` - **ID**: `cpt-cf-construct-dod-subject-control-pagination`

The system **MUST** cut a list into cursor pages of at most 200 items, cutting a larger `limit` to 200, with `$filter` and `$orderby` over the fields the list names and a tiebreaker that makes every order total. A cursor **MUST** be refused for another order or filter, and an unknown field **MUST** be the caller's mistake.

**Implements**:
- `cpt-cf-construct-algo-subject-control-paginate`

**Touches**:
- Code: `api/rest/pagination.rs`, for the Review Queue's and Administrator Fact Management's lists too

**Verified by**: `the_default_page_is_newest_first_and_a_limit_past_the_maximum_is_cut_to_it`, `following_the_cursor_walks_every_fact_once_in_order`, `ties_are_broken_by_the_fact_id`, `a_filter_keeps_the_facts_it_matches`, `an_unknown_field_or_a_cursor_of_another_order_is_the_callers_mistake` (pagination), `the_list_filters_orders_and_pages` (routes).

### Subject Control's Events

- [x] `p1` - **ID**: `cpt-cf-construct-dod-subject-control-events`

The system **MUST** log each view, delete and export as an event with the tenant, the subject, the caller and the categories, and never a value.

**Implements**:
- `cpt-cf-construct-algo-subject-control-log`

**Touches**:
- Log target: `construct::audit`

**Verified by**: `each_event_names_its_kind_ids_and_categories_and_no_value`, and the recorded events in the service tests.

## 6. Acceptance Criteria

- [ ] A subject sees all its facts with origin, also while personalization is off (`the_owner_sees_every_fact_with_the_version_also_while_off_and_erasing`, `the_owner_lists_its_facts_newest_first_with_kind_origin_and_version`).
- [ ] A deleted fact is not served from the next read (`a_record_is_planned_checked_admitted_written_read_back_and_deleted_by_its_subject`, `the_owner_reads_one_fact_deletes_it_and_then_it_is_not_found`).
- [ ] The export is refused without its permission; another subject or tenant answers as if there were no profile (`the_export_is_a_json_attachment_and_403_without_its_permission`, `another_subject_or_a_refused_owner_gets_404_on_the_facts_resource`).
- [ ] Every operation asks for its own permission, and Construct declares it (`each_operation_asks_for_its_own_action_on_the_subject_facts`, `every_enforced_permission_is_declared_once`).
- [ ] Every answer is `no-store` with the security headers (`every_answer_is_no_store_with_the_security_headers`).
- [ ] The tests named in this document pass.

## 7. Non-Functional Considerations

- **Security**: The caller and the tenant come only from the platform. The view, one fact and the delete are owner only, checked before the policy service is asked, and each asks for its own action; refusals and unknown facts answer 404, so the answer tells nothing about another subject. The export answers 403 to a caller without its permission, and covers the caller's tenant only. Graph storage is read and written as Construct's processing identity. Every answer is `no-store` with `nosniff`, `X-Frame-Options: DENY` and a CSP that allows nothing.
- **Data integrity**: A delete is written against the profile's version, so it cannot remove a fact from a profile that changed under it; it reads again and retries. The fact is listed in the root before its node is soft-deleted, so no read serves it in between. Nothing is hard-deleted.
- **Reliability**: The rerun limit bounds a delete; past it the answer is 503, which the caller may retry.
- **Performance**: A list is cut from the whole profile in memory; a profile holds at most as many facts as one read takes, so a page costs one graph read.
- **Observability**: Each operation has a tracing span with the tenant and the subject; the export's also names the caller. Events go to `construct::audit`.
- **Not done in this part**: see the out-of-scope list in 1.2.
