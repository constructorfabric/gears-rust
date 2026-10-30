# Feature: Subject Settings


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [View One's Own Settings](#view-ones-own-settings)
  - [Change One's Own Setting](#change-ones-own-setting)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Read a Subject's Settings](#read-a-subjects-settings)
  - [Owner Only](#owner-only)
  - [Read the Tenant's Personalization Default](#read-the-tenants-personalization-default)
  - [Start the Settings Service](#start-the-settings-service)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Subject Settings Table](#subject-settings-table)
  - [Settings Read](#settings-read)
  - [Tenant Default From the Settings Service](#tenant-default-from-the-settings-service)
  - [Subject's Settings Routes](#subjects-settings-routes)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-subject-settings-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-subject-settings`

## 1. Feature Context

### 1.1 Overview

Subject Settings keeps, per subject, whether personalization is on and whether an erasure of the subject is under way. Record Intake, Admission and the Profile Reader read these two states. A subject without a stored row has the tenant's personalization default, a tenant setting in the settings service. The subject reads and changes its own setting through two routes. This document describes the first part of the feature: the table, the read that other features call, the tenant default, and the subject's settings routes.

### 1.2 Purpose

The PRD requires Construct to keep, for each subject, whether personalization is on, and lets a tenant administrator set the default for new subjects (`cpt-cf-construct-fr-settings`). The DESIGN names the table `construct__subject_settings` (`cpt-cf-construct-dbtable-subject-settings`) and the entity Subject Settings (`cpt-cf-construct-entity-subject-settings`), and leaves its columns to this document. It is the first of Construct's own tables (`cpt-cf-construct-db-own-tables`).

The table has one row per tenant and subject: the tenant id, the subject id, personalization on or off, and whether an erasure is under way. A subject without a row has not changed a setting. Its settings are the tenant's default with no erasure under way, and reading them stores nothing, so a change of the tenant default reaches every subject that has not chosen.

The subject reads and changes its personalization setting through `GET` and `PUT /api/construct/v1/subjects/{subject_id}/settings` (the code registers `/construct/v1/...`; the gateway adds the prefix). Owner only: the path names the caller, or the call is refused with 403. A change applies from the next request. It never touches the erasure flag: turning personalization on during an erasure stores the choice, and the erasure keeps everything off until it is done, as the DESIGN says.

**Out of scope** in this part:

- The settings in the subject's view and in the export, which belong to Subject Control.
- SDK client methods for settings.
- Marking and clearing the erasure. Erasure owns that; the column exists so that it can.
- Declaring the personalization default in the settings service. Nothing declares Construct's settings there yet, and whether the settings service runs in Construct's host is not decided. Until it does, the config key `personalization_default` (default on) stands in for the tenant setting.
- Retiring the foundation's placeholder route, client operations and table. Record Intake does that.
- Refusing records, dropping plans and hiding facts when personalization is off or an erasure is under way. Record Intake, Admission and the Profile Reader do that.

**Requirements**: `cpt-cf-construct-fr-settings`: the stored state, the tenant default, and the subject's read and change of its setting. Refusing records, dropping plans and hiding facts while personalization is off belong to the features named above. `cpt-cf-construct-fr-subject-access` is shared with Subject Control and is not touched in this part.

**Principles**: None.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-subject` | Reads its own settings and turns personalization on or off. |
| `cpt-cf-construct-actor-platform-auth` | Authenticates the caller and supplies its subject id and tenant in the security context. Decides, through the AuthZ resolver, whether the caller may read or change the settings ("Control the profiles one owns"). |
| `cpt-cf-construct-actor-settings-service` | Holds the tenant's personalization default for new subjects. |
| `cpt-cf-construct-actor-tenant-admin` | Sets the personalization default in the settings service. Construct keeps no tenant settings of its own. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), Subject Settings entity and the table `construct__subject_settings`
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.3
- **Dependencies**: `cpt-cf-construct-feature-gear-foundation`

## 2. Actor Flows (CDSL)

The read in section 3 is also a domain service that Record Intake, Admission and the Profile Reader call in process, without the owner check.

### View One's Own Settings

- [x] `p1` - **ID**: `cpt-cf-construct-flow-subject-settings-view`

**Actor**: `cpt-cf-construct-actor-subject`

**Success Scenarios**:
- The subject reads its own settings and gets 200 with its subject id, whether personalization is on, and whether an erasure is under way. A subject without stored settings gets the tenant's default and no erasure under way.

**Error Scenarios**:
- The path names another subject, in this tenant or another: 403. The policy service is not asked.
- The platform does not allow the caller to read the settings: 403.
- The subject id in the path is not a UUID: 400.
- The permission check or the tenant default cannot be read in time, or the settings service is unavailable: 503. Any other failure: 500.

**Steps**:
1. [x] - `p1` - Caller sends `GET /construct/v1/subjects/{subject_id}/settings`. The route is marked authenticated - `inst-view-send`
2. [x] - `p1` - API: the extractors take the security context, the service and the subject id from the path; a subject id that is not a UUID gets 400 - `inst-view-parse`
3. [x] - `p1` - Run `cpt-cf-construct-algo-subject-settings-owner-only` - `inst-view-owner`
4. [x] - `p1` - Run `cpt-cf-construct-algo-subject-settings-read` for the subject - `inst-view-read`
5. [x] - `p1` - **RETURN** 200 with `subject_id`, `personalization_enabled` and `erasure_in_progress`, marked `Cache-Control: no-store` so no proxy or browser cache keeps one subject's privacy state - `inst-view-return`

### Change One's Own Setting

- [x] `p1` - **ID**: `cpt-cf-construct-flow-subject-settings-change`

**Actor**: `cpt-cf-construct-actor-subject`

**Success Scenarios**:
- The subject turns personalization on or off and gets 200 with the settings as stored. The next request sees the change.

**Error Scenarios**:
- The path names another subject, in this tenant or another: 403, and nothing is stored. The policy service is not asked.
- The platform does not allow the caller to change the settings: 403, and nothing is stored.
- The body is not valid JSON: 400. It lacks `personalization_enabled`, the value is not a boolean, or it has another field, such as `erasure_in_progress`: 422.
- The permission check fails: 503 when a retry may help, 500 otherwise. The database fails: 500.

**Steps**:
1. [x] - `p1` - Caller sends `PUT /construct/v1/subjects/{subject_id}/settings` with `{"personalization_enabled": true|false}`. The route is marked authenticated - `inst-change-send`
2. [x] - `p1` - API: the extractors take the security context, the service, the subject id from the path and the JSON body. The body refuses unknown fields - `inst-change-parse`
3. [x] - `p1` - Run `cpt-cf-construct-algo-subject-settings-owner-only` - `inst-change-owner`
4. [x] - `p1` - Ask the AuthZ resolver, through the policy enforcer, to decide on `gts.cf.construct.subject.settings.v1~` with the action `update`, the subject id as the resource id and as the owner, and the caller's tenant as the owner-tenant property. A denial is 403 - `inst-change-scope`
5. [x] - `p1` - DB, in one transaction with the next step: decide, as in the read, whether the scope covers the key; **IF** not covered or undecidable, refuse with the read's out-of-scope error and write nothing. Otherwise INSERT the row (tenant, subject, personalization) through the secure ORM, checked against the scope; ON CONFLICT on (tenant, subject) UPDATE only `personalization_enabled`. A new row takes the column default for the erasure flag (no erasure under way); an existing row keeps its flag - `inst-change-store`
6. [x] - `p1` - DB: read the row back by tenant and subject within the scope, and commit. So the answer is this request's write even when the subject sends two changes at once. **RETURN** 200 with the settings as stored, marked `Cache-Control: no-store`. A client that disconnects before the commit stores nothing; after it, the one write stays, which a repeat of the request repeats without harm - `inst-change-return`

## 3. Processes / Business Logic (CDSL)

### Read a Subject's Settings

- [x] `p1` - **ID**: `cpt-cf-construct-algo-subject-settings-read`

**Input**: The security context and the subject id.

**Output**: The subject's settings: personalization on or off, and whether an erasure is under way. Or an error: forbidden, unavailable, internal or database.

**Steps**:
1. [x] - `p1` - Take the tenant from the subject tenant of the security context. Ask the AuthZ resolver, through the policy enforcer, to decide on the resource type `gts.cf.construct.subject.settings.v1~` (the settings' GTS type id) with the action `get`, the subject id as the resource id and as the owner, and the tenant as the owner-tenant property. The resource type supports the owner tenant, the resource id and the owner, so a policy can say "owner only" itself; an owner constraint is applied to `subject_id`, the subject being the owner of its settings. A denial is a forbidden error; a failed evaluation is unavailable or internal, mapped as in the foundation (`cpt-cf-construct-algo-gear-foundation-map-errors`) - `inst-read-scope`
2. [x] - `p1` - The repository decides, before it reads the row, whether the scope covers the key: the owner tenant, and the subject as resource id and owner. Each filter's property is mapped to its column by the entity itself (`ScopableEntity::resolve_property`), and only a UUID value matches, as in the secure ORM, which compares values by kind; constraints are OR-ed and the filters of one constraint AND-ed, as the secure ORM applies them. The lookup answers *stored*, *absent*, *not covered* or *undecidable*: a filter on a property the entity does not scope counts as not covering, and a group or tenant-subtree predicate, which cannot be checked without the database, makes it undecidable. **IF** not covered or undecidable, refuse with an out-of-scope error, answered as forbidden, that names the caller, the caller's tenant, the subject and the subject's tenant, which on Record Intake's path is the one the connector named, with a message of its own for an undecidable predicate. The answer is the same whether or not a row exists, and a scope that leaves the row out never turns into the tenant's default (an opt-out read as on, an erasure under way read as none) - `inst-read-cover`
3. [x] - `p1` - DB: acquire a connection and SELECT the row of the subject in the caller's tenant from `construct__subject_settings` through the secure ORM, filtered by the scope. The row is looked up by tenant and subject together (`SubjectKey`), never by the subject alone, because the same subject id can have a row in another tenant - `inst-read-select`
4. [x] - `p1` - **IF** the row exists, **RETURN** its two states - `inst-read-stored`
5. [x] - `p1` - Otherwise run `cpt-cf-construct-algo-subject-settings-tenant-default` for the tenant. **RETURN** personalization as the default says and no erasure under way. Store nothing - `inst-read-default`

Record Intake runs steps 2 to 5 directly (`settings_within`), under the scope of its own permission decision and for the tenant the connector named, so a connector needs no second permission. A scope that does not cover the subject's key is refused, as in step 2, so a row it leaves out never becomes the tenant's default. A scope that covers several tenants still reads only the named tenant's row.

### Owner Only

- [x] `p1` - **ID**: `cpt-cf-construct-algo-subject-settings-owner-only`

**Input**: The security context and the subject id in the path.

**Output**: Allowed, or a forbidden error.

**Steps**:
1. [x] - `p1` - **IF** the subject id is the caller's (`SecurityContext::subject_id`), continue. The caller's tenant is the only tenant the routes work in, so a subject of another tenant is never the caller - `inst-owner-compare`
2. [x] - `p1` - Otherwise refuse with a forbidden error, before the policy service is asked and before anything is read or written. Its message names the caller, the caller's tenant and the requested subject, so the log line written where the error is mapped says who was refused which settings - `inst-owner-refuse`
3. [x] - `p1` - On the routes the refusal is 403 with the reason `ACCESS_DENIED` and the resource type `gts.cf.construct.subject.settings.v1~`; the internal message stays out of the answer. A 403 rather than a 404: a subject without stored settings answers 200, so a 404 would not look like any real subject - `inst-owner-refuse-return`

### Read the Tenant's Personalization Default

- [x] `p1` - **ID**: `cpt-cf-construct-algo-subject-settings-tenant-default`

**Input**: The security context, the tenant id, and the configured fallback.

**Output**: Whether personalization is on for a new subject of the tenant. Or an unavailable or internal error.

The setting key is `gts.cf.core.settings.setting_type.v1~cf.construct.personalization.default_enabled.v1~`, read for the scope `/tenants/{tenant_id}`. Its value is a boolean.

**Steps**:
Errors are not logged here: as in the foundation, every domain error is logged once, where it is mapped to a response, and its message names the setting key. Each time the configured fallback is used, which is not an error, the reason is logged with the tenant id and the key: at warn level the first time for each reason (no client, not declared, retired), at debug level after that. So a host that ignores the tenant setting shows in the log without flooding it.

1. [x] - `p1` - Resolve the settings client (`SettingsReaderClient`) from ClientHub on each read, so the order in which gears start does not matter. **IF** no settings client is registered, **RETURN** the configured fallback - `inst-default-client`
2. [x] - `p1` - Read the effective value of the setting for the tenant scope, with the caller's security context. **IF** no answer comes within the read timeout (5 seconds by default), **RETURN** an unavailable error - `inst-default-read`
3. [x] - `p1` - **IF** the read succeeds, **RETURN** the value. A value that is not a boolean is an internal error - `inst-default-value`
4. [x] - `p1` - **IF** the settings service answers not found for the setting's declaration, or says the setting was retired, **RETURN** the configured fallback. A not found for anything else, such as the tenant scope, is not a missing declaration and goes to the last step - `inst-default-undeclared`
5. [x] - `p1` - **IF** the settings service is unavailable, or the failure is one a retry may cure (deadline exceeded, resource exhausted: the categories the foundation also treats as retryable), **RETURN** an unavailable error. The settings service's retry delay is not passed on, because the unavailable error carries none, as on the policy-service path. The fallback is not used: a guess could turn personalization on for a tenant whose default is off - `inst-default-unavailable`
6. [x] - `p1` - **IF** the read fails for any other reason, such as a denied read or a not found that is not the declaration's, **RETURN** an internal error - `inst-default-failed`

### Start the Settings Service

- [x] `p1` - **ID**: `cpt-cf-construct-algo-subject-settings-start`

**Input**: The gear configuration, the database provider, the policy enforcer and ClientHub.

**Output**: The settings service, held by the gear for its routes.

**Steps**:
1. [x] - `p1` - In `init`, build the tenant default from ClientHub and the config key `personalization_default` (default on; an unknown key is refused), and the settings service on the database provider and the policy enforcer. Store it once; a second initialization fails. The REST capability hook registers the two routes with it - `inst-settings-start`

## 4. States (CDSL)

None. The two states are stored values, not a state machine: the subject turns personalization on and off through the settings routes, and Erasure sets and clears the erasure flag.

## 5. Definitions of Done

### Subject Settings Table

- [x] `p1` - **ID**: `cpt-cf-construct-dod-subject-settings-storage`

The system **MUST** supply to the runtime a migration, `m002_subject_settings`, that creates the table `construct__subject_settings` with the columns `tenant_id`, `subject_id`, `personalization_enabled` and `erasure_in_progress` (default false), and the primary key (`tenant_id`, `subject_id`). The primary key leads with the tenant, so it also serves the tenant filter of every scoped read. The migration **MUST** be safe to run twice and **MUST NOT** edit `initial_001`. The entity **MUST** be declared tenant-scoped for the secure ORM, with the tenant column `tenant_id`, and `subject_id` as both the resource column and the owner column: the subject owns its settings.

**Implements**:
- `cpt-cf-construct-algo-subject-settings-read`

**Touches**:
- DB Table: `construct__subject_settings` (`cpt-cf-construct-dbtable-subject-settings`)
- Entities: Subject Settings (`cpt-cf-construct-entity-subject-settings`)

**Verified by**: `every_backend_keys_the_table_by_tenant_then_subject`, `every_backend_declares_the_erasure_default`, `mysql_ddl_uses_binary_uuid_columns`, `identifiers_are_namespaced_and_within_63_bytes`, `sqlite_ddl_is_safe_to_run_twice` (migration), and `stored_settings_round_trip`, `subject_without_a_row_is_not_found`, `row_of_another_tenant_is_not_found`, `same_subject_keeps_one_row_per_tenant`, `erasure_flag_defaults_to_cleared_when_the_insert_leaves_it_out`, `second_row_for_the_same_tenant_and_subject_is_refused`, `a_scope_over_several_tenants_reads_only_the_named_tenants_row` (repository). The SQLite DDL runs in every repository and service test, so the default and the primary key are shown there by behavior. The MySQL and PostgreSQL DDL uses the same `IF NOT EXISTS` form and is only asserted as text, so running it twice is not shown on those backends. No test: the `down` step.

### Settings Read

- [x] `p1` - **ID**: `cpt-cf-construct-dod-subject-settings-read`

The system **MUST** give the other features one read of a subject's settings, in the caller's tenant only, behind its own permission check (`gts.cf.construct.subject.settings.v1~`, `get`). A subject without a row **MUST** get the tenant's default and no erasure under way, and the read **MUST NOT** store a row.

**Implements**:
- `cpt-cf-construct-algo-subject-settings-read`

**Touches**:
- DB Table: `construct__subject_settings`
- Entities: Subject Settings

**Verified by**: `subject_without_a_row_gets_the_tenant_default`, `stored_row_wins_over_the_tenant_default`, `reading_the_default_stores_nothing`, `settings_of_another_tenant_are_not_visible`, `caller_without_permission_is_denied_before_any_read`, `unavailable_tenant_default_is_an_unavailable_error`, `service_asks_for_the_subject_settings_resource_and_the_tenant` (service). The policy service is a stub; the real AuthZ resolver was not run.

### Tenant Default From the Settings Service

- [x] `p1` - **ID**: `cpt-cf-construct-dod-subject-settings-tenant-default`

The system **MUST** read the personalization default from the settings service through ClientHub, for the tenant scope, within a bounded time. It **MUST** use the configured fallback only when no settings client is registered, the setting's declaration is not found, or the setting was retired, and **MUST** log the first use of the fallback per reason at warn level. It **MUST** fail with an unavailable error when the settings service is unavailable, does not answer in time, or fails in a way a retry may cure, and with an internal error otherwise.

**Implements**:
- `cpt-cf-construct-algo-subject-settings-tenant-default`

**Touches**:
- Settings service: the setting `cf.construct.personalization.default_enabled.v1`

**Verified by**: `without_a_settings_client_the_configured_default_stands`, `the_tenant_value_wins_over_the_configured_default`, `reads_the_construct_key_for_the_tenant_scope`, `an_undeclared_setting_falls_back_to_the_configured_default`, `a_retired_setting_falls_back_to_the_configured_default`, `a_not_found_for_anything_but_the_declaration_is_an_internal_error`, `an_unavailable_settings_service_is_an_unavailable_error`, `a_retryable_failure_is_an_unavailable_error`, `a_read_without_an_answer_in_time_is_an_unavailable_error`, `any_other_failure_is_an_internal_error`, `a_value_that_is_not_a_boolean_is_an_internal_error`, `the_fallback_keeps_answering_after_its_first_warning`. The settings client is a stub registered in a ClientHub; the real settings service was not run. No test checks the log output, so the warn-once logging is verified by review only.

### Subject's Settings Routes

- [x] `p1` - **ID**: `cpt-cf-construct-dod-subject-settings-routes`

The system **MUST** serve `GET` and `PUT /construct/v1/subjects/{subject_id}/settings`, authenticated, owner only, with their own permission checks (`get`, `update`). A change **MUST** apply from the next request, **MUST NOT** change the erasure flag, and **MUST** refuse a body with any field other than `personalization_enabled`. Another subject or tenant **MUST** be refused with 403 before the policy service is asked, and nothing is stored.

**Implements**:
- `cpt-cf-construct-flow-subject-settings-view`
- `cpt-cf-construct-flow-subject-settings-change`
- `cpt-cf-construct-algo-subject-settings-owner-only`
- `cpt-cf-construct-algo-subject-settings-start`

**Touches**:
- API: `GET`, `PUT /construct/v1/subjects/{subject_id}/settings`
- DB Table: `construct__subject_settings`
- Config: `personalization_default`

**Verified by**: `owner_without_stored_settings_gets_200_with_the_tenant_default`, `owner_changes_the_setting_and_reads_it_back`, `the_erasure_flag_is_shown_and_survives_a_change`, `another_subject_gets_403_for_both_routes`, `the_owner_never_sees_its_row_of_another_tenant`, `both_routes_answer_no_store`, `owner_the_platform_denies_gets_403`, `a_body_that_tries_to_set_the_erasure_flag_gets_422`, `a_body_without_the_setting_gets_422_and_bad_json_gets_400`, `a_subject_id_that_is_not_a_uuid_gets_400` (routes), `owner_reads_its_own_settings`, `owner_changes_personalization_and_the_next_read_sees_it`, `turning_personalization_on_does_not_end_an_erasure`, `another_subject_is_refused_before_the_policy_service_is_asked`, `the_owner_reads_only_its_row_of_its_own_tenant`, `an_owner_only_policy_lets_the_owner_read_and_change`, `an_owner_constraint_for_another_caller_is_refused`, `a_scope_that_does_not_cover_the_key_is_refused_with_or_without_a_row`, `a_change_the_scope_does_not_cover_is_refused_and_writes_nothing`, `the_scope_refusal_names_the_callers_tenant_and_the_keys_tenant`, `an_out_of_scope_settings_route_error_is_403_on_the_settings_resource_without_its_ids`, `an_out_of_scope_error_on_the_record_path_is_403_without_its_ids`, `an_undecidable_scope_is_refused_with_a_message_naming_the_predicate`, `a_refused_read_back_after_an_accepted_write_is_an_internal_error`, `a_change_in_one_tenant_leaves_the_same_subject_in_another_tenant_alone`, `owner_without_permission_is_denied_and_nothing_is_stored`, `a_change_asks_for_the_update_action` (service), `setting_personalization_creates_a_row_without_an_erasure`, `setting_personalization_keeps_the_erasure_flag_of_an_existing_row`, `setting_personalization_outside_the_scope_is_refused_before_the_write`, `setting_personalization_under_another_owner_is_refused_before_the_write`, `under_the_owners_own_scope_the_row_is_written_and_read`, `find_decides_coverage_from_the_key_with_the_secure_orm_rules`, `constraints_combine_as_or_with_undecidable_below_covered`, `coverage_agrees_with_the_secure_orm_on_every_small_scope` (repository; the last one checks the coverage decision against the secure ORM's own read and write on every small `Eq`/`In` scope), `settings_errors_about_the_request_name_the_settings_resource`, `settings_failures_of_the_gear_map_as_everywhere_else` (error mapping), and `personalization_default_is_read` (config). No test runs two changes at once against one row; the transaction is verified by review. The routes run in process on in-memory SQLite with stub policy services. `init` is not run in tests; the real gateway (the 401 case), the real AuthZ resolver and MySQL/PostgreSQL were not exercised.

## 6. Acceptance Criteria

- [ ] The table is scoped by tenant through the secure data path; a cross-tenant read returns nothing (`row_of_another_tenant_is_not_found`, `settings_of_another_tenant_are_not_visible`).
- [ ] A subject without a row gets the tenant's default, with no erasure under way (`subject_without_a_row_gets_the_tenant_default`), and the read stores nothing (`reading_the_default_stores_nothing`).
- [ ] A stored row wins over the tenant's default (`stored_row_wins_over_the_tenant_default`).
- [ ] A caller without permission is refused before anything is read (`caller_without_permission_is_denied_before_any_read`).
- [ ] The default is read from the settings service through ClientHub, with a stub in the tests (`the_tenant_value_wins_over_the_configured_default`, `reads_the_construct_key_for_the_tenant_scope`), and falls back to the configured value only when there is no settings client or no declaration (`without_a_settings_client_the_configured_default_stands`, `an_undeclared_setting_falls_back_to_the_configured_default`, `a_retired_setting_falls_back_to_the_configured_default`, `a_not_found_for_anything_but_the_declaration_is_an_internal_error`).
- [ ] A settings service that is unavailable, too slow or throttling gives an unavailable error, never the fallback (`an_unavailable_settings_service_is_an_unavailable_error`, `a_retryable_failure_is_an_unavailable_error`, `a_read_without_an_answer_in_time_is_an_unavailable_error`).
- [ ] One row per tenant and subject: a second row is refused, and an insert without the erasure flag stores it cleared (`second_row_for_the_same_tenant_and_subject_is_refused`, `erasure_flag_defaults_to_cleared_when_the_insert_leaves_it_out`).
- [ ] The owner reads and changes its setting through the routes, and the next read sees the change (`owner_without_stored_settings_gets_200_with_the_tenant_default`, `owner_changes_the_setting_and_reads_it_back`).
- [ ] Another subject, in this tenant or another, is refused with 403 on both routes, before the policy service is asked, and nothing is stored; the refusal names the settings resource (`another_subject_gets_403_for_both_routes`, `another_subject_is_refused_before_the_policy_service_is_asked`).
- [ ] Another tenant's data stays out of reach: the owner never sees its own subject's row of another tenant, and a change touches only the caller's tenant (`the_owner_never_sees_its_row_of_another_tenant`, `the_owner_reads_only_its_row_of_its_own_tenant`, `a_change_in_one_tenant_leaves_the_same_subject_in_another_tenant_alone`).
- [ ] Both routes answer `Cache-Control: no-store` (`both_routes_answer_no_store`).
- [ ] A change keeps the erasure flag, and the body cannot set it (`the_erasure_flag_is_shown_and_survives_a_change`, `turning_personalization_on_does_not_end_an_erasure`, `a_body_that_tries_to_set_the_erasure_flag_gets_422`).
- [ ] The tests named in this document pass. They run on an in-memory SQLite database.

## 7. Non-Functional Considerations

- **Security**: The settings are a resource of their own GTS type, `gts.cf.construct.subject.settings.v1~`, with the subject as owner, so the platform's policy can enforce owner only as well as the gear's own check. The tenant and the caller come only from the security context. The routes are owner only: another subject is refused before anything else happens. Each read and each change asks the AuthZ resolver for its own permission, and the scope it returns filters the select and checks the insert, so another tenant's row is never read or written. The body cannot set the erasure flag. The settings service is read with the caller's security context.
- **Data integrity**: One row per tenant and subject, enforced by the primary key. Reading never writes. A change is one upsert that updates only `personalization_enabled`, so two changes at once cannot create two rows; the write and its read-back share one transaction, so each answer is its own write. Rows are keyed by tenant and subject together (`SubjectKey`). The erasure flag starts cleared and is never changed by the subject.
- **Reliability**: An unavailable, slow or throttling settings service is an error, not a silent default, because personalization is a privacy setting. Each read is bounded by a timeout (5 seconds by default). The settings client is resolved on each read, so a settings service that starts after Construct is picked up.
- **Observability**: The read carries a tracing span with the tenant id and the subject id. The first use of the fallback for each reason is logged at warn level with the tenant id, the key and the reason, later uses at debug level. Errors are logged once, where they are mapped to a response, as in the foundation; their message names the setting key.
- **Rollback**: The migration has a `down` step that drops the table. It has no test.
- **Not done in this part**: see the out-of-scope list in 1.2.
