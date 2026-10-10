# Feature: Subject Settings


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Read a Subject's Settings](#read-a-subjects-settings)
  - [Read the Tenant's Personalization Default](#read-the-tenants-personalization-default)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Subject Settings Table](#subject-settings-table)
  - [Settings Read](#settings-read)
  - [Tenant Default From the Settings Service](#tenant-default-from-the-settings-service)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-subject-settings-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-subject-settings`

## 1. Feature Context

### 1.1 Overview

Subject Settings keeps, per subject, whether personalization is on and whether an erasure of the subject is under way. Record Intake, Admission and the Profile Reader read these two states. A subject Construct holds no row for starts with the tenant's personalization default, a tenant setting in the settings service. This document describes the first part of the feature, which Story 5.7.1 builds: the table, the read that other features call, and the tenant default.

### 1.2 Purpose

The PRD requires Construct to keep, for each subject, whether personalization is on, and lets a tenant administrator set the default for new subjects (`cpt-cf-construct-fr-settings`). The DESIGN names the table `construct__subject_settings` (`cpt-cf-construct-dbtable-subject-settings`) and the entity Subject Settings (`cpt-cf-construct-entity-subject-settings`), and leaves its columns to this document. It is the first of Construct's own tables (`cpt-cf-construct-db-own-tables`).

The table has one row per tenant and subject: the tenant id, the subject id, personalization on or off, and whether an erasure is under way. A subject without a row has not changed a setting. Its settings are the tenant's default with no erasure under way, and reading them stores nothing, so a change of the tenant default reaches every subject that has not chosen.

**Out of scope** in this part (Story 5.7.1):

- The subject's settings routes, `GET` and `PUT /api/construct/v1/subjects/{subject_id}/settings`. Story 5.7 leaves out the subject's own routes. So there is no write of a row yet, and no SDK client method for settings.
- The settings in the subject's view and in the export, which belong to Subject Control.
- Marking and clearing the erasure. Erasure owns that; the column exists so that it can.
- Declaring the personalization default in the settings service. Nothing declares Construct's settings there yet (Story 5.11 decides whether the settings service runs in Construct's host). Until it does, a configured fallback stands in for the tenant setting.
- Wiring the read into the gear's startup. The first feature that reads the settings, Record Intake, builds the service in `init` and adds the config key for the fallback.
- Retiring the foundation's placeholder route, client operations and table. Record Intake does that.
- Refusing records, dropping plans and hiding facts when personalization is off or an erasure is under way. Record Intake, Admission and the Profile Reader do that.

**Requirements**: `cpt-cf-construct-fr-settings` (partial: the stored state and the tenant default). `cpt-cf-construct-fr-subject-access` is shared with Subject Control and is not touched in this part.

**Principles**: None.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-platform-auth` | Decides, through the AuthZ resolver, whether the caller may read a subject's settings. Supplies the caller's tenant in the security context. |
| `cpt-cf-construct-actor-settings-service` | Holds the tenant's personalization default for new subjects. |
| `cpt-cf-construct-actor-tenant-admin` | Sets the personalization default in the settings service. Construct keeps no tenant settings of its own. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), Subject Settings entity and the table `construct__subject_settings`
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.3
- **Dependencies**: `cpt-cf-construct-feature-gear-foundation`

## 2. Actor Flows (CDSL)

**Use cases**: None in this part. The subject's settings routes come later (see 1.2).

This part has no public surface. The read is a domain service that Record Intake, Admission and the Profile Reader call in process.

## 3. Processes / Business Logic (CDSL)

### Read a Subject's Settings

- [x] `p1` - **ID**: `cpt-cf-construct-algo-subject-settings-read`

**Input**: The security context and the subject id.

**Output**: The subject's settings: personalization on or off, and whether an erasure is under way. Or an error: forbidden, unavailable, internal or database.

**Steps**:
1. [x] - `p1` - Take the tenant from the subject tenant of the security context. Ask the AuthZ resolver, through the policy enforcer, to decide on the resource type `construct.subject_settings` with the action `get`, the subject id as the resource id, and the tenant as the owner-tenant property. A denial is a forbidden error; a failed evaluation is unavailable or internal, mapped as in the foundation (`cpt-cf-construct-algo-gear-foundation-map-errors`) - `inst-read-scope`
2. [x] - `p1` - DB: acquire a connection and SELECT the row of the subject from `construct__subject_settings` through the secure ORM, filtered by the scope and by the tenant, so a scope that covers several tenants still reads only this tenant's row - `inst-read-select`
3. [x] - `p1` - **IF** the row exists, **RETURN** its two states - `inst-read-stored`
4. [x] - `p1` - Otherwise run `cpt-cf-construct-algo-subject-settings-tenant-default` for the tenant. **RETURN** personalization as the default says and no erasure under way. Store nothing - `inst-read-default`

Record Intake runs steps 2 to 4 directly (`settings_within`), under the scope of its own permission decision and for the tenant the connector named, so a connector needs no second permission.

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

## 4. States (CDSL)

None in this part. The two states are stored values, not a state machine: a subject turns personalization on and off through the settings routes, and Erasure sets and clears the erasure flag. Both come later.

## 5. Definitions of Done

### Subject Settings Table

- [x] `p1` - **ID**: `cpt-cf-construct-dod-subject-settings-storage`

The system **MUST** supply to the runtime a migration, `m002_subject_settings`, that creates the table `construct__subject_settings` with the columns `tenant_id`, `subject_id`, `personalization_enabled` and `erasure_in_progress` (default false), and the primary key (`tenant_id`, `subject_id`). The primary key leads with the tenant, so it also serves the tenant filter of every scoped read. The migration **MUST** be safe to run twice and **MUST NOT** edit `initial_001`. The entity **MUST** be declared tenant-scoped for the secure ORM, with the tenant column `tenant_id` and the resource column `subject_id`.

**Implements**:
- `cpt-cf-construct-algo-subject-settings-read`

**Touches**:
- DB Table: `construct__subject_settings` (`cpt-cf-construct-dbtable-subject-settings`)
- Entities: Subject Settings (`cpt-cf-construct-entity-subject-settings`)

**Verified by**: `every_backend_keys_the_table_by_tenant_then_subject`, `every_backend_declares_the_erasure_default`, `mysql_ddl_uses_binary_uuid_columns`, `identifiers_are_namespaced_and_within_63_bytes` (migration), and `stored_settings_round_trip`, `subject_without_a_row_is_not_found`, `row_of_another_tenant_is_not_found`, `same_subject_keeps_one_row_per_tenant`, `erasure_flag_defaults_to_cleared_when_the_insert_leaves_it_out`, `second_row_for_the_same_tenant_and_subject_is_refused`, `a_scope_over_several_tenants_reads_only_the_named_tenants_row` (repository). The SQLite DDL runs in every repository and service test, so the default and the primary key are shown there by behavior. The MySQL and PostgreSQL DDL is only asserted as text. No test: the `down` step, and running the migration twice.

### Settings Read

- [x] `p1` - **ID**: `cpt-cf-construct-dod-subject-settings-read`

The system **MUST** give the other features one read of a subject's settings, in the caller's tenant only, behind its own permission check (`construct.subject_settings`, `get`). A subject without a row **MUST** get the tenant's default and no erasure under way, and the read **MUST NOT** store a row.

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

## 6. Acceptance Criteria

- [ ] The table is scoped by tenant through the secure data path; a cross-tenant read returns nothing (`row_of_another_tenant_is_not_found`, `settings_of_another_tenant_are_not_visible`).
- [ ] A subject without a row gets the tenant's default, with no erasure under way (`subject_without_a_row_gets_the_tenant_default`), and the read stores nothing (`reading_the_default_stores_nothing`).
- [ ] A stored row wins over the tenant's default (`stored_row_wins_over_the_tenant_default`).
- [ ] A caller without permission is refused before anything is read (`caller_without_permission_is_denied_before_any_read`).
- [ ] The default is read from the settings service through ClientHub, with a stub in the tests (`the_tenant_value_wins_over_the_configured_default`, `reads_the_construct_key_for_the_tenant_scope`), and falls back to the configured value only when there is no settings client or no declaration (`without_a_settings_client_the_configured_default_stands`, `an_undeclared_setting_falls_back_to_the_configured_default`, `a_retired_setting_falls_back_to_the_configured_default`, `a_not_found_for_anything_but_the_declaration_is_an_internal_error`).
- [ ] A settings service that is unavailable, too slow or throttling gives an unavailable error, never the fallback (`an_unavailable_settings_service_is_an_unavailable_error`, `a_retryable_failure_is_an_unavailable_error`, `a_read_without_an_answer_in_time_is_an_unavailable_error`).
- [ ] One row per tenant and subject: a second row is refused, and an insert without the erasure flag stores it cleared (`second_row_for_the_same_tenant_and_subject_is_refused`, `erasure_flag_defaults_to_cleared_when_the_insert_leaves_it_out`).
- [ ] The tests named in this document pass. They run on an in-memory SQLite database.

## 7. Non-Functional Considerations

- **Security**: The tenant comes only from the security context. Each read asks the AuthZ resolver for its own permission, and the scope it returns filters the select, so another tenant's row is never read. The settings service is read with the caller's security context.
- **Data integrity**: One row per tenant and subject, enforced by the primary key. Reading never writes. The erasure flag starts cleared.
- **Reliability**: An unavailable, slow or throttling settings service is an error, not a silent default, because personalization is a privacy setting. Each read is bounded by a timeout (5 seconds by default). The settings client is resolved on each read, so a settings service that starts after Construct is picked up.
- **Observability**: The read carries a tracing span with the tenant id and the subject id. The first use of the fallback for each reason is logged at warn level with the tenant id, the key and the reason, later uses at debug level. Errors are logged once, where they are mapped to a response, as in the foundation; their message names the setting key.
- **Rollback**: The migration has a `down` step that drops the table. It has no test.
- **Not done in this part**: see the out-of-scope list in 1.2.
