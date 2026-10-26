# Feature: Profile Writer


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Shape of a Fact](#shape-of-a-fact)
  - [Origin of a Fact](#origin-of-a-fact)
  - [Cardinality of a Fact](#cardinality-of-a-fact)
  - [Write a Plan](#write-a-plan)
  - [Read a Profile](#read-a-profile)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Person Types](#person-types)
  - [Fact Origin in the Payload](#fact-origin-in-the-payload)
  - [Cardinality Table](#cardinality-table)
  - [Profile Root and Has-Fact Edge](#profile-root-and-has-fact-edge)
  - [Profile Version](#profile-version)
  - [Fact Node from a Plan](#fact-node-from-a-plan)
  - [Type Registration](#type-registration)
  - [One Write per Plan](#one-write-per-plan)
  - [Profile Read](#profile-read)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-profile-writer-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-profile-writer`

## 1. Feature Context

### 1.1 Overview

The Profile Writer (`cpt-cf-construct-component-profile-writer`) stores an admitted plan in graph storage, and the
planner reads the profile back through the same layout. The planner's work started this document with the part it stores:
Construct's person types (`cpt-cf-construct-contract-person-types`), what one stored fact looks like, where its origin
is kept, and how many values of a property one subject may hold. This part adds the write and the read:
the subject's root node, one write per plan with the root as the compare-and-set, the removal of replaced and removed
facts, and the registration of the types.

### 1.2 Purpose

The DESIGN leaves "the properties of the person types" to the feature design. A person has four categories:
identity, role, skill and preference. Each is one GTS graph node type, which the profile writer stores and the
planner, the profile reader and the subject control read.

**In scope**: the four person types, the shape of a fact, the origin of a fact, the cardinality of each property; the
profile's root node and the edge from the root to a fact; the write of an admitted plan and the read of a profile;
the registration of the types.

**The layout.** Each subject has one root node, and every fact hangs from it by one has-fact edge. The root is an
owned node of Construct's type `cf.construct.profile.root.v1`. It is not a reference node: a reference node's
`source.system` claims a source namespace per tenant for the first writer, so a root that stood for the platform user
would claim the account management's namespace, or be refused once account management claims it, and it would gain
nothing over an owned node. The edge is a static edge with no payload, because the fact carries its own origin. The
types are registered with graph storage only: graph storage keeps its own type catalogue per tenant, validates every
node against it, and does not publish to the types registry.

**The write model.** Each plan is one ingest, with the root's expected version as the compare-and-set of the whole
profile, as the DESIGN and ADR-0002 say. Graph storage's ingest adds and updates but deletes nothing, and a delete is
its own call. So a replace or a remove lists the old fact on the root in the same ingest, and the old fact's delete
runs after it. The listing is what commits the removal; the delete only finishes it. Deletes stay soft until graph
storage ships a hard delete (gears-rust #5194); the delete sits behind one interface, so that swap changes nothing
else. Scope replacement, which would hard-delete in the same ingest, was set aside: replacing a
subject's whole fact set to delete one fact is a workaround, not a delete.

**Out of scope**: how the planner shows facts to the model; the rerun of the planner after a conflict, which the
Planner's processing does; wiring the writer and the read into the gear and building processing's own security
context per tenant; the root of a subject after an erasure, which needs a new root key because a deleted key cannot
be reused (Erasure, `cpt-cf-construct-feature-erasure`); hard delete (gears-rust #5194).

**Requirements**: `cpt-cf-construct-fr-fact-decisions`, `cpt-cf-construct-fr-fact-origin`

**Sequences**: `cpt-cf-construct-seq-record-in` (the write of the admitted plan), `cpt-cf-construct-seq-entity-replaced`

**Principles**: `cpt-cf-construct-principle-model-never-sees-ids`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-graph-storage` | Stores each fact as a node of a person type and validates it against the type's whole chain. |
| `cpt-cf-construct-actor-connector` | The usual source of a fact. Its record becomes the fact's origin. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), §3.1 Domain Model
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.8
- **Graph storage**: the owned node family and the provenance attribute type in graph storage's base ontology
- **Dependencies**: `cpt-cf-construct-feature-gear-foundation`

## 2. Actor Flows (CDSL)

**Use cases**: None. The writer and the read are internal: processing calls them in process, through two ports
defined beside the profile, `PlanWriter` and `ProfileSource` (`domain::profile`). A write in a tenant other than the
security context's is refused.

## 3. Processes / Business Logic (CDSL)

### Shape of a Fact

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-profile-writer-fact-shape`

1. [ ] - `p1` - A fact is one node of the person type of its category, derived from graph storage's owned node:
   `gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.<category>.v1~` - `inst-fact-node`
2. [ ] - `p1` - The node's `payload` holds one fact property and `origin`, and `record` when the fact came from a
   record; nothing else. The fact property names the fact. It is the plain key the model writes
   (`cpt-cf-construct-principle-model-never-sees-ids`) - `inst-fact-property`
3. [ ] - `p1` - A property whose value is an object (an entry such as a job or a skill) carries the fact's fields. A
   plan step adds, replaces or removes the whole value: a change to one field is a replace of the fact -
   `inst-fact-fields`
4. [ ] - `p1` - The payload is closed: an unknown property is refused, and a new property is a backward-compatible
   change of the type - `inst-fact-closed`

### Origin of a Fact

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-profile-writer-fact-origin`

1. [ ] - `p1` - `origin` is graph storage's provenance attribute
   (`gts.cf.core.graph.attribute.v1~cf.core.graph.provenance.v1~`) - `inst-origin-type`
2. [ ] - `p1` - The provenance attribute is closed and has no member for a record, so a fact that came from a record
   carries `record` beside `origin`: the connector, and the record's identity (`provenance`) and `version`. Together
   they name "the connector and record it came from" (`cpt-cf-construct-fr-fact-origin`) - `inst-origin-record`
3. [ ] - `p1` - What each source puts in the origin:

   | Source | `produced_by` | `method` | `model` | `record` |
   |---|---|---|---|---|
   | A record | the connector's subject | the record type | — | the connector, `provenance`, `version` |
   | An MCP call | the subject who said it | `mcp.manage_facts` | the agent that called | — |
   | A reviewer's correction | the reviewer | `review.correction` | — | — |
   | A tenant administrator's fact | the administrator | `admin.edit` | — | — |

   For an MCP call this names both parties the DESIGN asks for: the subject who said it and the agent. The subject
   is also the root of the profile the fact hangs from - `inst-origin-sources`
4. [ ] - `p1` - `produced_at` is when the source asserted the fact: the record's observation time, or the time of the
   call or the action. `confidence` is the plan step's confidence, when there is one - `inst-origin-produced-at`
5. [ ] - `p1` - The storing time is graph storage's element timestamp. It is not copied into the payload -
   `inst-origin-storing-time`
6. [ ] - `p1` - The planner does not show `origin` or `record` to the model: they hold identifiers -
   `inst-origin-hidden`

### Cardinality of a Fact

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-profile-writer-fact-cardinality`

1. [ ] - `p1` - A fact holds one value. Several values of a property are several facts, each its own node; a list inside
   one value would need its own type and a reason, and no property has one. A day-based window, such as
   `focus_windows`, is one fact per day - `inst-cardinality-one-value`
2. [ ] - `p1` - Each fact property is `One` or `Many`. `One`: a subject holds at most one value, such as `full_name`.
   `Many`: a subject may hold several, such as `skills`, or a preference a person can have more than one of, such as
   `notification_type` - `inst-cardinality-kinds`
3. [ ] - `p1` - The table lives in the SDK next to the schemas, because graph storage refuses unknown `x-` keywords in
   a type - `inst-cardinality-place`
4. [ ] - `p1` - The planner's tools refuse to add a second value of a `One` property and tell the model to replace
   it. A one-step plan without the planner turns such an add into a replace. Admission does not check it: its
   checks are the ones the DESIGN lists - `inst-cardinality-use`

### Write a Plan

- [x] `p1` - **ID**: `cpt-cf-construct-algo-profile-writer-write`

**Input**: Processing's security context for the tenant, and an admitted plan with the profile version it was built on.

**Output**: Stored, or a conflict when the profile changed after the plan was built on it. Or an error: the profile is full, graph storage is unavailable or does not answer in time, or something is defective: the plan, the stored root, or the deployment when graph storage refuses Construct's own permission.

**Steps**:
1. [x] - `p1` - A plan without steps stores nothing and answers stored. A plan for another tenant than the security context's is refused. Otherwise make sure Construct's graph types exist in the tenant: register the root, the edge and the person types the first time this process writes there, and remember the tenant only when the registration succeeded. Registering the same schemas again converges, so a restart or a second instance is safe - `inst-write-register`
2. [x] - `p1` - Read the profile as Read a Profile does: the root and every live fact one has-fact edge from it, also the facts the root lists for removal whose delete has not run yet. **IF** the root's version, none for a subject without a root, is not the plan's, **RETURN** conflict: the profile changed after the plan was built, logged at debug - `inst-write-read-root`
3. [x] - `p1` - Of the removals the root still lists, keep only those that are live facts of this root. A listed fact that is gone was removed and drops off the list; a listed key that is not a fact of this root is never deleted, so a root someone else rewrote cannot make Construct delete another node - `inst-write-pending`
4. [x] - `p1` - Build one ingest: a node and a has-fact edge from the root for each added fact and each replacing fact, and the root, listing the kept removals and the facts this plan replaces or removes. **IF** the plan removes a key that is not a live fact of the profile, the plan is defective. **IF** the live facts, those still listed included, plus the new ones would pass the limit (999 facts, what one read takes), **RETURN** profile full: a full profile takes removals, and new facts once its deletes have run - `inst-write-batch`
5. [x] - `p1` - A fact node is its category's person type, with the one property, its `origin` and, for a record's fact, its `record`. It claims its new key with `expected_version = 0`, so a key in use is a conflict, never an overwrite. A step that changes one property of a node rather than a whole fact, or a fact without exactly one property, is a defect; the message names the kind of change, never a value - `inst-write-fact`
6. [x] - `p1` - The origin names the producer, the method, the model for an agent, `produced_at` in UTC (the record's `observed_at`, or the time of the write for a person or an agent) and the step's confidence as the decimal the model gave, as the table in Origin of a Fact says - `inst-write-origin`
7. [x] - `p1` - The root's expected version is its version, or 0 for a subject without one: 0 claims a key no node holds, so two first writes cannot both create the root. A stored version beyond graph storage's range is a defect of the root, not a conflict - `inst-write-expected`
8. [x] - `p1` - Send the ingest with a new idempotency key, waiting at most 10 seconds by default. **IF** graph storage answers `CAS_CONFLICT`, nothing was stored; read the root again. Every write of Construct's moves the counter: **IF** the counter moved, another write landed in between, **RETURN** conflict, logged at warn with the expected and the stored version. **IF** it did not, it is a defect, internal and logged at error: the root was written outside Construct or its key is held by a removed node, so every later write is refused too, or a key a new fact claims is already held. A send without an answer keeps running in graph storage, so a resend can lose the compare-and-set to it: once a send's result was unknown, a refused compare-and-set is sent once more with the same key, and when the earlier send committed its receipt answers stored. **IF** the result is unknown (no answer in time, unavailable, deadline exceeded, or a lost serialization race), send the same request with the same key again after a pause that doubles each time, plus a random share of it, logged at warn, up to 3 sends in all; after the last one **RETURN** unavailable. Any other refusal is an error, an unknown error included: it says nothing about the write - `inst-write-ingest`
9. [x] - `p1` - Once stored, remove the facts the root now lists, one after another, within one call timeout in all. The first removal that fails, or the timeout, ends the cleanup: that fact and the rest stay listed and are logged at warn; reads already leave them out, and a later write finishes them. **RETURN** stored - `inst-write-cleanup`
10. [x] - `p1` - A removal is graph storage's soft delete of the fact's node, which also tombstones its edge, waiting at most 10 seconds by default. A fact already tombstoned counts as removed; a key graph storage does not hold is an error naming the key, and stays listed - `inst-write-remove`

### Read a Profile

- [x] `p1` - **ID**: `cpt-cf-construct-algo-profile-writer-read`

**Input**: Processing's security context for the tenant, the tenant and the subject.

**Output**: The profile: the root's version, none for a subject without a profile, and its facts. Or an error.

**Steps**:
1. [x] - `p1` - A tenant other than the security context's is refused. Traverse one has-fact edge from the subject's root, taking at most 1000 nodes and waiting at most 10 seconds by default; no answer in time is unavailable - `inst-read-traverse`
2. [x] - `p1` - **IF** there is no root, **RETURN** no version and no facts. **IF** the traversal was cut short by its node budget, **RETURN** profile full, because a partial profile would let the planner add what is already stored; cut short for any other reason, **RETURN** internal: it is a limit of graph storage's, not a full profile. **IF** the root's key is held by a node of another type, or the root names another subject, it is a defect, internal. Otherwise read the version and the removal list from the root - `inst-read-root`
3. [x] - `p1` - **RETURN** every node of a person type that a has-fact edge from this root points to and that the root does not list for removal, as its category, its property and its value. The traversal walks edges both ways and matches derived types, so a node only linked into the root, or reached by another edge type, is no fact. A node a has-fact edge from the root points to that is of no person type, or that holds not exactly one fact property besides its origin and record, is a defect, internal and named by its key - `inst-read-facts`

## 4. States (CDSL)

None. A fact is created, replaced or removed by a plan; it has no state of its own.

## 5. Definitions of Done

### Person Types

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-person-types`

The SDK **MUST** ship the four person types as JSON schemas and **MUST** expose them with their ids for registration.
It **MUST** give the schema of each property on its own, with the shapes the type defines once put in place.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-fact-shape`

**Touches**:
- Contract: `cpt-cf-construct-contract-person-types`
- Code: `construct_sdk::person_types::{PERSON_TYPES, property_schema}`, `gears/construct/construct-sdk/schemas/`

**Verified by**: the SDK unit tests `each_schema_names_its_type_and_derives_from_the_owned_node`,
`a_whole_node_of_each_type_fits_its_chain_with_a_real_origin`, `a_payload_holds_exactly_one_fact_and_its_origin`,
`each_constrained_value_fits_its_form_and_nothing_else`, `every_text_value_has_a_length_limit_it_enforces`,
`every_number_has_bounds_it_enforces` and `every_property_schema_stands_alone`, and `gts-validator`.

### Fact Origin in the Payload

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-fact-origin`

Every person type **MUST** require `origin` as graph storage's provenance attribute, and **MUST** take `record` beside
it for a fact that came from a record.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-fact-origin`

**Touches**:
- Requirement: `cpt-cf-construct-fr-fact-origin`
- Code: the `origin` and `record` members of the schemas in `gears/construct/construct-sdk/schemas/`

**Verified by**: the SDK unit tests `the_origin_is_required_and_is_graph_storage_provenance`,
`a_fact_from_a_record_names_the_record_beside_its_origin` and `every_type_takes_the_same_record`.

### Cardinality Table

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-cardinality`

The SDK **MUST** state the cardinality of every fact property of every person type, and of nothing else.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-fact-cardinality`

**Touches**:
- Code: `construct_sdk::person_types::{Cardinality, FactProperty, PROPERTIES}`

**Verified by**: the SDK unit tests `every_fact_property_has_a_cardinality_and_only_those` and
`each_property_holds_one_value_unless_a_person_can_have_several`.

### Profile Root and Has-Fact Edge

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-root`

The SDK **MUST** ship the root node type, an owned node whose closed payload holds the subject, the version (from 1)
and the removal list, and the has-fact edge type, a static edge with no payload. It **MUST** list them for
registration before the person types.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-write`

**Touches**:
- Code: `construct_sdk::profile_types::{PROFILE_ROOT, HAS_FACT, graph_types}`, `construct_sdk::gts::{PROFILE_ROOT_TYPE, HAS_FACT_TYPE}`

**Verified by**: the SDK unit tests `each_type_names_itself_and_derives_from_its_family`,
`a_root_with_its_subject_version_and_removals_fits`, `a_root_refuses_what_it_does_not_declare` and
`the_root_and_the_edge_register_before_the_person_types`.

### Profile Version

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-version`

The profile version **MUST** sit behind one interface that reads it from a stored root, gives the root write's expected version and the next version, and stamps it on the root being written, opaque to the planner. Graph storage returns no node version on read, so the first implementation **MUST** keep a counter in the root's payload: the root is created at 1 and every write adds one, so every root write changes the node and the counter equals graph storage's stored version. Construct is the root's only writer. The rest of the root's payload **MUST NOT** depend on the counter, so an implementation that reads graph storage's version **MAY** replace it with a root type that drops the counter, without changing the writer or the read. A version the counter cannot carry into graph storage's range **MUST** be an error, not a conflict.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-write`

**Touches**:
- Code: `infra::storage::graph::root::{ProfileVersion, PayloadCounter, Root}`

**Verified by**: `a_new_subject_creates_its_root_at_version_one_and_every_write_adds_one`, `a_version_beyond_graph_storages_range_is_a_defect_not_a_conflict`, `the_counter_reads_the_version_a_root_carries`, `a_root_without_a_readable_version_is_a_defect`, `a_root_payload_reads_back_as_written_with_the_counter_stamped`, `a_root_without_a_valid_payload_names_the_root_and_what_is_wrong` (unit), and the version read back in `a_first_plan_creates_the_root_and_reads_back_whole` and `a_replace_stores_the_new_fact_and_removes_the_old_in_one_write`.

### Fact Node from a Plan

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-fact-payload`

The writer **MUST** store each new fact as a node of its category's person type, with the one property, the origin of the table in Origin of a Fact, the step's confidence as the decimal the model gave, and the record for a record's fact, and **MUST** claim the node's new key. It **MUST** take the key and the property from the step's own change, and **MUST** refuse a change of one property of a node without naming its value.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-write`
- `cpt-cf-construct-algo-profile-writer-fact-origin`

**Touches**:
- Code: `infra::storage::graph::fact::{fact_node, fact_of}`

**Verified by**: `a_records_fact_names_the_connector_its_record_and_when_it_was_observed`, `a_confidence_is_stored_as_the_decimal_the_model_gave`, `a_confidence_that_is_no_finite_number_is_a_defect`, `an_agents_fact_names_the_subject_and_the_agent_as_model`, `a_persons_fact_names_the_person_and_a_step_without_confidence_stores_none`, `a_removal_stores_no_node`, `a_step_of_no_known_category_is_a_defect`, `a_fact_must_hold_exactly_one_property`, `a_property_change_is_refused_without_its_value_in_the_message`, `a_stored_fact_reads_back_as_its_property_and_value`, `a_fact_node_that_holds_no_single_fact_is_a_defect_named_by_its_key`, `a_records_time_is_stored_in_utc`, `a_confidence_reads_back_as_the_value_given` and `a_stored_fact_reads_back_as_written` (property tests) (unit), and graph storage's validation of every written node in the integration tests, including `a_value_its_type_refuses_stores_nothing`.

### Type Registration

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-registration`

Construct's graph types **MUST** be registered with graph storage in each tenant before the first write there, cached per process; a registration that failed **MUST NOT** be remembered. A restart against a tenant that has them **MUST** converge rather than conflict. Registration needs the `admin` action on graph storage's type resource, so the deployment grants it to Construct's processing identity.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-write`

**Touches**:
- Code: `infra::storage::graph::types::GraphTypes`

**Verified by**: `the_types_register_again_after_a_restart_without_conflict`, `registration_is_remembered_per_tenant_and_a_failed_one_is_not`, and the first write of every integration test, which registers in a new tenant.

### One Write per Plan

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-write`

The writer **MUST** store an admitted plan in one ingest that lands whole or not at all, with the root's expected version as the compare-and-set, so two plans on one profile never both land, and **MUST** report a root whose version no longer matches its counter as a defect rather than a conflict. A replaced or removed fact **MUST** be listed on the root in that ingest and **MUST** be removed after it; a removal that fails **MUST** stay listed until a later write finishes it, and only a live fact of the root **MUST** ever be removed. The writer **MUST NOT** let a profile grow past what one read takes. Every graph storage call **MUST** be bounded in time, and a write whose result is unknown **MUST** be sent again with the same idempotency key, a bounded number of times.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-write`

**Touches**:
- Sequences: `cpt-cf-construct-seq-record-in`, `cpt-cf-construct-seq-entity-replaced`
- Code: `infra::storage::graph::writer::{GraphPlanWriter, FactRemoval, SoftDelete}`, the `domain::profile::PlanWriter` port

**Verified by**: `a_first_plan_creates_the_root_and_reads_back_whole`, `a_replace_stores_the_new_fact_and_removes_the_old_in_one_write`, `a_removal_that_is_done_drops_off_the_root_on_the_next_write`, `a_removed_fact_is_gone_from_the_next_read`, `a_plan_built_on_an_older_profile_conflicts_and_stores_nothing`, `two_plans_on_the_same_profile_never_both_land`, `two_first_writes_create_the_root_once` (both hold the two writes until both reach the ingest), `a_root_written_outside_construct_refuses_every_write_as_a_defect`, `a_new_fact_whose_key_is_held_is_a_defect_and_stores_nothing`, `a_refusal_after_a_send_without_an_answer_replays_the_committed_send`, `a_refusal_is_replayed_once_at_most`, `a_refusal_without_an_unknown_send_is_not_replayed`, `a_write_is_sent_three_times_at_most_and_each_call_waits_ten_seconds`, `a_removal_that_fails_stays_listed_and_a_later_write_finishes_it`, `the_cleanup_stops_at_the_first_failed_removal_and_every_key_stays_listed`, `a_cleanup_that_runs_out_of_time_leaves_every_key_listed_and_the_write_stored`, `a_soft_delete_of_a_fact_already_removed_is_done`, `a_soft_delete_of_a_fact_graph_storage_does_not_hold_fails_and_names_it`, `a_listed_key_that_is_not_a_fact_of_the_root_is_never_deleted`, `a_plan_that_removes_a_fact_not_in_its_profile_is_refused`, `a_full_profile_takes_no_new_fact_but_takes_a_removal`, `a_profile_at_the_limit_takes_no_new_fact`, `a_writer_refuses_a_limit_past_what_one_read_takes`, `a_write_whose_answer_was_lost_is_sent_again_with_its_key_and_lands_once`, `a_lost_serialization_race_is_sent_again`, `a_write_that_stays_unavailable_gives_up_after_its_attempts`, `a_write_that_never_gets_an_answer_gives_up_after_its_attempts_as_unavailable`, `the_pause_between_sends_grows_and_carries_up_to_as_much_again`, `the_pause_carries_a_random_share_of_up_to_all_of_it`, `a_writer_given_one_attempt_sends_once`, `a_send_without_an_answer_in_time_is_sent_again`, `only_a_write_that_is_safe_to_send_again_is_resent`, `graph_storage_failures_map_to_what_a_caller_can_act_on`, `a_value_its_type_refuses_stores_nothing`, `a_property_change_is_refused_and_stores_nothing`, `an_empty_plan_writes_nothing` and `a_profile_of_another_tenant_than_the_context_is_refused`. They run against graph storage's own service over its in-memory store, with a stub policy service; Postgres and the real AuthZ resolver were not run.

### Profile Read

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-read`

The profile read **MUST** return the root's version and every fact one has-fact edge from the root, in one call bounded in time, **MUST** leave out a fact the root lists for removal, **MUST** read as facts only the nodes a has-fact edge from the root points to, **MUST** answer profile full rather than return a profile cut by the node budget, and **MUST** refuse a tenant other than the security context's.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-read`

**Touches**:
- Code: `infra::storage::graph::reader::GraphProfileSource`, the `domain::profile::ProfileSource` port

**Verified by**: `a_subject_without_a_profile_reads_empty`, `a_first_plan_creates_the_root_and_reads_back_whole`, `a_profile_read_asks_for_the_root_and_at_most_its_facts`, `a_profile_cut_short_by_the_read_limit_is_full_not_partial`, `a_profile_past_what_one_read_takes_is_full`, `a_profile_read_cut_short_for_another_reason_is_internal`, `a_node_with_an_edge_into_the_root_is_not_read_as_a_fact`, `a_node_reached_by_an_edge_of_another_type_is_not_read_as_a_fact`, `a_has_fact_edge_to_a_node_of_no_person_type_is_a_defect`, `a_root_key_held_by_another_type_is_a_defect`, `a_root_that_names_another_subject_is_a_defect` (unit), `a_read_without_an_answer_in_time_is_unavailable` (with the default bound), `a_profile_of_another_tenant_than_the_context_is_refused`, and the listed fact left out in `a_removal_that_fails_stays_listed_and_a_later_write_finishes_it`.

## 6. Acceptance Criteria

- [ ] Graph storage accepts the four person types, and validates a node of each type against the whole chain,
      origin included.
- [ ] A node whose payload has no origin, two facts, an unknown property, or an incomplete record is refused.
- [ ] Every fact property has exactly one cardinality.
- [ ] Construct's types are registered before the first write in each tenant, idempotently: a restart against an
      already-registered tenant converges rather than conflicting
      (`the_types_register_again_after_a_restart_without_conflict`).
- [ ] A plan lands whole or not at all (`a_value_its_type_refuses_stores_nothing`,
      `a_replace_stores_the_new_fact_and_removes_the_old_in_one_write`).
- [ ] Two records for one subject at the same moment never mix: the second write conflicts and is planned again
      (`two_plans_on_the_same_profile_never_both_land`, `two_first_writes_create_the_root_once`,
      `a_plan_built_on_an_older_profile_conflicts_and_stores_nothing`), including a retried write
      (`a_write_whose_answer_was_lost_is_sent_again_with_its_key_and_lands_once`).
- [ ] Only a live fact of the subject's root is ever deleted (`a_listed_key_that_is_not_a_fact_of_the_root_is_never_deleted`).
- [ ] A profile never grows past what one read takes (`a_full_profile_takes_no_new_fact_but_takes_a_removal`).

## 7. Non-Functional Considerations

- **Compatibility**: a closed payload lets a new property be added without a new major version.
- **Privacy**: no stored value is copied into the schemas. The schemas carry only property names and
  constraints. No log line of the writer or the read holds a value.
- **Deletion**: removals are soft deletes until gears-rust #5194 ships a hard delete. A soft-deleted fact keeps its
  payload and embedding as a tombstone, and an erasure by scope replacement skips tombstones. When #5194 ships, the
  facts soft-deleted before it need a one-off hard delete; whether #5194 can hard-delete an already tombstoned node is
  open with graph storage.
- **Reliability**: a write is one transaction in graph storage. Every graph storage call waits at most 10 seconds by default, because graph storage cannot cut off a statement it has started. A write whose result is unknown is sent again with its idempotency key, which replays the recorded result, after a doubling pause with a random share added, so writers that failed together do not retry together; after 3 sends it is unavailable and processing drops the record. Graph storage's embedding is on by default, so an embedding provider that fails fails the write.
- **Errors**: a refusal of Construct's own permission by graph storage is internal, logged at error: the deployment's grant is missing, the caller was not denied. A failure a retry may cure is unavailable. A full profile has its own error, a failed precondition (`PROFILE_FULL`, logged at warn with the tenant, the subject, the fact count and the limit as fields) on the API, because it is the subject's state, not the gear's failure. A root whose version no longer matches its counter is internal, logged at error. Everything else is internal. No error message holds a fact value.
- **Limits**: one read takes at most 1000 nodes, graph storage's default traversal ceiling, so a profile holds at most 999 facts. The writer counts a fact listed for removal until its delete has run, so a profile never passes the limit even while deletes fail; at the limit it takes plans that remove, and new facts once the deletes have run. A profile cut short by the read's node budget fails as full rather than being returned partially.
