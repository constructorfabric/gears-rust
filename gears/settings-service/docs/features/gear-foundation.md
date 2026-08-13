<!-- Created: 2026-08-10 by Constructor Tech -->
<!-- Updated: 2026-08-10 by Constructor Tech -->

# Feature: Gear Foundation, SDK Contracts and Cross-Cutting Infrastructure

- [ ] `p1` - **ID**: `cpt-cf-settings-service-featstatus-gear-foundation`

- [ ] `p1` - `cpt-cf-settings-service-feature-gear-foundation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Gear Initialization and ClientHub Registration](#gear-initialization-and-clienthub-registration)
  - [Setting Key Parsing and Validation](#setting-key-parsing-and-validation)
  - [Optimistic Concurrency Precondition Evaluation](#optimistic-concurrency-precondition-evaluation)
  - [Domain Error to Problem Mapping](#domain-error-to-problem-mapping)
  - [Authorization Enforcement and Credential Step-Up](#authorization-enforcement-and-credential-step-up)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [SDK Crate, Models and Value Objects](#sdk-crate-models-and-value-objects)
  - [SDK Trait Contracts](#sdk-trait-contracts)
  - [Reader Degradation Contract and Typed Projection](#reader-degradation-contract-and-typed-projection)
  - [RFC-9457 Problem Mapping](#rfc-9457-problem-mapping)
  - [Persistence Adapter and Migration Harness](#persistence-adapter-and-migration-harness)
  - [Gear Scaffold and ClientHub Registration](#gear-scaffold-and-clienthub-registration)
  - [REST and OData Infrastructure](#rest-and-odata-infrastructure)
  - [Policy Enforcement Point and Step-Up Gate](#policy-enforcement-point-and-step-up-gate)
  - [Audit Emitter](#audit-emitter)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Establishes the `settings-service-sdk` crate and the gear scaffold that every later Settings Service feature is built on: SDK models and trait contracts, the reader degradation contract with its typed projection over the platform canonical error, the RFC-9457 Problem mapping, PostgreSQL persistence with a migration harness, REST and OData infrastructure, the `PolicyEnforcer` PEP with credential step-up, and the Audit Emitter.

### 1.2 Purpose

`gears/settings-service/` currently contains documentation and no Rust source. This feature is what makes any code exist at all, and it is the only feature in the decomposition with no dependency of its own.

Its scope is deliberately drawn around what more than one later feature needs. Category Management, Setting Declarations, Typed Value Validation, and Value Resolution all need the same persistence adapter, the same `If-Match` precondition handling, the same authorization enforcement point, the same Problem mapping, and the same audit path. Building those once here is what keeps the later features about their domain rather than about plumbing.

Two contracts in this feature are consumed from outside the service and are therefore load-bearing beyond the gear itself: the `SettingsReaderClient` trait, which is the platform's in-process hot read path, and its degradation contract, which tells a consumer how to behave when the Settings Service is unreachable. Settings are a boot-time dependency, so getting that contract wrong makes every consuming gear's startup fragile.

**Requirements**: `cpt-cf-settings-service-fr-authn-role-gating`, `cpt-cf-settings-service-nfr-security-baseline`, `cpt-cf-settings-service-nfr-availability`

**Principles**: `cpt-cf-settings-service-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-internal-caller` | Consumes the `SettingsReaderClient` trait in process and must handle its degradation contract |
| `cpt-cf-settings-service-actor-contributing-module` | Consumes the `SettingsContributionClient` trait to register and retire its own declarations |
| `cpt-cf-settings-service-actor-authn-resolver` | Authenticates the caller and carries the credential step-up assertion |
| `cpt-cf-settings-service-actor-authz-resolver` | Supplies the authorization decision and the `AccessScope` constraints the PEP enforces |
| `cpt-cf-settings-service-actor-platform-admin` | The operator whose session is re-authenticated when a behavior-affecting action demands step-up |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — `cpt-cf-settings-service-interface-settings-read-sdk`
- **Design**: [DESIGN.md](../DESIGN.md) — §4.2 (Audit Emitter), §4.3 (API Contracts, Error Response Format), §4.5 (Service-to-Service Pattern), §4.7 (Database schemas), §4.8 (Security and Authorization), §4.9 (Deployment Topology), §4.10 (Technology Stack)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.1
- **Dependencies**: None. This is the root of the dependency graph.
- **Not applicable**: No domain entity or REST resource is delivered here, so there is no state machine and no actor flow. UX is out of scope (backend gear, no user interface). Performance targets are set at the system level in the PRD NFR section.

## 2. Actor Flows (CDSL)

Not applicable. This feature delivers SDK contracts and gear infrastructure with no user-facing interaction of its own. The actor flows that exercise these contracts belong to the features built on top of it: category administration in entry 2.2, declaration authoring in 2.3, and effective-value reads in 2.5.

## 3. Processes / Business Logic (CDSL)

### Gear Initialization and ClientHub Registration

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-gear-foundation-gear-init`

