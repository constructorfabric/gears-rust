# Feature: Erasure


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Ask to Erase Everything](#ask-to-erase-everything)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Run an Erasure](#run-an-erasure)
  - [The Root Key of a Generation](#the-root-key-of-a-generation)
  - [A Record Taken Before an Erasure](#a-record-taken-before-an-erasure)
  - [Run the Marked Erasures](#run-the-marked-erasures)
  - [Log Deletion's Events](#log-deletions-events)
  - [Wire Erasure](#wire-erasure)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [The Erasure Request](#the-erasure-request)
  - [The Erasure Run](#the-erasure-run)
  - [The Root After an Erasure](#the-root-after-an-erasure)
  - [The New Root Key](#the-new-root-key)
  - [Erasure's Storage](#erasures-storage)
  - [Deletion's Events](#deletions-events)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-erasure-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-erasure`

## 1. Feature Context

### 1.1 Overview

Erasure lets the subject erase everything Construct holds about it. The request turns personalization off and marks the erasure under way in one write, so from the answer on the Profile Reader serves nothing about the subject and nothing is stored for it. Deletion then removes the subject's facts and its profile root from graph storage, and its review requests and record identities from Construct's tables. Only the personalization setting, off, and the audit events are kept. Every step can run again; an erasure that stopped is run again by a job on the cluster leader. This feature builds the Deletion component (`cpt-cf-construct-component-deletion`); retention runs the same delete code by age (`cpt-cf-construct-feature-retention`).

### 1.2 Purpose

The PRD requires that the subject can erase everything, that deleted data is not served from the next read and is gone from all storage within 30 days (`cpt-cf-construct-fr-subject-delete`, `cpt-cf-construct-nfr-deletion-time`), and that erasure sets personalization off and keeps an erasure under way, so nothing is served and no plan is stored even when personalization is turned on again before it is done (`cpt-cf-construct-fr-settings`).

**The route.** The DESIGN writes the path with the gateway's `/api` prefix; the code registers it without it.

| Route | Answer |
|-------|--------|
| `POST /construct/v1/subjects/{subject_id}/erasure` | `202`, the settings with the erasure under way |

**Who may.** Owner only: the path names the caller, and the caller needs the permission to erase its own data, `erase` on `gts.cf.construct.subject.fact.v1~`, which Construct declares as a GTS permission instance. Another subject, and a caller the platform refuses, get `404`, as on the subject's other fact routes. An application calls it with the subject's own token.

**The request.** The body is empty. The request sets personalization off and marks the erasure under way, with the time of the mark, in one write on the subject's settings row: an insert, or an update when the row exists, which keeps a mark's time, so two requests at once, or a request beside a settings change, both land. From then on the Profile Reader serves nothing about the subject, Record Intake refuses its records with `ERASURE_IN_PROGRESS` and Admission drops its plans, also when the subject turns personalization on meanwhile. The subject's own list and export, and an administrator's read, are not served to applications or agents; they still show the facts until the erasure removes them (`cpt-cf-construct-feature-subject-control`, `cpt-cf-construct-feature-admin-facts`). The answer is `202` with `{subject_id, personalization_enabled: false, erasure_in_progress: true}`, marked `no-store`; `GET .../settings` shows `erasure_in_progress` false once every step is done. A request while one is under way keeps the first mark's time and marks nothing new. Each request is recorded as an event.

**The run.** The cluster leader's deletion job runs the erasure in the background, once its mark is at least 140 seconds old (step 2), within `deletion.job_interval_secs` (300) after that. The mark already stops all serving, so the run only has to remove what is held. Runs of one erasure can still overlap: the leader election is advisory, so two instances may both think they lead for a moment. That is safe: every step can run again, the removals are against the profile's version and read again on a conflict, and a run is tied to its mark: it reads the mark's time and the profile generation the erasure erases from one row, and every graph step stays on that generation. Each read of the profile names its generation, and the version it gives does too, so a removal planned on the erased profile conflicts with a newer root and is read again; a read that finds the profile in a later generation, because another run finished the erasure, stops the run, and the root removal reads the root of the erased generation, never the current one. After the wait the run also goes on only while the erasure it started is under way, and the finish clears only that mark. So a run whose erasure another run finished meanwhile, even just after its own check, never removes the profile the subject started since, nor finishes a later erasure. As Construct's processing identity it:

1. Removes every live fact of the subject as one plan of removals, with the subject as origin, written against the profile's version and read and written again on a conflict within the rerun limit and the write budget, as the subject's delete is. A plan stored meanwhile by processing conflicts with it, and runs through Admission again, which drops it. The root stays: a write admitted before the mark can still attach a fact to it, and the removal after the wait finds that fact only through the root.
2. Waits until 140 seconds have passed since the mark: as long as a write that passed its check before the mark can still land. That is the 20-second budget before a write, the 90 seconds the writer is awaited, by processing as by every other write, and 30 seconds for a statement graph storage still runs after that. A record whose settings were read before the mark has kept its identity long before: Record Intake takes a record within one receive window, 10 seconds, from the read of its subject's settings to the insert of its identity (`cpt-cf-construct-flow-record-intake-submit`), and the wait is at least three windows, which leaves room for the commit and for the clocks of two instances to differ. Admission answers within that budget, so a settings read from before the mark that answers late admits nothing.
3. Checks that the erasure it started is still under way, then removes, as in step 1, any fact such a write left during the wait, and soft-deletes the profile root of the generation the erasure erases, together with every fact the root still lists for removal whose delete has not run, four at once and all within six call timeouts. When a live fact is still on the root, because a write landed after the removal, or the deletes run out of time, the root stays and the run ends unavailable; the next run goes on from there. A root such a write created where the subject had none goes too, so nothing is left in a generation no one reads again.
4. In one transaction: removes the subject's review requests, open and closed, and its record identities, moves its profile generation on, and clears the mark, only while it is the mark this run started. The erasure event is recorded.

**What is kept.** The settings row, with personalization off, which stays off until the subject turns it on, and the profile generation. Audit events stay until the Audit gear's retention period ends. Graph storage keeps the soft-deleted facts and root, see the gap below.

**The new root key.** Graph storage never gives a deleted key out again, so the profile created after an erasure needs a root key of its own. The settings row keeps the subject's profile generation, 0 for a subject never erased, and each erasure moves it on in its last step. Generation 0 has the root key every profile had, `construct:<tenant>:root:<subject>`; a later generation `n` has `construct:<tenant>:root:<subject>:<n>`. The Profile Writer and every profile read find the root of the current generation. Between the root's removal and the last step the subject's current root is the removed one, so a read finds no profile.

**The job.** An erasure stops when a step fails or panics, or when the instance stops; its mark stays. A panic is logged with the subject and ends only that run: the job goes on with the others. The job on the cluster leader runs, every `deletion.job_interval_secs` (300) and in a loop of its own beside retention's, so a long retention sweep does not hold it off, every erasure marked at least 140 seconds ago and still under way, the one tried least lately first, up to 100 per run. A younger erasure would only sleep out its wait and hold up the others, so it waits for a later run. The leader is elected through the platform cluster gear's leader election, with the cluster profile `construct`, linearizable; the deployment binds that profile. A host that binds no such profile resolves no election, since a resolve makes the profile a requirement of the whole host's readiness: the job does not run, and Construct says so once in a warning. A cluster that binds it but does not answer yet is tried again, with a warning at each try, after a pause that doubles from 5 seconds to 5 minutes with up to as much again at random. Either way an erasure stays marked, serves nothing, and is not finished until a leader runs it.

**Construct's identity.** The run acts as Construct's processing identity with the subject type `application`. The platform's policy service takes only `user` and `application` and denies any other, so with another type every graph storage and settings call of the run, and of retention and processing, is denied. Policies grant the permissions to Construct's application id, not to the type, so other applications' grants do not reach it.

**Without a processing identity.** The request marks the erasure as anywhere, so nothing is served; the run cannot read or write graph storage, so the erasure stays marked until a processing identity is configured. The gear says so in a warning at start; each run that finds no identity logs it at debug only.

**Known gaps.**

- Graph storage has only a soft delete until it ships a hard delete (gears-rust #5194). The removed facts and the root stay in graph storage as tombstones, with their content and embeddings, so the 30-day removal of `cpt-cf-construct-nfr-deletion-time` is not met yet. The delete sits behind one interface (`FactRemoval`), so the swap changes nothing here; the tombstones left before it need a one-off removal.
- The 140-second wait bounds a write that started before the mark, but nothing fences one: graph storage cannot cancel or bound a statement it already sent to its database, as it says itself, until toolkit-db offers a server-side statement bound (gears-rust #4761), and no `statement_timeout` is set by default. A write still committing past the wait lands under the erased generation's root, which no read and no later erasure looks at again: its facts are never served, but stay in graph storage until graph storage's tenant offboarding or a purge removes them. The deployment **SHOULD** set graph storage's database `statement_timeout` to 30 seconds or less, which turns the bound into a guarantee.
- An erasure that keeps failing is tried again every run until someone fixes its cause. It goes behind the erasures not yet tried, so it does not hold them up, and a failure a rerun does not cure is logged as an error each time; nothing counts the attempts or gives up.
- Once an erasure has removed the record identities, a replay of the subject's records is a new record, refused while personalization is off; after the subject turns it on again, it is taken. A connector **MUST NOT** send again a record refused with `ERASURE_IN_PROGRESS` (`cpt-cf-construct-feature-record-intake`).

**Out of scope**:

- Retention by age and tenant exit (`cpt-cf-construct-feature-retention`).
- Graph storage's purge and hard delete, which are external.
- Telling the subject when the erasure is done, other than the settings route.

**Requirements**: `cpt-cf-construct-nfr-deletion-time`; shared: `cpt-cf-construct-fr-subject-delete` (the erase-everything part), `cpt-cf-construct-fr-settings` (personalization off and the erasure under way).

**Principles**: None. **Constraints**: `cpt-cf-construct-constraint-graph-soft-delete-only`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-subject` | Asks to erase everything, through an application that acts for it. |
| `cpt-cf-construct-actor-graph-storage` | Holds the facts and the root that are soft-deleted. |
| `cpt-cf-construct-actor-audit` | Receives the request and erasure events, through the structured log until the Audit gear ships. |
| `cpt-cf-construct-actor-platform-auth` | Authenticates the caller and decides, through the AuthZ resolver, whether it may erase its data. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), the Deletion component and the erasure sequence
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.14
- **Dependencies**: `cpt-cf-construct-feature-subject-control`, `cpt-cf-construct-feature-record-intake`, `cpt-cf-construct-feature-review-queue`

## 2. Actor Flows (CDSL)

### Ask to Erase Everything

- [x] `p1` - **ID**: `cpt-cf-construct-flow-erasure-request`

**Actor**: `cpt-cf-construct-actor-subject`

**Success Scenarios**:
- The subject gets 202 with personalization off and the erasure under way; the Profile Reader serves nothing about it from then on, and the erasure runs.
- A request while one is under way gets the same answer and keeps the first mark.

**Error Scenarios**:
- Another subject, or the platform refuses: 404, and nothing is marked.
- The settings store does not answer: 503.

**Steps**:
1. [x] - `p1` - Caller sends `POST /construct/v1/subjects/{subject_id}/erasure`, authenticated - `inst-request-send`
2. [x] - `p1` - API: the extractors take the security context, the service and the subject id - `inst-request-parse`
3. [x] - `p1` - Owner only: **IF** the path does not name the caller, forbidden; ask the AuthZ resolver for `erase` on the subject's facts, with the caller's tenant as the owner tenant; **IF** it refuses or its scope does not reach the subject, forbidden - `inst-request-own`
4. [x] - `p1` - DB, under the settings scope of step 3: INSERT the subject's settings row with personalization off, the erasure under way and the time of the mark; **IF** the row exists, UPDATE it to personalization off and the erasure under way, keeping a mark's time. **IF** the scope does not cover the row, out of scope and nothing is written - `inst-request-mark`
5. [x] - `p1` - Record the request event; the leader's deletion job runs the erasure (`cpt-cf-construct-algo-erasure-rerun`) - `inst-request-hand-off`
6. [x] - `p1` - **RETURN** 202 with `{subject_id, personalization_enabled: false, erasure_in_progress: true}`, `no-store` - `inst-request-return`

## 3. Processes / Business Logic (CDSL)

### Run an Erasure

- [x] `p1` - **ID**: `cpt-cf-construct-algo-erasure-erase`

**Input**: The subject's key.

**Output**: Done, not under way, or an error that leaves the mark set.

**Steps**:
1. [x] - `p1` - Build Construct's context and scope in the subject's tenant; without a processing identity, not configured. Read when the erasure was marked and the profile generation it erases, from one row; **IF** no erasure is under way, **RETURN** not under way - `inst-erase-marked`
2. [x] - `p1` - Remove every live fact of the subject as one plan of removals with the subject as origin, written against the profile's version, read and written again after a short pause on a conflict, within the rerun limit and the write budget, as `cpt-cf-construct-algo-subject-control-delete` does; **IF** a read finds the profile in another generation than step 1 read, **RETURN** not under way, writing nothing. **IF** the profile kept changing, or the write did not answer and may have landed, unavailable; any other failure of the write, such as a refusal, as it is - `inst-erase-facts`
3. [x] - `p1` - After the wait of step 4 only: graph storage: read the profile of the generation step 1 read, whatever the current one is. **IF** there is no root, go on. **IF** a live fact is not listed for removal, **RETURN** unavailable. Otherwise soft-delete each fact the root lists for removal, four at once, **IF** they are not all deleted within six call timeouts, **RETURN** unavailable; then the root, with its edges - `inst-erase-root`
4. [x] - `p1` - Wait until a write checked before the mark can no longer land, 140 seconds after the mark. **IF** the erasure under way is no longer the one step 1 read, **RETURN** not under way. Do step 2 again, for a write that landed during the wait, then step 3 - `inst-erase-wait`
5. [x] - `p1` - DB, in one transaction: clear the mark, while it is the one step 1 read, and move the profile generation on; **IF** it was not set, **RETURN** not under way. Remove the subject's review requests and record identities. Reset the subject's profile schema to the default (a subject with no schema row gets one, at version 1 with nothing charged, so a schema change that read none conflicts). Record the erasure event and **RETURN** done - `inst-erase-finish`

### The Root Key of a Generation

- [x] `p1` - **ID**: `cpt-cf-construct-algo-erasure-root-key`

**Input**: The subject's key.

**Output**: The node key of the subject's profile root.

**Steps**:
1. [x] - `p1` - DB: read the subject's profile generation from its settings row in its tenant; a subject without a row is in generation 0 - `inst-root-read`
2. [x] - `p1` - **RETURN** `construct:<tenant>:root:<subject>` for generation 0, and `construct:<tenant>:root:<subject>:<generation>` for any other - `inst-root-key`

### A Record Taken Before an Erasure

- [x] `p1` - **ID**: `cpt-cf-construct-algo-erasure-generation`

**Input**: A received record about a subject, with the profile generation intake read for the subject when it took the record.

**Output**: The record goes on to Admission, or it is dropped.

**Steps**:
1. [x] - `p1` - Before Admission, DB: read the subject's profile generation (`cpt-cf-construct-algo-erasure-root-key`). **IF** it is past the record's, drop the record at Admission with the reason `erased_since_taken`, also when the subject turned personalization on again. An erasure marked after this read stops the write at Admission's settings read, or waits it out - `inst-generation-admit`

### Run the Marked Erasures

- [x] `p1` - **ID**: `cpt-cf-construct-algo-erasure-rerun`

**Steps**:
1. [x] - `p1` - **IF** the host has no cluster client, or its client says the cluster profile `construct` is not bound, warn once and **RETURN**, resolving nothing, so the host's readiness does not wait for a profile it does not bind. Resolve the leader election of the cluster profile `construct`, linearizable, scoped `construct`, and join the election `deletion`; while this instance leads, run the deletion job every `deletion.job_interval_secs`; **IF** there is no election, or the lead is lost, warn and try again after a pause doubling from 5 seconds to 5 minutes, with jitter - `inst-rerun-leader`
2. [x] - `p1` - DB: list, across tenants as Construct's own bookkeeping, the subjects whose erasure is under way and was marked at least 140 seconds ago, so its wait is over, by when the job last tried it, or by its mark when never tried, the earliest first, up to 100 - `inst-rerun-find`
3. [x] - `p1` - DB: for each, note the attempt's time on its settings row while the erasure is under way; a failure to note it is a warning and the erasure runs anyway - `inst-rerun-attempt`
4. [x] - `p1` - Run `cpt-cf-construct-algo-erasure-erase` for each, one after another; log a failure and go on, as a warning when the next run may cure it and as an error otherwise - `inst-rerun-run`

### Log Deletion's Events

- [x] `p1` - **ID**: `cpt-cf-construct-algo-erasure-log`

**Input**: An erasure requested, erased, facts expired or rows expired event.

**Steps**:
1. [x] - `p1` - Write it to the structured log under the target `construct::audit`, with the event's name and the tenant; the subject for all but rows expired, and the subject as caller for a request; the counts for retention's events; never a value - `inst-log-event`

### Wire Erasure

- [x] `p1` - **ID**: `cpt-cf-construct-algo-erasure-wire`

**Steps**:
1. [x] - `p1` - At start, give the profile reader and the writer the profile generations kept in the settings table - `inst-wire-generations`
2. [x] - `p1` - Build Deletion over the profile writes, the writer's root removal, graph storage's root listing, Construct's tables, the review request store and the retention periods; the erasure request service over Construct's tables; and the deletion job; register the erasure route - `inst-wire-deletion`
3. [x] - `p1` - Keep the job and the cluster leadership until the gear starts - `inst-wire-tasks`
4. [x] - `p1` - On start, run the job under the leadership; on stop, cancel it, abort it when still running at the stop deadline, wait a second for it, and tell the host when it panicked or did not end - `inst-wire-start`

## 4. States (CDSL)

None as a state machine of its own: the erasure flag of the subject settings (`cpt-cf-construct-dbtable-subject-settings`) is set by the request and cleared by the run's last step, and the profile generation only moves on.

## 5. Definitions of Done

### The Erasure Request

- [x] `p1` - **ID**: `cpt-cf-construct-dod-erasure-request`

The system **MUST** let only the subject, with the permission to erase its data, ask to erase everything, and **MUST** answer any other caller like a subject that does not exist. The request **MUST** set personalization off and mark the erasure under way in one write before it answers, so the Profile Reader serves nothing about the subject and nothing is stored for it from the answer on, and **MUST** answer 202 with those settings. A second request **MUST NOT** put the end of the erasure off.

**Implements**:
- `cpt-cf-construct-flow-erasure-request`

**Touches**:
- API: `POST /construct/v1/subjects/{subject_id}/erasure`
- DB Table: `construct__subject_settings` (`cpt-cf-construct-dbtable-subject-settings`)

**Verified by**: `the_subject_marks_its_erasure_which_is_said`, `a_second_request_keeps_the_erasure_marked_and_is_recorded`, `another_subject_is_refused_before_the_platform_is_asked_and_nothing_is_marked`, `the_subject_without_the_permission_is_refused_and_nothing_is_marked`, `the_subject_is_asked_for_the_erase_permission_on_its_facts`, `a_settings_row_the_scope_leaves_out_is_refused_and_nothing_is_said` (service), `the_owner_is_answered_202_with_the_erasure_under_way_and_no_store`, `another_subject_or_a_refusal_answers_404_and_marks_nothing` (routes), `a_mark_turns_personalization_off_and_marks_the_erasure_on_a_new_row`, `a_second_mark_keeps_the_first_time_and_turns_personalization_off_again`, `a_mark_the_scope_leaves_out_is_refused_and_writes_nothing`, `a_mark_on_a_subject_with_settings_turns_personalization_off_and_takes_the_time` (store), `every_enforced_permission_is_declared_once_on_its_resource_and_action` (permissions).

### The Erasure Run

- [x] `p1` - **ID**: `cpt-cf-construct-dod-erasure-run`

The system **MUST** remove every fact and the profile root of a subject whose erasure is under way, then its review requests and record identities, including the identity of a record whose settings were read before the mark, and **MUST** keep only the settings row, with personalization off, and the audit events. Every step **MUST** be safe to run again, and the mark **MUST** stay until every step is done. An erasure that stopped **MUST** be run again by the cluster leader.

**Implements**:
- `cpt-cf-construct-algo-erasure-erase`
- `cpt-cf-construct-algo-erasure-rerun`
- `cpt-cf-construct-algo-erasure-wire`

**Touches**:
- Graph storage: the subject's fact nodes and root (soft-deleted)
- DB Tables: `construct__subject_settings`, `construct__review_requests`, `construct__record_ids`
- Cluster: the leader election `construct/deletion` of the cluster profile `construct`
- Config: `deletion.job_interval_secs`, `processing.max_reruns`

**Verified by**: `an_erasure_removes_every_fact_and_the_root_then_finishes_and_says_so`, `no_erasure_under_way_changes_nothing`, `a_fact_written_after_the_removal_leaves_the_erasure_marked_for_the_next_run`, `an_interrupted_erasure_completes_when_it_runs_again`, `an_erasure_without_a_processing_identity_is_not_configured_and_stays_marked`, `an_erasure_removes_the_tables_rows_only_once_the_receive_window_after_its_mark_is_over`, `an_erasure_whose_write_conflicts_reads_the_profile_again_and_removes_it_all`, `an_erasure_whose_write_may_have_landed_is_unavailable_and_stays_marked`, `an_erasure_another_run_finished_meanwhile_is_not_under_way_and_says_nothing`, `a_write_that_lands_during_the_wait_is_removed_before_the_erasure_finishes`, `a_run_whose_erasure_another_run_finished_leaves_the_new_profile`, `a_run_paused_after_its_check_leaves_the_new_profile_another_run_started`, `a_run_that_reads_a_new_generations_profile_first_leaves_it`, `an_erasure_removes_the_root_of_the_generation_it_erases` (Deletion), `a_finish_for_another_mark_leaves_the_erasure_under_way_and_removes_nothing` (store), `a_record_admitted_on_a_settings_read_past_the_budget_is_not_written`, `an_agents_plan_admitted_on_a_settings_read_past_the_budget_is_not_written` (processing), `the_job_runs_the_erasures_past_their_wait_and_leaves_a_fresh_one_for_later`, `the_job_takes_its_intervals_from_the_config`, `erasures_that_cannot_be_listed_leave_retention_to_run`, `an_attempt_that_cannot_be_noted_still_runs_the_erasure`, `an_erasure_whose_write_is_refused_fails_as_refused_not_as_unavailable`, `a_root_whose_listed_deletes_never_end_stays_and_its_removal_is_unavailable_in_time`, `the_write_runs_by_the_answer_deadline`, `a_records_write_that_never_ends_is_dropped_at_the_writer_limit`, `the_job_runs_while_this_instance_leads_and_stops_when_it_loses_the_lead`, `a_host_without_the_construct_profile_resolves_no_election_and_runs_no_job`, `a_cluster_that_does_not_answer_yet_is_tried_again_after_a_pause`, `a_retention_sweep_that_does_not_end_does_not_hold_off_an_erasure`, `a_rerun_that_panics_is_logged_and_the_next_run_finishes_the_erasure`, `the_election_pauses_start_at_the_first_and_double_up_to_the_longest` (job), `start_runs_the_leaders_job_and_stop_ends_it`, `a_deletion_task_that_ignores_stop_is_aborted_at_the_deadline`, `a_deletion_task_that_panicked_makes_stop_fail` (gear), `a_record_not_taken_within_the_receive_window_is_unavailable_and_keeps_nothing`, `an_identity_insert_past_the_receive_window_is_rolled_back_before_the_hand_off`, `the_check_and_the_insert_share_one_receive_window`, `a_check_and_an_insert_that_fit_in_the_receive_window_together_take_the_record`, `a_record_taken_after_its_receive_window_is_named_in_a_warning_and_a_late_repeat_is_not` (intake), `an_erased_subject_is_served_nothing_at_once_keeps_only_its_setting_and_later_starts_anew` (pipeline), `a_taken_record_carries_its_subjects_generation` (intake), `a_record_taken_before_an_erasure_is_dropped_after_it_though_personalization_is_on` (processing). The cluster gear's own election backend was not run in a test: a scripted election over the cluster SDK's stub client stands in for it, and a fake leadership in the gear's tests.

### The Root After an Erasure

- [x] `p1` - **ID**: `cpt-cf-construct-dod-erasure-root`

The system **MUST** soft-delete the subject's profile root only once no live fact is on it, and **MUST** delete every fact the root still lists for removal first, since the root is their only record.

**Implements**:
- `cpt-cf-construct-algo-erasure-erase`

**Touches**:
- Graph storage: the root node and the facts it lists for removal

**Verified by**: `the_root_goes_once_its_facts_are_gone_and_the_next_generation_starts_a_new_profile`, `the_root_deletes_the_facts_it_still_lists_before_it_goes`, `a_root_whose_listed_fact_cannot_be_deleted_stays`, `a_root_removal_leaves_the_root_of_a_later_generation` (graph storage's in-memory service).

### The New Root Key

- [x] `p1` - **ID**: `cpt-cf-construct-dod-erasure-root-key`

The system **MUST** keep each subject's profile generation and **MUST** read and write its profile under the root key of that generation, so a profile created after an erasure gets a new root, while every profile not erased keeps the root key it had. A record **MUST** carry the generation its subject was in when intake took it, and processing **MUST** drop a record whose generation an erasure has moved on since, so a record taken before an erasure is never stored after it.

**Implements**:
- `cpt-cf-construct-algo-erasure-root-key`
- `cpt-cf-construct-algo-erasure-generation`

**Touches**:
- DB Table: `construct__subject_settings`, column `profile_generation`
- Graph storage: the root key

**Verified by**: `the_root_key_names_tenant_and_subject`, `a_root_key_reads_back_as_the_generation_it_was_built_for`, `the_root_goes_once_its_facts_are_gone_and_the_next_generation_starts_a_new_profile` (graph), `each_erasure_moves_the_generation_on_once_more`, `the_generation_is_read_only_in_the_subjects_tenant` (store), `an_erased_subject_is_served_nothing_at_once_keeps_only_its_setting_and_later_starts_anew` (pipeline).

### Erasure's Storage

- [x] `p1` - **ID**: `cpt-cf-construct-dod-erasure-storage`

The system **MUST** supply a migration, `m006_erasure_retention`, that adds to `construct__subject_settings` the column `profile_generation` (`BIGINT NOT NULL DEFAULT 0`) and the columns `erasure_marked_at` and `erasure_attempted_at` (nullable timestamps), to `construct__record_ids` the index `idx_construct__record_ids__received` on (`tenant_id`, `received_at`) for retention, and the table `construct__tenants` (`tenant_id` primary key, `first_seen`), the tenants Construct has held a row for, which retention keeps sweeping. A row stored before takes generation 0, no mark and no attempt. The migration **MUST** be safe to run twice, and its step back **MUST** refuse while a subject was erased or is being erased, since it drops the generation and such a subject would read and write its removed root again. The store **MUST** clear the mark and the attempt, move the generation on and remove the subject's rows in one transaction, only while the mark is set, and **MUST** touch no other subject's rows.

**Implements**:
- `cpt-cf-construct-algo-erasure-erase`
- `cpt-cf-construct-algo-erasure-rerun`

**Touches**:
- DB Tables: `construct__subject_settings`, `construct__review_requests`, `construct__record_ids`

**Verified by**: `a_row_stored_before_takes_generation_zero_and_a_second_run_is_a_no_op`, `the_step_back_is_refused_once_a_subject_was_erased` (migration), `the_finish_clears_the_mark_moves_the_generation_on_and_removes_only_the_subjects_rows`, `a_finish_without_a_mark_removes_nothing`, `the_stopped_erasures_are_those_marked_before_the_cut_of_any_tenant`, `an_erasure_marked_before_its_time_was_kept_counts_as_marked_long_ago`, `a_tenant_stays_known_after_retention_removed_its_rows` (store), on `SQLite`; and on `PostgreSQL`, in the `construct-pg` lane (`make test-integration LANE=construct-pg`, Docker required), `m006_runs_twice_and_steps_back_only_while_no_subject_was_erased`, `the_mark_read_back_finishes_its_erasure_and_no_other` and `the_tenants_are_the_noted_ones_and_those_with_a_row`. The `MySQL` form comes from the schema builder and is not run.

### Deletion's Events

- [x] `p1` - **ID**: `cpt-cf-construct-dod-erasure-events`

The system **MUST** log each erasure request and each finished erasure with the tenant and the subject, and retention's removals with their counts, and never a value.

**Implements**:
- `cpt-cf-construct-algo-erasure-log`

**Touches**:
- Log target: `construct::audit`

**Verified by**: `each_event_is_logged_to_the_audit_target_with_its_name_ids_and_counts` (log), and the recorded events in `the_subject_marks_its_erasure_which_is_said`, `an_erasure_removes_every_fact_and_the_root_then_finishes_and_says_so`, `retention_removes_only_the_facts_past_the_period_and_closes_their_open_requests` and `retention_records_the_rows_it_removed_and_nothing_when_there_were_none`.

## 6. Acceptance Criteria

- [ ] After an erasure request the reader serves nothing for the subject at once, and Construct's tables then hold only its setting, off (`an_erased_subject_is_served_nothing_at_once_keeps_only_its_setting_and_later_starts_anew`).
- [ ] An interrupted erasure completes on a rerun (`an_interrupted_erasure_completes_when_it_runs_again`, `the_job_runs_the_erasures_past_their_wait_and_leaves_a_fresh_one_for_later`).
- [ ] An erasure that keeps failing does not hold up the others (`an_erasure_that_keeps_failing_goes_behind_the_others_and_says_so`, `a_tried_erasure_goes_behind_the_untried_and_a_finish_forgets_the_attempt`).
- [ ] A record received just before the request leaves no identity behind (`an_erasure_removes_the_tables_rows_only_once_the_receive_window_after_its_mark_is_over`, `a_record_not_taken_within_the_receive_window_is_unavailable_and_keeps_nothing`).
- [ ] A profile created after an erasure gets a new root (`the_root_goes_once_its_facts_are_gone_and_the_next_generation_starts_a_new_profile`).
- [ ] Only the subject may erase, and the answer is `no-store` (`another_subject_or_a_refusal_answers_404_and_marks_nothing`, `the_owner_is_answered_202_with_the_erasure_under_way_and_no_store`).
- [ ] The tests named in this document pass.

## 7. Non-Functional Considerations

- **Security**: The caller and the tenant come only from the platform. Only the subject, with the permission to erase its data, may ask; any other caller is answered as a subject that does not exist. Graph storage is read and written as Construct's processing identity, the settings row under the caller's settings scope for the request and under Construct's tenant scope for the run. The job lists the marked erasures across tenants as Construct's own bookkeeping, then runs each in its own tenant.
- **Privacy**: The Profile Reader serves nothing about the subject from the answer on; the subject's own list and export and an administrator's read show its facts until they are removed. No event holds a value. Graph storage keeps tombstones until its hard delete ships, see the known gaps.
- **Data integrity**: Facts are removed as one plan against the profile's version, so a plan written meanwhile conflicts and runs through Admission again, which drops it. The root goes only once no live fact is on it. The tables' rows go with the mark in one transaction, and the generation moves on with it, so a profile is never written under a removed root.
- **Reliability**: Each step can run again. A run that fails leaves the mark set, and the leader's job runs it again every interval. A panic in an erasure is logged with its subject; one in the job is logged and fails the gear's stop. The facts' removal is bounded by the rerun limit and the 20-second budget; a started write is awaited for at most 90 seconds, and the deletes a root still lists for at most six call timeouts. A request's run stops five minutes after the mark.
- **Performance**: An erasure finishes within `deletion.job_interval_secs` after the 140 seconds its mark must wait; the job runs one erasure at a time, up to 100 per run, and holds no database connection between them.
- **Observability**: The request has a tracing span with the tenant, the subject and the caller; Deletion's run a span with the tenant and the subject. Events go to `construct::audit`. A stopped run, a store that did not answer, a missing leader election, and at start a missing processing identity, are warnings; a run that failed in a way a rerun does not cure is an error. Gaining and losing the lead are logged.
- **Not done in this part**: see the out-of-scope list and the known gaps in 1.2.
