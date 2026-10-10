# Feature: Record Intake


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Send One Record](#send-one-record)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Check a Record Against Its Type](#check-a-record-against-its-type)
  - [Hand a Received Record to Processing](#hand-a-received-record-to-processing)
  - [Write a Drop Event](#write-a-drop-event)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Record Types](#record-types)
  - [Intake Checks](#intake-checks)
  - [Record Identity Table](#record-identity-table)
  - [Connector Switch](#connector-switch)
  - [Hand-off](#hand-off)
  - [Drop Event](#drop-event)
  - [Intake Route](#intake-route)
  - [Client in ClientHub](#client-in-clienthub)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-record-intake-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-record-intake`

## 1. Feature Context

### 1.1 Overview

Record Intake is the single entry for connectors (`cpt-cf-construct-component-results-api`). A connector sends one record per request for one tenant. Intake checks the record against its GTS type and the types it derives from, checks that the connector is on and, for a record about a subject, that personalization is on with no erasure under way. It then keeps the record's identity, so that a resend is a repeat, hands the record to background processing, and answers received, repeat or refused at once. Until the Planner (Story 5.2) delivers the processing, a stub refuses every record: intake answers `503` and keeps nothing, so no record is taken and lost. This document describes Story 5.7.2.

### 1.2 Purpose

The PRD requires Construct to take records one at a time, check each against its type and base types, and answer received, repeat or refused (`cpt-cf-construct-fr-record-intake`). A refusal names the type, the place in the record and the broken rule, and changes nothing. A repeat changes no fact. Records are refused while personalization is off or an erasure is under way (`cpt-cf-construct-fr-settings`). Construct depends only on the base record shape (`cpt-cf-construct-principle-base-record-only`): a record of a newly registered type is taken with no change to Construct.

**The record types.** The record base type `gts.cf.connectors.core.record.v1~` and the four derived types (chat message, enrichment fact candidate, mastery course catalog, mastery student mastery) are defined in rolos-cyber (`docs/construct/GTS/schemas/`, ADR 0003), which stays their source of truth (`cpt-cf-construct-contract-gts-record`). The SDK crate holds byte-for-byte copies under `construct-sdk/schemas/` and submits them to the link-time schema inventory, which the types registry drains at boot. The connectors are not Rust and cannot register their types themselves, and the in-host types registry fills only at boot. The envelope is the base: `type`, `provenance`, `version`, `observed_at` and `payload` are required, `subject_id` is optional (absent only for source-scoped records), and the top level takes no other member. A pushed record **MUST NOT** carry `id`.

**The tenant and the connector.** A connector serves many tenants with one login, so it names the tenant in the `tenant` query parameter, never in the record (`cpt-cf-construct-fr-access-control`, as amended with this feature). The AuthZ resolver must authorize the connector for that tenant: intake asks for the resource type `construct.record` with the action `send`, passes the named tenant as the context tenant (root only) and as the owner tenant, and every later step runs under the scope this one decision returns. The connector identity is the subject id of the connector's login. The subject's settings are read only in the named tenant, also when the scope covers more than one tenant.

**The record identity.** One row per tenant and record identity in `construct__record_ids` (`cpt-cf-construct-dbtable-record-ids`). The identity is (tenant, connector, provenance, version), as the record base type states. Inserting it is the repeat check, so two copies sent at once cannot both be received. The table holds no record content.

**Taking a record.** The identity insert and the hand-off run in one transaction, which commits only after processing took the record. A hand-off that refuses (no processing wired in, or a full queue) rolls the identity back and answers `503`, so the connector can send the record again. A request cancelled before the commit, by a client that goes away or the gateway's timeout, leaves no identity behind either. A request cancelled while the commit is under way can leave the record handed off without its identity; a resend is then processed twice, which the Profile Writer's idempotent write tolerates.

**Out of scope**:

- The Planner's entry point and background processing; a stub that refuses every record stands in (Story 5.2).
- Connectors on or off in the settings service. This feature reads the switch through a small interface whose first version takes the deployment config (every connector on unless configured off), the fallback Story 5.11 names; declaring and reading it in the settings service waits for 5.11.
- Writing drop events to the Audit gear, which does not exist yet; they go to the structured log.
- Erasure and retention of record identities.
- How the personalization check, the identity insert and the later write stay in step when personalization is turned off between them; Admission's own check covers the write, and the DESIGN leaves the rest to later features.
- Moving the connector side (the chat connector still sends a list to `/results/records`) to this wire format, Story 3.3 (ROL-16816).

**Requirements**: `cpt-cf-construct-fr-record-intake`, `cpt-cf-construct-fr-settings` (shared: records refused while personalization is off or an erasure is under way), `cpt-cf-construct-fr-access-control` (the tenant named at intake), `cpt-cf-construct-fr-tenant-isolation`.

**Principles**: `cpt-cf-construct-principle-base-record-only`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-connector` | Sends one record per request for one tenant, and resends with a new version when the content changes. |
| `cpt-cf-construct-actor-platform-auth` | Authenticates the connector and decides, through the AuthZ resolver, whether it may send records for the named tenant. |
| `cpt-cf-construct-actor-types-registry` | Resolves a record's type and the types it derives from. |
| `cpt-cf-construct-actor-settings-service` | Holds the tenant's personalization default (read through Subject Settings); later also connectors on or off. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), Results API component, the record-in sequence and the table `construct__record_ids`
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.4
- **Dependencies**: `cpt-cf-construct-feature-gear-foundation`, `cpt-cf-construct-feature-subject-settings`

## 2. Actor Flows (CDSL)

### Send One Record

- [x] `p1` - **ID**: `cpt-cf-construct-flow-record-intake-submit`

**Actor**: `cpt-cf-construct-actor-connector`

**Success Scenarios**:
- A record that passes every check, with a new identity, is received: `202` with `{"outcome":"received"}`. It is handed to processing.
- A record with an identity already received is a repeat: `200` with `{"outcome":"repeat"}`. Nothing changes, even when its content differs.

**Error Scenarios**:
- No `tenant` query parameter, or one that is not a UUID: `400` problem, before any check.
- A body that is not JSON: `400`; no JSON content type: `415`. Both are problem responses.
- The platform does not authorize the connector for the named tenant: `403`. Nothing is read or stored.
- The record is refused: `422` problem with one field violation (the place as a JSON pointer, or `(record)` for the record as a whole; the broken rule; the reason code) and the record's type as the resource name. Nothing is stored. Reason codes: `ID_IN_PUSH`, `SCHEMA_VIOLATION`, `UNKNOWN_TYPE`, `ABSTRACT_TYPE`, `NOT_A_RECORD_TYPE`, `CONNECTOR_OFF`, `PERSONALIZATION_OFF`, `ERASURE_IN_PROGRESS`.
- Processing cannot take the record (no planner yet, or a full queue): `503`, and nothing is stored, so the record can be sent again.
- The types registry, the settings service or the policy service is unavailable, too slow or throttling: `503`. Any other failure: `500`, without its cause.
- A refusal names the record's type only once the envelope has accepted it as a well-formed type id, so a malformed `type` is never repeated. The `422` status is the REST route's: through the in-process client a refusal is the same invalid-argument error without an HTTP status.

**Steps**:
1. [x] - `p1` - Connector sends `POST /construct/v1/records?tenant=<tenant id>` with one record as the JSON body. The route is authenticated - `inst-submit-send`
2. [x] - `p1` - API: the security context, the `tenant` query parameter and the JSON body are extracted; a missing or malformed one is answered with a problem before the service runs - `inst-submit-parse`
3. [x] - `p1` - Ask the AuthZ resolver for `construct.record` / `send`, with the named tenant as the context tenant (root only) and as the owner-tenant property. A denial is `403`. The connector identity is the subject id of the caller - `inst-submit-authorize`
4. [x] - `p1` - Run `cpt-cf-construct-algo-record-intake-check-record` on the record. **IF** it is refused, **RETURN** the refusal (`422`) - `inst-submit-refused`
5. [x] - `p1` - **IF** the connector is off for the tenant, **RETURN** a refusal `CONNECTOR_OFF` - `inst-submit-connector`
6. [x] - `p1` - **IF** the record names a subject, read the subject's settings in the named tenant under the scope of step 3 (`cpt-cf-construct-algo-subject-settings-read`). **IF** an erasure is under way, **RETURN** a refusal `ERASURE_IN_PROGRESS`; **IF** personalization is off, **RETURN** a refusal `PERSONALIZATION_OFF`. A record without a subject skips this step - `inst-submit-subject`
7. [x] - `p1` - DB: in one transaction, INSERT the identity (tenant, the SHA-256 of connector, provenance and version, the parts, the subject and the time received) into `construct__record_ids` through the secure ORM, checked against the scope of step 3 - `inst-submit-insert`
8. [x] - `p1` - **IF** the identity is there already, **RETURN** repeat (`200`) - `inst-submit-repeat`
9. [x] - `p1` - Otherwise hand the record to processing (`cpt-cf-construct-algo-record-intake-hand-off`). **IF** processing refuses it, roll the transaction back and **RETURN** unavailable (`503`). Otherwise commit and **RETURN** received (`202`) - `inst-submit-received`
10. [x] - `p1` - API: write the outcome as `{"outcome": "received" | "repeat"}` with its status; map an error inside a span that names the tenant and the connector, so its one log line names them too - `inst-submit-answer`
11. [x] - `p1` - In process, `ConstructClientV1::submit_record` runs the same service and returns `RecordOutcome`; a refusal is an invalid-argument error whose reason is one of the codes in `construct_sdk::reason` - `inst-submit-client`

## 3. Processes / Business Logic (CDSL)

### Check a Record Against Its Type

- [x] `p1` - **ID**: `cpt-cf-construct-algo-record-intake-check-record`

**Input**: The record as JSON.

**Output**: The record's envelope, or a refusal that names the place and the rule, and the type once the envelope has accepted it. Never a value from the record.

**Steps**:
1. [x] - `p1` - **IF** the record has an `id`, **RETURN** a refusal `ID_IN_PUSH` at `/id`, without naming the type, which nothing has checked yet - `inst-check-id`
2. [x] - `p1` - Run the type check (steps 3 to 6) through the record-types interface; **IF** it refuses, **RETURN** its refusal - `inst-check-type`
3. [x] - `p1` - Check the record against the record base type's schema, which this gear ships, so no registry is needed. **IF** it breaks a rule, **RETURN** a refusal `SCHEMA_VIOLATION` at the first broken rule's place, without naming the type: the broken rule may be the type itself. A missing or an unexpected member is named by its own pointer; a body that is not an object is refused as a whole - `inst-check-base`
4. [x] - `p1` - Look the record's `type` up in the types registry, read from ClientHub on each check, within a timeout (5 seconds by default). **IF** the registry does not know it, or calls it malformed, **RETURN** a refusal `UNKNOWN_TYPE` at `/type`. No registry client, no answer in time or a retryable failure is unavailable; any other failure is internal, and its message keeps the registry's diagnostic for the log - `inst-check-lookup`
5. [x] - `p1` - **IF** the type is abstract, **RETURN** a refusal `ABSTRACT_TYPE`; **IF** it does not derive from the record base type, **RETURN** a refusal `NOT_A_RECORD_TYPE`. The base itself never gets here: the envelope's `type` pattern only takes a derived type - `inst-check-derives`
6. [x] - `p1` - Check the record against the type's effective schema, which includes the base (JSON Schema draft 7). The compiled schema of each type is kept for the life of the process, since a registered type version does not change. **IF** it breaks a rule, **RETURN** a refusal `SCHEMA_VIOLATION` at the first broken rule's place - `inst-check-schema`
7. [x] - `p1` - **RETURN** the envelope read from the checked record: `type`, `provenance`, `version` and `subject_id` - `inst-check-envelope`

### Hand a Received Record to Processing

- [x] `p1` - **ID**: `cpt-cf-construct-algo-record-intake-hand-off`

**Input**: A received record: the tenant, the connector, the envelope and the content, in memory only. It carries no security context: the connector's token can expire before processing runs, so processing builds its own context and scope for the tenant (one Construct writer identity per tenant). The connector identity is there for the plan's origin and the audit events.

**Output**: Taken, or refused with an unavailable error. The hand-off returns at once; the Planner's implementation puts the record on a bounded queue and refuses when it is full.

**Steps**:
1. [x] - `p1` - Until the Planner's entry point exists, refuse every record with an unavailable error: intake then rolls the identity back and answers `503` - `inst-hand-off-refuse`

### Write a Drop Event

- [x] `p1` - **ID**: `cpt-cf-construct-algo-record-intake-drop-event`

**Input**: A received record that processing drops, and the cause.

**Output**: One content-free event: tenant, connector, record type, subject and cause.

**Steps**:
1. [x] - `p1` - Write the event to the structured log at warn level under the target `construct::audit`, until the Audit gear ships. The subject is a plain id, only when the record names one; no record content is written. The Planner, the Sensitive-Data Checks, Admission and the Profile Writer emit it - `inst-drop-event-log`

## 4. States (CDSL)

None. A record identity is inserted once and never changes; erasure and retention remove it later.

## 5. Definitions of Done

### Record Types

- [x] `p1` - **ID**: `cpt-cf-construct-dod-record-intake-types`

The SDK **MUST** ship the record base type and its four derived types as byte-for-byte copies of the rolos-cyber schemas and submit them to the link-time inventory. Intake **MUST** check a record against the base envelope, its type's registration, abstractness, derivation from the base, and its type's effective schema, in that order, and report the first broken rule with its place and without the value. The SDK **MUST** expose the reason codes (`construct_sdk::reason`).

**Implements**:
- `cpt-cf-construct-algo-record-intake-check-record`

**Touches**:
- Contract: `cpt-cf-construct-contract-gts-record`
- Entities: Record (`cpt-cf-construct-entity-record`)

**Verified by**: `every_schema_file_declares_the_type_it_is_registered_under`, `only_the_base_is_abstract_and_every_other_type_derives_from_it`, `every_record_type_is_in_the_link_time_inventory` (SDK), and `the_base_schema_constant_is_the_base_entry` (SDK), and `a_valid_record_of_every_derived_type_passes`, `a_broken_envelope_is_refused_before_the_type_is_looked_up`, `a_body_that_is_not_an_object_is_refused_as_a_whole`, `an_unexpected_member_is_named`, `a_missing_payload_member_of_the_derived_type_is_named`, `a_type_that_requires_a_subject_refuses_a_record_without_one`, `a_refusal_does_not_repeat_the_value`, `a_malformed_type_is_not_echoed`, `an_unregistered_type_is_refused`, `the_base_type_itself_is_refused_by_the_envelope`, `an_abstract_derived_type_is_refused`, `a_type_that_does_not_derive_from_the_base_is_refused`, `without_a_types_registry_the_check_is_unavailable`, `a_registry_that_fails_in_a_retryable_way_is_unavailable`, `a_registry_that_does_not_answer_in_time_is_unavailable`, `a_registry_that_calls_the_type_id_malformed_refuses_the_record`, `any_other_registry_failure_is_internal_and_keeps_its_diagnostic`, and the property test `any_json_is_answered_without_a_panic` (record types, with the SDK's schemas in a mock types registry). That the copies equal the rolos-cyber files is checked by review; no test reads rolos-cyber.

### Intake Checks

- [x] `p1` - **ID**: `cpt-cf-construct-dod-record-intake-checks`

Intake **MUST** authorize the connector for the named tenant once, refuse a record with an `id`, a broken type, a connector that is off, or, for a record about a subject, personalization off or an erasure under way, and only then insert the identity and hand the record off, in one transaction that commits only when processing took it. A refused record, or one processing does not take, **MUST NOT** store anything.

**Implements**:
- `cpt-cf-construct-flow-record-intake-submit`

**Touches**:
- DB Table: `construct__record_ids`, `construct__subject_settings` (read)

**Verified by**: `a_valid_record_is_received_handed_off_and_its_identity_kept`, `the_same_identity_again_is_a_repeat_and_changes_nothing`, `the_identity_is_tenant_connector_provenance_and_version`, `a_record_with_an_id_is_refused_without_naming_its_type`, `a_record_that_breaks_its_type_is_refused_with_the_place_and_nothing_is_stored`, `a_connector_that_is_off_is_refused`, `personalization_off_for_the_subject_is_refused`, `a_new_subject_takes_the_tenant_default`, `an_erasure_under_way_is_refused_even_with_personalization_on`, `the_subject_settings_of_another_tenant_do_not_apply`, `a_record_without_a_subject_skips_the_subject_checks`, `a_connector_without_permission_is_refused_before_the_record_is_checked`, `a_connector_authorized_only_for_its_own_tenant_cannot_write_into_another`, `a_retryable_policy_failure_is_unavailable`, `an_unanswered_policy_request_is_unavailable`, `any_other_policy_failure_is_internal_and_hides_its_cause`, `without_processing_a_record_is_unavailable_and_leaves_no_identity`, `intake_asks_for_the_record_resource_with_the_named_tenant`, `every_refusal_reason_has_its_wire_code` (service), and `a_scope_over_several_tenants_reads_only_the_named_tenants_row` (subject settings repository). The policy service is a stub; the real AuthZ resolver was not run. No test cancels a request mid-transaction.

### Record Identity Table

- [x] `p1` - **ID**: `cpt-cf-construct-dod-record-intake-storage`

The system **MUST** supply a migration, `m003_record_ids`, that creates `construct__record_ids` (tenant, record key, connector, provenance, version, optional subject, time received) with the primary key (tenant, record key) and the index `idx_construct__record_ids__subject`, and drops the foundation's placeholder table `construct__foundation_notes`. `initial_001` is not edited. The record key is the SHA-256, in hex, of the connector, the provenance and the version, each followed by a NUL, because the identity can be longer than a MySQL key. The entity is tenant-scoped for the secure ORM.

**Implements**:
- `cpt-cf-construct-flow-record-intake-submit`

**Touches**:
- DB Table: `construct__record_ids` (`cpt-cf-construct-dbtable-record-ids`), `construct__foundation_notes` (dropped)

**Verified by**: `every_backend_keys_the_table_by_tenant_then_record_key`, `the_placeholder_note_table_is_dropped` (runs the migrations on SQLite and checks the table is gone), `identifiers_are_namespaced_and_within_63_bytes`, `the_names_sort_in_the_order_the_migrations_must_run` (migrations), `a_new_identity_is_inserted`, `a_second_insert_of_the_same_identity_is_a_repeat`, `an_insert_outside_the_scope_is_forbidden_and_stores_nothing`, `the_key_separates_the_parts_of_the_identity` (repository). The SQLite DDL runs in every test; the MySQL and PostgreSQL DDL is only asserted as text. No test: the `down` step.

### Connector Switch

- [x] `p1` - **ID**: `cpt-cf-construct-dod-record-intake-connector-switch`

Intake **MUST** read whether a connector is on through one interface. Its first version **MUST** take the deployment config: every connector is on unless `connectors_off` lists its identity, a UUID. A value that is not a UUID **MUST** stop the gear from starting, and a UUID in another spelling is the same connector.

**Implements**:
- `cpt-cf-construct-flow-record-intake-submit`

**Verified by**: `a_connector_that_is_off_is_refused` (service), and the config tests `both_keys_are_read`, `a_connector_that_is_not_a_uuid_is_rejected`, `a_connector_in_another_spelling_is_the_same_connector`.

### Hand-off

- [x] `p1` - **ID**: `cpt-cf-construct-dod-record-intake-hand-off`

A received record **MUST** go to processing through one interface that returns at once and can refuse. It **MUST** carry the tenant, the connector identity and the record, and **MUST NOT** carry the caller's security context or scope; processing builds its own for the tenant. Until the Planner exists, the wired-in hand-off **MUST** refuse every record, so intake answers `503` and keeps no identity.

**Implements**:
- `cpt-cf-construct-algo-record-intake-hand-off`

**Verified by**: `without_a_planner_the_hand_off_refuses_every_record` (stub), `without_processing_a_record_is_unavailable_and_leaves_no_identity` (service), `without_processing_a_record_gets_503_and_can_be_sent_again` (routes), and the recording hand-off in every received-record test.

### Drop Event

- [x] `p1` - **ID**: `cpt-cf-construct-dod-record-intake-drop-event`

The system **MUST** define one content-free drop event and write it to the structured log until the Audit gear ships.

**Implements**:
- `cpt-cf-construct-algo-record-intake-drop-event`

**Verified by**: `a_drop_event_names_the_record_and_its_cause`, `the_drop_event_is_logged_without_record_content` (captures the log). Nothing emits it yet; the Planner will.

### Intake Route

- [x] `p1` - **ID**: `cpt-cf-construct-dod-record-intake-route`

The system **MUST** register `POST /construct/v1/records` with OperationBuilder, authenticated, with the `tenant` query parameter, answering `202` received, `200` repeat, `422` refused and `503` when processing cannot take the record, and RFC 9457 problems for every error. Only the route adds the `422` to a refusal. A refusal is logged once, at info level, where it is mapped. The foundation's placeholder route, client operations, service, repository, entity and note model **MUST** be gone.

**Implements**:
- `cpt-cf-construct-flow-record-intake-submit`

**Touches**:
- API: `POST /construct/v1/records` (the DESIGN's `POST /api/construct/v1/records` behind the gateway prefix)

**Verified by**: `a_received_record_gets_202`, `a_repeat_gets_200`, `a_refused_record_gets_422_naming_type_place_and_rule`, `a_record_with_an_id_gets_422`, `a_connector_without_permission_gets_403`, `a_request_without_a_tenant_gets_a_problem_and_takes_nothing`, `a_body_that_is_not_json_gets_a_problem`, `without_processing_a_record_gets_503_and_can_be_sent_again` (routes), `refused_on_the_route_is_422_naming_the_type_the_place_and_the_rule`, `refused_for_an_in_process_caller_carries_no_http_status`, `a_refusal_of_the_whole_record_names_the_record_as_the_place`, `other_errors_map_the_same_on_the_route_and_in_process` (error mapping), `each_outcome_has_its_wire_name` (DTO).

### Client in ClientHub

- [x] `p1` - **ID**: `cpt-cf-construct-dod-record-intake-client`

`ConstructClientV1` **MUST** offer `submit_record`, the same operation as the route, returning `RecordOutcome`, and the gear **MUST** register it in ClientHub. Its errors **MUST** carry no HTTP status.

**Implements**:
- `cpt-cf-construct-flow-record-intake-submit`

**Verified by**: `the_client_trait_is_dyn_compatible` (SDK), `refused_for_an_in_process_caller_carries_no_http_status` (error mapping). The local client is a direct call into the service, which the service tests cover; the registration at startup has no test.

## 6. Acceptance Criteria

- [ ] An invalid payload is refused with the schema violation named, and nothing is half-written (`a_record_that_breaks_its_type_is_refused_with_the_place_and_nothing_is_stored`, `a_refused_record_gets_422_naming_type_place_and_rule`).
- [ ] The same record identity a second time answers repeat and changes nothing (`the_same_identity_again_is_a_repeat_and_changes_nothing`, `a_repeat_gets_200`).
- [ ] A connector without permission for the record's tenant is refused (`a_connector_without_permission_is_refused_before_the_record_is_checked`, `a_connector_authorized_only_for_its_own_tenant_cannot_write_into_another`, `a_connector_without_permission_gets_403`).
- [ ] Records are refused while personalization is off or an erasure is under way, and when the connector is off (`personalization_off_for_the_subject_is_refused`, `an_erasure_under_way_is_refused_even_with_personalization_on`, `a_connector_that_is_off_is_refused`).
- [ ] A record of a registered derived type is taken with no type-specific code: one schema check covers every type (`a_valid_record_of_every_derived_type_passes`, `a_record_without_a_subject_skips_the_subject_checks`).
- [ ] While no processing is wired in, a record is answered `503` and nothing is stored, so it can be sent again (`without_processing_a_record_is_unavailable_and_leaves_no_identity`, `without_processing_a_record_gets_503_and_can_be_sent_again`).
- [ ] The tests named in this document pass. They run on an in-memory SQLite database.

## 7. Non-Functional Considerations

- **Security**: The connector is taken only from the platform. The tenant comes only from the `tenant` query parameter, never from the record, and the platform must authorize the connector for it before anything is read; every read and the insert run under that one decision's scope. A refusal never repeats a value from the record.
- **Data integrity**: The insert is the repeat check, so two copies sent at once cannot both be received. Record content is never written to a store.
- **Reliability**: An unavailable, slow or throttling types registry is a `503`, never a guess. Both the types registry and the settings service are resolved from ClientHub on each request, so their start order does not matter. The identity is kept only once processing took the record, so a refused hand-off or a cancelled request leaves nothing behind. A record lost when an instance stops after it was taken leaves its identity behind; a resend is a repeat, as the DESIGN states.
- **Observability**: The intake span carries the tenant and the connector, and every error is logged once, inside it, where it is mapped. A refusal is logged at info level with its reason code, type and place. A drop is a warn-level event under `construct::audit`, with no record content.
- **Performance**: A type's schema is compiled once and kept for the life of the process.
- **Known risk, outside this feature**: the types registry lets any authenticated caller register a type, with no permission check, so a type that derives from the record base can be added by anyone who can log in. Allow-listing types in Construct would contradict `cpt-cf-construct-fr-record-intake` (a new type needs no Construct change); the authorization belongs in the types registry.
- **Rollback**: The migration's `down` drops `construct__record_ids` and does not bring back the placeholder table.
- **Not done by this feature**: see the out-of-scope list in 1.2.
