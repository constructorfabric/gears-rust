# Decomposition: Construct


<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Gear Foundation - HIGH](#21-gear-foundation---high)
  - [2.2 Model Client - HIGH](#22-model-client---high)
  - [2.3 Subject Settings - HIGH](#23-subject-settings---high)
  - [2.4 Record Intake - HIGH](#24-record-intake---high)
  - [2.5 Planner - HIGH](#25-planner---high)
  - [2.6 Sensitive-Data Checks - HIGH](#26-sensitive-data-checks---high)
  - [2.7 Admission - HIGH](#27-admission---high)
  - [2.8 Profile Writer - HIGH](#28-profile-writer---high)
  - [2.9 Profile Reader - HIGH](#29-profile-reader---high)
  - [2.10 Subject Control - HIGH](#210-subject-control---high)
  - [2.11 Review Queue - HIGH](#211-review-queue---high)
  - [2.12 Administrator Fact Management - MEDIUM](#212-administrator-fact-management---medium)
  - [2.13 Agent Access over MCP - MEDIUM](#213-agent-access-over-mcp---medium)
  - [2.14 Erasure - HIGH](#214-erasure---high)
  - [2.15 Retention and Tenant Exit - MEDIUM](#215-retention-and-tenant-exit---medium)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-construct-status-overall`

## 1. Overview

This document splits the Construct design (`cpt-cf-construct-design-construct-gear`) into features. Each feature is a unit that can later get its own FEATURE document and be built and tested on its own. The sources are the [PRD](./PRD.md), the [DESIGN](./DESIGN.md) and three ADRs: [`cpt-cf-construct-adr-construct-is-a-gear`](./ADR/0001-cpt-cf-construct-adr-construct-is-a-gear.md), [`cpt-cf-construct-adr-graph-storage-as-is`](./ADR/0002-cpt-cf-construct-adr-graph-storage-as-is.md) and [`cpt-cf-construct-adr-one-model-interface`](./ADR/0003-cpt-cf-construct-adr-one-model-interface.md). The PRD is canonical for requirements, the DESIGN for components, tables and sequences, and the ADRs for decisions; this document follows them.

**Strategy.** The cuts follow the DESIGN's own component boundaries, in the order data flows through Construct:

1. The gear foundation.
2. The model client and the subject settings, which other features read.
3. The record pipeline: Record Intake, the Planner, the Sensitive-Data Checks, Admission and the Profile Writer.
4. The read path.
5. Subject control, split into the subject's own view and delete, the Review Queue and the administrator operations.
6. Agent access over MCP (Model Context Protocol, the protocol AI agents use to call tools).
7. Erasure, then retention.

The DESIGN's largest component, subject control, is split across four features. Subject Control owns the component; Subject Settings, the Review Queue and Administrator Fact Management extend it.

**The gear foundation is built.** The backend shell already exists in `gears/construct/construct` and `gears/construct/construct-sdk`. The feature `cpt-cf-construct-feature-gear-foundation` describes that code as it is. Its FEATURE document is [features/gear-foundation.md](./features/gear-foundation.md), and the shell code carries `@cpt` markers for it. It is a placeholder, not a DESIGN element. Its note table and entity have no DESIGN id and are not counted as Data. It registers its REST route with OperationBuilder (the toolkit's builder for REST operations) and its client in ClientHub (the platform's registry of in-process clients).

**Interfaces and contracts.** The REST API (`cpt-cf-construct-interface-rest-api`) is realized by the features that add routes. The MCP tools (`cpt-cf-construct-interface-mcp-tools`) are realized by Agent Access over MCP. The Rust SDK (`cpt-cf-construct-interface-rust-sdk`) has one primary owner, the Gear Foundation, which holds the client trait; each later feature extends it. The interface, contract and topology ids in this paragraph are tracked in prose and have no checkbox line. The deployment topology (`cpt-cf-construct-topology-deployment`) is Construct running as a gear on the platform. Record Intake takes the connector record types (`cpt-cf-construct-contract-gts-record`), and the Profile Writer uses Construct's person types (`cpt-cf-construct-contract-person-types`). Both are GTS (Global Type System) types, the platform's versioned and registered type identifiers.

**External prerequisites.** These are not Construct features. They are named here because features depend on them:

- The graph storage SDK client, which stores and serves the profile graph. Construct uses its compare-and-set (a write that succeeds only while a node still has the expected version) as it is. Erasure, retention and tenant exit also need graph storage's purge and tenant offboarding, which ship after its first version.
- The Audit gear, which holds all of Construct's audit events. Construct has no audit component, requirement or table of its own; each feature that writes audit events calls the Audit gear.
- The LLM gateway, or any service with the OpenAI chat completions API, behind the model client.
- The types registry, the settings service, the platform AuthN and AuthZ resolvers, OAGW (the platform's outbound API gateway) and the cluster leader election, as listed in the DESIGN.

**Ownership and overlaps.** Each requirement, component, table and sequence has one primary owning feature. Where another feature touches the same item, the entry says `shared` and names the owner. A shared line is ticked when the part of the item that its feature owns is done; the definition itself is ticked when every part is done. The main overlaps are:

- `cpt-cf-construct-fr-subject-delete`: the subject's single-fact delete belongs to Subject Control; the erase-everything path belongs to Erasure, which carries the component `cpt-cf-construct-component-deletion`. Both run the same delete rules.
- `cpt-cf-construct-fr-fact-origin`: the Profile Writer stores the origin with each fact. The Profile Reader, Subject Control, the Review Queue, Administrator Fact Management and Agent Access over MCP set or return it for their own fact sources.
- `cpt-cf-construct-fr-sensitive-data-guardrails`: the Sensitive-Data Checks own detection. Admission enforces block and redact. The Review Queue, Administrator Fact Management and Agent Access over MCP send their values through both.
- `cpt-cf-construct-fr-tenant-isolation` and `cpt-cf-construct-fr-access-control`: the foundation owns the platform pattern (tenant from the security context, one permission per operation). Every later feature applies it to its own operations. These two requirements are deliberately not repeated as shared lines in the later entries.
- The review request hides a fact in the reader, makes the planner skip it, and closes as deleted when its fact is deleted or removed. The Review Queue creates the table, adds the two checks to the reader and the planner when it lands, and owns the closing. When it lands it also adds the closing call to the Subject Control delete path. Retention calls the Review Queue itself, as it depends on it through Erasure. Until the Review Queue lands no fact is under review. So the reader, the planner and Subject Control stay free of a dependency on the review table.
- The entity Profile: the Profile Writer owns it. The Planner, the Profile Reader and Subject Control read it through the graph storage client, not through the writer, so this adds no dependency edge.
- `cpt-cf-construct-fr-subject-access` includes review requests and settings in the subject's view. Subject Control owns the view; Subject Settings and the Review Queue add their data to it.
- The sequence `cpt-cf-construct-seq-record-in` runs through Record Intake, the Planner, the Sensitive-Data Checks, Admission and the Profile Writer, and it also uses the Model Client and the Subject Settings. Intake owns it and the later pipeline features complete its steps. It is complete only when features 2.4 to 2.8 exist.
- The four paged list reads: Subject Control owns the subject's facts list and the shared pagination mechanism. The Review Queue owns the subject's review requests and the open review requests. Administrator Fact Management owns the administrator read of a subject's facts.

**Priorities.** In the PRD every requirement is `p1`. A feature's priority here sets its build order only. `p1` features form the core path that a connector, a reader and a subject need first. `p2` features build on that path. The priority marker on a referenced item shows the build-order tier of the feature that owns it, not the item's own priority in the PRD or DESIGN. A shared reference carries the tier of its primary owner.

**Ordering and parallelism.** The dependency graph is in section 3. After the foundation, the model client and the settings can start together. Intake needs the settings. The planner needs the model client and intake, because it builds the entry point that takes a received record. The reader needs the settings. The Sensitive-Data Checks, Admission and the writer follow the planner as a chain; Admission also needs the settings, and the writer also lists intake directly. Subject control needs the reader and the settings. The Review Queue needs subject control, the writer, Admission and the checks. The administrator operations need the Review Queue, Subject Control, the Profile Writer and Admission. MCP needs the reader, the writer and the planner. Erasure needs subject control, intake and the Review Queue. Retention needs erasure and the reader. So intake and the reader can start together once the settings exist, beside the model client, and subject control can be built beside the planner chain. The planner waits for both the model client and intake. MCP can start as soon as the writer and the reader exist. Once the Review Queue exists, the administrator operations and erasure can be built in parallel.

**Decisions taken in this document.** These choices concern which feature owns a piece of work. They do not change a component, a table or a sequence of the DESIGN. The design owner may overrule any of them:

- Review hooks (with the drop audit event, one of the two choices closest to a design call): the Review Queue creates the review table and, when it lands, adds the check that hides a fact under review to the Profile Reader and the check that skips it to the Planner.
- Profile entity: the Profile Writer owns it, and the Planner, the Profile Reader and Subject Control read it through graph storage, so they get no dependency on the writer.
- Hand-off from intake to the planner: Record Intake defines the interface that hands a received record to the planner, and the Planner builds the entry point that implements it, so the Planner depends on Record Intake.
- One-step plan path: the Review Queue owns the path that turns an add or an edit into a plan with one add or replace step, and Administrator Fact Management uses it.
- Root node of a new subject: the Profile Writer owns writing the root node, including creating it for a new subject, in the same write as the facts.
- Drop audit event (with the review hooks, one of the two choices closest to a design call): Record Intake owns the shape of the content-free drop audit event, and the Planner, the Sensitive-Data Checks, Admission and the Profile Writer reuse it.
- Placeholder retirement: Record Intake, the first feature that adds a real route, replaces the foundation's placeholder route, client operations and table. Subject Settings adds the first of Construct's own tables and leaves the placeholder in place.
- Node keys: the Planner gives new node keys, including to a returning deleted entity, and the one-step path of the Review Queue gives them the same way. The Profile Writer stores the keys the plan carries and finds the new root key after an erasure.
- Erase route: Erasure owns the erase route and the call into Deletion, although the DESIGN credits the hand-off to the Subject control component, because Subject Control is built earlier.
- Closing call from retention: Retention owns the closing call into the Review Queue for the facts it removes.

## 2. Entries

### 2.1 [Gear Foundation](features/gear-foundation.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-construct-feature-gear-foundation`

- **Purpose**: The gear shell on which every other feature is built. It gives Construct the standard gear anatomy, one authenticated route, tenant-scoped storage and a client in ClientHub. It proves the platform pattern that all later features copy: caller and tenant come from the platform, and each operation has its own permission. It already exists in code.

- **Depends On**: None

- **Scope**:
  - The SDK crate in `construct-sdk` (package `cf-gears-construct-sdk`) with the client trait `ConstructClientV1` and its models, and the gear crate in `construct` (package `cf-gears-construct`) with API, domain and infrastructure layers (`cpt-cf-construct-adr-construct-is-a-gear`)
  - One authenticated REST route that creates a foundation note in the caller's tenant, registered with OperationBuilder (the toolkit's builder for REST operations)
  - A tenant-scoped entity `FoundationNote` over the secure ORM (the toolkit's tenant-scoped database layer), with migrations the gear supplies at runtime
  - The table `construct__foundation_notes`. Database objects follow the `construct__` naming prefix of the object-namespacing ADR
  - A client registered in ClientHub (the platform's registry of in-process clients) for in-process callers, with operations to create and to read one note
  - One permission per operation, checked through the AuthZ resolver, and the tenant taken only from the security context
  - RFC 9457 Problem errors through the toolkit error mapping

- **Out of scope**:
  - Any DESIGN table, entity, component or sequence. The note table and entity are a placeholder that Construct's own model replaces; they have no DESIGN id
  - Construct's own permissions and routes for records, profiles and subjects
  - The Rust SDK interface of the full design (`cpt-cf-construct-interface-rust-sdk`), which grows with each later feature; the foundation owns the client trait of the shell only
  - Retiring the placeholder route, client operations and table: the first feature that adds a real route replaces them, which is Record Intake (`cpt-cf-construct-feature-record-intake`)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-construct-fr-tenant-isolation`
  - [ ] `p1` - `cpt-cf-construct-fr-access-control`

- **Design Principles Covered**:

  - None. The shell is a placeholder and applies no DESIGN principle.

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - `FoundationNote` (placeholder, no DESIGN id)

- **Design Components**:

  - None. The shell is not a DESIGN component. It realizes `cpt-cf-construct-adr-construct-is-a-gear` and the gear anatomy of `cpt-cf-construct-tech-rust-gear`.

- **API**:
  - POST /construct/v1/foundation-notes (as registered in code; the DESIGN's public paths carry an `/api` prefix that a deployment sets in the gateway's `prefix_path`)

- **Sequences**:

  - None.

- **Data**:

  - None. `construct__foundation_notes` is a placeholder without a DESIGN id and is not counted as Data.

### 2.2 [Model Client](features/model-client.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-construct-feature-model-client`

- **Purpose**: One small interface through which Construct calls language models, so the rest of Construct never depends on one model service. The planner and the sensitive-data checks both need it.

- **Depends On**: `cpt-cf-construct-feature-gear-foundation`

- **Scope**:
  - The model interface: messages and tools go in; text, tool calls or a structured answer come out (`cpt-cf-construct-adr-one-model-interface`)
  - The chat completions adapter, through OAGW (the platform's outbound API gateway), for OpenAI-compatible servers
  - The LLM gateway adapter, used once the gateway runs
  - Choosing the adapter by deployment configuration
  - No knowledge of records, plans or verdicts in the model client

- **Out of scope**:
  - Prompts, the planner loop and the sensitive-data checks
  - Approving models or endpoints; model policy belongs to the LLM gateway
  - Cap values and model-call timeouts, which the design leaves to the feature design

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-construct-nfr-guardrail-detection` (shared; the model calls of the model checks. Primary owner: `cpt-cf-construct-feature-sensitive-data-checks`)

  The component also supports `cpt-cf-construct-fr-fact-decisions`, which the Planner owns.

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-construct-principle-one-model-interface`

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - None.

- **Design Components**:

  - [ ] `p1` - `cpt-cf-construct-component-model-client`

- **API**:
  - None. The interface is internal; no REST route.

- **Sequences**:

  - None.

- **Data**:

  - None.

### 2.3 [Subject Settings](features/subject-settings.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-construct-feature-subject-settings`

- **Purpose**: Keeps, per subject, whether personalization is on and whether an erasure is under way. Intake, admission and the reader all read this state. It is also the first of Construct's own tables, so it carries the shared schema unit.

- **Depends On**: `cpt-cf-construct-feature-gear-foundation`

- **Scope**:
  - The table `construct__subject_settings` through toolkit-db, scoped to the tenant, with the `construct__` naming of the table, its indexes and its constraints
  - Read and change of a subject's personalization setting by the subject
  - A new subject starting with the tenant's default, read from the settings service
  - Personalization off and an erasure under way as stored state that other features read
  - A setting change applies from the next request
  - The subject's settings shown in the subject's view and included in the export (the view and the export themselves belong to subject control)

- **Out of scope**:
  - Refusing records, dropping plans and hiding facts when personalization is off; intake, admission and the reader enforce that
  - Marking and clearing the erasure state; erasure owns that
  - Tenant settings storage, which belongs to the settings service
  - The other two own tables, which belong to Record Intake and the Review Queue

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-construct-fr-settings`
  - [ ] `p1` - `cpt-cf-construct-fr-subject-access` (shared; the settings in the subject's view. Primary owner: `cpt-cf-construct-feature-subject-control`)

- **Design Principles Covered**:

  - None.

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - Subject Settings

- **Design Components**:

  - [ ] `p1` - `cpt-cf-construct-component-subject-control` (shared; the settings part. Primary owner: `cpt-cf-construct-feature-subject-control`)

- **API**:
  - GET /api/construct/v1/subjects/{subject_id}/settings
  - PUT /api/construct/v1/subjects/{subject_id}/settings

- **Sequences**:

  - None.

- **Data**:

  - [ ] `p1` - `cpt-cf-construct-db-own-tables` (primary owner; the shared schema unit. Record Intake and the Review Queue add their tables)
  - [ ] `p1` - `cpt-cf-construct-dbtable-subject-settings`
  - [ ] `p1` - `cpt-cf-construct-entity-subject-settings`

### 2.4 [Record Intake](features/record-intake.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-construct-feature-record-intake`

- **Purpose**: The single entry for connectors. It takes one record per request, checks it, and answers at once with received, repeat or refused. It decides which records reach processing and keeps no record content. It realizes the DESIGN's Results API component.

- **Depends On**: `cpt-cf-construct-feature-gear-foundation`, `cpt-cf-construct-feature-subject-settings`

- **Scope**:
  - Checking the record envelope and its GTS (Global Type System) type, with its base types, through the types registry
  - Receiving the connector record types that derive from the shared record base type `gts.cf.connectors.core.record.v1~` (`cpt-cf-construct-contract-gts-record`)
  - Refusal with the type, the place in the record and the broken rule, for a broken envelope or type, a connector that is off, personalization off or an erasure under way
  - The table `construct__record_ids`; inserting the record identity is the repeat check, also for the record of a deleted fact
  - Answers received, repeat or refused; a repeat never changes a fact
  - Connector on or off read from the settings service
  - Holding the content of a received record only in memory, for the background processing that the Planner's entry point starts
  - The hand-off: the interface through which a received record is handed to the planner
  - The shape of the content-free drop audit event, which the Planner, the Sensitive-Data Checks, Admission and the Profile Writer reuse
  - Loss of a received record without an audit event when an instance stops
  - Depending only on the base record shape, never on a connector's type
  - Replacing the foundation's placeholder route, client operations and table, in the same migration that creates `construct__record_ids`

- **Out of scope**:
  - Deciding what a record means (planner), checks and admission rules, and storage
  - Erasing record identities (erasure) and retention of them (retention)
  - The entry point that takes a record through the hand-off and starts the background processing; the Planner builds it
  - The exact wire status codes for the three answers, left to the feature design

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-construct-fr-record-intake`
  - [ ] `p1` - `cpt-cf-construct-fr-settings` (shared; records refused while personalization is off. Primary owner: `cpt-cf-construct-feature-subject-settings`)

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-construct-principle-base-record-only`

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - Record

- **Design Components**:

  - [ ] `p1` - `cpt-cf-construct-component-results-api`

- **API**:
  - POST /api/construct/v1/records

- **Sequences**:

  - [ ] `p1` - `cpt-cf-construct-seq-record-in` (primary owner; complete only when features 2.4 to 2.8 exist, as the planner, checks, admission and writer features complete its later steps)

- **Data**:

  - [ ] `p1` - `cpt-cf-construct-db-own-tables` (shared; the table `construct__record_ids`. Primary owner: `cpt-cf-construct-feature-subject-settings`)
  - [ ] `p1` - `cpt-cf-construct-dbtable-record-ids`
  - [ ] `p1` - `cpt-cf-construct-entity-record`

### 2.5 [Planner](features/planner.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-construct-feature-planner`

- **Purpose**: Decides how a record changes the subject's profile. A language model proposes a plan of add, replace and remove steps; the planner builds it in memory and writes nothing. This is how Construct keeps one consistent profile and avoids contradicting facts.

- **Depends On**: `cpt-cf-construct-feature-gear-foundation`, `cpt-cf-construct-feature-model-client`, `cpt-cf-construct-feature-record-intake`

- **Scope**:
  - The entry point that takes a received record through the hand-off interface that Record Intake defines and starts the planning; a failed record is dropped
  - Reading the subject's current profile and its version from graph storage
  - The agent loop: the model calls a fixed set of tools in rounds, and the tools only add steps to a plan in memory
  - Numbered values and plain keys for the model, never IDs; mapping each number back to its node key, and giving each new entity a new node key, also an entity that returns after a delete (the soft-deleted key cannot be reused)
  - Plans that touch only the record's subject; a replace removes the old value in the same plan
  - Caps on rounds and tokens; a dropped record when a cap is hit or the loop fails, with an audit event without content in the drop event shape that Record Intake owns
  - Running again on a write conflict, up to a small limit
  - The plan entity with its origin, the profile version and each step's confidence

- **Out of scope**:
  - Sensitive-data checks, admission and storage
  - Skipping a fact under an open review request; added by the Review Queue when it lands (`cpt-cf-construct-feature-review-queue`)
  - Plans without a planner (reviewer's corrected value, administrator fact), which go through the one-step plan path of the Review Queue
  - Cap values and the rerun limit, left to the feature design

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-construct-fr-fact-decisions`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-construct-principle-model-never-sees-ids`

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - Plan
  - Profile (shared; reads only, through the graph storage client, so no dependency on the writer. Primary owner: `cpt-cf-construct-feature-profile-writer`)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-construct-component-planner`

- **API**:
  - None. The planner is internal and runs after intake or an MCP call.

- **Sequences**:

  - [ ] `p1` - `cpt-cf-construct-seq-entity-replaced` (shared; the replace step in the plan. Primary owner: `cpt-cf-construct-feature-profile-writer`)
  - [ ] `p1` - `cpt-cf-construct-seq-record-in` (shared; the plan for a received record. Primary owner: `cpt-cf-construct-feature-record-intake`)

- **Data**:

  - [ ] `p1` - `cpt-cf-construct-entity-plan`

### 2.6 [Sensitive-Data Checks](features/sensitive-data-checks.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-construct-feature-sensitive-data-checks`

- **Purpose**: Finds special-category content in the values a plan would store, before anything is stored. The profile must not become a store of sensitive data, and graph storage embeds text on ingest, so a late clean-up would be too late.

- **Depends On**: `cpt-cf-construct-feature-planner`, `cpt-cf-construct-feature-model-client`

- **Scope**:
  - One check per sensitive-data kind: personal IDs, health, finance, biometrics, credentials, contacts, location, politics, sex and other protected data
  - Each check is a pattern check in code, a model check with a structured answer, or both; model checks get the values as a tool call result, never in the prompt
  - Checks run in parallel and each returns block, redact or allow
  - The verdict entity with the block or redact action for each kind
  - A dropped record when a check fails or hits a cap, with an audit event without content in the drop event shape that Record Intake owns
  - No switch to turn the checks off
  - The reference test set that scores detection per sensitive-data kind against the thresholds of `cpt-cf-construct-nfr-guardrail-detection`

- **Out of scope**:
  - Enforcing the verdicts, which Admission does
  - The audit event for each block and redaction, written by Admission
  - The exact patterns and the list of kinds that also get a model check, left to the feature design

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-construct-fr-sensitive-data-guardrails`
  - [ ] `p1` - `cpt-cf-construct-nfr-guardrail-detection`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-construct-principle-verdicts-before-storage`

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - Sensitive-Data Verdict

- **Design Components**:

  - [ ] `p1` - `cpt-cf-construct-component-sensitive-data-checks`

- **API**:
  - None. The checks are internal.

- **Sequences**:

  - [ ] `p1` - `cpt-cf-construct-seq-record-in` (shared; the verdicts for a received record's plan. Primary owner: `cpt-cf-construct-feature-record-intake`)

- **Data**:

  - [ ] `p1` - `cpt-cf-construct-entity-sensitive-data-verdict`

### 2.7 [Admission](features/admission.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-construct-feature-admission`

- **Purpose**: The deterministic code that decides what is stored. The model only proposes; Admission checks the plan against the tenant's rules and the verdicts, and drops or trims it. Nothing reaches storage without passing here.

- **Depends On**: `cpt-cf-construct-feature-sensitive-data-checks`, `cpt-cf-construct-feature-subject-settings`

- **Scope**:
  - Source trust, confidence floor, allowed entity kinds and write caps, read from the settings service as admission rules
  - Personalization still on and no erasure under way, read from the subject settings
  - Dropping the whole plan when any of these admission checks fails
  - Enforcing verdicts on affected values only: block drops the value and keeps the rest; redact removes the found content
  - An audit event without content for each block, redaction and drop, naming the sensitive-data kind for a block or redaction, so the data protection officer can count them; a drop uses the drop event shape that Record Intake owns
  - Passing the admitted plan to the profile writer
  - Making no model call and writing nothing to graph storage

- **Out of scope**:
  - Detecting sensitive data (Sensitive-Data Checks) and storing the plan (Profile Writer)
  - How trust and the confidence floor apply to reviewer, administrator and agent plans; the FEATURE document of Admission resolves it

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-construct-fr-sensitive-data-guardrails` (shared; enforcement part. Primary owner: `cpt-cf-construct-feature-sensitive-data-checks`)
  - [ ] `p1` - `cpt-cf-construct-fr-settings` (shared; plans dropped while personalization is off. Primary owner: `cpt-cf-construct-feature-subject-settings`)

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-construct-principle-model-proposes-code-decides`

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - Plan (shared; owned by `cpt-cf-construct-feature-planner`)
  - Sensitive-Data Verdict (shared; owned by `cpt-cf-construct-feature-sensitive-data-checks`)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-construct-component-admission`

- **API**:
  - None. Admission is internal.

- **Sequences**:

  - [ ] `p1` - `cpt-cf-construct-seq-record-in` (shared; the admission of a received record's plan. Primary owner: `cpt-cf-construct-feature-record-intake`)
  - [ ] `p1` - `cpt-cf-construct-seq-entity-replaced` (shared; the admission of the replace plan. Primary owner: `cpt-cf-construct-feature-profile-writer`)

- **Data**:

  - None. Admission only reads the subject settings table, which belongs to `cpt-cf-construct-feature-subject-settings`.

### 2.8 [Profile Writer](features/profile-writer.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-construct-feature-profile-writer`

- **Purpose**: Stores an admitted plan in graph storage whole and in order. It keeps two records for one subject from mixing, and stores the origin and storing time with every fact. With this feature, the record pipeline from Record Intake to storage is complete.

- **Depends On**: `cpt-cf-construct-feature-admission`, `cpt-cf-construct-feature-planner`, `cpt-cf-construct-feature-record-intake`

- **Scope**:
  - One graph storage write per plan, with the root node's expected version and graph storage's idempotency key
  - A write whose result is unknown is sent again with the same key; a write after a conflict is a new write with a new key
  - Storing the node keys that the plan carries; the Writer does not assign them (the Planner and the one-step path of the Review Queue do), so storing the same plan again gives the same facts
  - Rejection on a changed profile, and a rerun of the planner up to a small limit; a dropped record past the limit, with an audit event without content in the drop event shape that Record Intake owns
  - Origin and storing time stored with each entity
  - The profile entity: the subject node as root, and entities that hang from it
  - Writing the profile's root node, including creating it for a new subject, in the same single write as the facts. How two first writes for the same new subject are kept from both succeeding is settled in the FEATURE document of the Profile Writer, as the DESIGN does not specify it
  - Finding the new root key of a profile created after an erasure; the Profile Writer owns it, and the mechanism is left to the feature design
  - Using Construct's GTS person types for the profile's categories and facts, registered in the types registry and with graph storage (`cpt-cf-construct-contract-person-types`)
  - Re-admission and a new write for a value without a planner, on a conflict
  - Making the next read return the stored value and never a replaced or removed one
  - Using graph storage's compare-and-set (a write that succeeds only while a node still has the expected version) as it is (`cpt-cf-construct-adr-graph-storage-as-is`)

- **Out of scope**:
  - Deciding or checking the plan, and serving the profile
  - The entry point from Record Intake into the planning (Planner)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-construct-fr-deterministic-storage`
  - [ ] `p1` - `cpt-cf-construct-fr-fact-origin`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-construct-principle-whole-plan-or-nothing`
  - [ ] `p1` - `cpt-cf-construct-principle-no-mixed-records`

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - Profile

- **Design Components**:

  - [ ] `p1` - `cpt-cf-construct-component-profile-writer`

- **API**:
  - None. The writer is internal.

- **Sequences**:

  - [ ] `p1` - `cpt-cf-construct-seq-concurrent-records`
  - [ ] `p1` - `cpt-cf-construct-seq-entity-replaced` (primary owner; the write that removes the old value and adds the new one together. The Planner and Admission share it)
  - [ ] `p1` - `cpt-cf-construct-seq-record-in` (shared; the write of a received record's plan. Primary owner: `cpt-cf-construct-feature-record-intake`)

- **Data**:

  - [ ] `p1` - `cpt-cf-construct-entity-profile`

### 2.9 [Profile Reader](features/profile-reader.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-construct-feature-profile-reader`

- **Purpose**: The only path by which applications and agents read a profile. Graph storage cannot apply Construct's read rules, so this feature applies them: owner only, platform permissions, personalization and erasure state.

- **Depends On**: `cpt-cf-construct-feature-gear-foundation`, `cpt-cf-construct-feature-subject-settings`

- **Scope**:
  - Serving the profile of one subject grouped by category (entity kind), each fact with its origin and time, naming the subject in every response
  - Serving only for the owner: to the owner and to applications and agents that act for the owner
  - Asking the platform, on each read, which categories the caller may read; a denied category looks like an empty one
  - Serving nothing while personalization is off or an erasure is under way
  - One audit event without content for each read: which application or agent, which subject, which categories, and when
  - Permission changes applying from the next read
  - Not serving applications' direct reads from graph storage (`cpt-cf-construct-principle-single-profile-owner`)

- **Out of scope**:
  - The subject's own view, the export and administrator reads (Subject Control and Administrator Fact Management)
  - Hiding facts under review; added by the Review Queue when it lands (`cpt-cf-construct-feature-review-queue`)
  - Hiding items past the retention period (Retention and Tenant Exit)
  - Writing the profile and the MCP read tool

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-construct-fr-profile-read`
  - [ ] `p1` - `cpt-cf-construct-fr-fact-origin` (shared; each served fact with its origin. Primary owner: `cpt-cf-construct-feature-profile-writer`)
  - [ ] `p1` - `cpt-cf-construct-fr-settings` (shared; nothing served while personalization is off. Primary owner: `cpt-cf-construct-feature-subject-settings`)
  - [ ] `p1` - `cpt-cf-construct-nfr-deletion-time` (shared; nothing served while an erasure is under way. Primary owner: `cpt-cf-construct-feature-erasure`)

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-construct-principle-single-profile-owner`

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - Profile (shared; reads only, through the graph storage client, so no dependency on the writer. Primary owner: `cpt-cf-construct-feature-profile-writer`)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-construct-component-profile-reader`

- **API**:
  - GET /api/construct/v1/subjects/{subject_id}/profile

- **Sequences**:

  - None.

- **Data**:

  - None. The reader only reads graph storage and the subject settings table.

### 2.10 [Subject Control](features/subject-control.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-construct-feature-subject-control`

- **Purpose**: Lets the subject see all of their facts and delete any of them, and lets the data protection officer export a subject's data. It owns the subject control component that Subject Settings, the Review Queue and Administrator Fact Management extend.

- **Depends On**: `cpt-cf-construct-feature-profile-reader`, `cpt-cf-construct-feature-subject-settings`

- **Scope**:
  - The subject's own view of all facts with origin, in every category, with the settings; personalization off and erasure never block it
  - Delete of one fact: a soft delete in graph storage with the root node's expected version, and a rerun on conflict up to the same small limit
  - A deleted fact is not served from the next read
  - An audit event without content for each fact delete
  - The export of all of a subject's data as a machine-readable file, for the data protection officer on the subject's request, under its own permission
  - A read audit event for the subject's own view and for the export
  - The shared pagination mechanism: cursor pagination with a maximum page size, and OData `$filter` and `$orderby`; this feature uses it for the subject's facts list

- **Out of scope**:
  - Personalization settings (Subject Settings) and review requests (Review Queue), which add their data to the view and the export
  - Closing an open review request as deleted when the subject deletes its fact; the Review Queue adds that closing call to the delete path when it lands
  - Erase everything (Erasure), including the erase route and the call into Deletion; erasure owns them
  - Refusing a repeat of a deleted fact's record; this relies on the repeat check of Record Intake
  - Administrator operations
  - Page size limit and filter fields, left to the feature design

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-construct-fr-subject-access`
  - [ ] `p1` - `cpt-cf-construct-fr-subject-delete` (primary owner for the single-fact delete; the erase-everything part belongs to `cpt-cf-construct-feature-erasure`)
  - [ ] `p1` - `cpt-cf-construct-fr-fact-origin` (shared; each fact in the subject's view with its origin. Primary owner: `cpt-cf-construct-feature-profile-writer`)
  - [ ] `p1` - `cpt-cf-construct-nfr-deletion-time` (shared; a deleted fact not served from the next read. Primary owner: `cpt-cf-construct-feature-erasure`)

- **Design Principles Covered**:

  - None.

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-construct-constraint-graph-soft-delete-only` (primary owner; the soft delete, first built here. Erasure shares it)

- **Domain Model Entities**:
  - Profile (shared; reads only, through the graph storage client, so no dependency on the writer. Primary owner: `cpt-cf-construct-feature-profile-writer`)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-construct-component-subject-control`

- **API**:
  - GET /api/construct/v1/subjects/{subject_id}/facts
  - DELETE /api/construct/v1/subjects/{subject_id}/facts/{fact_id}
  - GET /api/construct/v1/subjects/{subject_id}/export

- **Sequences**:

  - None.

- **Data**:

  - None. The view reads the settings table and graph storage.

### 2.11 [Review Queue](features/review-queue.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-construct-feature-review-queue`

- **Purpose**: Lets the subject mark a fact as incorrect and lets assigned reviewers resolve the request. While a request is open, the fact is not served and no record or agent changes it, so a wrong fact from an outside source can be corrected.

- **Depends On**: `cpt-cf-construct-feature-subject-control`, `cpt-cf-construct-feature-profile-writer`, `cpt-cf-construct-feature-admission`, `cpt-cf-construct-feature-sensitive-data-checks`

- **Scope**:
  - The table `construct__review_requests`, keyed by the graph node key of the entity under review
  - Creating a request with an optional comment; the subject sees its state and outcome
  - Adding, when this feature lands, the check that hides a fact under an open request from applications and agents to the Profile Reader (the subject still sees it) and the check that skips it to the Planner; until then no fact is under review (DESIGN, the `cpt-cf-construct-fr-review-request` row: the Profile Reader hides the fact and the planner skips it; `cpt-cf-construct-component-profile-reader`, `cpt-cf-construct-component-planner`)
  - Listing open requests to the reviewers the tenant assigns, read from the settings service, under its own permission
  - Paged reads of the subject's review requests and of the open review requests, with the pagination of Subject Control
  - Resolving as corrected, deleted or rejected with a reason
  - The one-step plan path, which has no planner and which this feature owns: a review resolution with a corrected value, or an administrator add or edit, becomes a plan with one add or replace step, passes the Sensitive-Data Checks and Admission, and is stored with the reviewer or the administrator as origin; if blocked or dropped, the request stays open
  - A deleted resolution works like the subject's delete; a rejected fact is served again
  - Closing a request as deleted when the subject deletes the fact or retention removes it first; when it lands, this feature adds the closing call to the Subject Control delete path, as it does for the reader and the planner
  - The one-step path gives a new entity its node key the same way the Planner does
  - Audit events without content for each correction and each resolution
  - Review requests shown in the subject's view and export

- **Out of scope**:
  - How a request that opens while a plan is in flight stops that plan; the FEATURE documents of the Review Queue and the Profile Writer resolve it
  - Administrator edits that close a request (Administrator Fact Management calls this feature)
  - Retention removal itself (Retention and Tenant Exit)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-construct-fr-review-request`
  - [ ] `p1` - `cpt-cf-construct-fr-fact-origin` (shared; the reviewer as origin. Primary owner: `cpt-cf-construct-feature-profile-writer`)
  - [ ] `p1` - `cpt-cf-construct-fr-sensitive-data-guardrails` (shared; the corrected value through the checks and Admission. Primary owner: `cpt-cf-construct-feature-sensitive-data-checks`)
  - [ ] `p1` - `cpt-cf-construct-fr-subject-access` (shared; review requests in the subject's view. Primary owner: `cpt-cf-construct-feature-subject-control`)
  - [ ] `p1` - `cpt-cf-construct-fr-subject-delete` (shared; a deleted resolution works like the subject's delete. Primary owner: `cpt-cf-construct-feature-subject-control`)

- **Design Principles Covered**:

  - None.

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - Review Request
  - Plan (shared; owned by `cpt-cf-construct-feature-planner`)
  - Profile (shared; owned by `cpt-cf-construct-feature-profile-writer`)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-construct-component-subject-control` (shared; the review part. Primary owner: `cpt-cf-construct-feature-subject-control`)
  - [ ] `p1` - `cpt-cf-construct-component-planner` (shared; the skip of a fact under an open request. Primary owner: `cpt-cf-construct-feature-planner`)
  - [ ] `p1` - `cpt-cf-construct-component-profile-reader` (shared; the hiding of a fact under an open request. Primary owner: `cpt-cf-construct-feature-profile-reader`)

- **API**:
  - POST /api/construct/v1/subjects/{subject_id}/review-requests
  - GET /api/construct/v1/subjects/{subject_id}/review-requests
  - GET /api/construct/v1/admin/review-requests
  - POST /api/construct/v1/admin/review-requests/{request_id}/resolution

- **Sequences**:

  - [ ] `p1` - `cpt-cf-construct-seq-review`

- **Data**:

  - [ ] `p1` - `cpt-cf-construct-db-own-tables` (shared; the table `construct__review_requests`. Primary owner: `cpt-cf-construct-feature-subject-settings`)
  - [ ] `p1` - `cpt-cf-construct-dbtable-review-requests`
  - [ ] `p1` - `cpt-cf-construct-entity-review-request`

### 2.12 [Administrator Fact Management](features/admin-facts.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-construct-feature-admin-facts`

- **Purpose**: Lets a tenant administrator read all facts of a subject, and add, edit or delete one directly. It is for checking and fixing profiles when no connector or agent can, and for test setup. It sits under its own `admin` paths and its own permission.

- **Depends On**: `cpt-cf-construct-feature-subject-control`, `cpt-cf-construct-feature-profile-writer`, `cpt-cf-construct-feature-admission`, `cpt-cf-construct-feature-review-queue`

- **Scope**:
  - Reading all facts of a subject with origin
  - A read audit event without content for each administrator read
  - Add and edit, which use the one-step plan path of the Review Queue, with the administrator as origin
  - An add for a subject with no profile creating the profile, by reusing the writer
  - Delete working like the subject's delete
  - While personalization is off, an add or edit stores nothing and the call says so; read and delete still work
  - An edit of a missing fact stores nothing
  - A stored edit or a delete closing an open review request as corrected or deleted, through the Review Queue; a blocked or dropped edit leaves it open
  - An audit event without content for each add, edit and delete
  - The paged administrator read of a subject's facts, with the pagination of Subject Control

- **Out of scope**:
  - The one-step plan path itself, which belongs to the Review Queue (`cpt-cf-construct-feature-review-queue`)
  - The wire shapes of add and edit, left to the feature design
  - The gateway scope rule on `/api/construct/v1/admin/**`, which a deployment turns on
  - Review Queue operations (`cpt-cf-construct-feature-review-queue`)

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-construct-fr-admin-facts`
  - [ ] `p1` - `cpt-cf-construct-fr-fact-origin` (shared; the administrator as origin. Primary owner: `cpt-cf-construct-feature-profile-writer`)
  - [ ] `p1` - `cpt-cf-construct-fr-sensitive-data-guardrails` (shared; added and edited values through the checks and Admission. Primary owner: `cpt-cf-construct-feature-sensitive-data-checks`)
  - [ ] `p1` - `cpt-cf-construct-fr-subject-delete` (shared; the administrator's delete works like the subject's delete. Primary owner: `cpt-cf-construct-feature-subject-control`)

- **Design Principles Covered**:

  - None.

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - Plan (shared; owned by `cpt-cf-construct-feature-planner`)
  - Profile (shared; owned by `cpt-cf-construct-feature-profile-writer`)
  - Review Request (shared; owned by `cpt-cf-construct-feature-review-queue`)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-construct-component-subject-control` (shared; the administrator operations. Primary owner: `cpt-cf-construct-feature-subject-control`)

- **API**:
  - GET /api/construct/v1/admin/subjects/{subject_id}/facts
  - POST /api/construct/v1/admin/subjects/{subject_id}/facts
  - PUT /api/construct/v1/admin/subjects/{subject_id}/facts/{fact_id}
  - DELETE /api/construct/v1/admin/subjects/{subject_id}/facts/{fact_id}

- **Sequences**:

  - None.

- **Data**:

  - None.

### 2.13 [Agent Access over MCP](features/mcp-tools.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-construct-feature-mcp-tools`

- **Purpose**: Gives AI agents two MCP tools: read the profile, and add, replace or remove a fact on the subject's instruction. Any standard MCP client works with no Construct-specific code, and an agent can act only for the subject and tenant it is authorized for.

- **Depends On**: `cpt-cf-construct-feature-profile-reader`, `cpt-cf-construct-feature-profile-writer`, `cpt-cf-construct-feature-planner`

- **Scope**:
  - Construct's own MCP endpoint
  - The read tool, through the profile reader like any application
  - The manage-facts tool: it takes what the subject said and the recent conversation, runs through the same planner, checks and writer as a record, waits and returns a summary of changes
  - Origin naming the agent and the subject
  - A result of "nothing changed" when the plan is dropped or personalization is off or an erasure is under way
  - Caller and tenant taken only from the platform; AuthZ decides whether the caller may act for the named subject
  - A call for another subject or tenant answered like a subject that does not exist
  - Not exposing the planner's own tools

- **Out of scope**:
  - Tool names, inputs, outputs and wire codes, left to the feature design
  - Using the serverless runtime's MCP server once it ships

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-construct-fr-mcp-tools`
  - [ ] `p2` - `cpt-cf-construct-fr-mcp-manage-facts`
  - [ ] `p2` - `cpt-cf-construct-fr-mcp-caller-binding`
  - [ ] `p1` - `cpt-cf-construct-fr-fact-origin` (shared; the agent and the subject as origin. Primary owner: `cpt-cf-construct-feature-profile-writer`)
  - [ ] `p1` - `cpt-cf-construct-fr-sensitive-data-guardrails` (shared; agent changes through the checks and Admission. Primary owner: `cpt-cf-construct-feature-sensitive-data-checks`)

- **Design Principles Covered**:

  - None.

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - Plan (shared; owned by `cpt-cf-construct-feature-planner`)
  - Profile (shared; owned by `cpt-cf-construct-feature-profile-writer`)

- **Design Components**:

  - [ ] `p2` - `cpt-cf-construct-component-mcp-tools`

- **API**:
  - MCP tool: read the profile
  - MCP tool: add, replace or remove a fact

- **Sequences**:

  - [ ] `p2` - `cpt-cf-construct-seq-agent-mcp`

- **Data**:

  - None.

### 2.14 [Erasure](features/erasure.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-construct-feature-erasure`

- **Purpose**: Lets the subject erase everything. Construct stops serving the subject's data at once and removes it, so no read path and no storage holds it past the threshold of `cpt-cf-construct-nfr-deletion-time`. It owns the deletion component and the deletion-time requirement.

- **Depends On**: `cpt-cf-construct-feature-subject-control`, `cpt-cf-construct-feature-record-intake`, `cpt-cf-construct-feature-review-queue`

- **Scope**:
  - The erase route and the erase request that is handed to Deletion; the DESIGN credits the hand-off to the Subject control component, but the route lands with Erasure because Subject Control is built earlier
  - Setting personalization off and marking the erasure under way, so the reader serves nothing and Admission drops plans, even if personalization is turned on again
  - Soft-deleting the subject's entities and the root node, with the expected version, and a rerun on conflict
  - Removing the subject's review requests and record identities, including an identity inserted by a record received just before the request
  - Keeping only the subject's personalization setting, which stays off, and audit events
  - An erasure audit event without content
  - Repeatable steps, with a rerun until all steps are done, and clearing the mark "erasure under way" when every step is done
  - Leaving the soft-deleted root node, so that a profile created after an erasure needs a new root key; the Profile Writer finds that key
  - Meeting the removal threshold of `cpt-cf-construct-nfr-deletion-time`, which needs graph storage's purge

- **Out of scope**:
  - Retention by age and tenant exit (Retention and Tenant Exit)
  - Purging graph storage; it is an external prerequisite
  - How a halted erasure is found and run again, left to the feature design

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-construct-nfr-deletion-time`
  - [ ] `p1` - `cpt-cf-construct-fr-subject-delete` (shared; the erase-everything part. Primary owner: `cpt-cf-construct-feature-subject-control`)
  - [ ] `p1` - `cpt-cf-construct-fr-settings` (shared; erasure sets personalization off and marks the erasure under way. Primary owner: `cpt-cf-construct-feature-subject-settings`)

- **Design Principles Covered**:

  - None.

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-construct-constraint-graph-soft-delete-only` (shared; the soft delete of the subject's entities and the root node. Primary owner: `cpt-cf-construct-feature-subject-control`)

- **Domain Model Entities**:
  - Subject Settings (shared; owned by `cpt-cf-construct-feature-subject-settings`)
  - Review Request (shared; owned by `cpt-cf-construct-feature-review-queue`)
  - Profile (shared; owned by `cpt-cf-construct-feature-profile-writer`)
  - Record (shared; owned by `cpt-cf-construct-feature-record-intake`)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-construct-component-deletion`
  - [ ] `p1` - `cpt-cf-construct-component-subject-control` (shared; the erase route. Primary owner: `cpt-cf-construct-feature-subject-control`)

- **API**:
  - POST /api/construct/v1/subjects/{subject_id}/erasure

- **Sequences**:

  - [ ] `p1` - `cpt-cf-construct-seq-erasure`

- **Data**:

  - None. Erasure removes rows from tables that other features own.

### 2.15 [Retention and Tenant Exit](features/retention.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-construct-feature-retention`

- **Purpose**: Removes data when the tenant's retention period ends and when a tenant leaves. Data is not kept longer than its purpose needs. It reuses the erasure code. The retention job can ship first; tenant exit waits on graph storage's tenant offboarding, an external prerequisite.

- **Depends On**: `cpt-cf-construct-feature-erasure`, `cpt-cf-construct-feature-profile-reader`

- **Scope**:
  - The retention period as a tenant setting read from the settings service; without it, data stays until deleted
  - A retention job on the cluster leader that deletes facts, review requests and record identities, counted from when each was stored, created or received
  - The same delete code as erasure; a rerun on conflict keeps a fact that a record changed in between
  - The reader no longer serving an item past the period, even before the job removes it
  - Tenant exit following the platform's tenant offboarding protocol: removing the tenant's data from Construct's own tables, while graph storage removes the graph through its own offboarding
  - Closing an open review request as deleted when retention removes its fact, by calling the Review Queue (retention depends on it through Erasure)
  - Audit events staying until the Audit gear's retention period ends

- **Out of scope**:
  - Setting the period, which tenant administrators do in the settings service
  - Graph storage purge and offboarding, which are external
  - The identity and tenant scope for the job and tenant exit, left to the feature design

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-construct-fr-retention`
  - [ ] `p1` - `cpt-cf-construct-nfr-deletion-time` (shared; retention and tenant exit meet the same threshold. Primary owner: `cpt-cf-construct-feature-erasure`)
  - [ ] `p1` - `cpt-cf-construct-fr-subject-delete` (shared; the retention period ends the keeping of record identities. Primary owner: `cpt-cf-construct-feature-subject-control`)

- **Design Principles Covered**:

  - None.

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - Record (shared; owned by `cpt-cf-construct-feature-record-intake`)
  - Review Request (shared; owned by `cpt-cf-construct-feature-review-queue`)
  - Profile (shared; owned by `cpt-cf-construct-feature-profile-writer`)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-construct-component-deletion` (shared; the retention job and tenant exit. Primary owner: `cpt-cf-construct-feature-erasure`)

- **API**:
  - None. The job runs on the cluster leader; tenant exit follows the platform protocol.

- **Sequences**:

  - None.

- **Data**:

  - None. Retention removes rows from tables that other features own.

---

## 3. Feature Dependencies

```text
cpt-cf-construct-feature-gear-foundation
    ↓
    ├─→ cpt-cf-construct-feature-model-client
    │       ↓
    │       └─→ cpt-cf-construct-feature-planner (also needs record-intake; uses gear-foundation directly)
    │               ↓
    │               └─→ cpt-cf-construct-feature-sensitive-data-checks (also needs model-client)
    └─→ cpt-cf-construct-feature-subject-settings
            ↓
            ├─→ cpt-cf-construct-feature-record-intake (also uses gear-foundation directly)
            ├─→ cpt-cf-construct-feature-admission (also needs sensitive-data-checks)
            │       ↓
            │       └─→ cpt-cf-construct-feature-profile-writer (also needs planner, record-intake)
            │               ↓
            │               └─→ cpt-cf-construct-feature-mcp-tools (also needs profile-reader, planner)
            └─→ cpt-cf-construct-feature-profile-reader (also uses gear-foundation directly)
                    ↓
                    └─→ cpt-cf-construct-feature-subject-control (also needs subject-settings)
                            ↓
                            └─→ cpt-cf-construct-feature-review-queue (also needs profile-writer, admission, sensitive-data-checks)
                                    ↓
                                    ├─→ cpt-cf-construct-feature-admin-facts (also needs subject-control, profile-writer, admission)
                                    └─→ cpt-cf-construct-feature-erasure (also needs subject-control, record-intake)
                                            ↓
                                            └─→ cpt-cf-construct-feature-retention (also needs profile-reader)
```

**Dependency Rationale**:

- `cpt-cf-construct-feature-model-client` requires `cpt-cf-construct-feature-gear-foundation`: the model client is a component of the gear and needs its crates and configuration.
- `cpt-cf-construct-feature-subject-settings` requires `cpt-cf-construct-feature-gear-foundation`: it adds the first own table on the shell's secure ORM and migration set.
- `cpt-cf-construct-feature-record-intake` requires `cpt-cf-construct-feature-subject-settings`: intake refuses records while personalization is off or an erasure is under way, and reads that state from the settings table. It also uses `cpt-cf-construct-feature-gear-foundation` directly, for its route and its table.
- `cpt-cf-construct-feature-planner` requires `cpt-cf-construct-feature-model-client`: every round of the agent loop is a model call. It requires `cpt-cf-construct-feature-record-intake` because it implements the hand-off interface that intake defines. It also uses `cpt-cf-construct-feature-gear-foundation` directly, as code inside the gear shell.
- `cpt-cf-construct-feature-sensitive-data-checks` requires `cpt-cf-construct-feature-planner`: the checks run on the values of a plan, and the plan entity belongs to the planner. It also needs `cpt-cf-construct-feature-model-client` for model checks.
- `cpt-cf-construct-feature-admission` requires `cpt-cf-construct-feature-sensitive-data-checks`: it enforces the verdicts. It also requires `cpt-cf-construct-feature-subject-settings` to read the personalization and erasure state.
- `cpt-cf-construct-feature-profile-writer` requires `cpt-cf-construct-feature-admission`: it stores only admitted plans. It requires `cpt-cf-construct-feature-planner` because a write conflict reruns the planner. It lists `cpt-cf-construct-feature-record-intake` directly because it stores the plan of a received record (`cpt-cf-construct-seq-record-in`); the planner already depends on intake.
- `cpt-cf-construct-feature-profile-reader` requires `cpt-cf-construct-feature-subject-settings`: it serves nothing while personalization is off or an erasure is under way. It also uses `cpt-cf-construct-feature-gear-foundation` directly, for its route.
- `cpt-cf-construct-feature-subject-control` requires `cpt-cf-construct-feature-profile-reader`: the subject's view shares the read rules with the reader. It also requires `cpt-cf-construct-feature-subject-settings` to show the settings in the view.
- `cpt-cf-construct-feature-review-queue` requires `cpt-cf-construct-feature-subject-control`, `cpt-cf-construct-feature-profile-writer`, `cpt-cf-construct-feature-admission` and `cpt-cf-construct-feature-sensitive-data-checks`: a corrected value is a plan that passes the checks and Admission and is stored by the writer.
- `cpt-cf-construct-feature-admin-facts` requires `cpt-cf-construct-feature-review-queue`: a stored edit or a delete closes an open review request. It also requires the subject control, writer and Admission features, whose paths it reuses.
- `cpt-cf-construct-feature-mcp-tools` requires `cpt-cf-construct-feature-profile-reader`, `cpt-cf-construct-feature-profile-writer` and `cpt-cf-construct-feature-planner`: the read tool uses the reader, and the manage-facts tool uses the planner and the writer.
- `cpt-cf-construct-feature-erasure` requires `cpt-cf-construct-feature-subject-control`, `cpt-cf-construct-feature-record-intake` and `cpt-cf-construct-feature-review-queue`: erasure reuses the delete rules and the soft delete first built in Subject Control (`cpt-cf-construct-constraint-graph-soft-delete-only`, shared), and it removes the record identities and the review requests.
- `cpt-cf-construct-feature-retention` requires `cpt-cf-construct-feature-erasure`: it runs the same delete code by age. It requires `cpt-cf-construct-feature-profile-reader` so the reader stops serving items past the period.
- The graph has no cycles. The review request hooks in the reader and the planner, and the closing of a request when its fact is deleted or removed in Subject Control or in retention, are covered as follows. `cpt-cf-construct-feature-review-queue` adds the two checks to the reader and the planner, and the closing call to the Subject Control delete path, when it lands. Retention calls the Review Queue itself. So no earlier feature depends on a later one.
- No `p1` feature depends on a `p2` feature.
- `cpt-cf-construct-feature-model-client` and `cpt-cf-construct-feature-subject-settings` depend only on the foundation and can be built in parallel.
- `cpt-cf-construct-feature-record-intake` and `cpt-cf-construct-feature-profile-reader` are independent of each other once the settings exist, and can be built in parallel. The planner follows `cpt-cf-construct-feature-record-intake` and `cpt-cf-construct-feature-model-client`.
- `cpt-cf-construct-feature-mcp-tools` can start as soon as the writer and the reader exist.
- `cpt-cf-construct-feature-admin-facts` and `cpt-cf-construct-feature-erasure` are independent of each other and can be developed in parallel once the Review Queue and record intake exist.