**Input**: ToolKit-supplied gear configuration at startup

**Output**: A gear ready to serve, with its client traits resolvable through `ClientHub`

**Steps**:
1. [x] - `p1` - Read deployment-owned bootstrap configuration from ToolKit config: database endpoint, broker endpoint, service identity, TLS material, ports - `inst-gf-init-1`
2. [x] - `p1` - **IF** any required bootstrap value is absent → **RETURN** startup failure, because bootstrap configuration is deployment-owned and is never itself a managed setting - `inst-gf-init-2`
3. [x] - `p1` - Establish the PostgreSQL connection pool and construct the `SecureConn` and `DBRunner` handles - `inst-gf-init-3`
4. [x] - `p1` - Run outstanding schema migrations to completion - `inst-gf-init-4`
5. [x] - `p1` - **IF** a migration fails → **RETURN** startup failure without serving traffic, so no request observes a partially migrated schema - `inst-gf-init-5`
6. [x] - `p1` - Resolve the `TypesRegistryClient` and the Policy Decision client through `ClientHub` - `inst-gf-init-6`
7. [ ] - `p1` - Register the gear's own `SettingsReaderClient` and `SettingsContributionClient` implementations into `ClientHub` - `inst-gf-init-7`
8. [ ] - `p1` - Bind each registered trait according to the active deployment profile: the in-process implementation when co-located, the same trait over REST when out of process - `inst-gf-init-8`
9. [x] - `p1` - Mark the gear ready and begin serving - `inst-gf-init-9`

### Setting Key Parsing and Validation

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-gear-foundation-key-parse`

**Input**: Candidate setting key string

**Output**: A parsed key value object carrying its value-type segment, instance segment, category, and leaf name, or a validation problem

GTS grammar is **not** re-implemented here. The platform GTS identifier library is the single source of truth for the prefix, the lowercase rule, the permitted character set, and the four-name-token-per-segment shape. This algorithm adds only what that library cannot know: that a setting key is exactly a type followed by an instance, and where the category and leaf name sit inside the instance segment.

**Steps**:
1. [x] - `p1` - Validate the whole candidate as a GTS identifier through the platform GTS validator, disallowing wildcards, since a concrete setting key never carries one - `inst-gf-key-1`
2. [x] - `p1` - **IF** validation fails → **RETURN** its problem unchanged in substance, preserving whether the fault was identifier-level or segment-level and, for a segment fault, the segment number and byte offset it reported - `inst-gf-key-2`
3. [x] - `p1` - **IF** the identifier does not consist of exactly two segments → **RETURN** a validation problem stating that a setting key is a value type followed by an instance id, naming how many segments were found - `inst-gf-key-3`
4. [x] - `p1` - **IF** the first segment is not a GTS type → **RETURN** a validation problem, because the value-type half must be a registered type - `inst-gf-key-4`
5. [x] - `p1` - **IF** the second segment is a GTS type → **RETURN** a validation problem, because the setting is an instance and a trailing terminator would make it a type - `inst-gf-key-5`
6. [x] - `p1` - Read the owning category from the instance segment's namespace token and the leaf name from its type token; both authoring parties place them in those positions - `inst-gf-key-6`
7. [x] - `p1` - **RETURN** the parsed key value object exposing both segments without re-normalizing the input, so a stored key and a supplied key compare identically - `inst-gf-key-7`

### Optimistic Concurrency Precondition Evaluation

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-gear-foundation-precondition`

**Input**: Request `If-Match` header and the current representation of the target resource

**Output**: Proceed verdict, or the precondition problem to return

