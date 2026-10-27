# Feature: Profile Reader


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Read a Profile](#read-a-profile)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Serve a Profile](#serve-a-profile)
  - [Wire Processing and the Read](#wire-processing-and-the-read)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Profile Read](#profile-read)
  - [Stored Facts](#stored-facts)
  - [Read Event](#read-event)
  - [Profile Route](#profile-route)
  - [Client in ClientHub](#client-in-clienthub)
  - [Gear Wiring](#gear-wiring)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-profile-reader-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-profile-reader`

## 1. Feature Context

### 1.1 Overview

The Profile Reader (`cpt-cf-construct-component-profile-reader`) is the only path by which applications and agents read a subject's profile. It serves the subject's facts grouped by category, each with its property and value, who it came from, when it was stored and when its source asserted it. It serves them only to a caller the platform lets act for the subject, only the categories the caller may read, and nothing while personalization is off or an erasure is under way. Each read is an audit event without content. This feature also wires the processing of received records into the gear, so that a record sent to intake becomes facts that this read serves.

### 1.2 Purpose

The PRD requires Construct to serve a subject's profile by category to the applications and agents that act for the subject, under platform permissions per category (`cpt-cf-construct-fr-profile-read`), and to serve nothing while personalization is off or an erasure is under way (`cpt-cf-construct-fr-settings`, `cpt-cf-construct-nfr-deletion-time`). Applications never read graph storage directly (`cpt-cf-construct-principle-single-profile-owner`).

**Who may read.** The profile is served only for its owner: to the owner, and to applications and agents that act for the owner. The platform decides which: the reader asks the AuthZ resolver for the resource type `construct.profile` with the action `read`, the subject as the resource id and the tenant as the context tenant (root only) and as the owner tenant. A caller the resolver refuses, or whose scope does not reach the subject in the tenant, gets the same answer as a subject that does not exist (`404`), on REST and in process alike, so the answer tells nothing about who has a profile. Each constraint of the policy answer is one path to the profile: the tenant, the subject and the categories must all come from the same constraint, so two constraints for other pairs never add up to a grant. The subject itself, reading in its own tenant, needs a constraint that reaches the tenant. Anyone else needs a constraint that names the subject as the resource id: a constraint that limits the tenant alone says which tenant a caller reaches, not that it may act for every subject in it, and a policy plugin that answers only with a tenant clamp lets each subject read only itself. A filter Construct cannot decide in memory (a group or tenant-subtree filter), or one on a property the profile does not have, grants nothing.

**The tenant.** The tenant is the caller's own, or the one named in the `tenant` query parameter. An application or agent that acts for users can serve users of several tenants with one login, as a connector does at record intake, so it names the tenant, and the AuthZ resolver must authorize it for that tenant. `cpt-cf-construct-fr-access-control` and `cpt-cf-construct-fr-tenant-isolation` are amended for this, beside record intake.

**The categories.** The categories are the person types: identity, role, skill and preference. The readable ones come from the same policy answer: the categories named by the constraints that reach the tenant and the subject, or every category for a reaching constraint that names none. How they are asked is set with `profile_category_check`, which a deployment with a processing identity must set: `one_call` takes them from the one policy answer, and fits only a policy plugin that answers with category constraints, since an answer without one lets every category be read; `per_category` asks once per category with the category as a resource property, all at once, after the settings showed the profile is served. Either way, category-level permission needs a policy plugin that decides on the category: a plugin that ignores it, such as the static one, lets whoever may read the subject read every category. A refusal, or an answer the platform's enforcer cannot compile, hides that category, as it hides the subject on the first call; a policy call that fails fails the read: a partial profile is never served. A category the caller may not read is not listed, like a category without facts, so the two look the same.

**The facts.** The facts are Construct's own nodes in graph storage. They are read as Construct's processing identity, a configured service account, not as the caller, through the same read the planner uses (`cpt-cf-construct-algo-profile-writer-read`): a fact the root lists for removal is not served. Each fact is served with its id (the graph node key), its one property with its value, its origin kind (`connector`, `agent`, `reviewer` or `administrator`), its storing time (graph storage's timestamp) and its `produced_at`, when its source asserted it (the record's `observed_at`, or the time of the call or the action).

**The answer.** Every answer of the route, errors included, is marked `Cache-Control: no-store` and carries `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY` and `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'`; HSTS is the TLS ingress's. `200` with `{subject_id, categories: [{entity_kind, facts: [{fact_id, properties, origin: {kind}, stored_at, produced_at}]}]}`, marked `Cache-Control: no-store`. A subject without a profile, or one whose profile is not served now, answers `200` with no categories. `404` for a subject the caller may not act for. `503` when graph storage, the settings or the policy service cannot answer. `501`, which a caller does not retry, when no processing identity is configured, also for a profile that would be served empty. `500` for a profile larger than one read takes.

**Processing.** Intake hands a received record to a bounded queue in memory, and a worker the gear starts with it runs each record through the planner, the sensitive-data checks, Admission and the profile writer, as `cpt-cf-construct-seq-record-in` describes. It runs when both a processing identity and a model are configured; otherwise intake keeps dropping each received record with a drop event, as before the planner existed. The admission rules come from the deployment config, the same for every tenant, until Construct's settings are declared in the settings service; the defaults trust every connector, set the confidence floor at 0.6, allow every entity kind, and cap a plan at 100 steps and a text piece at 1000 characters. No real sensitive-data check runs yet, so every value is allowed (`cpt-cf-construct-feature-sensitive-data-checks`), and the gear warns at start-up that it runs without one. The worker counts the records it finishes and drops (by stage and reason) and how long each takes, and reports the queue depth and the records in flight as metrics. When the gear stops, it logs how many records were still queued or in flight and are lost.

**Config.**

| Key | Default | Meaning |
|---|---|---|
| `processing_identity` | none | The service account Construct reads and writes graph storage as, in every tenant |
| `model.upstream_alias`, `model.model`, `model.timeout_secs` | none, none, 60 | The planner's model, an OAGW upstream with the chat completions API |
| `processing.queue_capacity` | 256 | Records waiting at most, up to 100000; a full queue refuses a record with `503` |
| `processing.workers` | 4 | Records processed at once, up to 256 |
| `processing.max_rounds`, `processing.max_tokens` | 8, 32000 | The planner's caps for one record |
| `processing.max_reruns` | 2 | Plans again after a write conflict, up to 10 |
| `admission.*` | see above | The admission rules; `trusted_connectors` lists connector identities as UUIDs |
| `profile_category_check` | none; required with a processing identity | How the readable categories are asked: `one_call` or `per_category` |

A queue capacity above 100000, more than 256 workers, more than 10 reruns, a processing identity without `profile_category_check`, an entity kind that is not a category, a confidence floor outside 0 to 1, a step cap outside 1 to 10000, a value cap outside 1 to 100000, a trusted connector that is not a UUID, or a model upstream alias that is not 1 to 63 letters, digits, `-` or `_` stops the gear from starting.

**Out of scope**:

- Hiding a fact under an open review request; the Review Queue adds it (`cpt-cf-construct-feature-review-queue`).
- Hiding items past the retention period (`cpt-cf-construct-feature-retention`).
- The subject's own view, the export and administrator reads (`cpt-cf-construct-feature-subject-control`, `cpt-cf-construct-feature-admin-facts`).
- The MCP read tool (`cpt-cf-construct-feature-mcp-tools`).
- Reading the admission rules from the settings service, and the real sensitive-data checks.

**Requirements**: `cpt-cf-construct-fr-profile-read`; shared: `cpt-cf-construct-fr-fact-origin` (each served fact with its origin), `cpt-cf-construct-fr-settings` (nothing served while personalization is off), `cpt-cf-construct-nfr-deletion-time` (nothing served while an erasure is under way).

**Principles**: `cpt-cf-construct-principle-single-profile-owner`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-consumer-app` | Reads a subject's profile for the subject, by category. |
| `cpt-cf-construct-actor-platform-auth` | Decides whether the caller may act for the subject in the tenant, and which categories it may read. |
| `cpt-cf-construct-actor-graph-storage` | Holds the profile; Construct reads it as its processing identity. |
| `cpt-cf-construct-actor-audit` | Takes the read events once it ships; until then they go to the structured log. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), the Profile reader component and the REST API
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.9
- **Dependencies**: `cpt-cf-construct-feature-gear-foundation`, `cpt-cf-construct-feature-subject-settings`, `cpt-cf-construct-feature-profile-writer`

## 2. Actor Flows (CDSL)

### Read a Profile

- [x] `p1` - **ID**: `cpt-cf-construct-flow-profile-reader-read`

**Actor**: `cpt-cf-construct-actor-consumer-app`

**Steps**:
1. [x] - `p1` - Client: `GET /construct/v1/subjects/{subject_id}/profile`, with `tenant` in the query when the subject is not in the caller's own tenant - `inst-profile-send`
2. [x] - `p1` - API: the security context, the subject id and the query are extracted; a malformed one, or an unknown query member, is answered with a problem before the read runs - `inst-profile-parse`
3. [x] - `p1` - Serve the profile (`cpt-cf-construct-algo-profile-reader-read`) - `inst-profile-serve`
4. [x] - `p1` - **RETURN** `200` with the subject and its categories, marked `Cache-Control: no-store` - `inst-profile-return`
5. [x] - `p1` - In process, `ConstructClientV1::read_profile` runs the same read and returns `SubjectProfile`; a subject the caller may not act for is a not-found error - `inst-profile-client`

## 3. Processes / Business Logic (CDSL)

### Serve a Profile

- [x] `p1` - **ID**: `cpt-cf-construct-algo-profile-reader-read`

**Input**: The caller's security context, the named tenant if any, and the subject.

**Output**: The subject's readable facts by category, possibly none. Or an error: not visible, unavailable or internal, and no other.

**Steps**:
1. [x] - `p1` - Take the tenant from the query, or else from the caller's security context. The refusal's reason from the platform is kept for the log. Ask the AuthZ resolver for `construct.profile` / `read`, with the subject as the resource id and the tenant as the context tenant (root only) and as the owner-tenant property. **IF** it refuses, or no single constraint of the scope it returns reaches both the tenant and the subject, the subject is not visible. A constraint reaches the subject when it names it as the resource id, or, for the subject itself reading in its own tenant, when it names no resource id - `inst-read-authorize`
2. [x] - `p1` - Build Construct's processing context for the tenant; without a processing identity the read is unavailable. Read the subject's settings in the tenant under the scope's tenant constraints (`cpt-cf-construct-algo-subject-settings-read`); a settings scope that does not reach the subject is not visible, like a refusal. **IF** personalization is off or an erasure is under way, no category is readable - `inst-read-settings`
3. [x] - `p1` - Otherwise take the readable categories: those named by the constraints that reach the tenant and the subject, or every category for one that names none. With `per_category`, ask the AuthZ resolver once per category, all at once, with the category as a resource property, and take the ones it allows; a refusal or an answer that cannot be compiled hides the category, the latter logged at warn level, and a call that fails fails the read - `inst-read-categories`
4. [x] - `p1` - **IF** a category is readable, read the subject's stored facts as Construct's processing context, and group the facts of each readable category in category order, leaving out categories without facts. A profile larger than one read takes is the gear's own failure - `inst-read-facts`
5. [x] - `p1` - A stored fact is read from the profile as the planner reads it (`cpt-cf-construct-algo-profile-writer-read`), with its origin kind from its stored origin, its storing time from graph storage, and its `produced_at` - `inst-read-stored`
6. [x] - `p1` - Write one read event: the tenant, the subject, the caller and the categories served, never a value. **RETURN** the categories - `inst-read-event`
7. [x] - `p1` - Until the Audit gear ships, the read event goes to the structured log at info level under the target `construct::audit`, as `profile_read` - `inst-read-log`
8. [x] - `p1` - A subject that is not visible is answered `404` on every transport, like a subject that does not exist; the refusal is logged at warn level with the caller, the subject and the tenant, and the answer names none of the reason - `inst-read-hide`

### Wire Processing and the Read

- [x] `p1` - **ID**: `cpt-cf-construct-algo-profile-reader-wire`

**Input**: The gear's config, graph storage's client, the subject settings, and the policy enforcer.

**Output**: The hand-off intake uses, the profile read, and the processing worker when processing is configured.

**Steps**:
1. [x] - `p1` - Build the profile read over graph storage, read as the configured processing identity. Without an identity, the read answers unavailable - `inst-wire-read`
2. [x] - `p1` - **IF** a processing identity and a model are configured, build the planner over the model, the sensitive-data checks (none yet, which is warned about at start-up), Admission with the configured rules (an absent trusted-connector or entity-kind list is warned about at start-up), and the profile writer, behind a bounded queue that intake hands records to, with the worker's metrics. Otherwise intake keeps dropping received records with a drop event - `inst-wire-processing`
3. [x] - `p1` - When the gear starts, start the worker that processes queued records, at most the configured number at once; when it stops, log how many records are still queued or in flight and stop it, aborting it at the stop deadline and waiting a moment for it to end; a worker that panicked makes the stop fail. A record whose processing panics is dropped with a drop event at the `worker` stage and an error log, and the worker goes on. A record still queued is lost, and its identity stays, so a resend is a repeat - `inst-wire-start`

## 4. States (CDSL)

None. The read keeps no state; processing keeps queued records in memory only.

## 5. Definitions of Done

### Profile Read

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-reader-read`

The system **MUST** serve a subject's profile only to a caller the AuthZ resolver lets act for the subject in the tenant: the subject itself on a constraint that reaches its tenant, anyone else only on a constraint that names the subject. It **MUST** answer any other caller like a subject that does not exist. It **MUST** take the tenant from the query or else from the caller, **MUST** serve only the categories the caller may read, **MUST** serve nothing while personalization is off or an erasure is under way, and **MUST** read the facts as Construct's processing identity.

**Implements**:
- `cpt-cf-construct-algo-profile-reader-read`

**Touches**:
- Requirement: `cpt-cf-construct-fr-profile-read`
- Code: `domain::profile_read::ProfileReadService`

**Verified by**: `a_caller_the_platform_lets_act_for_the_subject_reads_its_facts_by_category`, `a_caller_the_platform_refuses_does_not_see_the_subject_and_nothing_is_read`, `a_tenant_only_answer_lets_the_subject_read_itself_and_no_one_else`, `another_caller_reads_on_a_constraint_that_names_the_subject`, `a_named_tenant_the_scope_does_not_reach_is_not_visible`, `a_tenant_and_a_subject_from_two_different_constraints_do_not_make_a_grant`, `categories_come_only_from_the_constraints_that_reach_the_subject`, `a_scope_that_names_another_subject_does_not_reach_this_one`, `a_filter_construct_cannot_decide_or_does_not_know_grants_nothing`, `two_category_filters_in_one_constraint_grant_only_their_overlap`, `a_policy_service_that_fails_is_unavailable_and_an_answer_that_cannot_be_compiled_is_not_visible`, `a_grant_the_settings_scope_cannot_cover_is_not_visible_rather_than_forbidden`, `the_policy_service_is_asked_about_the_named_tenant_or_the_callers_own`, `a_category_the_scope_leaves_out_is_not_listed`, `per_category_the_policy_service_is_asked_for_each_category`, `per_category_an_answer_that_cannot_be_compiled_hides_its_category`, `per_category_another_caller_needs_each_category_answer_to_name_the_subject`, `per_category_a_failed_policy_call_fails_the_read_instead_of_hiding_the_category`, `the_platforms_reason_for_a_refusal_is_kept_for_the_log`, `a_profile_too_large_to_read_is_the_gears_own_failure_not_a_precondition`, `an_unconstrained_scope_grants_every_category_to_any_reader`, `several_paths_to_the_profile_grant_the_union_of_their_categories`, `the_settings_scope_keeps_an_unconstrained_scope_and_only_the_tenant_filters_of_others`, `nothing_is_served_while_personalization_is_off_or_an_erasure_is_under_way`, `the_facts_are_read_as_the_processing_identity_for_the_subject_and_tenant_asked` (service), `a_subject_that_is_not_visible_is_404_and_tells_nothing_more` (error mapping). The policy service is a stub; the real AuthZ resolver was not run.

### Stored Facts

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-reader-stored`

The system **MUST** read a subject's stored facts through the profile writer's read, with each fact's origin kind, storing time and `produced_at`, and **MUST NOT** serve a fact the root lists for removal.

**Implements**:
- `cpt-cf-construct-algo-profile-reader-read`

**Touches**:
- Code: `infra::storage::graph::reader::GraphProfileSource` as `StoredProfiles`, `infra::storage::graph::fact::stored_fact_of`

**Verified by**: `a_stored_fact_names_who_it_came_from_and_when`, `a_records_fact_was_produced_when_its_record_was_observed`, `the_stored_facts_leave_out_one_listed_for_removal_and_a_subject_without_a_root_has_none`, `the_application_read_leaves_out_a_fact_it_cannot_read_and_serves_the_rest` (graph storage's in-memory store), `a_profile_past_what_one_read_takes_is_full`, `a_stored_node_that_breaks_the_fact_shape_is_reported_with_its_key`, `a_produced_at_that_does_not_parse_is_served_as_absent_and_logged`, `a_produced_at_that_is_not_a_string_is_served_as_absent_and_logged` (the fact), `a_record_is_planned_checked_admitted_written_and_read_back`. A stored fact the read cannot decode is left out and logged with its defect, while the planner's read fails on it: an application is better served by the rest of the profile, whereas a plan built on a partial profile could add what is stored.

### Read Event

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-reader-event`

The system **MUST** write one content-free event per served read, naming the tenant, the subject, the caller and the categories served, through one interface that the Audit gear replaces.

**Implements**:
- `cpt-cf-construct-algo-profile-reader-read`

**Touches**:
- Code: `domain::profile_read::ProfileEvents`, `infra::profile_events::LogProfileEvents`

**Verified by**: the recorded events in `a_caller_the_platform_lets_act_for_the_subject_reads_its_facts_by_category`, `a_category_the_scope_leaves_out_is_not_listed` and `nothing_is_served_while_personalization_is_off_or_an_erasure_is_under_way`; no event for a refused read in `a_caller_the_platform_refuses_does_not_see_the_subject_and_nothing_is_read`; the log line in `a_read_is_logged_with_who_read_whose_categories_and_no_value`.

### Profile Route

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-reader-route`

The system **MUST** register `GET /construct/v1/subjects/{subject_id}/profile` through OperationBuilder, authenticated, with an optional `tenant` query parameter, answering the profile shape of 1.2 with `Cache-Control: no-store`, and `404` for a subject the caller may not act for.

**Implements**:
- `cpt-cf-construct-flow-profile-reader-read`

**Touches**:
- Code: `api::rest::{routes, handlers::get_profile, dto::ProfileDto}`

**Verified by**: `a_profile_reads_as_its_categories_with_each_facts_property_origin_and_times`, `a_subject_without_facts_reads_as_no_categories`, `a_subject_the_caller_may_not_act_for_is_404_like_one_that_does_not_exist`, `a_named_tenant_is_taken_and_anything_else_in_the_query_is_refused`, `the_named_tenant_reaches_the_policy_request`, `a_fact_without_a_produced_at_has_no_such_key`, `a_refused_read_is_logged_with_the_caller_the_subject_and_the_tenant` (routes), `every_answer_of_the_route_is_no_store` (the manage-facts route). `origin.kind` and `entity_kind` are enums in the OpenAPI schema.

### Client in ClientHub

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-reader-client`

The SDK client **MUST** offer `read_profile`, running the same read and answering `SubjectProfile`: its categories as `FactCategory`, and each fact with its id, its one `property` and `value`, its `FactOrigin`, and its times. The enums and the structs are `#[non_exhaustive]`.

**Implements**:
- `cpt-cf-construct-flow-profile-reader-read`

**Touches**:
- Code: `construct_sdk::{ConstructClientV1::read_profile, SubjectProfile, ProfileCategory, ProfileFact, FactCategory, FactOrigin}`, `domain::local_client::LocalClient`

**Verified by**: `read_profile_answers_the_profile_in_the_sdk_shape`, `read_profile_answers_a_subject_the_caller_may_not_act_for_as_not_found`, `read_profile_asks_about_the_named_tenant` (client), `the_client_trait_is_dyn_compatible` (SDK).

### Gear Wiring

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-reader-wiring`

The gear **MUST** run received records through the planner, the checks, Admission and the profile writer when a processing identity and a model are configured, and **MUST** otherwise keep dropping them with a drop event. It **MUST** start and stop the processing worker with the gear, and **MUST** depend on graph storage: the gear does not start without it, also with processing off.

**Implements**:
- `cpt-cf-construct-algo-profile-reader-wire`

**Touches**:
- Sequence: `cpt-cf-construct-seq-record-in`
- Code: `infra::pipeline::{build, assemble, Worker}`, `infra::processing_metrics::{ProcessingMetrics, WorkerLoad}`, `infra::processing_contexts::ServiceIdentity`, `infra::admission_rules::ConfigAdmissionRules`, `gear::ConstructGear`, `config::ConstructConfig`

**Verified by**: `a_record_is_planned_checked_admitted_written_and_read_back`, `a_record_whose_processing_panics_is_dropped_and_the_worker_goes_on`, `an_absent_list_is_warned_about`, `set_lists_are_not_warned_about`, `without_a_model_records_are_dropped_with_an_event_and_no_worker_runs`, `without_a_processing_identity_the_profile_read_is_not_configured`, `without_a_processing_identity_even_an_empty_profile_is_not_configured`, `the_configured_category_check_reaches_the_profile_read` (the pipeline, through `assemble` with a scripted model), `the_identity_acts_in_the_tenant_asked_and_its_scope_reaches_only_that_tenant` (the processing identity), `start_runs_the_worker_and_stop_ends_it`, `stop_after_its_deadline_aborts_a_worker_that_does_not_end`, `stop_says_how_many_records_it_loses`, `a_worker_that_panicked_makes_stop_fail`, `a_record_counts_as_in_flight_until_it_ends_also_when_it_is_aborted`, `the_queue_depth_follows_the_queue_and_reads_zero_once_it_is_gone`, `a_counted_drop_is_handed_on`, `a_processed_record_counts_once_with_its_duration`, `a_drop_counts_by_its_stage_and_reason`, `the_gauges_read_the_queue_and_the_records_in_flight` (the metrics, on an in-memory exporter), `without_processing_start_and_stop_do_nothing` (the gear), the config tests and the admission rules tests. The gear was not run in a host, and the chat completions model through OAGW was not called.

## 6. Acceptance Criteria

- [ ] A round trip: one connector record reaches intake, the planner returns a plan, the gear applies it in one write, and the result reads back out of the graph (`a_record_is_planned_checked_admitted_written_and_read_back`).
- [ ] A caller who may not act for the subject gets nothing, answered like a subject that does not exist (`a_caller_the_platform_refuses_does_not_see_the_subject_and_nothing_is_read`, `a_tenant_only_answer_lets_the_subject_read_itself_and_no_one_else`, `a_tenant_and_a_subject_from_two_different_constraints_do_not_make_a_grant`, `a_subject_the_caller_may_not_act_for_is_404_like_one_that_does_not_exist`).
- [ ] A denied category reads as empty (`a_category_the_scope_leaves_out_is_not_listed`, `categories_come_only_from_the_constraints_that_reach_the_subject`, `per_category_the_policy_service_is_asked_for_each_category`).
- [ ] Nothing is served while personalization is off or an erasure is under way (`nothing_is_served_while_personalization_is_off_or_an_erasure_is_under_way`).

## 7. Non-Functional Considerations

- **Security**: the caller is taken only from the platform. A named tenant is honored only when the AuthZ resolver authorizes the caller for it. Only the subject reads its own profile on a tenant constraint; anyone else needs the subject named in the policy answer. A caller that may not act for the subject cannot tell a missing profile from a refused one. The facts are read as Construct's processing identity, which the deployment grants graph storage's read, write, delete and type registration and the settings read, in every tenant.
- **Privacy**: the answer is marked `no-store`, with the security headers above. No log line holds a value; the read event names categories only.
- **Reliability**: the queue is bounded; a full queue refuses the record with `503`, and intake keeps no identity of it, so the connector sends it again. A record queued or in flight when the gear stops is lost, and the gear logs how many; its identity stays, so a resend is a repeat, as the DESIGN states.
- **Permissions**: changes apply from the next read; nothing is cached.
- **Deployment**: the gear now needs graph storage in its host. Without a processing identity the profile read answers `501`; without a model, received records are dropped with an event.
