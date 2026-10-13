# Feature: Planner


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Read the Profile](#read-the-profile)
  - [Show the Profile to the Model](#show-the-profile-to-the-model)
  - [Plan Tools](#plan-tools)
  - [Build the Plan](#build-the-plan)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Numbered Profile](#numbered-profile)
  - [Plan Tools](#plan-tools-1)
  - [Plan Building](#plan-building)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-planner-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-planner`

## 1. Feature Context

### 1.1 Overview

The planner decides how a record changes the subject's profile. A language model proposes add, replace and remove
steps through tools; the planner builds them into a plan in memory and writes nothing. This document covers the first
part: the numbered profile the model sees, the plan tools with their checks, and building the plan with its node keys
and origin. The agent loop with its caps, the prompt, the hand-off from intake and the rerun on
a write conflict follow in later changes.

### 1.2 Purpose

Construct keeps one consistent profile per subject and avoids contradicting facts (`cpt-cf-construct-fr-fact-decisions`).
The model works on numbered values and plain property names, never on IDs
(`cpt-cf-construct-principle-model-never-sees-ids`). The tools refuse a call that breaks a rule and tell the model
why, so the plan holds only steps that fit the person types.

**Out of scope**: the agent loop, its caps and drop events; the prompt; the hand-off from Record Intake and the
processing chain; the rerun on a write conflict; skipping a fact under an open review request (Review Queue); the
profile read.

**Requirements**: `cpt-cf-construct-fr-fact-decisions`

**Principles**: `cpt-cf-construct-principle-model-never-sees-ids`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-llm-gateway` | The model behind the model client. It sees the numbered profile and calls the plan tools. |
| `cpt-cf-construct-actor-graph-storage` | Holds the profile the planner reads. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), component `cpt-cf-construct-component-planner`, entities Plan and Profile
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.5
- **Dependencies**: `cpt-cf-construct-feature-model-client`; the person types and their cardinality in
  [profile-writer.md](profile-writer.md)

## 2. Actor Flows (CDSL)

**Use cases**: None in this part. The planner is internal; the flow from a received record comes with the hand-off.

## 3. Processes / Business Logic (CDSL)

### Read the Profile

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-planner-read-profile`

1. [ ] - `p1` - The planner works on the subject's current profile and its version, read from graph storage, not
   through the Profile Reader or the writer. The read itself comes with the change that reads graph storage -
   `inst-read-source`
2. [ ] - `p1` - Each fact comes with its node key, its category, its property and its value. The version is the root
   node's version, which the planner carries as an opaque value into the plan - `inst-read-facts`

### Show the Profile to the Model

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-planner-numbered-profile`

1. [ ] - `p1` - Each fact is one line: `n. property (category): value`, numbered from 1 in the order of the read -
   `inst-number-lines`
2. [ ] - `p1` - Every value is shown as compact JSON, a text value as a JSON string, so a line break in a value cannot
   start a line that looks like another value - `inst-number-values`
3. [ ] - `p1` - The model never sees a node key, a tenant or subject id, a type id or the fact's origin - `inst-number-no-ids`

### Plan Tools

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-planner-tools`

1. [ ] - `p1` - Three tools: `add(property, value, confidence)`, `replace(number, value, confidence)` and
   `remove(number, confidence)`. They only add steps to the plan in memory - `inst-tools-set`
2. [ ] - `p1` - `add` offers every property of the person types by name. The property decides the category -
   `inst-tools-properties`
3. [ ] - `p1` - A value is checked against its property's schema in the person type. A refusal names the broken rule
   and where it is broken, and the schema the property takes. It does not repeat the value. A refusal repeats at most
   80 characters of a tool or property name or of a parse error - `inst-tools-value`
4. [ ] - `p1` - A number must name a value of the profile, and only one step may use it - `inst-tools-number`
5. [ ] - `p1` - A property that holds one value takes no second one: an `add` is refused while the profile holds an
   untouched value of it, with "use replace n", or while the plan already adds or replaces one. After a `remove` of
   the old value, an `add` is allowed - `inst-tools-cardinality`
6. [ ] - `p1` - A value already held is refused: an `add` or a `replace` to a value another fact of the profile holds
   under the same property, or that the plan already writes there, and a `replace` with exactly the value the fact
   already has. Text, also each text field of an entry, is compared without case and spacing, so a `replace` that only
   fixes case or spacing is taken - `inst-tools-duplicate`
7. [ ] - `p1` - `replace` takes the complete new value: fields left out of an object are gone - `inst-tools-replace`
8. [ ] - `p1` - Confidence is a number from 0 to 1 - `inst-tools-confidence`
9. [ ] - `p1` - A refused call adds no step; the model gets the refusal's text as the tool result - `inst-tools-refusal`

### Build the Plan

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-planner-build`

1. [ ] - `p1` - Each number maps back to its node key - `inst-build-keys`
2. [ ] - `p1` - Each new value gets a new node key, `construct:{tenant id}:{random UUID}`, also a value that returns
   after a delete, because a soft-deleted key cannot be reused - `inst-build-new-keys`
3. [ ] - `p1` - Each value is one entity of its category with one property, and the step's entity kind is the
   category's plain name. An add is a new entity, a remove removes the entity, and a replace is one step with a new
   entity in place of the old one, so the old value goes in the same write - `inst-build-replace`
4. [ ] - `p1` - A text value is stored as it is; any other value as its JSON - `inst-build-values`
5. [ ] - `p1` - The plan holds the tenant and the subject, the profile version it was built on, none for a subject
   without a profile yet, and its origin - `inst-build-origin`
6. [ ] - `p1` - Each step carries the confidence the model gave it - `inst-build-confidence`

## 4. States (CDSL)

None. A plan lives in memory for one planning run.

## 5. Definitions of Done

### Numbered Profile

- [x] `p1` - **ID**: `cpt-cf-construct-dod-planner-numbered-profile`

The system **MUST** show the profile as numbered values of plain properties, and **MUST NOT** show an ID.

**Implements**:
- `cpt-cf-construct-algo-planner-numbered-profile`

**Touches**:
- Code: `domain::planner::draft::PlanDraft`

**Verified by**: `the_profile_is_shown_as_numbered_values_of_plain_properties_in_json`,
`a_line_break_in_a_value_cannot_forge_a_profile_line` and `the_model_input_holds_no_id`.

### Plan Tools

- [x] `p1` - **ID**: `cpt-cf-construct-dod-planner-tools`

The system **MUST** offer the add, replace and remove tools, and **MUST** refuse a call that breaks a rule without
adding a step.

**Implements**:
- `cpt-cf-construct-algo-planner-tools`

**Touches**:
- Code: `domain::planner::draft::PlanDraft::apply`, `domain::planner::tools`, `domain::planner::catalog::PropertyCatalog`

**Verified by**: one test per refusal in `domain::planner::draft_test`, each checking that the draft did not change,
and the catalog tests.

### Plan Building

- [x] `p1` - **ID**: `cpt-cf-construct-dod-planner-plan`

The system **MUST** map numbers to node keys, give each new value a new key in the tenant, and fill the plan's origin
and profile version.

**Implements**:
- `cpt-cf-construct-algo-planner-build`

**Touches**:
- Code: `domain::planner::build`, `domain::plan`

**Verified by**: `a_replace_is_one_entity_in_place_of_the_old_one`,
`an_add_and_a_remove_keep_their_confidence_in_a_plan_on_the_profile_version` and
`every_new_value_gets_its_own_new_key_also_when_it_returns`.

## 6. Acceptance Criteria

- [ ] For a record and a profile, the planner builds a plan of add, replace and remove steps with node keys, the
      profile version and the origin.
- [ ] No message or tool result sent to the model holds an ID.
- [ ] A tool call that breaks a rule is refused with a reason, and the plan does not change.

## 7. Non-Functional Considerations

- **Privacy**: the numbered profile and the record reach the configured model service through the model client. The
  planner logs neither.
- **Plan type**: the planner builds on `domain::plan`, the plan type the sensitive-data checks and Admission read.
