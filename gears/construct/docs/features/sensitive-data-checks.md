# Feature: Sensitive-Data Checks


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Run the Checks on a Plan](#run-the-checks-on-a-plan)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Checks Interface](#checks-interface)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-sensitive-data-checks-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-sensitive-data-checks`

## 1. Feature Context

### 1.1 Overview

The Sensitive-Data Checks (`cpt-cf-construct-component-sensitive-data-checks`) look for special-category content in the values a plan would store, before anything is stored. There is one check per sensitive-data kind, and each check answers with a verdict: for each value, content found or not. Admission enforces the verdicts. This part builds the interface, the runner that runs the checks in parallel, and a test check that can block. **No real check exists yet.** Until the owner of the checks is settled, a deployment runs none, so every value is allowed (see 1.2).

### 1.2 Purpose

The PRD requires Construct to keep special-category data out of the profile (`cpt-cf-construct-fr-sensitive-data-guardrails`). Graph storage embeds text on ingest, so nothing may reach storage before the verdicts are enforced (`cpt-cf-construct-principle-verdicts-before-storage`). The checks find the content and Admission acts on it.

**The kinds and their actions.** The ten sensitive-data kinds and the action for each come from the DESIGN's Sensitive-Data Verdict entity (`cpt-cf-construct-entity-sensitive-data-verdict`). Personal IDs, finance, contacts and location are redacted. Health, biometrics, credentials, politics, sex and other protected data are blocked. The action belongs to the kind, not to the check, so a check cannot weaken it.

**The text pieces.** A value is JSON, as the node stores it: plain text, or an object of fields such as a skill's name and level. The checks see each value's text pieces: every string and every number inside it, as text. The keys of an object are its type's field names and are not checked; booleans and nulls carry no text. So a check never sees JSON syntax, and a redaction can never break it.

**The verdict.** A check answers for its own kind with the text pieces it found content in, named by their place in the plan: the step, the property and the piece. For a kind that redacts, the answer also gives the byte ranges of the found content in the piece. A piece the answer does not name is allowed. A check answers about the values the plan would store; a removal stores nothing and is not checked.

**The plan.** The checks read a plan only through the list of values it would store. The Plan type is defined here, and the Planner builds plans in it; see [Admission](./admission.md), 1.2.

**Known gap: no real check in production.** The checks need an owner: who builds the pattern and model checks is still open, and nothing builds them yet. Until that is settled, a deployment wires in no check, so every value is allowed. Failing closed instead would mean Construct stores nothing anywhere until the real checks exist: no administrator adds, and no working round trip from a record to a stored fact. Nothing reaches storage through this feature yet in any case: until the Planner exists, the hand-off stub drops every received record.

**Out of scope**:

- The real checks: the patterns, which kinds also get a model check, and the model checks' prompts and caps. They wait for an owner.
- The reference test set that scores detection per kind against the thresholds of `cpt-cf-construct-nfr-guardrail-detection`; it needs the real checks.
- Enforcing the verdicts, and the audit events for each block and redaction. Admission does both.
- Wiring the runner into processing. The Planner does that when it hands its plan on.

**Requirements**: `cpt-cf-construct-fr-sensitive-data-guardrails` (partial: the interface and the runner; no detection). `cpt-cf-construct-nfr-guardrail-detection` is not touched.

**Principles**: `cpt-cf-construct-principle-verdicts-before-storage` (shared with Admission, which enforces the verdicts).

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-llm-gateway` | Answers the model checks, once they exist. No check calls it in this part. |
| `cpt-cf-construct-actor-audit` | Takes the drop event when a check fails, once it ships; until then the event goes to the structured log. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), the Sensitive-Data Checks component and the Sensitive-Data Verdict entity
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.6
- **Dependencies**: `cpt-cf-construct-feature-planner` (the plan; defined here, and the Planner builds on it), `cpt-cf-construct-feature-model-client` (for the model checks, which do not exist yet)

## 2. Actor Flows (CDSL)

**Use cases**: None. The checks are internal; the Planner, and later the Review Queue and the administrator operations, call them in process.

## 3. Processes / Business Logic (CDSL)

### Run the Checks on a Plan

- [x] `p1` - **ID**: `cpt-cf-construct-algo-sensitive-data-checks-run`

**Input**: A plan, and the checks the deployment wires in.

**Output**: One verdict per check, or a dropped record.

**Steps**:
1. [x] - `p1` - Take the text pieces of the values the plan would store, each with its place in the plan (step, property and piece). A removal has none - `inst-run-values`
2. [x] - `p1` - Run every check on the values at the same time, and wait for all of them. There is no switch to turn the checks off; a deployment chooses only which checks exist - `inst-run-parallel`
3. [x] - `p1` - **IF** a check fails, or answers for another kind or about a value the plan does not hold: for a plan built from a record, write the drop event (`cpt-cf-construct-algo-record-intake-drop-event`) with stage `checks` and reason `check_failed`; then **RETURN** dropped. A plan with no record behind it gets no drop event; its caller gets the dropped answer and audits it. The warning in the log names the kind, never a value - `inst-run-failed`
4. [x] - `p1` - **RETURN** the verdicts - `inst-run-return`
5. [x] - `p1` - The action of each verdict is the one the DESIGN gives its kind: redact for personal IDs, finance, contacts and location; block for the others - `inst-run-action`

## 4. States (CDSL)

None. The checks keep no state.

## 5. Definitions of Done

### Checks Interface

- [x] `p1` - **ID**: `cpt-cf-construct-dod-sensitive-data-checks-interface`

The system **MUST** define one check interface per sensitive-data kind that answers with a verdict naming the text pieces with found content and, for a kind that redacts, the byte ranges of that content. It **MUST** run every check on the text pieces of the values a plan would store, never on keys or JSON syntax, in parallel, with no switch to turn them off, and **MUST** drop the plan when a check fails or gives an answer that does not fit the plan, with the content-free drop event of Record Intake (stage `checks`) for a plan built from a record. The action of each kind **MUST** be the DESIGN's. A test check that can block **MUST** exist; a deployment **MAY** run no check until the real ones exist, and this part wires in none.

**Implements**:
- `cpt-cf-construct-algo-sensitive-data-checks-run`

**Touches**:
- Entities: Sensitive-Data Verdict (`cpt-cf-construct-entity-sensitive-data-verdict`), Plan (`cpt-cf-construct-entity-plan`)

**Verified by**: `every_kind_has_the_action_the_design_gives_it`, `the_values_are_those_the_plan_would_store_and_removals_have_none`, `an_object_value_gives_its_strings_and_numbers_but_not_its_keys`, `without_checks_every_value_is_allowed`, `each_check_answers_for_the_values_it_found_content_in`, `the_checks_run_in_parallel` (three checks that each wait for the other two), `a_failed_check_drops_the_record_with_a_drop_event`, `a_failed_check_on_a_plan_without_a_record_drops_it_without_a_record_event`, `an_answer_for_another_kind_drops_the_record`, `a_finding_in_a_value_the_plan_does_not_hold_drops_the_record`, `a_finding_in_a_piece_its_value_does_not_have_drops_the_record` (runner), and `checks_then_admission_store_nothing_the_checks_block` (with Admission). The checks are test checks that match a fixed text; no real check exists.

## 6. Acceptance Criteria

- [ ] A value a check blocks is not stored, and the result says so (`checks_then_admission_store_nothing_the_checks_block`, with [Admission](./admission.md)).
- [ ] The checks run in parallel, on the values the plan would store only (`the_checks_run_in_parallel`, `the_values_are_those_the_plan_would_store_and_removals_have_none`).
- [ ] A failed check drops the record with a content-free drop event, and nothing is stored (`a_failed_check_drops_the_record_with_a_drop_event`, `a_failed_check_on_a_plan_without_a_record_drops_it_without_a_record_event`).
- [ ] Without checks every value is allowed, and the PR states this as a known gap (`without_checks_every_value_is_allowed`).
- [ ] The tests named in this document pass.

## 7. Non-Functional Considerations

- **Security**: The action belongs to the kind, so no check can turn a block into a redaction. An answer that does not fit the plan drops the record rather than being guessed at.
- **Data protection**: No event and no log line holds a value: the drop event names the tenant, subject, connector, record identity, type, stage and reason, and the warning names the kind.
- **Reliability**: One failed check drops the whole record, so a value is never stored unchecked by a check that exists.
- **Performance**: The checks run at the same time, so the slowest check sets the time.
- **Not done in this part**: see the out-of-scope list and the known gap in 1.2.
