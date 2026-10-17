# Feature: Admission


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Admit a Plan](#admit-a-plan)
  - [Enforce the Verdicts](#enforce-the-verdicts)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Admission Rules](#admission-rules)
  - [Verdict Enforcement](#verdict-enforcement)
  - [Block and Redaction Events](#block-and-redaction-events)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-admission-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-admission`

## 1. Feature Context

### 1.1 Overview

Admission (`cpt-cf-construct-component-admission`) is the deterministic code that decides what of a plan is stored. The model only proposes (`cpt-cf-construct-principle-model-proposes-code-decides`). Admission checks the plan against the subject's settings and the tenant's admission rules, and drops the whole plan when one fails. It then enforces the verdicts of the Sensitive-Data Checks on the affected values only. It writes a content-free audit event for each drop, block and redaction, and hands the admitted plan on. It makes no model call and writes nothing to graph storage: it holds neither a model client nor a graph storage client. This document describes Story 5.7.3.

### 1.2 Purpose

The PRD requires that nothing reaches the profile unless deterministic rules let it through (`cpt-cf-construct-fr-sensitive-data-guardrails`, enforcement part), and that plans are dropped while personalization is off or an erasure is under way (`cpt-cf-construct-fr-settings`).

**The rules that drop the whole plan.** In this order, the first that fails names the reason:

1. Personalization on and no erasure under way, read from the subject's settings (`cpt-cf-construct-feature-subject-settings`), with the tenant's default for a subject without a row.
2. Source trust: the tenant trusts the connector named in the plan's origin.
3. The confidence floor: every step of a model's plan carries a confidence at or above the floor. A step without one, or with one that is not a number, is below it.
4. Allowed entity kinds: every step that stores a value has an allowed entity kind. A removal passes, so a subject's entity of a kind no longer allowed can still go.
5. Write caps: the plan holds no more steps than the cap, and no value is longer than the cap, in characters.

**Plans written by people.** A reviewer's corrected value and an administrator's added or edited fact have no model and no connector. They skip source trust and the confidence floor, and go through every other rule and the verdicts. DECOMPOSITION 2.7 leaves this to this document. An agent's plan comes from the model, so the confidence floor applies; it names no connector, so source trust has nothing to check.

**The admission rules.** They are tenant settings in the settings service, read through one interface that returns them as one set: the trusted connectors (any, or only some), the confidence floor, the allowed entity kinds (any, or only some), the step cap and the value cap. Whether the settings service holds them as one setting or several is decided with Construct's setting declarations (Story 5.11), not here. This part builds the interface and a stub in the tests. Avatar's values are a starting point for the declarations: a confidence floor of 0.6 and at most 100 writes per plan. Avatar cuts a plan over the cap; Admission drops it, as the DESIGN says.

**The verdicts.** A block drops the value and keeps the rest of the plan. A redaction removes the found content from the value. Block wins over redact on the same value. A value that cannot be redacted as the check asks, because the check named no range or a range that is empty, out of the value or not on a character boundary, is dropped like a blocked one. So is a value with nothing but white space left. A step that keeps no value is dropped. **A replace whose new value is blocked is dropped whole, and the old value stays.** This refines DECOMPOSITION 2.7's "block drops the value and keeps the rest": dropping only the new value would remove the old one and add nothing, which loses data. A redacted value keeps its step.

**The drop event.** A plan built from a connector's record is dropped with the drop event of Record Intake (`cpt-cf-construct-algo-record-intake-drop-event`): the tenant, the subject, the connector, the record's identity and type, stage `admission` and the reason. For that, the plan's origin carries the record's provenance and version. A plan with no record behind it (an agent's, a reviewer's or an administrator's) has no record drop event: Admission answers dropped with the reason, and the calling feature (the MCP tool, the Review Queue, the administrator operations) audits it in its own event.

**The result.** Admission answers dropped, with the reason, or admitted, with the plan to store and what it took out: the blocked values, the redacted values, each with its sensitive-data kind, and the steps dropped because a value was blocked. An admitted plan can be empty when every value was blocked; it stores nothing.

**The plan.** Story 5.2 (the Planner, ROL-17080) left the plan's shape undefined, so this feature defines it, and 5.2 agreed it and builds on it. A plan holds its tenant, its subject, its origin (a connector with the record's identity, type and `observed_at`, which the profile writer stores as the fact's `produced_at`; an agent; a reviewer; or an administrator), the profile version it was built on, and its steps. A step has the entity kind it touches, its change (add an entity or a property, replace a property or an entity, remove an entity or a property) and the model's confidence, which a person's plan does not have. Admission reads only the origin, each step's entity kind, confidence and change, and the values.

**The context.** Admission runs in the background, after intake answered. It takes the security context and the scope that processing builds for the tenant, never the connector's: the connector's token can expire before processing runs. Record intake hands off the tenant, the connector and the record, and no security context (`cpt-cf-construct-algo-record-intake-hand-off`).

**Out of scope**:

- Detecting sensitive data: the Sensitive-Data Checks (`cpt-cf-construct-feature-sensitive-data-checks`).
- Storing the admitted plan: the Profile Writer.
- Reading the admission rules from the settings service: it waits for the setting declarations (Story 5.11).
- Wiring Admission into processing, and building processing's own security context and scope. The Planner does that when it hands its plan on.
- Writing the audit events to the Audit gear, which does not exist yet; they go to the structured log.

**Requirements**: `cpt-cf-construct-fr-sensitive-data-guardrails` (enforcement part), `cpt-cf-construct-fr-settings` (plans dropped while personalization is off or an erasure is under way).

**Principles**: `cpt-cf-construct-principle-model-proposes-code-decides`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-settings-service` | Holds the tenant's admission rules, once they are declared. |
| `cpt-cf-construct-actor-audit` | Takes the drop, block and redaction events, once it ships; until then they go to the structured log. |
| `cpt-cf-construct-actor-dpo` | Counts the blocks and redactions per sensitive-data kind from the events. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), the Admission component, the Plan and Sensitive-Data Verdict entities, and the Record In sequence
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.7
- **Dependencies**: `cpt-cf-construct-feature-sensitive-data-checks`, `cpt-cf-construct-feature-subject-settings`

## 2. Actor Flows (CDSL)

**Use cases**: None. Admission is internal; processing, and later the Review Queue and the administrator operations, call it in process.

## 3. Processes / Business Logic (CDSL)

### Admit a Plan

- [x] `p1` - **ID**: `cpt-cf-construct-algo-admission-admit`

**Input**: The security context and scope processing built for the tenant, a plan, and the verdicts of the checks.

**Output**: Admitted, with the plan to store and what was taken out; or dropped, with the reason. Or an error when the settings or the rules cannot be read: nothing is decided, no event is written, and the caller drops the record.

**Steps**:
1. [x] - `p1` - Read the subject's settings in the plan's tenant under the scope (`cpt-cf-construct-algo-subject-settings-read`). **IF** an erasure is under way, drop the plan with `erasure_in_progress`; **IF** personalization is off, drop it with `personalization_off` - `inst-admit-subject`
2. [x] - `p1` - Read the tenant's admission rules. **IF** the plan breaks one, drop it with that rule's reason, checking in the order of steps 3 to 6 - `inst-admit-rules`
3. [x] - `p1` - **IF** the origin is a connector the rules do not trust, the reason is `untrusted_source`. Other origins name no connector - `inst-admit-trust`
4. [x] - `p1` - **IF** the plan is not written by a person and a step has no confidence, or one that is not a number or is below the floor, the reason is `below_confidence_floor` - `inst-admit-confidence`
5. [x] - `p1` - **IF** a step that stores a value has an entity kind the rules do not allow, the reason is `entity_kind_not_allowed` - `inst-admit-kinds`
6. [x] - `p1` - **IF** the plan has more steps than the step cap, or a value has more characters than the value cap, the reason is `write_cap_exceeded` - `inst-admit-caps`
7. [x] - `p1` - To drop a plan built from a record, write the drop event (`cpt-cf-construct-algo-record-intake-drop-event`) with stage `admission` and the reason. **RETURN** dropped with the reason. No block or redaction event is written - `inst-admit-drop`
8. [x] - `p1` - Enforce the verdicts (`cpt-cf-construct-algo-admission-enforce`) and **RETURN** admitted - `inst-admit-verdicts`

### Enforce the Verdicts

- [x] `p1` - **ID**: `cpt-cf-construct-algo-admission-enforce`

**Input**: A plan that passed the rules, and the verdicts.

**Output**: The plan to store, the blocked and redacted values with their kinds, and the dropped steps.

**Steps**:
1. [x] - `p1` - For each value a verdict names, collect the kinds that block it, and the ranges of each kind that redacts it - `inst-enforce-collect`
2. [x] - `p1` - **IF** any kind blocks the value, the value is blocked, for each of those kinds - `inst-enforce-block`
3. [x] - `p1` - Otherwise remove every range of every redacting kind from the value. **IF** a kind named no range, a range is empty, out of the value or not on a character boundary, or only white space is left, the value is blocked for the redacting kinds; otherwise it is redacted for them - `inst-enforce-redact`
4. [x] - `p1` - Drop each blocked value from its step. **IF** the step is a replace, drop the whole step, so the old value stays; **IF** the step keeps no value, drop it - `inst-enforce-steps`
5. [x] - `p1` - Write one event for each blocked value and kind, and one for each redacted value and kind. An event names the tenant, the subject, the origin and the kind, never the value - `inst-enforce-events`
6. [x] - `p1` - Until the Audit gear ships, the events go to the structured log at warn level under the target `construct::audit`, as `value_blocked` and `value_redacted` - `inst-enforce-log`

## 4. States (CDSL)

None. Admission keeps no state; it reads the subject's settings and the tenant's rules on each plan.

## 5. Definitions of Done

### Admission Rules

- [x] `p1` - **ID**: `cpt-cf-construct-dod-admission-rules`

The system **MUST** drop the whole plan, with the reason in the result and, for a plan built from a record, the content-free drop event of Record Intake (stage `admission`), when personalization is off or an erasure is under way for the subject, or the plan breaks source trust, the confidence floor, the allowed entity kinds or the write caps. A plan written by a person **MUST** skip source trust and the confidence floor and **MUST** go through every other rule. The rules **MUST** be read through one interface that returns the tenant's rules as one set. Admission **MUST NOT** make a model call or write to graph storage. A failed read of the settings or the rules **MUST** decide nothing.

**Implements**:
- `cpt-cf-construct-algo-admission-admit`

**Touches**:
- DB Table: `construct__subject_settings` (read, through Subject Settings)
- Entities: Plan (`cpt-cf-construct-entity-plan`), Subject Settings (`cpt-cf-construct-entity-subject-settings`)

**Verified by**: `a_plan_that_passes_every_rule_is_admitted_unchanged`, `personalization_off_drops_the_plan`, `an_erasure_under_way_drops_the_plan_even_with_personalization_on`, `a_connector_the_tenant_does_not_trust_drops_the_plan`, `a_trusted_connector_passes`, `a_step_below_the_confidence_floor_drops_the_whole_plan`, `a_step_at_the_floor_passes`, `a_model_step_without_a_confidence_or_with_one_that_is_not_a_number_drops_the_plan`, `an_agents_plan_meets_the_floor_but_has_no_connector_to_trust`, `a_plan_written_by_a_person_skips_trust_and_the_confidence_floor`, `a_plan_written_by_a_person_goes_through_every_other_rule`, `an_entity_kind_the_tenant_does_not_allow_drops_the_plan`, `removing_an_entity_of_a_kind_no_longer_allowed_passes`, `a_plan_over_the_write_caps_is_dropped`, `the_value_cap_counts_characters_not_bytes`, `a_dropped_plan_writes_no_block_event_even_with_block_verdicts`, `unreadable_settings_or_rules_decide_nothing`. The settings and the rules are stubs. That Admission makes no model call and writes nothing to graph storage is shown by its type, which holds no model or graph storage client, and checked by review.

### Verdict Enforcement

- [x] `p1` - **ID**: `cpt-cf-construct-dod-admission-verdicts`

The system **MUST** enforce the verdicts on the affected values only: block drops the value and keeps the rest; redact removes the found content. Block **MUST** win over redact. A value that cannot be redacted as asked, or is empty after it, **MUST** be dropped like a blocked one. A replace whose new value is blocked **MUST** be dropped whole, so the old value stays; a step that keeps no value **MUST** be dropped. The result **MUST** name the blocked and redacted values with their kinds, and the dropped steps.

**Implements**:
- `cpt-cf-construct-algo-admission-enforce`

**Touches**:
- Entities: Sensitive-Data Verdict (`cpt-cf-construct-entity-sensitive-data-verdict`), Plan (`cpt-cf-construct-entity-plan`)

**Verified by**: `a_blocked_value_is_not_stored_the_rest_is_and_the_result_says_so`, `a_step_that_keeps_no_value_is_dropped`, `a_replace_whose_new_value_is_blocked_is_dropped_whole_so_the_old_value_stays`, `every_value_blocked_leaves_an_admitted_plan_with_nothing_to_store`, `a_redacted_value_keeps_its_step_without_the_found_content`, `overlapping_redactions_of_two_kinds_both_apply`, `block_wins_over_redact_on_the_same_value`, `a_value_that_cannot_be_redacted_as_asked_is_blocked`, `a_plan_written_by_a_person_goes_through_the_verdicts`, `checks_then_admission_store_nothing_the_checks_block`.

### Block and Redaction Events

- [x] `p1` - **ID**: `cpt-cf-construct-dod-admission-events`

The system **MUST** write one content-free event for each blocked value and kind and each redacted value and kind, naming the tenant, the subject, the origin and the sensitive-data kind, so the data protection officer can count them per kind. Until the Audit gear ships, the events **MUST** go to the structured log through one interface that the Audit gear replaces.

**Implements**:
- `cpt-cf-construct-algo-admission-enforce`

**Verified by**: `a_block_is_logged_with_its_kind`, `a_redaction_is_logged_with_its_kind`, `an_admission_drop_is_logged_in_the_record_drop_shape_without_content` (capture the log), `a_plan_without_a_record_has_no_record_drop_event`, and the recorded events in `a_blocked_value_is_not_stored_the_rest_is_and_the_result_says_so` (no content) and `a_redacted_value_keeps_its_step_without_the_found_content`.

## 6. Acceptance Criteria

- [ ] A value the checks block, or a plan Admission drops, is not stored, and the result says so (`a_blocked_value_is_not_stored_the_rest_is_and_the_result_says_so`, `checks_then_admission_store_nothing_the_checks_block`, and every drop test, which checks the reason in the result).
- [ ] Admission makes no model call and writes nothing to graph storage (by its type, which holds neither client; checked by review).
- [ ] Plans written by people skip trust and the confidence floor and go through everything else (`a_plan_written_by_a_person_skips_trust_and_the_confidence_floor`, `a_plan_written_by_a_person_goes_through_every_other_rule`, `a_plan_written_by_a_person_goes_through_the_verdicts`).
- [ ] A replace whose new value is blocked keeps the old value (`a_replace_whose_new_value_is_blocked_is_dropped_whole_so_the_old_value_stays`).
- [ ] Each block, redaction and drop is an event without content (`a_block_is_logged_with_its_kind`, `a_redaction_is_logged_with_its_kind`, `an_admission_drop_is_logged_in_the_record_drop_shape_without_content`). A plan with no record behind it is audited by its caller (`a_plan_without_a_record_has_no_record_drop_event`).
- [ ] The tests named in this document pass.

## 7. Non-Functional Considerations

- **Security**: Admission takes no security context from the connector; processing builds its own for the tenant. The tenant's rules and the subject's settings are read on each plan, so a change applies to the next plan.
- **Data protection**: No event holds a value. A value that cannot be redacted exactly is dropped rather than stored with content that may remain.
- **Reliability**: A settings service or database that cannot answer decides nothing: the caller drops the record, and no rule is guessed.
- **Observability**: `admit` carries a tracing span with the tenant, the subject and the origin. Drops, blocks and redactions are warn-level events under `construct::audit`.
- **Not done in this part**: see the out-of-scope list in 1.2.
