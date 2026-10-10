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
  - [Profile Read Port](#profile-read-port)
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
part: the profile read port, the numbered profile the model sees, the plan tools with their checks, and building the
plan with its node keys and origin. The agent loop with its caps, the prompt, the hand-off from intake and the rerun on
a write conflict follow in later changes.

### 1.2 Purpose

Construct keeps one consistent profile per subject and avoids contradicting facts (`cpt-cf-construct-fr-fact-decisions`).
The model works on numbered values and plain property names, never on IDs
(`cpt-cf-construct-principle-model-never-sees-ids`). The tools refuse a call that breaks a rule and tell the model
why, so the plan holds only steps that fit the person types.

**Out of scope**: the agent loop, its caps and drop events; the prompt; the hand-off from Record Intake and the
processing chain; the rerun on a write conflict; skipping a fact under an open review request (Review Queue); the
graph storage implementation of the profile read.

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

1. [ ] - `p1` - The planner reads the subject's current profile and its version through the `ProfileSource` port, not
   through the Profile Reader or the writer - `inst-read-port`
2. [ ] - `p1` - Each fact comes with its node key, its category, its property and its value. The version is the root
   node's version, which the planner carries as an opaque value into the plan - `inst-read-facts`
3. [ ] - `p1` - The graph storage implementation of the port comes with the writer's read side. Until then the port
   has no implementation in the gear - `inst-read-later`

### Show the Profile to the Model

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-planner-numbered-profile`

1. [ ] - `p1` - Each fact is one line: `n. property (category): value`, numbered from 1 in the order of the read -
   `inst-number-lines`
2. [ ] - `p1` - A text value is shown as it is; any other value as compact JSON - `inst-number-values`
3. [ ] - `p1` - The model never sees a node key, a tenant or subject id, a type id or the fact's origin - `inst-number-no-ids`

### Plan Tools

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-planner-tools`

1. [ ] - `p1` - Three tools: `add(property, value, confidence)`, `replace(number, value, confidence)` and
   `remove(number, confidence)`. They only add steps to the plan in memory - `inst-tools-set`
2. [ ] - `p1` - `add` offers every property of the person types by name. The property decides the category -
   `inst-tools-properties`
3. [ ] - `p1` - A value is checked against its property's schema in the person type. A refusal names the broken rule
   and the schema the property takes - `inst-tools-value`
4. [ ] - `p1` - A number must name a value of the profile, and only one step may use it - `inst-tools-number`
5. [ ] - `p1` - A property that holds one value takes no second one: an `add` is refused while the profile holds an
   untouched value of it, with "use replace n", or while the plan already adds one. After a `remove` of the old value,
   an `add` is allowed - `inst-tools-cardinality`
6. [ ] - `p1` - Confidence is a number from 0 to 1 - `inst-tools-confidence`
7. [ ] - `p1` - A refused call adds no step; the model gets the refusal's text as the tool result - `inst-tools-refusal`

### Build the Plan

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-planner-build`

1. [ ] - `p1` - Each number maps back to its node key - `inst-build-keys`
2. [ ] - `p1` - Each new value gets a new node key, `construct:{tenant id}:{random UUID}`, also a value that returns
   after a delete, because a soft-deleted key cannot be reused - `inst-build-new-keys`
3. [ ] - `p1` - A replace is one step with the old key and a new key, so the old value goes in the same write -
   `inst-build-replace`
4. [ ] - `p1` - The plan holds the tenant, the subject, the profile version it was built on and its origin. For a
   record the origin is the connector, the record type, its provenance and version, and its `observed_at` -
   `inst-build-origin`
5. [ ] - `p1` - Each step carries the confidence the model gave it - `inst-build-confidence`

## 4. States (CDSL)

None. A plan lives in memory for one planning run.

## 5. Definitions of Done

### Profile Read Port

- [x] `p1` - **ID**: `cpt-cf-construct-dod-planner-profile-read`

The system **MUST** read the profile through one port that returns the facts with their node keys and the profile
version.

**Implements**:
- `cpt-cf-construct-algo-planner-read-profile`

**Touches**:
- Code: `domain::profile::ProfileSource`

**Verified by**: the plan tests, which build a draft from a profile as the port returns it.

### Numbered Profile

- [x] `p1` - **ID**: `cpt-cf-construct-dod-planner-numbered-profile`

The system **MUST** show the profile as numbered values of plain properties, and **MUST NOT** show an ID.

**Implements**:
- `cpt-cf-construct-algo-planner-numbered-profile`

**Touches**:
- Code: `domain::planner::draft::PlanDraft`

**Verified by**: `the_profile_is_shown_as_numbered_values_of_plain_properties` and `the_model_input_holds_no_id`.

### Plan Tools

- [x] `p1` - **ID**: `cpt-cf-construct-dod-planner-tools`

The system **MUST** offer the add, replace and remove tools, and **MUST** refuse a call that breaks a rule without
adding a step.

**Implements**:
- `cpt-cf-construct-algo-planner-tools`

**Touches**:
- Code: `domain::planner::draft::PlanDraft::apply`, `domain::planner::catalog::PropertyCatalog`

**Verified by**: `a_call_that_breaks_a_rule_is_refused_and_adds_no_step`, `a_refusal_tells_the_model_what_to_do_instead`,
`a_single_value_can_be_added_after_its_old_value_is_removed`, and the catalog tests.

### Plan Building

- [x] `p1` - **ID**: `cpt-cf-construct-dod-planner-plan`

The system **MUST** map numbers to node keys, give each new value a new key in the tenant, and fill the plan's origin
and profile version.

**Implements**:
- `cpt-cf-construct-algo-planner-build`

**Touches**:
- Code: `domain::planner::draft::PlanDraft::finish`, `domain::plan`

**Verified by**: `a_replace_is_one_step_with_the_old_key_and_a_new_one`,
`an_add_and_a_remove_become_steps_of_one_plan_on_the_profile_version`,
`every_new_value_gets_its_own_new_key_also_when_it_returns` and `the_record_origin_comes_from_the_received_record`.

## 6. Acceptance Criteria

- [ ] For a record and a profile, the planner builds a plan of add, replace and remove steps with node keys, the
      profile version and the record's origin.
- [ ] No message or tool result sent to the model holds an ID.
- [ ] A tool call that breaks a rule is refused with a reason, and the plan does not change.

## 7. Non-Functional Considerations

- **Privacy**: the numbered profile and the record reach the configured model service through the model client. The
  planner logs neither.
- **Plan type**: `domain::plan` holds the plan shape agreed for Story 5.7.3, until that change brings its own.