**Steps**:
1. [x] - `p1` - **IF** the operation is a mutating `PATCH` or `DELETE` and no `If-Match` header is present → **RETURN** `428` precondition required - `inst-gf-precond-1`
2. [ ] - `p1` - Compute the current ETag from the target's persisted representation - `inst-gf-precond-2`
3. [x] - `p1` - **IF** the supplied `If-Match` does not equal the current ETag → **RETURN** `412` precondition failed - `inst-gf-precond-3`
4. [x] - `p1` - **RETURN** proceed, and carry the computed ETag forward so the handler can emit a refreshed value on success - `inst-gf-precond-4`

### Domain Error to Problem Mapping

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-gear-foundation-problem-mapping`

**Input**: A `DomainError` raised anywhere in the service

**Output**: An RFC-9457 problem document and its HTTP status

**Steps**:
1. [x] - `p1` - Map the `DomainError` variant to a canonical error category, from which the platform derives the HTTP status and the stable `gts://` type URI - `inst-gf-problem-1`
2. [ ] - `p1` - Set `Content-Type` to `application/problem+json` - `inst-gf-problem-2`
3. [ ] - `p1` - Populate the required members `type`, `title`, `status`, and `trace_id`, taking `trace_id` from the ambient request trace context - `inst-gf-problem-3`
4. [x] - `p1` - **IF** the failure is a field-level validation rejection → attach one violation per offending field, each carrying the field, a stable machine-readable reason, and a human-readable description - `inst-gf-problem-4`
5. [x] - `p1` - **IF** the error carries an authorization or entitlement denial → emit the denial without disclosing whether the target resource exists - `inst-gf-problem-5`
6. [x] - `p1` - **IF** the variant is unrecognized → map to `500` with a generic title, never leaking an internal message into the response body - `inst-gf-problem-6`
7. [ ] - `p1` - **RETURN** the problem document and status - `inst-gf-problem-7`

### Authorization Enforcement and Credential Step-Up

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-gear-foundation-authz-stepup`

**Input**: Authenticated request context, the target GTS resource type, the required action, and whether the action is behavior-affecting

**Output**: An `AccessScope` for the caller, or a denial

**Steps**:
1. [ ] - `p1` - Require an authenticated principal on the request context - `inst-gf-authz-1`
2. [ ] - `p1` - **IF** authentication is absent or invalid → **RETURN** denial - `inst-gf-authz-2`
3. [x] - `p1` - Ask the Policy Decision client for a decision on the action against the target GTS resource type - `inst-gf-authz-3`
4. [x] - `p1` - **IF** the decision cannot be obtained → **RETURN** denial, failing closed rather than proceeding on an unknown verdict - `inst-gf-authz-4`
5. [x] - `p1` - **IF** the decision is deny → **RETURN** denial - `inst-gf-authz-5`
6. [ ] - `p1` - **IF** the action is behavior-affecting → require a valid credential step-up assertion established at the identity provider - `inst-gf-authz-6`
7. [ ] - `p1` - **IF** the step-up assertion is absent, expired, or not bound to this principal → **RETURN** denial - `inst-gf-authz-7`
8. [x] - `p1` - Build the `AccessScope` from the decision's constraints - `inst-gf-authz-8`
9. [x] - `p1` - **RETURN** the `AccessScope` for the handler to apply as a query and visibility predicate - `inst-gf-authz-9`

## 4. States (CDSL)

Not applicable. This feature introduces no domain entity and therefore no lifecycle. Entity state enters the service with `SettingDeclaration` in entry 2.3, which carries `active` and `retired`.

## 5. Definitions of Done

**Five of these close in a later entry, by design.** This feature builds infrastructure and, per [DECOMPOSITION](../DECOMPOSITION.md) entry 2.1, owns **no domain table** — so the clauses below that require a resource to be enforced, bound, or queried have nothing here to demonstrate against. They are satisfied by the first entry that exercises them, in the same way entry 2.2's no-orphan rule becomes enforceable only when 2.3 creates the foreign key.

| Definition of Done | Clause awaiting a consumer | Closes in |
|---|---|---|
| SDK Trait Contracts | the traits facade local against remote so `ClientHub` can bind either | 2.5 |
| Gear Scaffold and ClientHub Registration | registering the client traits into `ClientHub` | 2.5 |
| REST and OData Infrastructure | rejecting an expression on an unmapped field or unsupported operator | 2.2 |
| Policy Enforcement Point and Step-Up Gate | enforcing against a target resource type; applying `AccessScope` as a query predicate | 2.2 |
| Audit Emitter | publishing domain events alongside audit records | the Apply-side wave |

`SettingsReaderClient` is the reason two of these wait until 2.5 rather than 2.2: it is the effective-**value** read path, so nothing implements it until value resolution exists, and there is nothing to register before then.

The step-up half of *Policy Enforcement Point and Step-Up Gate* waits on a contract this service does not own at all — DESIGN.md §4.2 assigns it to `authn-resolver`. See the DECOMPOSITION note on the Apply-side wave.

### SDK Crate, Models and Value Objects

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-gear-foundation-sdk-models`

