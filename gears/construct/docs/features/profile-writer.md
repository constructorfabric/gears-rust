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
  - [Owner and Number of a Node](#owner-and-number-of-a-node)
  - [Write a Plan](#write-a-plan)
  - [Read a Profile](#read-a-profile)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Person Types](#person-types)
  - [Fact Origin in the Payload](#fact-origin-in-the-payload)
  - [Owner Table](#owner-table)
  - [Profile Root and Has-Fact Edge](#profile-root-and-has-fact-edge)
  - [Profile Version](#profile-version)
  - [Fact Node from a Plan](#fact-node-from-a-plan)
  - [Type Registration](#type-registration)
  - [One Write per Plan](#one-write-per-plan)
  - [Profile Read](#profile-read)
- [6. Acceptance Criteria](#6-acceptance-criteria)
  - [Schema at Run Time](#schema-at-run-time)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-profile-writer-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-profile-writer`

## 1. Feature Context

### 1.1 Overview

The Profile Writer (`cpt-cf-construct-component-profile-writer`) stores an admitted plan in graph storage, and the
planner reads the profile back through the same layout. The planner's work started this document with the part it stores:
Construct's profile schema (`cpt-cf-construct-contract-person-types`): the node kinds and link kinds a person's profile
holds, what one stored node looks like, where its origin and the places it was learned from are kept, who owns it, and how
many nodes of a kind one subject may hold. This part adds the write and the read:
the subject's root node, one write per plan with the root as the compare-and-set, the removal of replaced and removed
facts, and the registration of the types, per person.

### 1.2 Purpose

The DESIGN leaves "the properties of the person types" to the feature design. A person's profile is a graph of nodes
and links, defined by a schema of node kinds and link kinds. The default schema holds the things a person works with
(`person`, `project`, `product`, `activity`, `team`, `customer`, `partner`, `competitor`), the entries that hang on
them (`commitment`, `decision`, `question`, `metric`), the reference nodes (`ticket`, `source`), the person's own
direct properties (`me`), and the links between them. Each node kind and each link kind is one GTS graph type, which
the profile writer stores and the planner, the profile reader and the subject control read. The kinds are named by
their registered names, which the model, the REST API and the permission policy use as the category.

**In scope**: the node and link kinds, the shape of a fact (a node), the origin of a fact, the owner and the number of
nodes of each kind; the
profile's root node and the edge from the root to a fact; the write of an admitted plan and the read of a profile;
the registration of the types.

**The layout.** Each subject has one root node, and every fact hangs from it by one has-fact edge. The root is an
owned node of Construct's type `gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.profile.root.v1~`. It is not a reference node: a reference node's
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
context per tenant; the profile generation that gives the root of a subject after an erasure a new key, because a
deleted key cannot be reused, which Erasure keeps (`cpt-cf-construct-algo-erasure-root-key`); hard delete (gears-rust
#5194).

**Requirements**: `cpt-cf-construct-fr-fact-decisions`, `cpt-cf-construct-fr-fact-origin`

**Sequences**: `cpt-cf-construct-seq-record-in` (the write of the admitted plan), `cpt-cf-construct-seq-entity-replaced`

**Principles**: `cpt-cf-construct-principle-model-never-sees-ids`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-graph-storage` | Stores each fact as a node of a node kind's type, and each link as an edge of a link kind's type, and validates them against the type's whole chain. |
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

1. [ ] - `p1` - A fact is one node of the graph type of its kind, derived from graph storage's owned node:
   `gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.u<person>.<kind>.v1~`. The type id carries a
   per-person prefix, `u` and the person's subject id as 32 hex digits, so two people's kinds never collide in one
   tenant - `inst-fact-node`
2. [ ] - `p1` - The node's `payload` holds one property, named like the kind, whose value is the object of the node's
   fields, and `origin`; `learned`, the tool and the message of each write that touched it, newest last, at most 50;
   `record` when the node came from a record; and `owner`, the key of the node that owns it, for an entry kind;
   nothing else. The kind name is the plain key the model writes
   (`cpt-cf-construct-principle-model-never-sees-ids`) - `inst-fact-property`
3. [ ] - `p1` - A plan step adds, replaces or removes the whole value: a change to one field is a replace of the node.
   A replace changes the node where it stands, under its key, so its links and the nodes it owns stay -
   `inst-fact-fields`
4. [ ] - `p1` - The payload is closed: an unknown property is refused, and a new property is a backward-compatible
   change of the type - `inst-fact-closed`
5. [ ] - `p1` - A link is one edge of the graph type of its link kind, a static edge whose payload holds `fields`,
   `origin` and `record`. The type names the kinds at its ends. Graph storage deletes an edge with either of its nodes -
   `inst-fact-link`

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

### Owner and Number of a Node

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-profile-writer-fact-cardinality`

1. [ ] - `p1` - Every node is owned by the profile root, which is the person, or by a node: an entry (`commitment`,
   `decision`, `question`, `metric`) is owned by a thing. Forgetting a node forgets the nodes it owns, however deep -
   `inst-cardinality-one-value`
2. [ ] - `p1` - Two nodes of one kind and one owner with the same value of the kind's key property (`name`, `text`,
   `label` or `id`), compared without case and spacing, are the same node. The kind `me`, the person's own name, age,
   profession and description, holds one node at most - `inst-cardinality-kinds`
3. [ ] - `p1` - The kinds, their owners, their key property and whether they are reference nodes live in the SDK next
   to the schemas, because graph storage refuses unknown `x-` keywords in a type - `inst-cardinality-place`
4. [ ] - `p1` - The planner's tools refuse to add a second node of one name and tell the model the number of the node
   that exists, so it links to it. A one-step plan without the planner adds only a kind the root owns, and turns a second
   `me` into a refusal naming the held one. Admission does not check it: its checks are the ones the DESIGN lists -
   `inst-cardinality-use`

### Write a Plan

- [x] `p1` - **ID**: `cpt-cf-construct-algo-profile-writer-write`

**Input**: Processing's security context for the tenant, an admitted plan with the profile version it was built on, and a deadline: the caller's, or 90 seconds from the start for a write no caller waits on. No call to graph storage starts once the deadline passed, a call still running then is cut, and no resend starts when its pause would pass it.

**Output**: Stored, or a conflict when the profile changed after the plan was built on it. Or an error: the profile is full, graph storage is unavailable or does not answer in time, the deadline passed (unavailable when it passed before the ingest was sent: nothing was written; result unknown when it passed after), or something is defective: the plan, the stored root, or the deployment when graph storage refuses Construct's own permission.

**Steps**:
1. [x] - `p1` - A plan without steps stores nothing and answers stored. A plan for another tenant than the security context's is refused. Otherwise make sure Construct's graph types exist in the tenant: register the root, the edge, and the node and link kinds of the person written for, the first time this process writes there for that person, and remember the tenant only when the registration succeeded. Registering the same schemas again converges, so a restart or a second instance is safe; two first registrations in a tenant race, and the one refused as a conflict is sent once more - `inst-write-register`
2. [x] - `p1` - Read the profile as Read a Profile does: the root and every live fact one has-fact edge from it, also the facts the root lists for removal whose delete has not run yet. **IF** the root's version, none for a subject without a root, is not the plan's, **RETURN** conflict: the profile changed after the plan was built, logged at debug - `inst-write-read-root`
3. [x] - `p1` - Of the removals the root still lists, keep only those that are live facts of this root. A listed fact that is gone was removed and drops off the list; a listed key that is not a fact of this root is never deleted, so a root someone else rewrote cannot make Construct delete another node - `inst-write-pending`
4. [x] - `p1` - Build one ingest: a node and a has-fact edge from the root for each added fact and each replacing fact, and the root, listing the kept removals and the facts this plan replaces or removes. **IF** the plan removes a key that is not a live fact of the profile, the plan is defective. **IF** the live facts, those still listed included, plus the new ones would pass the limit (999 facts, what one read takes), **RETURN** profile full: a full profile takes removals, and new facts once its deletes have run - `inst-write-batch`
5. [x] - `p1` - A fact node is its category's type for the person, with the one property, its `origin`, the places it was learned from, its owner for an entry kind and, for a record's fact, its `record`. A replace of a node under its own key changes the node where it stands and adds this write's place to the ones it holds; a link is an edge between the keys the step names, with its fields, origin and record; removing a node removes the nodes it owns and, as graph storage deletes a node with its edges, its links; removing a link deletes its edge after the write has committed; a person's own forget leaves a mark on the root for each message the node was learned from, so the same message does not create it again. It claims its new key with `expected_version = 0`, so a key in use is a conflict, never an overwrite. A step that changes one property of a node rather than a whole fact, a fact without exactly one property, or a property named `origin` or `record`, which the envelope would overwrite, is a defect; the message names the kind of change or the name, never a value - `inst-write-fact`
6. [x] - `p1` - The origin names the producer, the method, the model for an agent, `produced_at` in UTC (the record's `observed_at`, or the time of the write for a person or an agent) and the step's confidence as the decimal the model gave, as the table in Origin of a Fact says - `inst-write-origin`
7. [x] - `p1` - The root's expected version is its version, or 0 for a subject without one: 0 claims a key no node holds, so two first writes cannot both create the root. A stored version beyond graph storage's range is a defect of the root, not a conflict. Every write first notes the tenant for retention (`cpt-cf-construct-feature-retention`), since a profile may be the tenant's only data; the store keeps the tenants it noted, so that is one insert per tenant and process - `inst-write-expected`
8. [x] - `p1` - Send the ingest with a new idempotency key, waiting at most 10 seconds by default. **IF** graph storage answers `CAS_CONFLICT`, nothing was stored; read the root again. Every write of Construct's moves the counter: **IF** the counter moved, another write landed in between, **RETURN** conflict, logged at warn with the expected and the stored version. **IF** it did not, it is a defect, internal and logged at error: the root was written outside Construct or its key is held by a removed node, so every later write is refused too, or a key a new fact claims is already held. A send without an answer keeps running in graph storage, so a resend can lose the compare-and-set to it: once a send's result was unknown, a refused compare-and-set is sent once more with the same key, and when the earlier send committed its receipt answers stored. **IF** the result is unknown (no answer in time, unavailable, deadline exceeded, or a lost serialization race), send the same request with the same key again after a pause that doubles each time, plus a random share of it, logged at warn, up to 3 sends in all, the send that checks a refusal included; after the last one, or a refusal after an unknown result with no send left to check it, **RETURN** result unknown. A send throttled after an unknown result is result unknown too. Result unknown answers `503` with a detail that says the write may have been stored; every other unavailable write stored nothing. Any other refusal is an error, an unknown error included: it says nothing about the write - `inst-write-ingest`
9. [x] - `p1` - Once stored, remove the facts the root now lists, one after another, within one call timeout in all. The first removal that fails, or the timeout or the deadline, ends the cleanup: that fact and the rest stay listed, logged at warn with how many are left and which limit cut the cleanup, or at error when the removal failed as a defect, such as a missing grant, which no later write cures; reads already leave them out, and a later write finishes them. **RETURN** stored - `inst-write-cleanup`
10. [x] - `p1` - A removal is graph storage's soft delete of the fact's node, which also tombstones its edge, waiting at most 10 seconds by default. A fact already tombstoned counts as removed; a key graph storage does not hold is an error naming the key, and stays listed - `inst-write-remove`
11. [x] - `p1` - **IF** the batch would pass the limit while removals are still listed, run those removals as the cleanup does, read the profile again and build the batch once more; a changed version is a conflict. The listed facts count until their delete has run, and only a stored write runs it, so failed deletes would otherwise keep the profile full - `inst-write-full`

### Read a Profile

- [x] `p1` - **ID**: `cpt-cf-construct-algo-profile-writer-read`

**Input**: Processing's security context for the tenant, the tenant and the subject.

**Output**: The profile: the root's version, none for a subject without a profile, and its facts. Or an error.

**Steps**:
1. [x] - `p1` - A tenant other than the security context's is refused. Traverse one has-fact edge from the subject's root in its current generation (`cpt-cf-construct-algo-erasure-root-key`), taking at most 1000 nodes and waiting at most 10 seconds by default; no answer in time is unavailable. A read that graph storage marks as seeing more than one graph state (a write landed while it ran, on a store without snapshots) is sent again, three times in all, then unavailable: its root could carry a newer version than its facts. Graph storage refusing the 1000 nodes is internal and names its `traversal_max_nodes` setting, which the deployment has set too low - `inst-read-traverse`
2. [x] - `p1` - **IF** there is no root, **RETURN** no version and no facts. **IF** the traversal was cut short by its node budget, **RETURN** profile full, because a partial profile would let the planner add what is already stored; cut short for any other reason, **RETURN** internal: it is a limit of graph storage's, not a full profile. **IF** the root's key is held by a node of another type, or the root names another subject, it is a defect, internal. Otherwise read the version and the removal list from the root - `inst-read-root`
3. [x] - `p1` - **RETURN** every node of a node kind's type that a has-fact edge from this root points to and that the root does not list for removal, as its category, its property and its value. The traversal walks edges both ways and matches derived types, so a node only linked into the root, or reached by another edge type, is no fact. The links between those nodes are read in the same traversal, one hop further. A node a has-fact edge from the root points to that is of no node kind, or that holds not exactly one fact property besides its origin and record, is a defect, internal and named by its key - `inst-read-facts`

## 4. States (CDSL)

None. A fact is created, replaced or removed by a plan; it has no state of its own.

## 5. Definitions of Done

### Person Types

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-person-types`

The SDK **MUST** ship the default schema, its node kinds and link kinds, and **MUST** build the graph type of each for a
person, with the per-person prefix, for registration. It **MUST** give the schema of the fields of each kind on its own.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-fact-shape`

**Touches**:
- Contract: `cpt-cf-construct-contract-person-types`
- Code: `construct_sdk::linked_schema::{default_schema, Schema, SchemaPrefix}`, `gears/construct/construct-sdk/schemas/`

**Verified by**: the SDK unit tests `the_default_schema_has_the_kinds_of_the_linked_profile`,
`every_link_names_kinds_of_the_schema_and_no_name_is_used_twice`, `the_links_of_the_story_are_there`,
`the_prefix_of_a_person_keeps_the_kinds_of_two_people_apart`,
`the_graph_types_are_every_node_and_link_kind_and_derive_from_their_family`,
`a_node_holds_the_object_of_its_fields_in_one_property_named_like_its_kind`,
`a_node_of_an_entry_kind_names_its_owner` and `a_link_holds_its_fields_its_origin_and_the_record`, and
`gts-validator`.

### Fact Origin in the Payload

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-fact-origin`

Every node and link type **MUST** require `origin` as graph storage's provenance attribute, and **MUST** take `record`
beside it for a node or link that came from a record, and `learned` on a node.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-fact-origin`

**Touches**:
- Requirement: `cpt-cf-construct-fr-fact-origin`
- Code: the `origin` and `record` members of the schemas in `gears/construct/construct-sdk/schemas/`

**Verified by**: the SDK unit tests `a_node_holds_the_object_of_its_fields_in_one_property_named_like_its_kind` and
`a_link_holds_its_fields_its_origin_and_the_record`, and the graph tests
`each_node_and_link_records_the_tool_and_the_message_it_came_from`.

### Owner Table

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-cardinality`

The SDK **MUST** state the owner of every node kind, the property that names a node of it, and whether the profile holds
one node of it only, and the kinds at the ends of every link kind.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-fact-cardinality`

**Touches**:
- Code: `construct_sdk::linked_schema::{NodeKind, LinkKind, Owner}`

**Verified by**: the SDK unit tests `every_link_names_kinds_of_the_schema_and_no_name_is_used_twice` and
`the_default_schema_has_the_kinds_of_the_linked_profile`.

### Profile Root and Has-Fact Edge

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-root`

The SDK **MUST** ship the root node type, an owned node whose closed payload holds the subject, the version (from 1)
and the removal list, and the has-fact edge type, a static edge with no payload. It **MUST** list them for
registration before the node and link kinds.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-write`

**Touches**:
- Code: `construct_sdk::profile_types::{PROFILE_ROOT, HAS_FACT, graph_types}`, `construct_sdk::gts::{PROFILE_ROOT_TYPE, HAS_FACT_TYPE}`

**Verified by**: the SDK unit tests `each_type_names_itself_and_derives_from_its_family`,
`a_root_with_its_subject_version_and_removals_fits`, `a_root_refuses_what_it_does_not_declare` and
`the_root_and_the_edge_register_before_the_person_types`.

### Profile Version

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-version`

The profile version **MUST** sit behind one interface that reads it from a stored root, gives the root write's expected version and the next version, and stamps it on the root being written, opaque to the planner. Graph storage returns no node version on read, so the first implementation **MUST** keep a counter in the root's payload: the root is created at 1 and every write adds one, so every root write changes the node and the counter equals graph storage's stored version. Construct is the root's only writer. The rest of the root's payload **MUST NOT** depend on the counter, so an implementation that reads graph storage's version **MAY** replace it with a root type that drops the counter, without changing the writer or the read. The version a read gives **MUST** name the root's generation above the counter, the generation in the high 32 bits and the counter in the low 32, so a root created after an erasure never has a version a root of an earlier generation had: a plan built before the erasure conflicts and is planned again. Graph storage's expected version is the counter alone; a root of generation 0 has its counter as its version. A counter past 32 bits **MUST** be an error, not a conflict.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-write`

**Touches**:
- Code: `infra::storage::graph::root::{ProfileVersion, PayloadCounter, Root}`

**Verified by**: `a_new_subject_creates_its_root_at_version_one_and_every_write_adds_one`, `a_root_of_a_later_generation_never_reads_a_version_an_earlier_one_had`, `graph_storage_sees_only_the_counter_and_a_write_advances_it_in_its_generation`, `a_counter_at_its_limit_cannot_advance_and_one_beyond_it_is_a_defect`, `the_root_goes_once_its_facts_are_gone_and_the_next_generation_starts_a_new_profile`, `the_counter_reads_the_version_a_root_carries`, `a_root_without_a_readable_version_is_a_defect`, `a_root_payload_reads_back_as_written_with_the_counter_stamped`, `a_root_without_a_valid_payload_names_the_root_and_what_is_wrong` (unit), and the version read back in `a_first_plan_creates_the_root_and_reads_back_whole` and `a_replace_stores_the_new_fact_and_removes_the_old_in_one_write`.

### Fact Node from a Plan

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-fact-payload`

The writer **MUST** store each new fact as a node of its category's type for the person, with the one property, the origin of the table in Origin of a Fact, the step's confidence as the decimal the model gave, and the record for a record's fact, and **MUST** claim the node's new key. It **MUST** take the key and the property from the step's own change, and **MUST** refuse a change of one property of a node without naming its value.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-write`
- `cpt-cf-construct-algo-profile-writer-fact-origin`

**Touches**:
- Code: `infra::storage::graph::fact::{fact_node, fact_of}`

**Verified by**: `a_records_fact_names_the_connector_its_record_and_when_it_was_observed`, `a_confidence_is_stored_as_the_decimal_the_model_gave`, `a_confidence_that_is_no_finite_number_is_a_defect`, `an_agents_fact_names_the_subject_and_the_agent_as_model`, `a_persons_fact_names_the_person_and_a_step_without_confidence_stores_none`, `a_removal_stores_no_node`, `a_step_of_no_known_category_is_a_defect`, `a_fact_must_hold_exactly_one_property`, `a_property_change_is_refused_without_its_value_in_the_message`, `a_stored_fact_reads_back_as_its_property_and_value`, `a_fact_node_that_holds_no_single_fact_is_a_defect_named_by_its_key`, `a_fact_property_named_as_the_envelope_is_a_defect`, `a_records_time_is_stored_in_utc`, `a_confidence_reads_back_as_the_value_given` and `a_stored_fact_reads_back_as_written` (property tests) (unit), and graph storage's validation of every written node in the integration tests, including `a_value_its_type_refuses_stores_nothing`.

### Type Registration

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-registration`

Construct's graph types **MUST** be registered with graph storage in each tenant before the first write there, cached per process; a registration that failed **MUST NOT** be remembered. A restart against a tenant that has them **MUST** converge rather than conflict. Registration needs the `admin` action on graph storage's type resource, so the deployment grants it to Construct's processing identity.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-write`

**Touches**:
- Code: `infra::storage::graph::types::GraphTypes`

**Verified by**: `the_types_register_again_after_a_restart_without_conflict`, `registration_is_remembered_per_tenant_and_a_failed_one_is_not`, `a_registration_that_loses_a_race_is_sent_once_more`, and the first write of every integration test, which registers in a new tenant.

### One Write per Plan

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-write`

The writer **MUST** store an admitted plan in one ingest that lands whole or not at all, with the root's expected version as the compare-and-set, so two plans on one profile never both land, and **MUST** report a root whose version no longer matches its counter as a defect rather than a conflict. A replaced or removed fact **MUST** be listed on the root in that ingest and **MUST** be removed after it; a removal that fails **MUST** stay listed until a later write finishes it, and only a live fact of the root **MUST** ever be removed. The writer **MUST NOT** let a profile grow past what one read takes. Every graph storage call **MUST** be bounded in time, a write **MUST** end by its deadline and report a result it cut after the ingest was sent as unknown, and a write whose result is unknown **MUST** be sent again with the same idempotency key, a bounded number of times, while its deadline allows.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-write`

**Touches**:
- Sequences: `cpt-cf-construct-seq-record-in`, `cpt-cf-construct-seq-entity-replaced`
- Code: `infra::storage::graph::writer::{GraphPlanWriter, FactRemoval, SoftDelete}`, the `domain::planner::processing::PlanWriter` port

**Verified by**: `a_first_plan_creates_the_root_and_reads_back_whole`, `a_replace_stores_the_new_fact_and_removes_the_old_in_one_write`, `a_removal_that_is_done_drops_off_the_root_on_the_next_write`, `a_removed_fact_is_gone_from_the_next_read`, `a_plan_built_on_an_older_profile_conflicts_and_stores_nothing`, `two_plans_on_the_same_profile_never_both_land`, `two_first_writes_create_the_root_once` (both hold the two writes until both reach the ingest), `a_root_written_outside_construct_refuses_every_write_as_a_defect`, `a_new_fact_whose_key_is_held_is_a_defect_and_stores_nothing`, `a_refusal_after_a_send_without_an_answer_replays_the_committed_send`, `a_refusal_is_replayed_once_at_most`, `the_send_that_checks_a_refusal_counts_toward_the_attempts`, `a_refusal_on_the_last_send_after_an_unknown_one_stays_unknown`, `a_throttled_send_after_a_send_without_an_answer_leaves_the_result_unknown`, `a_replayed_refusal_after_another_write_is_a_conflict`, `a_refusal_without_an_unknown_send_is_not_replayed`, `the_default_writer_sends_three_times_and_waits_ten_seconds_for_each`, `a_removal_that_fails_stays_listed_and_a_later_write_finishes_it`, `the_cleanup_stops_at_the_first_failed_removal_and_every_key_stays_listed`, `a_cleanup_that_runs_out_of_time_leaves_every_key_listed_and_the_write_stored`, `a_soft_delete_of_a_fact_already_removed_is_done`, `a_soft_delete_of_a_fact_graph_storage_does_not_hold_fails_and_names_it`, `a_listed_key_that_is_not_a_fact_of_the_root_is_never_deleted`, `a_plan_that_removes_a_fact_not_in_its_profile_is_refused`, `a_full_profile_takes_no_new_fact_but_takes_a_removal`, `a_full_profile_runs_its_listed_removals_and_then_takes_the_new_fact`, `a_profile_at_the_limit_takes_no_new_fact`, `a_writer_refuses_a_limit_past_what_one_read_takes`, `a_write_whose_answer_was_lost_is_sent_again_with_its_key_and_lands_once`, `a_write_whose_deadline_passed_calls_nothing_and_writes_nothing`, `a_profile_read_that_hangs_past_the_deadline_is_cut_and_writes_nothing`, `a_send_the_deadline_cuts_is_not_sent_again_and_its_result_is_unknown`, `no_resend_starts_when_its_pause_would_pass_the_deadline`, `a_resend_within_the_deadline_keeps_its_key_and_lands_once`, `the_cleanup_after_a_stored_write_ends_at_the_deadline`, `a_lost_serialization_race_is_sent_again`, `a_write_that_stays_unavailable_gives_up_after_its_attempts`, `a_write_that_never_gets_an_answer_gives_up_after_its_attempts_as_unavailable`, `the_pause_between_sends_grows_and_carries_up_to_as_much_again`, `the_pauses_start_at_100_ms_and_double`, `a_writer_given_one_attempt_sends_once`, `a_send_without_an_answer_in_time_is_sent_again`, `only_a_write_that_is_safe_to_send_again_is_resent`, `graph_storage_failures_map_to_what_a_caller_can_act_on`, `a_value_its_type_refuses_stores_nothing`, `a_property_change_is_refused_and_stores_nothing`, `an_empty_plan_writes_nothing` and `a_profile_of_another_tenant_than_the_context_is_refused`. They run against graph storage's own service over its in-memory store, with a stub policy service; Postgres and the real AuthZ resolver were not run.

### Profile Read

- [x] `p1` - **ID**: `cpt-cf-construct-dod-profile-writer-read`

The profile read **MUST** return the root's version and every fact one has-fact edge from the root, in one call bounded in time, **MUST** leave out a fact the root lists for removal, **MUST** read as facts only the nodes a has-fact edge from the root points to, **MUST** answer profile full rather than return a profile cut by the node budget, **MUST NOT** use a read that saw more than one graph state, and **MUST** refuse a tenant other than the security context's.

**Implements**:
- `cpt-cf-construct-algo-profile-writer-read`

**Touches**:
- Code: `infra::storage::graph::reader::GraphProfileSource`, the `domain::planner::processing::ProfileSource` port

**Verified by**: `a_subject_without_a_profile_reads_empty`, `a_first_plan_creates_the_root_and_reads_back_whole`, `a_profile_read_asks_for_the_root_and_at_most_its_facts`, `a_profile_cut_short_by_the_read_limit_is_full_not_partial`, `a_profile_past_what_one_read_takes_is_full`, `a_profile_read_cut_short_for_another_reason_is_internal`, `a_read_that_saw_more_than_one_graph_state_is_sent_again`, `a_write_whose_reads_never_see_one_graph_state_is_unavailable_and_sends_nothing`, `graph_storage_that_takes_fewer_nodes_than_a_profile_read_names_its_setting`, `a_node_with_an_edge_into_the_root_is_not_read_as_a_fact`, `a_node_reached_by_an_edge_of_another_type_is_not_read_as_a_fact`, `a_has_fact_edge_to_a_node_of_no_person_type_is_a_defect`, `a_root_key_held_by_another_type_is_a_defect`, `a_root_that_names_another_subject_is_a_defect` (unit), `a_read_without_an_answer_in_time_is_unavailable` (with the default bound), `a_profile_of_another_tenant_than_the_context_is_refused`, and the listed fact left out in `a_removal_that_fails_stays_listed_and_a_later_write_finishes_it`.

## 6. Acceptance Criteria

- [ ] Graph storage accepts the node and link kinds of the default schema, and validates a node of each type against the whole chain,
      origin included.
- [ ] A node whose payload has no origin, two facts, an unknown property, or an incomplete record is refused.
- [ ] Every node kind has one owner, and every link kind names the kinds at its ends.
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

### Schema at Run Time

A person's schema is read and changed while the gear runs, through `GET` and `PATCH /construct/v1/subjects/{subject_id}/schema`
(REST only: the host calls the gear over HTTP; no SDK method). Both are authorized actions on the subject's schema,
owner only by default, as the subject's settings are. A change is a list of `add_node_kind`, `remove_node_kind`,
`add_link_kind`, `remove_link_kind`, `add_property`, `remove_property`, `add_link_field` and `remove_link_field`,
applied in order to a copy and checked as a whole, so all hold or none is made.

- **Storage**: the gear keeps the schema a host set as one row per tenant and subject in `construct__subject_schemas`,
  with a version that a change compares and moves, so two changes at once cannot both land. A person without a row, or whose row holds the default schema, has the default schema; the row
  also holds the count of names the person has made the process keep (below). A change is logged as
  `profile_schema_changed`.
- **Rules**: names are lower case letters, digits and `_` and at most 40 characters; `origin`, `record`, `learned` and
  `owner` are reserved; a name may not repeat; a kind's key is one of its properties; an owner or a link end names a
  kind of the schema; every property is a JSON schema (draft 7) with no `$ref` into the document (`#`, `#/..`), as graph storage places a property in the node's own type, where such a reference would resolve against the node. A schema holds at most 64 node kinds, 64 link kinds
  and 32 properties a kind. A refusal answers 422 with the reason and the schema does not change. Any failure after the names were charged (a refusal or an outage of graph storage, a change that loses a race, a database error) leaves them charged to the person; a refusal by the rules, or by the budget, charges nothing.
- **From the next message**: the planner, the profile read and the writer read the person's schema when they start.
  A message being planned finishes with the catalog it started with; the write then reads the schema again, so a
  change that lands between the two can refuse that message's write. The writer also reads the schema once more just before it stores, and answers a conflict when it differs from the one the write was built on; the message is planned again, up to the re-run limit, and dropped only then. A change landing after that check leaves a node of a removed kind hidden from every read, until the next change, an erasure or the retention period sweeps it, unless a change adds the kind back before then: the node then shows again as a live fact, under the kind's new property schema.
- **Graph storage**: a change registers the types of the schema for the person, under the per-person prefix, before it
  is stored; graph storage's refusal, such as a removed kind added again with a schema it holds as incompatible, is the
  answer and the schema does not change. Graph storage has no call to remove a type, so a removed kind stays registered, unused.
- **Removing a node kind**: the new schema is stored first, then the nodes of the kinds it lacks, with the nodes they
  own, are forgotten in one write. From the moment the schema is stored those nodes are in no read: the profile
  read, the planner and the subject's own view skip a node of a kind the schema does not have. If the removal fails or
  the process stops before it, the nodes stay in graph storage, hidden, and the call answers 503 and says so. Sending an
  empty change (`{"changes": []}`) or any other change that does not add the kind back finishes the removal; sending the same removal again is refused,
  as the kind is gone, and a change that adds the kind back does not finish it: the nodes show again with the kind. An erasure and the retention
  period remove such nodes too, and the removal is logged on the audit target with the node count. A message planned just before the change can still write a node of the removed kind;
  it is hidden and swept the same way. The kind also leaves the owners and link ends that name it; a link kind left
  with no kind at an end goes with it, and a kind left with no owner is refused, naming it.
- **Names kept in memory**: a kind, property or field name is kept for the life of the process, once. Free text (the
  descriptions) is not. The names a change brings in that the process does not keep yet are charged to the person
  before any is kept, by a compare-and-set on the schema's version and count, so a change that is then refused or loses a race
  has paid for what it leaked, and of two changes on one version only one goes on. A person may make the process keep
  at most 8 192 names over the life of their schema, removals not refunded and an erasure not resetting the count
  (a person's schema holds at most 4 224 names at a time); a change that brings in no new name is never refused for
  it. An erasure resets the schema to the default and keeps the count. A schema is also capped at 60 000 bytes as JSON.
- **Per-kind permission**: the profile read asks the policy for each kind of the person's schema, so an added kind is
  never readable without its own grant.
- **Index**: every node type declares an index on the key property of its kind, the one that names a node
  (`/payload/<kind>/<key>`, `name` for most kinds). Graph storage builds the index in the background after the type is
  registered, and a filter or sort on it is admitted only once it is built; its activation lifecycle is not shipped
  yet (graph storage ADR-0003), so it reports no time, and a kind added at run time may not be filtered or sorted on
  its name for a while. The gear itself filters and sorts
  nothing in graph storage, so no write or read of the gear waits for it.

## 7. Non-Functional Considerations

- **Compatibility**: a closed payload lets a new property be added without a new major version.
- **Privacy**: no stored value is copied into the schemas. The schemas carry only property names and
  constraints. No log line of the writer or the read holds a value.
- **Deletion**: removals are soft deletes until gears-rust #5194 ships a hard delete. A soft-deleted fact keeps its
  payload and embedding as a tombstone, and an erasure by scope replacement skips tombstones. When #5194 ships, the
  facts soft-deleted before it need a one-off hard delete; whether #5194 can hard-delete an already tombstoned node is
  open with graph storage.
- **Reliability**: a write is one transaction in graph storage. Every graph storage call waits at most 10 seconds by default, because graph storage cannot cut off a statement it has started. A write whose result is unknown is sent again with its idempotency key, which replays the recorded result, after a doubling pause with a random share added, so writers that failed together do not retry together; after 3 sends in all, the one that checks a refusal included, it is unavailable and processing drops the record. Graph storage's embedding is on by default, so an embedding provider that fails fails the write.
- **Errors**: a refusal of Construct's own permission by graph storage is internal, logged at error: the deployment's grant is missing, the caller was not denied. A failure a retry may cure is unavailable. A full profile has its own error, a failed precondition (`PROFILE_FULL`, logged at warn with the tenant, the subject, the fact count and the limit as fields) on the API, because it is the subject's state, not the gear's failure. A root whose version no longer matches its counter is internal, logged at error. Everything else is internal. No error message holds a fact value.
- **Limits**: one read takes at most 1000 nodes, graph storage's default traversal ceiling, so a profile holds at most 999 facts. A deployment **MUST NOT** set graph storage's `traversal_max_nodes` below 1000: every profile read, and so every write, would then fail as internal, naming the setting. The writer counts a fact listed for removal until its delete has run, so a profile never passes the limit even while deletes fail; at the limit it takes plans that remove, and new facts once the deletes have run. A replace adds its new fact before the old one's delete runs, so at the limit it is refused like an add: a subject at the limit takes no new value, a corrected one included, until removals free room. Splitting such a replace into a removal and a later add is left to the planner and not done yet. A profile cut short by the read's node budget fails as full rather than being returned partially.
