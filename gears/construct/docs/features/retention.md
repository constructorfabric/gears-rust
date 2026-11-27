# Feature: Retention


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [The Tenant's Retention Period](#the-tenants-retention-period)
  - [Remove What Is Past the Period](#remove-what-is-past-the-period)
  - [Hide What Is Past the Period](#hide-what-is-past-the-period)
  - [Tenant Exit](#tenant-exit)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [The Retention Period](#the-retention-period)
  - [The Retention Job](#the-retention-job)
  - [Nothing Served Past the Period](#nothing-served-past-the-period)
  - [A Deleted Tenant Leaves](#a-deleted-tenant-leaves)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p2` - **ID**: `cpt-cf-construct-featstatus-retention-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p2` - `cpt-cf-construct-feature-retention`

## 1. Feature Context

### 1.1 Overview

Retention removes what is older than the tenant's retention period: facts, counted from when they were stored, review requests from when they were created, and record identities from when they were received. A job on the cluster leader runs it with the same delete code as erasure (`cpt-cf-construct-feature-erasure`), and the Profile Reader stops serving a fact once it is past the period, before the job removes it. Tenant exit removes everything of a tenant the platform has deleted: the same job has graph storage offboard it, which removes its graph, and removes its rows from Construct's tables.

### 1.2 Purpose

The PRD requires that data is not kept longer than the tenant's retention period, which is a tenant setting; that without one, data stays until it is deleted; and that audit events follow the Audit gear's retention period instead (`cpt-cf-construct-fr-retention`). It also requires that deleted data is not served from the next read (`cpt-cf-construct-nfr-deletion-time`), and that the period ends the keeping of record identities (`cpt-cf-construct-fr-subject-delete`).

**The period.** The retention period is a tenant setting of the settings service. No Construct setting is declared there yet, so the deployment's config gives it, behind the same interface the settings service will implement: `retention.tenants` gives a tenant its own period in days, by tenant id, and `retention.default_days` gives every other tenant one. Without either, a tenant keeps its data until it is deleted; that is the default. A period is from 1 to 36,500 days.

**The job.** The cluster leader's deletion job (`cpt-cf-construct-algo-erasure-rerun`) runs retention every `deletion.retention_interval_secs` (3600), in a loop of its own beside the erasures, which run every `deletion.job_interval_secs` (300), so a long sweep does not hold them off. Periods are counted in days, and each sweep reads every profile of every swept tenant, so it runs far less often than the reruns. It sweeps each tenant listed in `retention.tenants` and, with a default period, every tenant that holds a row in Construct's tables or ever held one: the job keeps those tenants in `construct__tenants`, where the profile writer also notes a tenant on every write, once per tenant and process, so a profile created before tenants were noted is swept after its next write, since a profile an agent or an administrator wrote leaves no row of its own, so a tenant stays in the sweep after retention removed its rows and while its facts in graph storage remain. A period that reaches before the earliest time there is removes nothing. For each tenant, as Construct's processing identity, it:

1. Removes the closed review requests created before the period began, and the record identities received before it. An open request stays: step 2 closes it when its fact goes, with its event, and the next run removes it.
2. Lists the subjects that have a profile, through the profile roots in graph storage, a page at a time, and for each removes the facts stored before the period began, as one plan of removals with retention as origin, written against the profile's version and read and written again on a conflict within the rerun limit and the write budget. A fact that a write replaced meanwhile under a new key is stored anew, so it is decided again and kept. A node changed in place (the planner, a correction or an edit keeps its key) keeps the age of its first write: retention counts from when the node was first learned, not from its last change, so a project that is old enough goes with what it owns however recently it was edited. A write that should keep a node past the period learns it again under a new key. Then every open review request of the subject whose fact is gone closes as deleted, closed by Construct's identity: those of the facts removed now, and any an earlier run removed but could not close. Every close is tried; a subject that fails is logged and left for the next run, which finds what is left.

A record identity removed by retention no longer makes a resend of that record a repeat, so Record Intake refuses a record observed before the tenant's period began with `PAST_RETENTION` (`cpt-cf-construct-flow-record-intake-submit`): a resend more than a period later, or a record sent late with an old observation time, is not stored again.

**The read.** The Profile Reader does not serve a fact stored before the tenant's period began, also before the job removes it (`cpt-cf-construct-algo-profile-reader-read`). The subject's own view, the export and an administrator's read still show it until the job removes it. The planner's read of the profile leaves such a fact out too, so no plan is built on an expired value.

**Tenant exit.** A tenant that leaves the platform is marked deleted in the platform's tenant resolver; no event announces it. So each retention run asks the resolver about every tenant Construct keeps, those in `construct__tenants` and those with a row in its tables, also where no retention period is set, as Construct's processing identity in that tenant. For a tenant marked deleted, the run:

1. Asks graph storage to offboard the tenant (`cpt-cf-graph-storage-fr-tenant-offboarding`). Graph storage checks with the resolver itself that the tenant is deleted, blocks every further read and write of it, and removes its graph in the background, the profiles included. Asking again answers the offboarding it has, so each run asks once and learns how far it has gone. If graph storage refuses, nothing else happens: until the graph is blocked, the subjects' settings, their opt-outs and erasure marks, must stay.
2. Removes every row of the tenant's from Construct's tables: subject settings, review requests and record identities, 1000 at a time. An open review request is removed without being closed first: the tenant's profiles go with its graph, so there is no fact left for it to close on.
3. Once graph storage reports the graph gone, stops keeping the tenant in `construct__tenants`, so later runs no longer ask about it, and records the exit. A tenant `retention.tenants` names stays checked after its exit; later runs find nothing left and record nothing. The deployment removes such a tenant from its config.

A deleted tenant is not swept for retention: its data goes whatever its period. A tenant that is not deleted is left untouched; so is one the resolver does not know, with a warning on every run that its data stays until an operator removes it. Once the resolver does not answer, the run asks it about no further tenant and sweeps the rest for retention as before, so a resolver that is down does not hold up retention; the next run asks again. Each step can run again, so a run that stops part way is finished by a later one; at the hourly retention interval, a deleted tenant is gone well within the 30 days `cpt-cf-construct-nfr-deletion-time` allows, as long as graph storage's worker runs. The audit log records the rows removed and the end of the exit (`tenant_rows_removed`, `tenant_exited`); audit events already written stay until the Audit gear's retention period ends.

Graph storage grants its offboarding only to a caller the platform's policy allows `tenant_offboard` on `gts.cf.core.graph.tenant.v1~` for the named tenant, a permission no other implies. The deployment grants it to Construct's processing identity; without it, graph storage refuses, the run logs an error naming the permission, and the tenant stays until the grant is given. Without a tenant resolver in the client hub, no tenant exits, which is logged once.

**Known gaps.**

- Removal is graph storage's soft delete until it ships a hard delete (gears-rust #5194), as for erasure.
- Without the cluster profile `construct`, the job does not run, and nothing is removed for retention; the read still hides what is past the period.
- A fact's age counts from when it was stored, not from the record's observation time, so a record observed shortly before the period's start is kept up to one more period from when it was taken.
- A tenant restored from a backup after its exit comes back with its data. Keeping a deletion through a restore needs the platform's deletion ledger (gears-rust #5132 item 2), which does not exist yet.
- A tenant the resolver no longer knows, because the platform removed it for good before Construct asked, is taken as unknown and left untouched: nothing says it was deleted. Each run logs a warning for it; an operator removes its data. Graph storage answers such a tenant the same way.
- Record Intake does not refuse a record of a deleted tenant. Graph storage refuses the profile write, so no fact is stored, but the record's identity lands in Construct's table; the job finds the tenant again by that row and the next run removes it. A profile write leaves no row, so the tenant is not kept again after a write on another instance, and needs not be: graph storage refuses every write of an offboarded tenant.

**Out of scope**:

- Removing a deleted tenant's graph, which graph storage's own offboarding does; Construct starts it and waits for it.
- Setting the period in the settings service, and declaring Construct's settings there.
- Audit events, which the Audit gear's retention period removes.

**Requirements**: `cpt-cf-construct-fr-retention`; shared: `cpt-cf-construct-nfr-deletion-time` (nothing served past the period), `cpt-cf-construct-fr-subject-delete` (the period ends the keeping of record identities).

**Principles**: None. **Constraints**: None.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-tenant-admin` | Sets the retention period, in the deployment's config until the settings service holds it. |
| `cpt-cf-construct-actor-graph-storage` | Lists the profile roots and holds the facts that are soft-deleted; offboards a deleted tenant and removes its graph. |
| `cpt-cf-construct-actor-platform-auth` | The platform's tenant resolver says which tenant is deleted; the policy service grants Construct's identity the offboarding. |
| `cpt-cf-construct-actor-audit` | Receives the counts of what was removed and the end of a tenant's exit, through the structured log until the Audit gear ships. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), the Deletion component
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.15
- **Dependencies**: `cpt-cf-construct-feature-erasure`, `cpt-cf-construct-feature-profile-reader`

## 2. Actor Flows (CDSL)

None. No actor calls retention: the leader's job runs it.

## 3. Processes / Business Logic (CDSL)

### The Tenant's Retention Period

- [x] `p2` - **ID**: `cpt-cf-construct-algo-retention-period`

**Input**: The tenant id.

**Output**: The period, or none.

**Steps**:
1. [x] - `p2` - **RETURN** the tenant's own period from `retention.tenants`, else `retention.default_days`, else none, as days - `inst-period-read`

### Remove What Is Past the Period

- [x] `p2` - **ID**: `cpt-cf-construct-algo-retention-expire`

**Input**: The tenant id and its period.

**Steps**:
1. [x] - `p2` - Sweep each tenant in `retention.tenants` and, with a default period, each tenant that holds or held a row in Construct's tables, recording a new one in `construct__tenants`; a tenant the platform deleted exits instead (`cpt-cf-construct-algo-retention-tenant-exit`) and is not swept; read its period, and skip a tenant without one or one whose list or expiry fails, logged - `inst-expire-tenants`
2. [x] - `p2` - **IF** the period reaches before the earliest time there is, nothing. Build Construct's context and scope in the tenant. DB: delete the tenant's closed review requests created before the period began and its record identities received before it, 1000 at a time; record the counts when any was removed - `inst-expire-rows`
3. [x] - `p2` - Graph storage: list the tenant's profile roots a page of 100 at a time, each naming its subject, until no next page; a root whose payload cannot be read, or whose key is not that subject's root in the tenant, is left out and logged as an error. **IF** graph storage names a page it listed before as the next, the tenant's run stops with an error; **IF** 10000 pages were read and there is still a next page, the tenant's run stops there with an error - `inst-expire-subjects`
4. [x] - `p2` - For each subject, remove the facts stored before the period began as one plan of removals with retention as origin, written against the profile's version, read and written again after a conflict within the rerun limit and the write budget; record the count when any was removed. A subject that fails is logged - `inst-expire-facts`
5. [x] - `p2` - Close as deleted every open review request of the subject whose fact is not among its live facts, closed by Construct's identity, as `cpt-cf-construct-dod-review-queue-subject-control` says: a reviewer's resolution recorded on a request whose fact retention did not remove, and whose write is in the profile, closes it as that resolution; try every one, and log each failure with its fact; a subject with a failure counts as one with requests left open, not as one past its period - `inst-expire-close`

### Hide What Is Past the Period

- [x] `p2` - **ID**: `cpt-cf-construct-algo-retention-hide`

**Steps**:
1. [x] - `p2` - In the profile read and the planner's read, after the facts under review are left out, leave out every fact stored before the tenant's period began; a period reaching before the earliest time there is hides nothing - `inst-hide-expired`

### Tenant Exit

- [x] `p2` - **ID**: `cpt-cf-construct-algo-retention-tenant-exit`

**Input**: A tenant Construct keeps: in `retention.tenants`, in `construct__tenants`, or with a row in its tables.

**Steps**:
1. [x] - `p2` - On each retention run, ask the tenant resolver in the client hub for the tenant, as Construct's processing identity in it, within 10 seconds. **IF** it is not marked deleted, the tenant is left to retention; **IF** the resolver does not know it, too, with a warning that its data stays until an operator removes it. **IF** there is no identity or no resolver, no tenant exits, logged once; **IF** the resolver fails, the tenant is left to retention, logged as a warning when it did not answer and as an error when a rerun does not cure it, and asked again on the next run; **IF** it did not answer, the run asks it about no further tenant - `inst-exit-status`
2. [x] - `p2` - Graph storage: start the tenant's offboarding as Construct, within 10 seconds; it answers the offboarding it has when asked again. **IF** graph storage refuses, as when Construct is not granted `tenant_offboard`, or does not answer, the run stops here, nothing removed, logged as an error naming the permission or as a warning - `inst-exit-graph`
3. [x] - `p2` - DB: delete the tenant's subject settings, every review request of it, open or closed, and its record identities 1000 at a time, each table on its own, under the tenant's own scope; record the counts when any was removed. **IF** the graph is not gone yet, the tenant stays kept for the next run - `inst-exit-rows`
4. [x] - `p2` - **IF** graph storage reports the graph gone, DB: delete the tenant from `construct__tenants`, and record that the tenant exited when it was still kept; a tenant found again with nothing left, as one the config names, records nothing - `inst-exit-forget`
5. [x] - `p2` - Write the removed counts and the exit to the audit target `construct::audit` as `tenant_rows_removed` and `tenant_exited`, with the tenant and counts, never a value - `inst-exit-log`
6. [x] - `p2` - Wire tenant exit into the leader's deletion job with the processing identity, the tenant resolver from the client hub, graph storage's offboarding and Construct's tables - `inst-exit-wire`

## 4. States (CDSL)

None.

## 5. Definitions of Done

### The Retention Period

- [x] `p2` - **ID**: `cpt-cf-construct-dod-retention-period`

The system **MUST** take a tenant's retention period from the deployment's config, the tenant's own before the default, and **MUST** keep a tenant's data until it is deleted when neither gives one. A period out of range **MUST** stop the gear from starting.

**Implements**:
- `cpt-cf-construct-algo-retention-period`

**Touches**:
- Config: `retention.default_days`, `retention.tenants`

**Verified by**: `a_listed_tenant_has_its_own_period_and_every_other_the_default_or_none` (periods), `retention_keeps_data_by_default_and_the_job_runs_every_five_minutes`, `the_retention_and_deletion_keys_are_read`, `a_retention_or_deletion_value_out_of_range_is_refused` (config).

### The Retention Job

- [x] `p2` - **ID**: `cpt-cf-construct-dod-retention-job`

The system **MUST** remove, on the cluster leader, each tenant's review requests, record identities and facts that are older than its period, with the same delete code as erasure, so a fact a write changed meanwhile is decided again, and **MUST** close an open review request on a removed fact as deleted. A tenant without a period **MUST NOT** be swept. A sweep that fails or panics **MUST NOT** stop the job: it is logged, with the tenant where it is known, and the next run goes on.

**Implements**:
- `cpt-cf-construct-algo-retention-expire`

**Touches**:
- Graph storage: the profile roots (listed), the facts (soft-deleted), the root's version
- DB Tables: `construct__review_requests`, `construct__record_ids`
- Config: `deletion.retention_interval_secs`

**Verified by**: `retention_removes_only_the_facts_past_the_period_and_closes_their_open_requests`, `retention_with_nothing_past_the_period_writes_nothing`, `retention_walks_every_page_of_subjects`, `retention_whose_write_conflicts_decides_again_on_the_new_profile`, `retention_records_the_rows_it_removed_and_nothing_when_there_were_none`, `a_request_left_open_on_a_fact_an_earlier_run_removed_is_closed_by_the_next`, `a_subject_whose_requests_cannot_be_closed_does_not_stop_the_others`, `every_close_is_tried_when_one_fails_and_the_tenant_reports_the_request_left_open`, `a_period_reaching_before_the_earliest_time_removes_nothing`, `a_listing_that_hands_back_the_same_page_stops_retention_of_the_tenant`, `a_listing_that_never_ends_stops_at_the_page_limit_and_says_so`, `a_fact_retention_removes_closes_its_request_as_construct_though_a_resolution_was_recorded`, `a_fact_an_earlier_run_removed_closes_its_request_as_the_resolution_recorded` (Deletion), `the_job_sweeps_the_listed_tenants_and_with_a_default_every_tenant_held`, `without_a_period_nothing_is_swept`, `a_tenant_that_fails_does_not_stop_the_others`, `a_period_that_cannot_be_read_skips_only_its_tenant`, `without_the_held_tenants_only_the_listed_ones_are_swept`, `a_retention_sweep_that_does_not_end_does_not_hold_off_an_erasure`, `a_run_that_panics_is_logged_and_the_job_runs_again_after_the_interval`, `a_tenant_whose_sweep_panics_is_logged_and_the_next_tenant_is_swept` (job), `retention_removes_the_tenants_rows_from_before_the_cut_and_no_other`, `retention_removes_more_identities_than_one_batch_holds_batch_by_batch`, `the_tenants_are_those_with_a_row_in_any_table`, `a_tenant_stays_known_after_retention_removed_its_rows`, `a_noted_tenant_is_among_the_tenants_and_noting_it_again_changes_nothing`, `retention_leaves_an_open_request_to_the_removal_of_its_fact` (store), `every_write_notes_its_tenant_for_retention` (graph), `retention_runs_on_its_own_longer_interval` (job), `a_listing_that_comes_back_to_an_earlier_page_stops_retention_of_the_tenant` (Deletion), `the_subjects_with_a_root_are_listed_only_in_the_tenant`, `a_cursor_that_cannot_be_read_is_a_defect`, `only_a_root_key_of_the_subject_in_the_tenant_is_its_root`, `a_root_names_its_subject_and_an_unreadable_one_says_why` (the root listing, on graph storage's in-memory service, which answers one page), `the_directory_hands_on_graph_storages_cursor_and_leaves_out_a_root_it_cannot_read` (the root listing over a scripted page with a next cursor and two bad roots).

### Nothing Served Past the Period

- [x] `p2` - **ID**: `cpt-cf-construct-dod-retention-hide`

The Profile Reader **MUST NOT** serve, and the planner **MUST NOT** read, a fact stored before the tenant's retention period began, also before the job removes it. Both reads take the periods as a required collaborator, so neither can be wired without them. Record Intake **MUST NOT** take a record observed before the period began (`cpt-cf-construct-dod-record-intake-checks`).

**Implements**:
- `cpt-cf-construct-algo-retention-hide`

**Touches**:
- The profile read

**Verified by**: `a_fact_stored_before_the_tenants_retention_period_is_not_served`, `a_period_reaching_before_the_earliest_time_hides_nothing` (profile read), `the_planner_reads_no_fact_past_the_retention_period_while_the_subject_still_sees_it` (planner's read), `a_record_observed_before_the_tenants_retention_period_is_refused_and_one_within_it_taken` (intake).

### A Deleted Tenant Leaves

- [x] `p2` - **ID**: `cpt-cf-construct-dod-retention-tenant-exit`

The system **MUST** have graph storage offboard, on the cluster leader, a tenant the platform has deleted and, only once graph storage has taken the offboarding, **MUST** remove every row of the tenant's from Construct's tables, checking each tenant it keeps on every retention run, also where no retention period is set. It **MUST** keep the tenant until graph storage reports its graph gone, so a run that stops part way is finished by a later one, and **MUST** leave untouched a tenant that is not deleted or that the resolver does not know. A deleted tenant **MUST NOT** be swept for retention. A tenant whose exit fails or panics **MUST NOT** stop the job: it is logged with the tenant, and the next tenant goes on.

**Implements**:
- `cpt-cf-construct-algo-retention-tenant-exit`

**Touches**:
- Tenant resolver: `get_tenant`
- Graph storage: the tenant's offboarding
- DB Tables: `construct__subject_settings`, `construct__review_requests`, `construct__record_ids`, `construct__tenants`

**Verified by**: `a_deleted_tenant_is_asked_about_as_construct_in_that_tenant`, `a_present_or_unknown_tenant_is_left_untouched`, `without_a_resolver_or_an_identity_nothing_is_touched`, `a_resolver_that_fails_leaves_the_tenant_untouched`, `an_exit_offboards_the_graph_removes_the_rows_in_the_tenants_scope_and_forgets_it`, `while_graph_storage_is_removing_the_graph_the_rows_go_and_the_tenant_is_kept`, `a_refused_offboarding_leaves_every_row_in_place`, `a_graph_storage_that_does_not_answer_is_asked_again_by_the_next_run`, `a_table_failure_after_the_offboarding_keeps_the_tenant_for_the_next_run`, `a_forget_that_fails_keeps_the_tenant_and_records_no_exit_until_the_next_run`, `a_tenant_that_exited_before_finds_nothing_and_records_nothing` (tenant exit), `a_deleted_tenant_exits_and_is_not_swept_while_another_is_swept_as_before`, `a_deleted_tenant_exits_also_where_no_retention_period_is_set`, `a_tenant_that_is_present_or_unknown_is_left_untouched`, `a_tenant_whose_status_cannot_be_read_is_swept_as_before_and_asked_again`, `an_exit_that_graph_storage_has_not_finished_is_finished_by_a_later_run`, `without_a_tenant_resolver_every_tenant_is_left_to_retention`, `a_failed_exit_is_logged_and_the_next_tenant_still_exits`, `a_tenant_whose_exit_panics_is_logged_and_the_next_tenant_still_exits`, `a_status_check_that_panics_leaves_the_tenant_to_retention_and_goes_on`, `an_exit_under_way_is_said_and_a_graph_storage_that_does_not_answer_is_a_warning`, `a_tenant_the_resolver_does_not_know_is_kept_and_said_to_need_an_operator`, `a_resolver_failure_a_rerun_does_not_cure_is_an_error`, `a_deleted_tenant_the_config_names_exits_once_and_is_never_swept`, `without_the_kept_tenants_only_the_listed_ones_are_checked_and_swept`, `a_tenant_the_resolver_does_not_know_is_still_swept_for_retention`, `once_the_resolver_does_not_answer_the_run_asks_it_no_more_and_sweeps_on` (job), `a_tenants_exit_removes_every_row_of_it_in_every_table_and_no_other`, `a_tenants_exit_removes_more_identities_than_one_batch_holds`, `a_forgotten_tenant_is_no_longer_kept_and_a_later_write_notes_it_again`, `a_tenants_exit_under_a_scope_that_leaves_the_tenant_out_removes_nothing`, `forgetting_a_tenant_under_a_scope_that_leaves_it_out_keeps_it` (store), `construct_starts_the_offboarding_and_sees_it_done_once_graph_storage_removed_the_graph`, `a_tenant_the_platform_has_not_deleted_is_refused_by_graph_storage`, `a_refused_grant_names_the_permission_the_deployment_must_give` (graph storage's offboarding, its real service over its in-memory store), `each_answer_of_the_resolver_is_a_status_or_an_error`, `without_a_resolver_in_the_hub_no_tenant_is_checked_and_that_is_said_once`, `a_resolver_registered_after_the_first_question_is_found`, `a_resolver_that_does_not_answer_in_time_is_unavailable` (tenant resolver), `a_tenant_the_platform_deleted_leaves_construct_and_graph_storage_through_the_assembled_job` (the assembled job, Construct's tables and graph storage's service), `each_event_is_logged_to_the_audit_target_with_its_name_ids_and_counts` (events).

## 6. Acceptance Criteria

- [ ] A fact past the retention period is not served, and the job removes it (`a_fact_stored_before_the_tenants_retention_period_is_not_served`, `retention_removes_only_the_facts_past_the_period_and_closes_their_open_requests`).
- [ ] Review requests and record identities past the period are removed, in that tenant only (`retention_removes_the_tenants_rows_from_before_the_cut_and_no_other`).
- [ ] Without a period, nothing is removed (`without_a_period_nothing_is_swept`).
- [ ] A tenant marked deleted is removed: Construct's tables hold no row of it, and graph storage's offboarding reports it done (`a_tenant_the_platform_deleted_leaves_construct_and_graph_storage_through_the_assembled_job`, `a_tenants_exit_removes_every_row_of_it_in_every_table_and_no_other`).
- [ ] A refused offboarding removes nothing (`a_refused_offboarding_leaves_every_row_in_place`).
- [ ] A tenant that is not deleted is left untouched (`a_tenant_that_is_present_or_unknown_is_left_untouched`).
- [ ] A run that stops halfway is finished by a later run (`an_exit_that_graph_storage_has_not_finished_is_finished_by_a_later_run`).
- [ ] The tests named in this document pass.

## 7. Non-Functional Considerations

- **Security**: The job runs as Construct's processing identity in each tenant, and its rows are removed under that tenant's scope. No caller is involved. Tenant exit asks the resolver and graph storage as that identity in the leaving tenant; graph storage's offboarding needs the deployment's `tenant_offboard` grant to it, which no other permission implies. The identity's subject type is `application`, the one the platform's policy service takes for it, as `erasure.md` says.
- **Privacy**: Data older than the period is not served from the next read and is removed by the next run. No event holds a value; an event gives counts.
- **Data integrity**: Facts are removed as one plan against the profile's version, so a fact a write replaced meanwhile is kept. Rows are removed by their own time, so a request on a kept fact goes only when the request itself is past the period.
- **Reliability**: What fails is logged and left for the next run; a tenant's exit is kept until graph storage reports its graph gone, Construct's rows go only once graph storage took the offboarding, and every step can run again; a subject that fails does not stop the tenant's others. A listing that comes back to a page it listed, or is still going at 10000 pages, stops the tenant's run with an error. A panic in one tenant's sweep is logged with the tenant and the job goes on to the next.
- **Performance**: Each run reads every profile of each swept tenant, one at a time, so a run takes longer as tenants grow; the interval bounds how often.
- **Observability**: Events go to `construct::audit`; a failed tenant or subject is a warning, and a tenant that left any subject past its period ends with one error saying how many of how many, and one where only review requests stayed open with one warning.
- **Not done in this part**: see the out-of-scope list and the known gaps in 1.2.
