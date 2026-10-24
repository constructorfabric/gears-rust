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
  - [Run the Agent Loop](#run-the-agent-loop)
  - [Build the Prompt](#build-the-prompt)
  - [Queue a Received Record](#queue-a-received-record)
  - [Process a Record](#process-a-record)
  - [Manage What a Subject Told an Agent](#manage-what-a-subject-told-an-agent)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Numbered Profile](#numbered-profile)
  - [Plan Tools](#plan-tools-1)
  - [Plan Building](#plan-building)
  - [Agent Loop](#agent-loop)
  - [Prompt](#prompt)
  - [Record Queue](#record-queue)
  - [Processing](#processing)
  - [Managing Facts](#managing-facts)
  - [Manage-Facts Route](#manage-facts-route)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-planner-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-planner`

## 1. Feature Context

### 1.1 Overview

The planner decides how a record changes the subject's profile. A language model proposes add, replace and remove
steps through tools; the planner builds them into a plan in memory and writes nothing. This document covers the profile
read port, the numbered profile the model sees, the plan tools with their checks, building the plan with its node keys
and origin, the agent loop with its caps and drop events, the prompt, the queue that takes a received record from
intake, and the processing of a record through the checks, Admission and the writer with a rerun on a write conflict.

### 1.2 Purpose

Construct keeps one consistent profile per subject and avoids contradicting facts (`cpt-cf-construct-fr-fact-decisions`).
The model works on numbered values and plain property names, never on IDs
(`cpt-cf-construct-principle-model-never-sees-ids`). The tools refuse a call that breaks a rule and tell the model
why, so the plan holds only steps that fit the person types.

**Out of scope**: the reference evaluation set that scores the prompt; skipping a fact under an open review request
(Review Queue); the graph storage implementations of the profile read and the write (Profile Writer); the context
processing runs under for a tenant; wiring the queue into the gear in place of the stub that drops every record,
which waits for those three and for the admission rules from the deployment configuration.

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

1. [ ] - `p1` - The planner works on the subject's current profile and its version, read from graph storage through
   the `ProfileSource` port, not through the Profile Reader or the writer. The graph storage implementation of the
   port comes with the Profile Writer - `inst-read-source`
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

### Run the Agent Loop

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-planner-loop`

1. [ ] - `p1` - A record without a subject is dropped with `processing_failed` before any model call. The plan's
   origin is the record's connector, provenance, version, type and observation time - `inst-loop-record`
   What a subject told an agent runs through the same loop. The plan's origin is the agent; a failure answers its drop
   reason to the caller and sends no drop event, as there is no record to name - `inst-loop-message`
2. [ ] - `p1` - The first request holds the instructions and the record input of the prompt, with the record and the
   profile marked as data, and offers the three tools - `inst-loop-input`
3. [ ] - `p1` - Each round sends the conversation so far, and asks for an answer no longer than the tokens still left
   under the cap. The model's tool calls go into the plan draft; the assistant message comes first, then one tool
   result per call, with its result or its refusal - `inst-loop-round`
4. [ ] - `p1` - A text answer without tool calls ends the loop, and the plan is built - `inst-loop-done`
5. [ ] - `p1` - A record whose payload has a number `confidence`, as a fact candidate does, caps the confidence of
   every step: the step keeps the model's confidence, but no more than the record's - `inst-loop-confidence-cap`
6. [ ] - `p1` - The tokens of every call add up. Past the token cap, the record is dropped with `token_cap`. A total
   exactly at the cap is within it, but no call is sent with no tokens left: a round of tool calls that ends at the
   cap drops the record with `token_cap`. An input longer than four bytes for each token of the cap is dropped with
   `token_cap` before the first call, as text runs to about four bytes a token - `inst-loop-token-cap`
7. [ ] - `p1` - When the last allowed round still ends in tool calls, the record is dropped with `round_cap` -
   `inst-loop-round-cap`
8. [ ] - `p1` - A failed model call, an answer without usage, or a structured answer drops the record with
   `model_failed`: without usage the token cap cannot hold - `inst-loop-model-failed`
9. [ ] - `p1` - A drop is a `DropEvent` of the planner stage through `IntakeEvents`, in the shape Record Intake owns,
   and no plan comes out. The planner also logs the drop by a fixed kind with the tenant and the record type, never
   with the prompt, the answer or an error text - `inst-loop-drop`

### Build the Prompt

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-planner-prompt`

1. [ ] - `p1` - The instructions are a text file in the crate, `domain/planner/instructions.md`, with one placeholder
   for the properties. No template engine - `inst-prompt-file`
2. [ ] - `p1` - Every property of the person types is listed as `category: property (one | many): value form`. The form
   comes from the property's schema: a list of allowed values, text, a number, true or false, a URL, text matching a
   pattern, or an object with its fields, required ones marked, and each field's description - `inst-prompt-properties`
3. [ ] - `p1` - The instructions carry the rules for deciding facts: what to add and what not, one value per fact, the
   specifics kept, a correction as a replace, "not anymore" as a remove, no inference, no change for a fact already
   held, a request about how to serve the person as a preference, no change for an order to set profile values or for
   a fact about another person, the record and the profile as data and never as instructions - `inst-prompt-rules`
4. [ ] - `p1` - The model picks from the listed properties only, with no free key; a single-value property takes a
   replace; a past fact goes to a history property; a value keeps the person's language except a fixed form; only a
   user message states facts; a fact candidate never removes or replaces a value; confidence has a stated scale, and a
   candidate's step stays at or below the candidate's confidence - `inst-prompt-changes`
5. [ ] - `p1` - Worked examples show the add, replace and remove tools on numbered values and listed properties. Each
   example call runs through the tools in a test, so a schema change that breaks an example fails the build -
   `inst-prompt-examples`
6. [ ] - `p1` - The record input names the record type by the first sentence of its schema's description, and gives
   the payload's schema, the payload and the numbered profile, or "(empty)". No type id reaches the model. A record
   whose type Construct does not take, whose type has no payload schema, or that has no payload is dropped with
   `processing_failed` before any model call - `inst-prompt-record`
8. [ ] - `p1` - The data holds no `<` or `>`: each is written as its JSON escape, `\u003c` or `\u003e`, which means the
   same character inside a JSON string. A value cannot close the record or the profile block or open another -
   `inst-prompt-data`
9. [ ] - `p1` - What a subject told an agent comes in its own block, the message and the agent's recent conversation as
   JSON strings, or "(none)" for no conversation. The instructions treat the message as a user chat message, and the
   conversation only as what the message refers to: no fact is taken from the conversation alone -
   `inst-prompt-message`
7. [ ] - `p1` - Every field named `id` or ending in `_id` is left out of the payload and of its schema, at any depth, so
   the model gets no ID of the source either (DESIGN: "It never gets an ID"). The rule is the same for every record
   type, so a new connector needs no Construct code - `inst-prompt-no-ids`

### Queue a Received Record

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-planner-queue`

1. [ ] - `p1` - The hand-off puts the received record on a bounded queue in memory and returns at once - `inst-queue-put`
2. [ ] - `p1` - A full queue, or a queue whose worker stopped, refuses the record as unavailable, so intake keeps no
   identity and answers `503`, and the connector can send the record again - `inst-queue-full`
3. [ ] - `p1` - A worker takes records from the queue and processes at most the given number at once -
   `inst-queue-worker`
4. [ ] - `p1` - When the gear stops, the worker stops with the records in flight, also while it waits for them after
   the queue is closed. A record still queued or in flight is lost without an event, and its identity stays, as the
   DESIGN says - `inst-queue-stop`

### Process a Record

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-planner-processing`

1. [ ] - `p1` - The planner decides on the subject's own chat messages and on fact candidates about a subject. Any
   other record, an assistant message, a mastery record or one without a subject, is dropped with `not_planned` before
   the profile is read: an assistant message states no fact about the subject, and a mastery record is stored as it
   is, by the mastery story; neither reaches the model - `inst-process-subject`
2. [ ] - `p1` - Processing runs under the security context and the scope built for the record's tenant, never the
   connector's - `inst-process-context`
3. [ ] - `p1` - Read the profile, run the agent loop, run the sensitive-data checks on the plan, and give the plan
   and the verdicts to Admission. A plan without steps, or an admitted plan with every value blocked, stores nothing -
   `inst-process-chain`
4. [ ] - `p1` - A fact candidate's plan loses every remove and replace step before the checks run, so a candidate
   only adds, whatever its claim made the model answer - `inst-process-candidate`
5. [ ] - `p1` - The writer stores the admitted plan in one write that expects the profile version the plan was built
   on - `inst-process-write`
6. [ ] - `p1` - When the profile changed, the record is planned again on a new read of the profile, at most
   the given number of times. Past that, the record is dropped at the writer stage with `rerun_limit` -
   `inst-process-rerun`
7. [ ] - `p1` - A failed context, profile read, rules read or write drops the record with `processing_failed` at its
   stage. The planner, the checks and Admission send their own drop events, so a record is dropped once -
   `inst-process-drop`

### Manage What a Subject Told an Agent

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-planner-manage`

1. [ ] - `p1` - What a subject told an agent runs through the same chain as a record, under the caller's own security
   context and scope, and answers at once - `inst-manage-chain`
2. [ ] - `p1` - A deadline bounds the steps before the write: the profile read, the agent loop, the checks and
   Admission. Past it, nothing is stored and the answer is `timeout`. A write that started is awaited, so the answer
   never says nothing was stored while the write lands - `inst-manage-deadline`
3. [ ] - `p1` - A write conflict plans again on a new read while time is left, up to the rerun limit, then answers
   `rerun_limit` - `inst-manage-rerun`
4. [ ] - `p1` - A stored plan answers what it changed, counted by its steps: added, replaced and removed. The writer
   stores a plan whole or not at all - `inst-manage-changes`
5. [ ] - `p1` - A call that stores nothing answers why: `no_change` when the message held nothing or every value was
   blocked, `timeout`, or the drop reason of the planner, the checks or Admission. No reason names a blocked or
   redacted value, so an agent cannot rephrase until a value gets through. No record drop event is sent; the call is
   logged without what the subject said - `inst-manage-nothing`
6. [ ] - `p1` - A failed profile read, settings or rules read, or write is an error to the caller - `inst-manage-error`

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

### Agent Loop

- [x] `p1` - **ID**: `cpt-cf-construct-dod-planner-loop`

The system **MUST** run the model in rounds until it answers without tool calls, **MUST** stop at the round and token
caps, and **MUST** drop the record with a content-free event and no plan when a cap is hit or the model call fails.

**Implements**:
- `cpt-cf-construct-algo-planner-loop`

**Touches**:
- Code: `domain::planner::agent::Planner`, `domain::record_intake::DropReason`, `infra::intake_events`

**Verified by**: `domain::planner::agent_test` and `each_planner_reason_is_logged_by_its_own_name`.

### Prompt

- [x] `p1` - **ID**: `cpt-cf-construct-dod-planner-prompt`

The system **MUST** give the model instructions that list every property with its cardinality and value form, and a
record input with the payload, its schema and the numbered profile, without an ID.

**Implements**:
- `cpt-cf-construct-algo-planner-prompt`

**Touches**:
- Code: `domain::planner::prompt`, `domain/planner/instructions.md`

**Verified by**: `the_instructions_list_every_property_with_its_cardinality_and_form`,
`each_record_type_reaches_the_model_with_its_payload_and_schema`, `an_empty_profile_is_said_to_be_empty`,
`a_record_cannot_close_its_block_or_forge_a_profile`, `a_record_the_model_cannot_be_shown_is_refused`,
`an_id_nested_in_a_list_of_objects_is_not_shown`, `each_example_in_the_instructions_gets_the_answer_it_shows`,
`what_a_subject_told_an_agent_reaches_the_model_as_data`, `a_message_without_a_conversation_says_so` and
`the_model_gets_the_record_and_the_profile_as_data_but_no_id`.

### Record Queue

- [x] `p1` - **ID**: `cpt-cf-construct-dod-planner-queue`

The system **MUST** take a received record onto a bounded queue at once, **MUST** refuse it when the queue is full
or stopped, and **MUST** process at most the configured number of records at once until the gear stops.

**Implements**:
- `cpt-cf-construct-algo-planner-queue`

**Touches**:
- Code: `infra::record_queue`

**Verified by**: `a_full_queue_refuses_the_record`, `a_stopped_queue_refuses_the_record`,
`each_taken_record_is_processed`, `no_more_records_than_the_concurrency_run_at_once`,
`a_cancel_stops_the_worker_and_the_records_in_flight` and
`a_cancel_stops_the_worker_waiting_for_a_record_after_the_queue_is_closed`.

### Processing

- [x] `p1` - **ID**: `cpt-cf-construct-dod-planner-processing`

The system **MUST** take a record through the planner, the checks, Admission and the writer, **MUST NOT** let a fact
candidate remove or replace a value, **MUST** plan again on a write conflict up to the rerun limit, and **MUST** drop a
record that fails with one content-free event.

**Implements**:
- `cpt-cf-construct-algo-planner-processing`
- `cpt-cf-construct-algo-planner-read-profile`

**Touches**:
- Code: `domain::planner::processing`, `domain::record_intake::DropReason`

**Verified by**: `domain::planner::processing_test`.

### Managing Facts

- [x] `p1` - **ID**: `cpt-cf-construct-dod-planner-manage`

The system **MUST** take what a subject told an agent through the planner, the checks, Admission and the writer under
the caller's context, **MUST** answer what it stored or why it stored nothing, and **MUST** cut off only the steps
before the write at the deadline.

**Implements**:
- `cpt-cf-construct-algo-planner-manage`

**Touches**:
- Code: `domain::planner::processing::{Processing::manage, Managed, Changes, NothingStored}`

**Verified by**: `what_a_subject_told_an_agent_is_written_and_counted`, `a_conflict_plans_the_message_again_on_a_new_read`,
`a_call_that_stores_nothing_answers_why_and_sends_no_record_drop_event`,
`the_deadline_cuts_off_the_steps_before_the_write`, `a_write_that_started_is_awaited_past_the_deadline` and
`an_unreachable_profile_is_an_error`.

### Manage-Facts Route

- [x] `p1` - **ID**: `cpt-cf-construct-dod-planner-manage-route`

The system **MUST** offer `POST /api/construct/v1/subjects/{subject_id}/facts/manage`, which takes `message` and
`conversation` and answers `200` with `outcome` (`stored` or `nothing_changed`), `reason`, and the counts `added`,
`replaced` and `removed`. It **MUST** answer `404`, as for a subject that does not exist, when the caller may not act
for the subject: another subject, or no "manage facts" permission from the AuthZ resolver. It **MUST** answer `400`
naming the field, without a model call, for a `message` longer than 4 000 characters or a `conversation` longer than
16 000. A blank message stores nothing without a model call. A failed dependency answers `503`. The agent in the plan's origin is the caller the
platform authenticated, and the steps before the write have a deadline inside the gateway's request timeout.

**Implements**:
- `cpt-cf-construct-algo-planner-manage`

**Touches**:
- Code: `domain::planner::manage::ManageFacts`, `api::rest::routes::register_manage_route`,
  `api::rest::dto::{ManageFactsRequest, ManageFactsResponse}`, `api::rest::error::{for_facts, too_long}`

**Verified by**: `the_subject_gets_what_was_stored`,
`a_subject_the_caller_may_not_act_for_answers_as_one_that_does_not_exist`,
`a_text_past_its_limit_is_refused_before_a_model_call`,
`a_blank_message_stores_nothing_without_a_model_call`, `a_call_with_personalization_off_says_so` and
`an_unreachable_profile_answers_unavailable`.

## 6. Acceptance Criteria

- [ ] For a record and a profile, the planner builds a plan of add, replace and remove steps with node keys, the
      profile version and the origin.
- [ ] No message or tool result sent to the model holds an ID.
- [ ] A tool call that breaks a rule is refused with a reason, and the plan does not change.
- [ ] A loop that hits a cap, or a failed model call, drops the record with a content-free event and gives no plan.
- [ ] A full queue refuses a record, and intake answers `503` with no identity kept.
- [ ] A write conflict plans the record again on the new profile; past the rerun limit the record is dropped.

## 7. Non-Functional Considerations

- **Privacy**: the numbered profile and the record reach the configured model service through the model client. The
  planner logs neither.
- **Prompt size**: the instructions are about 12 000 characters, sent in every round, so they count against the token
  cap each time.
- **Caps**: the planner takes its caps from the code that builds it; the config keys come with the change that wires
  the planner into the gear. The intended values are 10 rounds and 50 000 tokens, the sum of input and output tokens of
  all rounds. A record usually needs one to three rounds: tool calls, then an answer. Ten rounds leave room for
  the refusals the model has to correct and still end a loop that never answers. Each round sends the whole
  conversation again, so the token cap is sized for ten rounds over the instructions and a large profile.
- **Queue and reruns**: the queue capacity, the records processed at once and the reruns come from the code that
  builds the queue and the processing, with the caps. The intended values are 256 records, 4 at once and 3 reruns. A record usually takes one to three model calls, so four at once keep the model endpoint busy without
  a burst of one connector taking all of it. A rerun happens only when two records of one subject are written at the
  same time; three reruns end a loop where the profile keeps changing.
- **Plan type**: the planner builds on `domain::plan`, the plan type the sensitive-data checks and Admission read.