The system **MUST** provide a `settings-service-sdk` crate carrying the domain models exchanged with consumers, including the request and response shapes of the reader trait, the effective-value response with its `source` member, the opaque secret handle type, and a parsed setting-key value object. Models **MUST** serialize stably, because consuming gears depend on that wire shape.

**Implements**:
- `cpt-cf-settings-service-algo-gear-foundation-key-parse`

**Constraints**: `cpt-cf-settings-service-constraint-supplied-as-gear`

**Touches**:
- Entities: SDK models, setting-key value object, `SecretHandle`

### SDK Trait Contracts

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-gear-foundation-sdk-traits`

The system **MUST** define `SettingsReaderClient` with `get_effective`, `get_effective_bulk`, and `resolve_secret`, and `SettingsContributionClient` with `register_declarations` and `retire_declarations`. `get_effective_bulk` **MUST** return an independent per-key outcome so a mixed batch never fails wholesale. The traits **MUST** facade local against remote so `ClientHub` can bind either without the consumer changing.

Every fallible trait method **MUST** return `Result<_, CanonicalError>`, the platform-wide error type. No settings-specific error type may appear in a trait signature: the platform ADR on SDK error surfaces fixes the boundary at canonical precisely so that adding a failure mode later is not a breaking change for every consuming gear. The typed view lives beside the trait, never inside it.

Change notification is **not** a reader-trait method. A consumer that must actively re-apply a setting subscribes through the consumer activation contract — `subscribe(keys, handler)` plus `report_outcome(...)`, specified by [DESIGN-activation](../DESIGN-activation.md) §4.2 *Consumer Activation SDK* and delivered in this SDK alongside the reader traits. An earlier draft listed a `watch` method on `SettingsReaderClient`: it named the same capability twice, and its stated shape — a plain change stream — could not carry the per-setting acknowledgement the contract requires, since delivery repeats until the consumer accounts for every notified key and an apply does not settle until it does. `watch` has since been removed from DESIGN.

**Implements**:
- `cpt-cf-settings-service-algo-gear-foundation-gear-init`

**Constraints**: `cpt-cf-settings-service-constraint-supplied-as-gear`

**Touches**:
- Entities: `SettingsReaderClient`, `SettingsContributionClient`

### Reader Degradation Contract and Typed Projection

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-gear-foundation-error-taxonomy`

Reader-facing failures **MUST** be mutually distinguishable, because each demands a different response from the consuming gear: `Unavailable` when the value could not be resolved and a retry may succeed; `Retired` when the declaration was withdrawn, so a retry will never help and the consumer should drop the dependency; and `NotFound` when no declaration row exists, which the consumer resolves against its own boot ordering. The service **MUST NOT** substitute a Schema Default on failure, since the default lives in the same database and is equally unreachable.

This distinction **MUST** be delivered as a typed projection over `CanonicalError`, not as a parallel error taxonomy. Two of the three outcomes have no canonical category of their own — `Retired` is not a canonical concept, and the credential-absent case of `resolve_secret` collides with the resolver's own `NotFound` — so without a projection a consumer would have to compare context strings to satisfy a contract this document makes mandatory.

The projection **MUST**:

- be infallible, built from `CanonicalError` rather than fallibly parsed from it
- carry a catch-all `Other { canonical }` variant, so a canonical category the SDK does not model still reaches the consumer with full fidelity and adding one later breaks nobody
- distinguish the credential-absent outcome of `resolve_secret` from the resolver's `NotFound`, since a consumer that mistakes one for the other will hand a non-secret placeholder to a backend as if it were a credential
- carry no transport fields; `instance` and `trace_id` belong to the Problem envelope
- keep each wire-string constant beside the typed value it projects into, with conversions in both directions, so the two cannot drift apart
- be pinned by round-trip Problem tests that assert every wire-string constant appears at its expected JSON path

