# Feature: Review Queue


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Flag One of One's Own Facts](#flag-one-of-ones-own-facts)
  - [List One's Own Review Requests](#list-ones-own-review-requests)
  - [List the Open Review Requests](#list-the-open-review-requests)
  - [Resolve a Review Request](#resolve-a-review-request)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Reviewer Only](#reviewer-only)
  - [Correct a Fact](#correct-a-fact)
  - [Remove a Fact Under Review](#remove-a-fact-under-review)
  - [Close a Request](#close-a-request)
  - [Hide a Fact Under Review](#hide-a-fact-under-review)
  - [Skip a Fact Under Review](#skip-a-fact-under-review)
  - [Log the Review Queue's Events](#log-the-review-queues-events)
  - [Wire the Review Queue](#wire-the-review-queue)
- [4. States (CDSL)](#4-states-cdsl)
  - [Review Request](#review-request)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Review Request Storage](#review-request-storage)
  - [The Subject's Flag and List](#the-subjects-flag-and-list)
  - [The Reviewer's List and Resolution](#the-reviewers-list-and-resolution)
  - [The One-Step Plan Path](#the-one-step-plan-path)
  - [A Fact Under Review Is Frozen](#a-fact-under-review-is-frozen)
  - [Review Requests in Subject Control](#review-requests-in-subject-control)
  - [The Review Queue's Events](#the-review-queues-events)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-review-queue-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-review-queue`

## 1. Feature Context

### 1.1 Overview

The Review Queue lets the subject mark one of its facts as incorrect, which opens a review request, and lets a reviewer of the tenant resolve the request as corrected, deleted or rejected. While a request is open, the fact is not served to applications and agents, and no record or agent changes it; the subject still sees it. The Review Queue owns the table `construct__review_requests`, the one-step plan path that turns a value set by hand into a plan without the planner, and the hooks that hide and skip a fact under review in the Profile Reader and in processing.

### 1.2 Purpose

The PRD requires that the subject can mark any fact as incorrect, with an optional comment; that while the request is open the fact is not served and not changed by a record or an agent; that the tenant's reviewers see the open requests and resolve each as corrected, deleted, or rejected with a reason; that a corrected value passes the same guardrails as any fact and names the reviewer as its origin; that a request closes as deleted when its fact is deleted first; and that the subject sees the state and outcome of its requests (`cpt-cf-construct-fr-review-request`). Each correction and each resolution is an event without content.

**The routes.** The DESIGN writes the paths with the gateway's `/api` prefix; the code registers them without it.

| Route | Who | Answer |
|-------|-----|--------|
| `POST /construct/v1/subjects/{subject_id}/review-requests` | the subject | `201`, the request |
| `GET /construct/v1/subjects/{subject_id}/review-requests` | the subject | `200`, a page of its requests |
| `GET /construct/v1/admin/review-requests` | a reviewer | `200`, a page of the tenant's open requests |
| `POST /construct/v1/admin/review-requests/{request_id}/resolution` | a reviewer | `200`, the closed request |

**Who may.** Flagging and the subject's list are the subject's own actions, under the permission to control the profiles one owns, as in Subject Control (`cpt-cf-construct-algo-subject-control-owner-only`): the path must name the caller, and the AuthZ resolver must allow `flag` (to flag) or `read` (to list) on `gts.cf.construct.subject.fact.v1~` for the subject in the caller's tenant; otherwise `404`. The two admin routes need the permission to resolve review requests: `resolve` on `gts.cf.construct.review.request.v1~` in the caller's tenant; a caller without it gets `403`. Only a constraint that names the caller's tenant, and nothing narrower, makes a reviewer. The permission covers every category: the reviewers the PRD names, tenant administrators and data protection officers, read every fact of the tenant's subjects anyway. A reviewer may not resolve a request on its own fact (`403`), so nobody corrects their own facts as a reviewer. The DESIGN makes the tenant's assigned reviewers a tenant setting in the settings service; Construct declares no settings there yet, so for now the permission alone decides who is a reviewer, and the tenant grants it to the people it assigns. Construct declares both permissions as GTS permission instances. The table is read and written under the caller's scope: the platform's scope on the subject's facts for the subject's calls and the export, the reviewer's for the admin routes, Construct's own for the profile read and processing.

**The request.** `{id, subject_id, fact_id, entity_kind, property, value, comment, state, outcome, reason, created_at, resolved_at, corrected_fact_id}`. `state` is `open` or `closed`; `outcome` is `corrected`, `deleted` or `rejected` once closed. When the subject flags a fact, the fact's property and value are copied into the request, so the reviewer sees what the subject flagged; the fact cannot change while the request is open, so the copy stays true. The value is cleared when the request closes, so a deleted or corrected value is not kept in Construct's table. `value`, `comment`, `outcome`, `reason`, `resolved_at` and `corrected_fact_id` are left out when they have none. A comment and a reason are trimmed, a blank one is none, and each is at most 1000 characters.

**The flag.** The body is `{fact_id, comment}`, `comment` optional and at most 1000 characters. The fact must be a live fact of the subject; another answers `404`. A fact may have at most one open request: a second flag answers `409`, also when two arrive at once. A fact that a record or a delete removes while the request is stored answers `404`, and the request is closed as deleted at once. A flag works while personalization is off or an erasure is under way, like the subject's other own actions.

**The lists.** Both are cursor pages, `{items, page_info: {next_cursor, prev_cursor, limit}}`, read from the table under the caller's scope: `limit` 50 by default and at most 200; `$filter` and `$orderby`; an opaque cursor that remembers the order and the filter. The subject's list holds all its requests, open and closed, newest first, filtered and ordered over `id`, `fact_id`, `state`, `outcome` and `created_at`. The reviewer's list holds the open requests of the caller's tenant, oldest first, so the queue is worked in order, filtered and ordered over `id`, `subject_id`, `fact_id`, `entity_kind` and `created_at`. `id` breaks every tie.

**The resolution.** The body is `{outcome, reason, properties}`. `reason` is required for `rejected` and optional otherwise, at most 1000 characters. `properties` is required for `corrected`, and only there: one entry, the fact's own property, with the new value. An unknown request, or one of another tenant, answers `404`. Each resolution claims the request before the fact is written, with a claim of its own, so two resolutions never run at once, also not two from the same reviewer. A `409` names its reason: `CLOSED` for a closed request; `BEING_RESOLVED` while another resolution holds it, which a retry after it may pass; `ALREADY_OPEN` for a second flag; `WRITTEN_BUT_CLOSED` when the resolution was stored but the subject's delete closed the request meanwhile; `WRITTEN_NOT_CLOSED` when the resolution was stored, or may have been, but the request could not be closed. The last two carry the outcome and the new fact's id in the message. Before a delete or a correction is written, the claim records it: the outcome, the new fact's id, the reviewer and the reason. A resolution that stored nothing gives the claim back and forgets that record; one whose write failed and may have landed, or whose close failed, keeps it. A claim frees itself after 140 seconds: the 20-second budget, the 90 seconds the writer is awaited at most, and 30 seconds for a statement graph storage still runs. The next resolution then closes the request as the recorded one when its write is in the profile (its new fact is there, or its deleted fact is gone), and otherwise goes on as asked. When the writer fails, the profile is read once more, and a write found there counts as stored. A resolution that finds its fact gone, a rejection included, closes the request as deleted and logs a warning.

- **Rejected**: the request closes with the reason. The fact is served again.
- **Deleted**: the fact is soft-deleted as the subject's delete does it, and the request closes. A fact already gone closes it all the same.
- **Corrected**: the new value goes through the one-step plan path: a plan with one replace step, the reviewer as origin, the sensitive-data checks and Admission like any plan, and the profile writer. The replace gives the fact a new id, `corrected_fact_id`. When the checks fail, a value is blocked, or Admission drops the plan (personalization off, a rule broken), nothing is stored, the request stays open, and the answer is `422` naming the reason. When the fact is gone before the correction is stored, nothing is stored and the request closes as deleted, which the answer shows. No sensitive-data check exists yet, so a correction, like record processing, runs only where the deployment opted in (`processing.run_without_sensitive_data_checks`); elsewhere it answers `501`. A rejection and a delete need no opt-in. Without a processing identity a delete and a correction answer `501`.

A delete and a correction are written against the profile's version, and read and written again after a short pause on a conflict, as the subject's delete does (`processing.max_reruns`, within 20 seconds); past either they answer `503` and the request stays open. The 20 seconds bound the reads, checks and pauses before each write; the budget does not cut a write that started, which the writer bounds at 90 seconds. When the writer fails or runs out, the profile is read once more, a write found there counts as stored, and one not found is answered as possibly written, never as not written. A correction takes its sensitive-data checks and Admission from the same place record processing does, so a check added there guards both, and the start-up warning that values are stored unchecked covers both.

**Frozen while open.** The profile read leaves out a fact under an open request, as it does for a category the caller may not read. Processing drops every planned step that would replace or remove such a fact, or add a property to it, before the checks; the drop is logged without content, with the subject, and a plan left empty stores nothing. A manage-facts call left with nothing answers `under_review`. The planner itself still sees the fact, so the model does not add it again; `FactsUnderReview::under_review` tells it which facts are under review if it wants to leave them out of the model's choices. The subject's own view still shows the fact. A plan whose step was checked before the request opened can still land a few milliseconds after it; the request then points at a fact that is gone, and its resolution closes it as deleted.

**In Subject Control.** Each fact in the subject's view carries its latest review request, `{id, state, outcome, created_at}`, or `null`. The export holds all the subject's requests. The subject's delete closes an open request on the fact as deleted; when that close fails, the delete answers `503`, and its repeat, which finds the fact gone, closes the request and answers `204`.

**Events.** Each flag writes a requested event with the request and the fact, and each resolution a resolved event with the outcome, the reviewer and, for a correction, the new fact id. A correction the checks or Admission refuse writes a refused event with the reason's name, and each page of the reviewers' list a listed event with the reviewer and the ids of the requests it served, as that page holds flagged values. None holds a value, a comment or a reviewer's reason. They go to the structured log until the Audit gear ships.

**Out of scope**:

- The tenant's list of assigned reviewers in the settings service; the permission decides until Construct declares its settings there.
- Closing a request when retention removes its fact; Retention calls the closing when it lands.
- Removing review requests on erasure (Erasure).
- Administrator Fact Management, which reuses the one-step plan path.
- In-process client methods for these operations.

**Requirements**: `cpt-cf-construct-fr-review-request`; shared: `cpt-cf-construct-fr-fact-origin` (the reviewer as origin), `cpt-cf-construct-fr-subject-access` (review requests in the subject's view), `cpt-cf-construct-fr-sensitive-data-guardrails` (a corrected value passes the checks and Admission).

**Principles**: None. **Constraints**: `cpt-cf-construct-constraint-graph-soft-delete-only`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-subject` | Flags a fact as incorrect and follows its requests, through an application that forwards its token. |
| `cpt-cf-construct-actor-tenant-admin` | Resolves review requests when the tenant assigns it. |
| `cpt-cf-construct-actor-dpo` | Resolves review requests when the tenant assigns it. |
| `cpt-cf-construct-actor-platform-auth` | Authenticates the caller and decides, through the AuthZ resolver, whether it may flag, list or resolve. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), the subject control component, the review sequence and the table `construct__review_requests`
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.11
- **Dependencies**: `cpt-cf-construct-feature-subject-control`, `cpt-cf-construct-feature-profile-writer`, `cpt-cf-construct-feature-admission`, `cpt-cf-construct-feature-sensitive-data-checks`

## 2. Actor Flows (CDSL)

### Flag One of One's Own Facts

- [x] `p1` - **ID**: `cpt-cf-construct-flow-review-queue-flag`

**Actor**: `cpt-cf-construct-actor-subject`

**Success Scenarios**:
- The subject gets 201 with the open request; the fact is no longer served to applications and agents.

**Error Scenarios**:
- Another subject, a refusal, or a fact the subject does not have: 404, and nothing is stored.
- The fact already has an open request: 409.
- A comment over 1000 characters: 400.
- No processing identity: 501. A dependency does not answer: 503.

**Steps**:
1. [x] - `p1` - Caller sends `POST /construct/v1/subjects/{subject_id}/review-requests` with `{fact_id, comment}`, authenticated - `inst-flag-send`
2. [x] - `p1` - API: the extractors take the security context, the service, the subject id and the body; a fact id not shaped like a node key answers 404, a comment over 1000 characters 400 - `inst-flag-parse`
3. [x] - `p1` - Run `cpt-cf-construct-algo-subject-control-owner-only` for `flag`, and read the subject's facts as in `cpt-cf-construct-algo-subject-control-read-facts`. **IF** no live fact has the id, no such fact - `inst-flag-read`
4. [x] - `p1` - Store an open request with a new id, the fact's id, category, property and value, the comment and the time. **IF** the fact already has an open request, refuse as a conflict. Read the facts again: **IF** the fact is gone, close the request as deleted and answer no such fact; **IF** the read fails, keep the request open - `inst-flag-store`
5. [x] - `p1` - Record a requested event. **RETURN** 201 with the request - `inst-flag-return`

### List One's Own Review Requests

- [x] `p1` - **ID**: `cpt-cf-construct-flow-review-queue-list-own`

**Actor**: `cpt-cf-construct-actor-subject`

**Success Scenarios**:
- The subject gets 200 with a page of its requests, open and closed, newest first.

**Error Scenarios**:
- Another subject, or a refusal: 404.
- A `$filter`, `$orderby` or cursor the list does not know: 400.

**Steps**:
1. [x] - `p1` - Caller sends `GET /construct/v1/subjects/{subject_id}/review-requests`, authenticated, with optional `limit`, `cursor`, `$filter` and `$orderby` - `inst-own-send`
2. [x] - `p1` - Run `cpt-cf-construct-algo-subject-control-owner-only` for `read` - `inst-own-authorize`
3. [x] - `p1` - Read the page of the subject's requests from the table, under the tenant and the subject - `inst-own-read`
4. [x] - `p1` - **RETURN** 200 with the page - `inst-own-return`

### List the Open Review Requests

- [x] `p1` - **ID**: `cpt-cf-construct-flow-review-queue-list-open`

**Actor**: `cpt-cf-construct-actor-tenant-admin`

**Success Scenarios**:
- The reviewer gets 200 with a page of the tenant's open requests, oldest first, each with the flagged value.

**Error Scenarios**:
- The platform refuses: 403.
- A `$filter`, `$orderby` or cursor the list does not know: 400.

**Steps**:
1. [x] - `p1` - Caller sends `GET /construct/v1/admin/review-requests`, authenticated, with optional `limit`, `cursor`, `$filter` and `$orderby` - `inst-open-send`
2. [x] - `p1` - Run `cpt-cf-construct-algo-review-queue-reviewer-only` - `inst-open-authorize`
3. [x] - `p1` - Read the page of the tenant's open requests from the table, under the scope, and record a listed event with the ids it holds - `inst-open-read`
4. [x] - `p1` - **RETURN** 200 with the page - `inst-open-return`

### Resolve a Review Request

- [x] `p1` - **ID**: `cpt-cf-construct-flow-review-queue-resolve`

**Actor**: `cpt-cf-construct-actor-tenant-admin`

**Success Scenarios**:
- The reviewer gets 200 with the closed request: rejected, deleted, or corrected with the new fact's id.
- A correction of a fact that is gone closes the request as deleted.

**Error Scenarios**:
- The platform refuses: 403.
- An unknown request, or one of another tenant: 404.
- A closed request: 409 `CLOSED`; one another resolution holds: 409 `BEING_RESOLVED`.
- A resolution written but not closed: 409 `WRITTEN_BUT_CLOSED` or `WRITTEN_NOT_CLOSED`.
- A body that does not fit the outcome: 400.
- A correction the checks or Admission do not let through: 422, and the request stays open.
- A correction without the opt-in, or a delete or correction without a processing identity: 501.
- The profile kept changing: 503, and the request stays open.

**Steps**:
1. [x] - `p1` - Caller sends `POST /construct/v1/admin/review-requests/{request_id}/resolution` with `{outcome, reason, properties}`, authenticated - `inst-resolve-send`
2. [x] - `p1` - API: the extractors take the security context, the service, the request id and the body; a body that does not fit its outcome answers 400 - `inst-resolve-parse`
3. [x] - `p1` - Run `cpt-cf-construct-algo-review-queue-reviewer-only`, and read the request in the caller's tenant. **IF** there is none, not found. **IF** it is closed, conflict. Claim it, and read in the same step what an earlier claim recorded; **IF** another claim holds it, conflict - `inst-resolve-read`
4. [x] - `p1` - **IF** an earlier claim recorded a write that is in the profile, run `cpt-cf-construct-algo-review-queue-close` as that one - `inst-resolve-settle`
5. [x] - `p1` - **IF** rejected, run `cpt-cf-construct-algo-review-queue-close` with the reason - `inst-resolve-reject`
6. [x] - `p1` - **IF** deleted, record the delete on the claim, run `cpt-cf-construct-algo-review-queue-remove`, then `cpt-cf-construct-algo-review-queue-close` - `inst-resolve-delete`
7. [x] - `p1` - **IF** corrected, run `cpt-cf-construct-algo-review-queue-correct`; on a stored correction, run `cpt-cf-construct-algo-review-queue-close` with the new fact's id; on a fact that is gone, close as deleted; on a refusal, record a refused event and give the claim back - `inst-resolve-correct`
8. [x] - `p1` - Record a resolved event. **RETURN** 200 with the closed request. **IF** the close failed, or found the request no longer held, read it again: closed as this resolution, **RETURN** it; closed otherwise, conflict `WRITTEN_BUT_CLOSED`; else conflict `WRITTEN_NOT_CLOSED`, with the claim kept - `inst-resolve-return`

## 3. Processes / Business Logic (CDSL)

### Reviewer Only

- [x] `p1` - **ID**: `cpt-cf-construct-algo-review-queue-reviewer-only`

**Input**: The security context.

**Output**: The scope of the caller's tenant, or a forbidden error.

**Steps**:
1. [x] - `p1` - Ask the AuthZ resolver for `resolve` on `gts.cf.construct.review.request.v1~`, with the caller's tenant as the owner tenant. **IF** it refuses, or no constraint of its scope reaches the tenant, refuse. **RETURN** the scope - `inst-reviewer-permission`

### Correct a Fact

- [x] `p1` - **ID**: `cpt-cf-construct-algo-review-queue-correct`

**Input**: The reviewer, the open request and the new value.

**Output**: The new fact's id, the fact is gone, or why nothing was stored.

**Steps**:
1. [x] - `p1` - **IF** the deployment did not opt in to storing values without sensitive-data checks, not configured. Start the budget (20 seconds), and build Construct's context and scope for the tenant - `inst-correct-start`
2. [x] - `p1` - Read the subject's facts, cut off when the budget runs out (unavailable). **IF** no live fact has the request's fact id, **RETURN** gone - `inst-correct-read`
3. [x] - `p1` - Build a plan with one step that replaces the fact with a new entity under a new node key, holding the new value, with the reviewer as origin and the version just read - `inst-correct-plan`
4. [x] - `p1` - Run the sensitive-data checks and Admission on it. **IF** a check fails, Admission drops the plan, or the value is blocked, **RETURN** nothing stored, with the reason - `inst-correct-admit`
5. [x] - `p1` - Record the correction with the new key on the claim; **IF** the claim no longer holds the request, conflict. Write it through the profile writer, for at most 90 seconds; when the writer fails, read the facts once more and count the new key found there as stored. **IF** stored, **RETURN** the new key. On a conflict, pause briefly and go back to step 2, at most `processing.max_reruns` times and while the budget lasts; then unavailable - `inst-correct-write`

### Remove a Fact Under Review

- [x] `p1` - **ID**: `cpt-cf-construct-algo-review-queue-remove`

**Input**: The reviewer and the open request.

**Output**: Done, or an error.

**Steps**:
1. [x] - `p1` - Start the budget, build Construct's context, and read the subject's facts. **IF** the fact is gone, **RETURN** done - `inst-remove-read`
2. [x] - `p1` - Write a plan with one removal, with the reviewer as origin, against the version just read; on a conflict, read and write again as in `cpt-cf-construct-algo-subject-control-delete` - `inst-remove-write`

### Close a Request

- [x] `p1` - **ID**: `cpt-cf-construct-algo-review-queue-close`

**Input**: The request, the outcome, who closed it, and the reason or the new fact id.

**Output**: The closed request, or a conflict.

**Steps**:
1. [x] - `p1` - Update the request only while it is open: set it closed with the outcome, the reason, the new fact id, who closed it and the time, and clear the copied value. **IF** no open request was updated, conflict - `inst-close-update`

### Hide a Fact Under Review

- [x] `p1` - **ID**: `cpt-cf-construct-algo-review-queue-hide`

**Input**: The facts the profile read would serve, and the subject's key.

**Output**: The facts without those under an open request.

**Steps**:
1. [x] - `p1` - Read the fact ids of the subject's open requests from the table, as Construct. Leave those facts out - `inst-hide-filter`

### Skip a Fact Under Review

- [x] `p1` - **ID**: `cpt-cf-construct-algo-review-queue-skip`

**Input**: A plan from the planner.

**Output**: The plan without the steps that touch a fact under review.

**Steps**:
1. [x] - `p1` - Read the fact ids of the subject's open requests as Construct, also for a manage-facts call, whose agent's scope may not reach them. Drop each step that replaces or removes one of them or adds a property to it, and log how many were dropped, without content - `inst-skip-drop`

### Log the Review Queue's Events

- [x] `p1` - **ID**: `cpt-cf-construct-algo-review-queue-log`

**Input**: A requested, resolved, refused or listed event.

**Steps**:
1. [x] - `p1` - Write it to the structured log under the target `construct::audit`, with the event's name, the tenant, the subject, the request, the fact, the caller and, for a resolution, the outcome, for a refusal its reason's name, and for a listed page its request ids; never a value, comment or reviewer's reason - `inst-log-event`

### Wire the Review Queue

- [x] `p1` - **ID**: `cpt-cf-construct-algo-review-queue-wire`

**Steps**:
1. [x] - `p1` - At start, build the review request store over the database, and the Review Queue over it, the profile read's graph source, the profile writer, the processing identity, the sensitive-data checks and Admission, with processing's rerun limit and the opt-in. Give the store to the profile read, to processing and to Subject Control - `inst-wire-review-queue`

## 4. States (CDSL)

### Review Request

- [x] `p1` - **ID**: `cpt-cf-construct-state-review-queue-request`

**States**: open, closed
**Initial State**: open

**Transitions**:
1. [x] - `p1` - **FROM** open **TO** closed **WHEN** a reviewer rejects it, deletes the fact, or stores a correction; or the subject deletes the fact; or a correction finds the fact gone - `inst-state-close`

A closed request never opens again; the subject flags the fact again for a new review.

## 5. Definitions of Done

### Review Request Storage

- [x] `p1` - **ID**: `cpt-cf-construct-dod-review-queue-storage`

The system **MUST** keep each review request, open or closed, in `construct__review_requests`, scoped to its tenant, with at most one open request per fact, which the database enforces. The flagged value **MUST** be cleared when the request closes.

**Implements**:
- `cpt-cf-construct-algo-review-queue-close`
- `cpt-cf-construct-state-review-queue-request`

**Touches**:
- DB Table: `construct__review_requests` (`cpt-cf-construct-dbtable-review-requests`)
- Entities: Review Request (`cpt-cf-construct-entity-review-request`)

**Verified by**: `an_open_request_reads_back_as_stored`, `a_scope_naming_the_subject_reaches_its_requests_and_another_subjects_does_not`, `a_second_open_request_on_a_fact_is_refused_and_a_closed_one_is_not`, `one_claim_holds_a_request_until_it_is_given_back_or_goes_stale`, `a_close_needs_the_claim_sets_the_outcome_clears_the_value_and_happens_once`, `a_correction_keeps_the_new_facts_id`, `the_open_request_on_a_fact_closes_by_the_fact_claimed_or_not`, `the_subjects_requests_are_newest_first_and_the_latest_per_fact_is_the_newest`, `a_scope_of_another_tenant_finds_claims_and_closes_nothing`, `an_unknown_request_is_none` (the store on `SQLite`), `every_backend_keys_one_open_request_per_fact_and_indexes_the_lists`, `mysql_ddl_pins_utf8mb4`, `identifiers_are_within_63_bytes` (migration), `a_review_text_is_trimmed_and_bounded_in_characters_not_bytes` (domain), `a_closed_or_unknown_request_cannot_be_claimed`, `a_row_construct_cannot_read_is_an_internal_error`.

### The Subject's Flag and List

- [x] `p1` - **ID**: `cpt-cf-construct-dod-review-queue-subject`

The system **MUST** let the subject, and only the subject, open a review request on one of its live facts with an optional comment, refuse a second open request on the same fact with 409, and list all its requests with their state and outcome, paged. Another subject, a refusal and an unknown fact **MUST** answer 404.

**Implements**:
- `cpt-cf-construct-flow-review-queue-flag`
- `cpt-cf-construct-flow-review-queue-list-own`

**Touches**:
- API: `POST`, `GET /construct/v1/subjects/{subject_id}/review-requests`
- DB Table: `construct__review_requests`

**Verified by**: `a_flag_opens_a_request_with_the_flagged_value_and_records_it`, `a_second_flag_on_a_fact_under_review_is_a_conflict`, `only_the_subject_flags_and_only_its_live_facts`, `a_flag_on_a_fact_removed_meanwhile_closes_its_request_and_is_no_such_fact`, `a_flag_whose_fact_cannot_be_read_again_is_answered_with_its_stored_request`, `each_operation_asks_for_its_own_permission`, `the_subject_lists_its_requests_and_a_reviewer_the_open_ones` (service), `the_subjects_list_is_newest_first_filters_and_pages`, `a_bad_filter_or_cursor_is_the_callers_mistake` (store), `the_subject_flags_a_fact_and_sees_its_request`, `a_second_flag_is_409_and_another_subject_or_an_unknown_fact_404`, `a_comment_past_its_limit_is_400_and_nothing_is_flagged`, `a_comment_and_a_reason_of_1000_characters_are_taken` (routes), `every_enforced_permission_is_declared_once_on_its_resource_and_action` (permissions).

### The Reviewer's List and Resolution

- [x] `p1` - **ID**: `cpt-cf-construct-dod-review-queue-reviewer`

The system **MUST** list the open requests of the caller's tenant, oldest first, to a caller with the permission to resolve review requests, and **MUST** let that caller resolve an open request as rejected with a reason, deleted, or corrected, never one on its own fact. The request **MUST** be claimed before its fact is written, so two reviewers never both resolve it. What a resolution is about to write **MUST** be recorded on its claim first, so a resolution that ends without closing the request is closed as it once its write is in the profile. A closed request, one another reviewer is resolving, or one written but not closed **MUST** answer 409 with a reason that tells them apart. A rejection serves the fact again; a delete removes it as the subject's delete does.

**Implements**:
- `cpt-cf-construct-flow-review-queue-list-open`
- `cpt-cf-construct-flow-review-queue-resolve`
- `cpt-cf-construct-algo-review-queue-reviewer-only`
- `cpt-cf-construct-algo-review-queue-remove`

**Touches**:
- API: `GET /construct/v1/admin/review-requests`, `POST /construct/v1/admin/review-requests/{request_id}/resolution`

**Verified by**: `a_caller_without_the_permission_or_with_a_narrower_scope_is_no_reviewer`, `a_reviewer_may_not_resolve_a_request_on_its_own_fact`, `a_rejection_closes_the_request_with_its_reason_and_keeps_the_fact`, `a_delete_removes_the_fact_as_the_reviewer_and_closes_the_request`, `a_delete_of_a_fact_already_gone_closes_the_request_as_deleted`, `a_closed_or_unknown_request_cannot_be_resolved`, `only_a_constraint_on_the_whole_tenant_makes_a_reviewer` (service), `the_reviewers_list_holds_the_tenants_open_requests_oldest_first` (store), `a_reviewer_lists_the_open_requests_and_a_caller_without_the_permission_gets_403`, `a_closed_request_is_409_and_an_unknown_one_404`, `a_body_that_does_not_fit_its_outcome_is_400_naming_the_field` (routes), `a_request_another_resolution_holds_is_a_conflict_until_its_hold_ends`, `a_request_closed_while_its_correction_was_written_is_a_conflict_that_says_so`, `a_write_that_may_have_landed_keeps_the_claim`, `a_failed_write_found_in_the_profile_closes_the_request`, `a_correction_whose_close_failed_is_closed_as_it_by_the_next_resolution`, `a_claim_outlasts_the_budget_and_the_writer`, `a_rejection_of_a_fact_gone_closes_the_request_as_deleted` (service), `a_stale_claim_gives_back_nothing_once_another_took_the_request`, `what_a_claim_recorded_reaches_the_next_claim_until_given_back_or_closed`, `the_reviewers_list_pages_oldest_first` (store), `a_held_or_unclosed_request_is_409_with_a_reason_a_client_tells_apart` (routes), `a_bad_query_of_the_reviewers_list_names_review_requests_and_the_part` (errors).

### The One-Step Plan Path

- [x] `p1` - **ID**: `cpt-cf-construct-dod-review-queue-one-step`

The system **MUST** turn a corrected value into a plan with one replace step and a new node key, with the reviewer as origin, pass it through the sensitive-data checks and Admission, and store it through the profile writer against the profile's version, reading and writing again on a conflict. When nothing is stored the request **MUST** stay open and the answer **MUST** say why; when the fact is gone the request **MUST** close as deleted. A correction **MUST** run only where the deployment opted in to storing values without sensitive-data checks.

**Implements**:
- `cpt-cf-construct-algo-review-queue-correct`
- `cpt-cf-construct-algo-review-queue-wire`

**Touches**:
- Graph storage: the fact's node (replaced), the root's version
- Config: `processing.run_without_sensitive_data_checks`, `processing.max_reruns`

**Verified by**: `a_correction_replaces_the_fact_under_a_new_key_as_the_reviewer`, `a_correction_rerun_after_a_conflict_keeps_the_key_it_stored`, `a_correction_admission_drops_stores_nothing_and_gives_the_request_back`, `a_correction_whose_value_is_blocked_stores_nothing_and_stays_open`, `a_correction_of_a_fact_already_gone_closes_the_request_as_deleted`, `a_correction_takes_only_the_facts_property_with_a_value_its_schema_takes`, `without_the_opt_in_a_correction_is_not_configured_and_the_rest_works` (service), `a_correction_needs_the_opt_in_to_store_values_unchecked` (pipeline), `a_correction_answers_the_closed_request_with_the_new_facts_id`, `a_correction_admission_refuses_is_422_and_the_request_stays_open` (routes), `a_correction_whose_check_fails_stores_nothing_and_gives_the_request_back`, `a_write_that_started_runs_past_the_budget_to_its_end`.

### A Fact Under Review Is Frozen

- [x] `p1` - **ID**: `cpt-cf-construct-dod-review-queue-frozen`

While a request is open, the profile read **MUST NOT** serve its fact, and processing **MUST** drop every step that would change or remove it, for records and for manage-facts calls alike. The subject's own view **MUST** still show it.

**Implements**:
- `cpt-cf-construct-algo-review-queue-hide`
- `cpt-cf-construct-algo-review-queue-skip`

**Touches**:
- Profile Reader, processing (`cpt-cf-construct-component-profile-reader`, `cpt-cf-construct-component-planner`)

**Verified by**: `a_fact_under_an_open_review_request_is_not_served`, `a_read_whose_facts_under_review_cannot_be_read_fails_instead_of_serving_them` (profile read), `a_step_on_a_fact_under_review_is_dropped_and_the_rest_written`, `a_call_whose_only_step_is_on_a_fact_under_review_stores_nothing`, `an_unreadable_review_store_drops_the_record_at_the_planner`, `a_manage_call_reads_the_facts_under_review_as_construct_not_as_the_agent`, `a_manage_call_drops_only_the_steps_on_a_fact_under_review`, `a_manage_call_with_the_review_store_down_writes_nothing` (processing), `a_change_touches_the_stored_entity_it_changes_and_a_new_entity_touches_none` (plan), `each_fact_carries_its_latest_review_request_and_the_export_holds_them_all` (the subject still sees it).

### Review Requests in Subject Control

- [x] `p1` - **ID**: `cpt-cf-construct-dod-review-queue-subject-control`

Each fact in the subject's view **MUST** carry its latest review request or none, the export **MUST** hold all the subject's requests, and the subject's delete of a fact **MUST** close its open request as deleted.

**Implements**:
- `cpt-cf-construct-algo-review-queue-close`

**Touches**:
- API: `GET /construct/v1/subjects/{subject_id}/facts`, `GET /construct/v1/subjects/{subject_id}/export`, `DELETE /construct/v1/subjects/{subject_id}/facts/{fact_id}`

**Verified by**: `each_fact_carries_its_latest_review_request_and_the_export_holds_them_all`, `the_subjects_delete_closes_an_open_request_on_the_fact_as_deleted`, `a_delete_whose_request_cannot_be_closed_is_unavailable_and_its_repeat_closes_it`, `a_delete_whose_writer_failed_after_it_landed_is_done` (Subject Control), `each_fact_shows_its_latest_review_request_or_null`, `the_export_is_a_json_attachment_and_403_without_its_permission` (routes).

### The Review Queue's Events

- [x] `p1` - **ID**: `cpt-cf-construct-dod-review-queue-events`

The system **MUST** log each flag, each resolution, each refused correction and each page of the reviewers' list as an event with the tenant and the caller, and the subject, the request and the fact where there is one; for a resolution the outcome, for a refusal its reason's name, for a page the ids of its requests; and never a value, comment or reviewer's reason.

**Implements**:
- `cpt-cf-construct-algo-review-queue-log`

**Touches**:
- Log target: `construct::audit`

**Verified by**: `a_flag_names_the_request_the_fact_and_the_subject_as_caller`, `a_correction_names_the_reviewer_the_outcome_and_the_new_fact`, `a_refused_correction_names_the_reviewer_and_the_reason`, `a_reviewers_read_names_the_requests_it_served`, and the recorded events in the service tests.

## 6. Acceptance Criteria

- [ ] A flagged fact is not served to applications and agents, and the subject still sees it with its request (`a_fact_under_an_open_review_request_is_not_served`, `each_fact_carries_its_latest_review_request_and_the_export_holds_them_all`).
- [ ] A record or agent does not change a fact under review (`a_step_on_a_fact_under_review_is_dropped_and_the_rest_written`, `a_call_whose_only_step_is_on_a_fact_under_review_stores_nothing`).
- [ ] A reviewer resolves a request as rejected, deleted or corrected; the subject sees the outcome (`a_rejection_closes_the_request_with_its_reason_and_keeps_the_fact`, `a_delete_removes_the_fact_as_the_reviewer_and_closes_the_request`, `a_correction_replaces_the_fact_under_a_new_key_as_the_reviewer`).
- [ ] A corrected value passes the checks and Admission and names the reviewer; when they refuse it, the request stays open (`a_correction_admission_drops_stores_nothing_and_gives_the_request_back`, `a_correction_whose_value_is_blocked_stores_nothing_and_stays_open`).
- [ ] The subject's delete closes an open request as deleted (`the_subjects_delete_closes_an_open_request_on_the_fact_as_deleted`).
- [ ] Every operation asks for its own permission, and Construct declares it (`each_operation_asks_for_its_own_permission`, `every_enforced_permission_is_declared_once_on_its_resource_and_action`).
- [ ] Every answer is `no-store` with the security headers (`every_answer_is_no_store_with_the_security_headers`).
- [ ] The tests named in this document pass.

## 7. Non-Functional Considerations

- **Security**: The caller and the tenant come only from the platform. Flagging and the subject's list are owner only, checked before the policy service is asked; refusals and unknown facts answer 404. The admin routes need the permission to resolve review requests in the caller's tenant and cover that tenant only; another tenant's request answers 404. Graph storage is read and written as Construct's processing identity. The table is read and written under the tenant's scope. Every answer is `no-store` with the security headers of the subject routes.
- **Privacy**: The flagged value is copied into the request only while it is open and is cleared on close. The comment and the reason are kept with the request until erasure or retention removes it. No event holds a value, a comment or a reviewer's reason.
- **Data integrity**: At most one open request per fact, enforced by a unique key on the tenant and the open fact. A close updates only an open request, so two resolutions cannot both close it. A correction and a delete are written against the profile's version; a replace keeps the old value until the new one is stored.
- **Reliability**: The rerun limit and the 20-second budget bound a delete and a correction; past either the answer is 503, and the request stays open. A write that started before the budget ran out is not cut off by it; it is awaited for at most 90 seconds.
- **Performance**: The lists are cut by the database with the secure ORM's `OData` pager. The profile read and processing read a subject's open requests in one query by tenant and subject.
- **Observability**: Each operation has a tracing span with the tenant, and the subject or the request. Events go to `construct::audit`.
- **Not done in this part**: see the out-of-scope list in 1.2.
