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
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Person Types](#person-types)
  - [Fact Origin in the Payload](#fact-origin-in-the-payload)
  - [Cardinality Table](#cardinality-table)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-profile-writer-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-profile-writer`

## 1. Feature Context

### 1.1 Overview

This document starts the Profile Writer feature with the part it stores: Construct's person types
(`cpt-cf-construct-contract-person-types`). It defines what one stored fact looks like, where its origin is kept, and
how many values of a property one subject may hold. It does not cover the write itself: one write per plan, the root
node, the compare-and-set and the rerun of the planner.

### 1.2 Purpose

The DESIGN leaves "the properties of the person types" to the feature design. A person has four categories:
identity, role, skill and preference. Each is one GTS graph node type, which the profile writer stores and the
planner, the profile reader and the subject control read.

**In scope**: the four person types, the shape of a fact, the origin of a fact, the cardinality of each property.

**Out of scope**: the write of a plan, the subject root node type and the type of the edge from the root to a fact,
the registration of the types, and how the planner shows facts to the model. The types are registered with graph
storage only: graph storage keeps its own type catalogue, validates every node against it, and does not publish to the
types registry. The writer's change registers them. Proposed for
the writer: the root node derives from graph storage's reference node, because the subject is a platform user
(`gts.cf.core.am.user.v1~`) that the account management owns; the edge from the root to a fact is a static edge with no
payload, because the fact carries its own origin.

**Requirements**: `cpt-cf-construct-fr-fact-decisions`, `cpt-cf-construct-fr-fact-origin`

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

**Use cases**: None in this part. The flows of the write come with the writer.

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

## 6. Acceptance Criteria

- [ ] Graph storage accepts the four person types, and validates a node of each type against the whole chain,
      origin included.
- [ ] A node whose payload has no origin, two facts, an unknown property, or an incomplete record is refused.
- [ ] Every fact property has exactly one cardinality.

## 7. Non-Functional Considerations

- **Compatibility**: a closed payload lets a new property be added without a new major version.
- **Privacy**: no stored value is copied into the schemas. The schemas carry only property names and
  constraints.