Variant count **MUST** be driven by what a consumer does differently, not by the service's internal vocabulary.

**Implements**:
- `cpt-cf-settings-service-algo-gear-foundation-problem-mapping`

**Touches**:
- Entities: reader degradation contract, typed projection, wire-string vocabulary

### RFC-9457 Problem Mapping

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-gear-foundation-problem-mapping`

The system **MUST** render every 4xx and 5xx response as `application/problem+json` carrying `type` as a `gts://` URI, `title`, `status`, and `trace_id`, and **MUST** carry field-level violations on a validation rejection, each naming the field, a stable machine-readable reason, and a human-readable description. An unrecognized internal error **MUST** map to `500` without leaking an internal message.

The gear **MUST NOT** render the document itself. Under the platform ADR on SDK error surfaces the gear's only decision is which **canonical error category** a `DomainError` maps to; the status, the `gts://` type URI, the title, and the placement of field violations are derived from that category by the platform renderer. A gear that minted its own status or its own type URI would put a second source of truth beside the platform's, which is what fixing the boundary at the canonical error exists to prevent.

Two consequences follow, and they are why this DoD no longer names `422` or an `errors` array:

- a validation rejection is the canonical *invalid-argument* category, which the platform renders as **`400`**, not `422`;
- field violations are carried in the problem document's context under the platform's own member and key names, not as a top-level `errors` array of `field`/`code`/`message`.

`DESIGN.md` §4.3 *Error Response Format* still shows the pre-ADR shape — a `422`, a service-minted `gts://…settings.error_validation.v1~` type, and a top-level `errors` array. **DESIGN is what needs correcting**, not this mapping; until it is, DESIGN §4.3's example is not implementable.

**Implements**:
- `cpt-cf-settings-service-algo-gear-foundation-problem-mapping`

**Touches**:
- Entities: `DomainError`, Problem document

### Persistence Adapter and Migration Harness

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-gear-foundation-persistence`

The system **MUST** provide a migration harness that runs outstanding migrations at startup, and **MUST** establish the schema prerequisites and persistence conventions that every settings table depends on — the database extensions their DDL assumes, and access through `SecureConn` with repositories generic over `DBRunner` rather than over a raw pool. A failed migration **MUST** prevent the gear from serving traffic rather than leaving a partially migrated schema reachable.

This DoD **MUST NOT** create a domain table or the SeaORM entity over it. `DECOMPOSITION.md` entry 2.1 scopes this feature to *"migration harness and shared schema conventions; no domain tables in this feature"*: `categories` belongs to entry 2.2, `setting_declarations` to 2.3, and `setting_values` to 2.5, so that a table and the code that reads it arrive together and neither outlives a later decision to reshape it. What cannot belong to any one feature is what lands here — a database extension is shared, idempotent, and would otherwise have every feature racing to create it.

An earlier wording required *"SeaORM entity scaffolding"* here. That could not be satisfied while the same wave excluded domain tables, since an entity requires a table to be an entity of.

**Implements**:
- `cpt-cf-settings-service-algo-gear-foundation-gear-init`

**Constraints**: `cpt-cf-settings-service-constraint-postgres-primary-storage`

**Touches**:
- Entities: persistence adapter, migration harness

### Gear Scaffold and ClientHub Registration

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-gear-foundation-gear-scaffold`

The system **MUST** provide a `#[toolkit::gear]` annotated gear that registers its client traits into `ClientHub` and binds them per the active deployment profile, and **MUST** take bootstrap configuration — database and broker endpoints, service identity, TLS, ports — from ToolKit config at gear init, never from a managed setting.

**Implements**:
- `cpt-cf-settings-service-algo-gear-foundation-gear-init`

**Constraints**: `cpt-cf-settings-service-constraint-supplied-as-gear`

**Touches**:
- Entities: gear scaffold, `ClientHub` registration

### REST and OData Infrastructure

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-gear-foundation-rest-odata`

The system **MUST** provide shared `OperationBuilder` wiring, OData `$filter`, `$select`, and `$orderby` parsing against a per-resource field mapping, cursor-based pagination helpers, and `If-Match` and ETag handling returning `428` when the header is missing on a mutating request and `412` when it is stale. An expression referencing an unmapped field or unsupported operator **MUST** be rejected rather than silently ignored.

**Implements**:
- `cpt-cf-settings-service-algo-gear-foundation-precondition`

**Touches**:
- Entities: OData field mapping, pagination cursor, ETag

### Policy Enforcement Point and Step-Up Gate

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-gear-foundation-authz-stepup`

The system **MUST** enforce authorization through the `PolicyEnforcer` PEP against the target GTS resource type, **MUST** derive an `AccessScope` from the decision's constraints for handlers to apply as a query and visibility predicate, and **MUST** verify a credential step-up assertion established at the identity provider before any behavior-affecting action. An authorization or entitlement decision that cannot be obtained **MUST** deny.

**Implements**:
- `cpt-cf-settings-service-algo-gear-foundation-authz-stepup`

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`, `cpt-cf-settings-service-constraint-step-up-at-idp`

**Touches**:
- Entities: `PolicyEnforcer`, `AccessScope`, step-up assertion

### Audit Emitter

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-gear-foundation-audit-emitter`

The system **MUST** provide the shared Audit Emitter through which every mutating feature publishes its audit records and domain events, supporting pre-image and post-image capture so later features can audit a mutation's before and after state.

**Touches**:
- Entities: Audit Emitter

## 6. Acceptance Criteria

- [ ] The gear starts against an empty database, runs all migrations to completion, and reports ready
- [ ] A gear start with a failing migration does not serve traffic and surfaces the failure
- [ ] A gear start with a missing required bootstrap value fails at startup rather than falling back to a default
- [ ] `SettingsReaderClient` and `SettingsContributionClient` resolve through `ClientHub` after initialization
- [ ] The same consumer code compiles and runs against both the in-process and the REST binding of `SettingsReaderClient`
- [x] `get_effective_bulk` returns an independent outcome per key, and one failing key does not fail the others in the batch
- [x] Every fallible trait method returns `CanonicalError`; no settings-specific error type appears in any trait signature
- [x] `Unavailable`, `Retired`, and `NotFound` are distinguishable by a consumer without string matching
- [x] The credential-absent outcome of `resolve_secret` is a different variant from the resolver's `NotFound`, so a placeholder can never be mistaken for a configured credential
- [x] A canonical category the projection does not model arrives as the catch-all variant with its canonical value intact, rather than being dropped or collapsed
- [x] Adding a variant to the projection leaves every trait signature unchanged
- [x] Each wire-string constant converts to its typed value and back, and an unrecognised string is preserved rather than discarded
- [x] A round-trip Problem test asserts every wire-string constant appears at its expected JSON path
- [x] The projection carries no `instance` or `trace_id` field
- [x] A reader failure never returns a substituted Schema Default in place of an error
- [x] A well-formed setting key parses into its value-type segment and instance segment, and the parsed key round-trips to a byte-identical string
- [x] The category and leaf name are recoverable from the instance segment's namespace and type tokens, for a module-supplied key as well as an admin-composed one
- [x] A bare value type, or a key with three or more segments, is rejected as not being a value type followed by an instance id
- [x] A trailing `~` on the instance segment is rejected, because that would make it a type
- [x] An identifier-level fault — missing `gts.` prefix, uppercase — is reported as identifier-level, and a segment-level fault such as `/` in a token is reported with its segment number
- [x] A segment carrying five name tokens before the version is rejected, since the GTS grammar admits exactly four
- [ ] Every 4xx and 5xx response carries `Content-Type: application/problem+json` with `type`, `title`, `status`, and `trace_id` populated
- [x] A validation rejection carries one field violation per offending field, each naming the field, a stable machine-readable reason, and a human-readable description
- [x] The gear selects a canonical error category and never mints its own HTTP status or `gts://` type URI
- [x] An unrecognized internal error maps to `500` and its response body contains no internal message
- [ ] A mutating `PATCH` or `DELETE` without `If-Match` returns `428`
- [ ] A mutating `PATCH` or `DELETE` with a stale `If-Match` returns `412`
- [ ] An OData expression on an unmapped field or with an unsupported operator is rejected rather than ignored
- [ ] A pagination cursor round-trips and reproduces a stable order across pages
- [ ] A request whose authorization decision cannot be obtained is denied rather than allowed
- [ ] A behavior-affecting action without a valid step-up assertion is denied, and one with a step-up assertion bound to a different principal is also denied
- [ ] An `AccessScope` returned by the PEP is applied as a query predicate and not merely as a post-filter
- [x] The Audit Emitter records a mutation with both pre-image and post-image available to the caller
