# Feature: Gear Foundation


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Create a Note Through the Route](#create-a-note-through-the-route)
  - [Use a Note Through the Client](#use-a-note-through-the-client)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Start the Gear](#start-the-gear)
  - [Validate the Note Text](#validate-the-note-text)
  - [Scope Note Access to the Caller's Tenant](#scope-note-access-to-the-callers-tenant)
  - [Map Domain Errors to Problem Errors](#map-domain-errors-to-problem-errors)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Gear Anatomy and Startup](#gear-anatomy-and-startup)
  - [Create Route](#create-route)
  - [Text Validation](#text-validation)
  - [Permission Check and Tenant Scoping](#permission-check-and-tenant-scoping)
  - [Client in ClientHub](#client-in-clienthub)
  - [RFC 9457 Error Behavior](#rfc-9457-error-behavior)
  - [Tenant-Scoped Storage and Migration](#tenant-scoped-storage-and-migration)
  - [Placeholder Boundary](#placeholder-boundary)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-gear-foundation-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-gear-foundation`

## 1. Feature Context

### 1.1 Overview

The Gear Foundation is the small backend shell that every other Construct feature is built on. It has one authenticated REST route, one tenant-scoped table, and one in-process client. This document describes the code as it is today.

### 1.2 Purpose

Construct needs a base that is a real platform gear: an SDK crate, an implementation crate, a permission check for each operation, and storage that is limited to the caller's tenant. The shell proves this pattern once, so later features can copy it. The decision to build Construct as a gear is in `cpt-cf-construct-adr-construct-is-a-gear`. The standard gear layout is described in `cpt-cf-construct-tech-rust-gear`.

The shell stores a "foundation note": an id, a tenant id and a text. The note is a placeholder. The entity `FoundationNote` is the placeholder entity. `NewFoundationNote` is the SDK request model, not a DESIGN entity. It has no DESIGN id, and the DESIGN has no table, entity, component or sequence for it. The first feature that adds a real route or table replaces it. Per DECOMPOSITION, that feature is Subject Settings.

**Out of scope** (as in DECOMPOSITION entry 2.1): any DESIGN table, entity, component or sequence; routes for records, profiles and subjects; the full Rust SDK interface `cpt-cf-construct-interface-rust-sdk`, which grows with each later feature (the foundation owns only the client trait of the shell); and the replacement of the placeholder, which the first feature with a real route or table does (Subject Settings).

**Requirements**: `cpt-cf-construct-fr-tenant-isolation`, `cpt-cf-construct-fr-access-control`

**Principles**: None. The shell is a placeholder and applies no DESIGN principle.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-platform-auth` | Authenticates the caller before the request reaches Construct. Supplies the caller identity and tenant in the security context. Decides, through the AuthZ resolver, whether the caller may create or read a note. |
| `cpt-cf-construct-actor-consumer-app` | The nearest PRD actor for a caller of the shell. It sends the create request or calls the in-process client. The shell is a placeholder, so the PRD describes no real consumer for the foundation note. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.1
- **ADR**: [ADR-0001](../ADR/0001-cpt-cf-construct-adr-construct-is-a-gear.md)
- **Dependencies**: None. Every other Construct feature depends on this one.

## 2. Actor Flows (CDSL)

**Use cases**: None. The shell is a placeholder and no PRD use case describes it.

The shell has these public surfaces:

- One REST route, `POST /construct/v1/foundation-notes`. The code registers the route without a prefix. The API gateway prepends its configured `prefix_path`, which is empty by default. The DESIGN lists public paths with an `/api` prefix, which a deployment sets. There is no GET route.
- The SDK client `ConstructClientV1` with two operations, `create_note` and `get_note`. The gear registers it in ClientHub, the platform registry of in-process clients. Callers in other gears use it.

### Create a Note Through the Route

- [ ] `p1` - **ID**: `cpt-cf-construct-flow-gear-foundation-create-note`

**Actor**: `cpt-cf-construct-actor-consumer-app`

**Success Scenarios**:
- The caller is allowed to create notes and sends a valid text. The route answers 201 with the new note: id, tenant id and text. The tenant id is the caller's tenant.

**Error Scenarios**:
- There is no authenticated identity: the gateway refuses the request (401) and the handler never runs. This is a gap: no test in this crate covers it, and the real gateway was not run.
- The request has no JSON content type, whatever the body: 415.
- The request has a JSON content type and the body is empty or is not valid JSON: 400.
- The body is valid JSON of the wrong shape, for example a `text` that is not a string: 422.
- The three body cases above are answered as `application/problem+json`. The toolkit's `Json` extractor produces them, in this order, before the handler body runs.
- The server cannot read the body, or the body is over the framework body limit: the status of the extractor's rejection (for example 413), as `application/problem+json`. The toolkit maps every `JsonRejection`, including the body-read rejection, in `libs/toolkit/src/api/rest/extract/error.rs`. No test covers this case.
- The text is empty after trimming, contains the NUL character, or is longer than the configured maximum in bytes: 400 with the field `text` named.
- The platform does not allow the caller to create notes: 403.
- The scope the platform returns does not cover the caller's tenant at insert: 403.
- The permission check itself fails, or the database connection or insert fails: 500, without internal detail.

**Steps**:
1. [ ] - `p1` - Caller sends `POST /construct/v1/foundation-notes` with a JSON content type and a JSON body that has one field, `text`. The route is marked authenticated, so the platform authenticates the caller first. - `inst-create-send`
2. [ ] - `p1` - **IF** the caller has no authenticated identity - `inst-create-unauth`
   1. [ ] - `p1` - The gateway refuses the request (401). The handler never runs. Gap: no test in this crate covers this step. - `inst-create-unauth-return`
3. [ ] - `p1` - API: the extractors of the handler run in parameter order: the `SecurityContext` and the service from the request extensions, then the toolkit's `Json` body extractor. The handler body runs only if all of them succeed. - `inst-create-parse`
4. [ ] - `p1` - **IF** the request has no JSON content type, whatever the body - `inst-create-bad-type`
   1. [ ] - `p1` - **RETURN** 415 as `application/problem+json` - `inst-create-bad-type-return`
5. [ ] - `p1` - **IF** the body cannot be read, or is over the framework body limit - `inst-create-bad-read`
   1. [ ] - `p1` - **RETURN** the status of the extractor's rejection (for example 413) as `application/problem+json`. No test covers this. - `inst-create-bad-read-return`
6. [ ] - `p1` - **IF** the request has a JSON content type and the body is empty or is not valid JSON - `inst-create-bad-body`
   1. [ ] - `p1` - **RETURN** 400 as `application/problem+json` - `inst-create-bad-body-return`
7. [ ] - `p1` - **IF** the body is valid JSON of the wrong shape, for example `{"text":5}` - `inst-create-bad-shape`
   1. [ ] - `p1` - **RETURN** 422 as `application/problem+json` - `inst-create-bad-shape-return`
8. [ ] - `p1` - Run `cpt-cf-construct-algo-gear-foundation-validate-text` on the text. It runs before the permission check. - `inst-create-validate`
9. [ ] - `p1` - **IF** validation fails - `inst-create-invalid`
   1. [ ] - `p1` - **RETURN** 400 with a field violation on `text` - `inst-create-invalid-return`
10. [ ] - `p1` - Run `cpt-cf-construct-algo-gear-foundation-scope-note-access` for the action `create` without a note id - `inst-create-scope`
11. [ ] - `p1` - **IF** access is denied - `inst-create-denied`
    1. [ ] - `p1` - **RETURN** 403 - `inst-create-denied-return`
12. [ ] - `p1` - Build the note with a new random id, the tenant of the security context, and the text exactly as sent (it is not trimmed) - `inst-create-build`
13. [ ] - `p1` - DB: acquire a database connection, then INSERT `construct__foundation_notes` (id, tenant_id, text) through the secure ORM, checked against the access scope - `inst-create-insert`
14. [ ] - `p1` - **IF** the scope does not cover the tenant of the note, the connection cannot be acquired, or the insert fails - `inst-create-insert-fail`
    1. [ ] - `p1` - The row is not stored (`insert_outside_scope_is_forbidden` checks this for the scope case). A connection failure takes the same branch as an insert failure. **RETURN** the domain error: forbidden for the scope case, the database error for the others. It is mapped by `cpt-cf-construct-algo-gear-foundation-map-errors` on the REST route - `inst-create-insert-fail-return`
15. [ ] - `p1` - **RETURN** 201 with the note as JSON: id, tenant_id, text - `inst-create-return`

### Use a Note Through the Client

- [ ] `p1` - **ID**: `cpt-cf-construct-flow-gear-foundation-client-note`

**Actor**: `cpt-cf-construct-actor-consumer-app`

**Success Scenarios**:
- Another gear resolves `ConstructClientV1` from ClientHub and calls `create_note` with a security context. It gets the note back. It then calls `get_note` with the note id and gets an equal note.

**Error Scenarios**:
- The id is unknown, or the note belongs to another tenant: not found, which is canonical status 404.
- Invalid text, denied access, or a failure inside (including a database connection or select failure): the canonical error for that case (see `cpt-cf-construct-algo-gear-foundation-map-errors`).

The client returns the note or a canonical error. It has no HTTP status of its own. The HTTP-only cases of the create flow (401, malformed body, wrong shape, missing content type) do not apply to it.

**Steps**:
1. [ ] - `p1` - Caller takes the client from ClientHub and calls `create_note` or `get_note` with its security context - `inst-client-call`
2. [ ] - `p1` - **IF** the call is `create_note` - `inst-client-create`
   1. [ ] - `p1` - Run the steps `inst-create-validate` to `inst-create-insert-fail` of `cpt-cf-construct-flow-gear-foundation-create-note` (validate, scope, build, insert). Read each RETURN in that range as producing the matching domain error (validation, forbidden, internal or database), not an HTTP status. The result is the new note or a domain error. The mapping to a canonical error happens once, in `inst-client-map`. - `inst-client-create-run`
3. [ ] - `p1` - **ELSE** the call is `get_note` - `inst-client-get`
   1. [ ] - `p1` - Run `cpt-cf-construct-algo-gear-foundation-scope-note-access` for the action `get` with the note id - `inst-client-get-scope`
   2. [ ] - `p1` - **IF** access is denied, the result is the forbidden domain error. Continue at the error-conversion step (`inst-client-map`); the database select does not run. - `inst-client-get-denied`
   3. [ ] - `p1` - DB: acquire a database connection, then SELECT one row from `construct__foundation_notes` where id equals the given id, restricted by the access scope (tenant). **IF** the connection or the select fails, the result is the database domain error; continue at the error-conversion step (`inst-client-map`). - `inst-client-get-select`
   4. [ ] - `p1` - **IF** no row is found, the result is the not-found domain error. The answer is the same for an unknown id and for another tenant's note. Continue at the error-conversion step (`inst-client-map`). - `inst-client-get-missing`
   5. [ ] - `p1` - Otherwise the result is the note. Continue at `inst-client-return`. - `inst-client-get-return`
4. [ ] - `p1` - **IF** the result is a domain error - `inst-client-map`
   1. [ ] - `p1` - Convert it with `cpt-cf-construct-algo-gear-foundation-map-errors` and **RETURN** the canonical error - `inst-client-map-return`
5. [ ] - `p1` - **RETURN** the note - `inst-client-return`

## 3. Processes / Business Logic (CDSL)

### Start the Gear

- [x] `p1` - **ID**: `cpt-cf-construct-algo-gear-foundation-start-gear`

**Input**: The gear configuration (optional), the database provider and the AuthZ resolver client, both supplied by the runtime.

**Output**: A running service and the client registered in ClientHub. The migration set and the REST route are handed over by two separate capability hooks (see the second list below the steps).

The gear declares the dependency `authz_resolver` and the capabilities `rest` and `db`. It holds no raw database connection of its own. The runtime supplies the database provider, and all statements go through the secure ORM.

**Steps** (the order of `init` in `gear.rs`):
1. [x] - `p1` - Read the config, or use defaults when none is set. The only key is `max_text_length` (bytes, default 1000). An unknown key is rejected. - `inst-start-config`
2. [x] - `p1` - Ask the runtime for the database provider. Fail `init` if the gear has none. - `inst-start-db`
3. [x] - `p1` - Resolve the AuthZ resolver client from ClientHub and build the policy enforcer. Fail `init` if it is missing. - `inst-start-authz`
4. [x] - `p1` - Convert the config to the service limit with `ServiceConfig::try_from`, which checks the range - `inst-start-config-range`
   1. [x] - `p1` - **IF** `max_text_length` is 0 or above 65535 (the capacity of a MySQL TEXT column), fail `init` with an error that names the allowed range - `inst-start-config-fail`
5. [x] - `p1` - Build the service from the database provider, the repository, the policy enforcer and the validated limit. Store it once. **IF** the gear is initialized a second time, fail. - `inst-start-service`
6. [x] - `p1` - Register `ConstructClientV1`, backed by the service, in ClientHub - `inst-start-client`
7. [x] - `p1` - **RETURN** success - `inst-start-return`

**Capability hooks, called separately by the runtime**:
1. [x] - `p1` - The runtime calls the database capability hook, which returns the migration set with one migration, `initial_001`. - `inst-start-migrations`
2. [x] - `p1` - When the runtime registers REST routes, the REST capability hook registers the single route on the router. It fails if the service is not initialized. - `inst-start-rest`

### Validate the Note Text

- [x] `p1` - **ID**: `cpt-cf-construct-algo-gear-foundation-validate-text`

**Input**: The text of a new note and the configured maximum length in bytes.

**Output**: Accepted, or a validation error on the field `text`.

**Steps**:
1. [x] - `p1` - **IF** the text is empty after trimming white space - `inst-text-blank`
   1. [x] - `p1` - **RETURN** a validation error "must not be empty" - `inst-text-blank-return`
2. [x] - `p1` - **IF** the text contains the NUL character - `inst-text-nul`
   1. [x] - `p1` - **RETURN** a validation error "must not contain the NUL character" - `inst-text-nul-return`
3. [x] - `p1` - **IF** the text is longer than the maximum, counted in bytes and not in characters - `inst-text-long`
   1. [x] - `p1` - **RETURN** a validation error that names the maximum - `inst-text-long-return`
4. [x] - `p1` - **RETURN** accepted. A text of exactly the maximum is accepted. - `inst-text-ok`

### Scope Note Access to the Caller's Tenant

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-gear-foundation-scope-note-access`

**Input**: The security context, the action (`create` or `get`), and, for `get`, the note id.

**Output**: An access scope. The flows pass it to the repository: inserts are checked against it and reads are filtered by it, so a note of another tenant is not visible. Or a denial.

**Steps**:
1. [ ] - `p1` - Take the tenant from the subject tenant of the security context. The tenant never comes from the request body or from any input of the caller. - `inst-scope-tenant`
2. [ ] - `p1` - Ask the AuthZ resolver, through the policy enforcer, to decide on the resource type `construct.foundation_note` with the action. The tenant is passed as a resource property. For `get`, the note id is passed as the resource id. The resource type supports the properties owner tenant and resource id. - `inst-scope-evaluate`
3. [ ] - `p1` - **IF** the platform denies the request, or the constraints it returns cannot be compiled into a scope - `inst-scope-denied`
   1. [ ] - `p1` - **RETURN** a forbidden domain error (shown to the caller as 403). A denial is logged at debug level, a compile failure at warn level (both in `domain/error.rs`). - `inst-scope-denied-return`
4. [ ] - `p1` - **IF** the evaluation itself fails - `inst-scope-failed`
   1. [ ] - `p1` - **RETURN** an internal domain error (shown to the caller as 500). It is logged at error level. - `inst-scope-failed-return`
5. [ ] - `p1` - **RETURN** the scope - `inst-scope-return`

### Map Domain Errors to Problem Errors

- [x] `p1` - **ID**: `cpt-cf-construct-algo-gear-foundation-map-errors`

**Input**: A domain error.

**Output**: A canonical error. On the REST route it is written as an RFC 9457 Problem response. The resource type id for notes is written in the source as `cf.construct.foundation.note.v1~`, with the toolkit's `gts_id!` macro. The final wire form was not confirmed (the macro adds a configurable prefix at compile time, and the tests build the expected value with the same macro).

**Steps**:
1. [x] - `p1` - First, scope errors from the secure ORM are turned into domain errors (`sea_orm_repo.rs`): denied and tenant-not-in-scope become forbidden, an invalid scope becomes internal, and a database error stays a database error. An unknown scope-error variant is logged at error level and then becomes internal. - `inst-map-scope`
2. [x] - `p1` - **IF** the error is not found - `inst-map-not-found`
   1. [x] - `p1` - **RETURN** not found (404) with the message "Note not found" and the resource type "note" - `inst-map-not-found-return`
3. [x] - `p1` - **IF** the error is a validation error - `inst-map-validation`
   1. [x] - `p1` - **RETURN** invalid argument (400) with a field violation: the field, the message and the reason `VALIDATION_ERROR` - `inst-map-validation-return`
4. [x] - `p1` - **IF** the error is forbidden - `inst-map-forbidden`
   1. [x] - `p1` - Log a warning (in `api/rest/error.rs`; an enforcer denial was already logged at debug level when it became a domain error). **RETURN** permission denied (403) with the reason `ACCESS_DENIED`. The internal message is not sent to the caller. - `inst-map-forbidden-return`
5. [x] - `p1` - **IF** the error is internal or from the database - `inst-map-internal`
   1. [x] - `p1` - Log at error level. **RETURN** internal (500). The tests check that the `detail` of the response does not contain the internal text. - `inst-map-internal-return`

## 4. States (CDSL)

None. A foundation note has no lifecycle: it is created once and read. It is never updated or deleted, and it has no status field. This document adds no state machine.

## 5. Definitions of Done

### Gear Anatomy and Startup

- [x] `p1` - **ID**: `cpt-cf-construct-dod-gear-foundation-anatomy`

The system **MUST** be built as two crates with separated layers: the SDK crate `cf-gears-construct-sdk` (client trait and models, with no infrastructure types) and the gear crate `cf-gears-construct` (API, domain and infrastructure layers). The gear **MUST** declare the dependency `authz_resolver` and the capabilities `rest` and `db`, and **MUST** hold no raw database connection. It **MUST** start as in `cpt-cf-construct-algo-gear-foundation-start-gear`.

**Implements**:
- `cpt-cf-construct-algo-gear-foundation-start-gear`

**Touches**:
- API: `POST /construct/v1/foundation-notes`
- Entities: `FoundationNote` (placeholder entity), `NewFoundationNote` (SDK request model, not a DESIGN entity)

**Verified by**: the config unit tests `max_text_length_is_bounded_by_the_text_column`, `unknown_keys_are_rejected`, `default_config_converts_to_the_default_service_limit` and `invalid_config_does_not_convert`. They cover the config conversion, which rejects a limit below 1 or above 65535. `init` is not run in tests. The startup order itself (database provider, AuthZ resolver, client registration, double initialization) has no test. The declared dependency (`deps = [authz_resolver]`), the capabilities (`rest`, `db`), the two-crate layout and the absence of a raw database connection are verified by review and compilation only.

### Create Route

- [x] `p1` - **ID**: `cpt-cf-construct-dod-gear-foundation-create-route`

The system **MUST** expose `POST /construct/v1/foundation-notes`, authenticated, taking a JSON body with `text` and answering 201 with id, tenant id and text. An empty or invalid JSON body with a JSON content type **MUST** get 400, valid JSON of the wrong shape (for example a `text` that is not a string) **MUST** get 422, and a request without a JSON content type, whatever the body, **MUST** get 415. All three **MUST** be `application/problem+json`, produced by the toolkit's `Json` extractor before the handler body runs. A body the server cannot read, or one over the framework body limit, **MUST** get the status of the extractor's rejection (for example 413) as `application/problem+json`. The route **MUST** be registered with OperationBuilder with the operation id `construct.create_foundation_note` and the tag `Foundation`, and it **MUST** declare explicitly that it requires no license feature: it calls `.require_license_features::<License>([])` with an empty list, which the builder demands for an authenticated route and which sets no license requirement on the operation. The handler **MUST** take the `SecurityContext` and the service from the request extensions, then the `Json` body, in that parameter order.

**Implements**:
- `cpt-cf-construct-flow-gear-foundation-create-note`

**Touches**:
- API: `POST /construct/v1/foundation-notes`
- Entities: `FoundationNote`

**Verified by**: `permitted_subject_gets_201_with_the_created_note`, `blank_text_gets_400`, `text_with_nul_character_gets_400`, `malformed_json_gets_400`, `empty_body_gets_400`, `malformed_json_gets_a_problem_body` (400, problem+json), `json_of_the_wrong_shape_gets_a_problem_body` (422, problem+json) and `missing_content_type_gets_a_problem_body` (415, problem+json) (routes), and `note_converts_to_dto_field_for_field`, `create_request_converts_to_new_note` (DTO). These run the router in process on an in-memory SQLite database. No route test sends oversized text: the 400 for oversize is shown by the service tests `blank_and_oversized_text_is_rejected`, `multibyte_text_over_the_byte_limit_is_rejected` and `text_of_exactly_the_byte_limit_is_accepted`, and by the error-mapping test `validation_maps_to_400_with_field_violation`. No test: authentication by the real gateway (the 401 case), an unreadable or oversized body, and the OperationBuilder registration, the authenticated flag, the empty license feature list, the operation id and the tag (no test asserts them).

### Text Validation

- [x] `p1` - **ID**: `cpt-cf-construct-dod-gear-foundation-text-validation`

The system **MUST** reject a text that is empty after trimming, contains the NUL character, or is longer than the configured maximum in bytes. It **MUST** accept a text of exactly the maximum. The maximum **MUST** be between 1 and 65535 and default to 1000.

**Implements**:
- `cpt-cf-construct-algo-gear-foundation-validate-text`

**Touches**:
- Entities: `NewFoundationNote` (SDK request model, not a DESIGN entity)

**Verified by**: `blank_and_oversized_text_is_rejected`, `text_with_nul_character_is_rejected`, `text_of_exactly_the_byte_limit_is_accepted`, `multibyte_text_over_the_byte_limit_is_rejected` (service), `validation_maps_to_400_with_field_violation` (error mapping), and the config tests listed under Gear Anatomy and Startup. No route test sends oversized text.

### Permission Check and Tenant Scoping

- [x] `p1` - **ID**: `cpt-cf-construct-dod-gear-foundation-access-scope`

The system **MUST** check the permission for each operation (`create`, `get`) on the resource type `construct.foundation_note` through the AuthZ resolver, **MUST** take the tenant only from the security context, and **MUST** limit every insert and read to the returned scope. A note of another tenant **MUST** look the same as an unknown note. This is the platform pattern for `cpt-cf-construct-fr-tenant-isolation` and `cpt-cf-construct-fr-access-control`.

**Implements**:
- `cpt-cf-construct-algo-gear-foundation-scope-note-access`
- `cpt-cf-construct-flow-gear-foundation-create-note`
- `cpt-cf-construct-flow-gear-foundation-client-note`

**Touches**:
- DB Table: `construct__foundation_notes`
- Entities: `FoundationNote`

**Verified by**: `permitted_subject_creates_and_reads_a_note`, `subject_without_permission_is_denied`, `note_of_another_tenant_is_not_found`, `unknown_note_is_not_found` (service), `insert_outside_scope_is_forbidden`, `insert_inside_scope_round_trips` (repository), and `denied_subject_gets_403` (routes). The denied-subject case is covered only with a stub resolver. It was never run through the real gateway and the real AuthZ resolver. No test: an AuthZ evaluation failure gives 500, and a compile failure gives 403 (`domain/error.rs`). The denial of `get_note` is tested on the service only, not through the client.

### Client in ClientHub

- [x] `p1` - **ID**: `cpt-cf-construct-dod-gear-foundation-client`

The system **MUST** provide `ConstructClientV1` in the SDK with `create_note` and `get_note`, both taking a security context and returning canonical errors. The gear **MUST** register it in ClientHub.

**Implements**:
- `cpt-cf-construct-flow-gear-foundation-client-note`

**Touches**:
- Entities: `FoundationNote` (placeholder entity), `NewFoundationNote` (SDK request model, not a DESIGN entity)

**Verified by**: `local_client_round_trips_a_note_and_maps_not_found`. The registration in ClientHub at startup is not tested.

### RFC 9457 Error Behavior

- [x] `p1` - **ID**: `cpt-cf-construct-dod-gear-foundation-error-mapping`

The system **MUST** answer every error as an RFC 9457 Problem with the status in `cpt-cf-construct-algo-gear-foundation-map-errors` and **MUST NOT** leak internal detail in 403 or 500 answers. Body errors (400, 422, 415) are also Problems, produced by the toolkit's `Json` extractor (see `cpt-cf-construct-dod-gear-foundation-create-route`). An unknown scope-error variant **MUST** be logged at error level before it is mapped. An enforcer denial is logged at debug level in the domain mapping and at warn level in the REST mapping.

**Implements**:
- `cpt-cf-construct-algo-gear-foundation-map-errors`

**Touches**:
- API: `POST /construct/v1/foundation-notes`

**Verified by**: `not_found_maps_to_404_with_resource_type`, `validation_maps_to_400_with_field_violation`, `forbidden_maps_to_403`, `internal_maps_to_500_without_leaking_detail`, `database_maps_to_500_without_leaking_detail` (error mapping), and `map_scope_error_maps_each_variant`, `unknown_scope_error_message_is_neutral` (repository). The two 500 tests check the `detail` of the response for the internal and the database error. No test checks the message content of a 403 body, and no test checks the log output.

### Tenant-Scoped Storage and Migration

- [x] `p1` - **ID**: `cpt-cf-construct-dod-gear-foundation-storage`

The system **MUST** supply to the runtime a migration that creates the table `construct__foundation_notes` (id, tenant id, text) and the index `idx_construct__foundation_notes__tenant` on the tenant column, following the `construct__` naming prefix. The migration **MUST** be safe to run twice (create if not exists). The entity **MUST** be declared tenant-scoped for the secure ORM, with the tenant column `tenant_id` and the resource column `id`.

**Implements**:
- `cpt-cf-construct-algo-gear-foundation-start-gear`

**Touches**:
- DB Table: `construct__foundation_notes`
- Entities: `FoundationNote`

**Verified by**: `mysql_ddl_uses_binary_uuid_columns`, `postgres_and_sqlite_ddl_use_namespaced_names`, `identifiers_are_namespaced_and_within_63_bytes`, `postgres_and_sqlite_ddl_are_unchanged` (migration). The SQLite DDL is run by the service, route and repository tests through the test migration runner. The MySQL and PostgreSQL DDL is only asserted as text and was never run. No test: the `down` step (drop table), and running the migration twice (the DDL uses `IF NOT EXISTS`, but no test runs it twice).

### Placeholder Boundary

- [ ] `p1` - **ID**: `cpt-cf-construct-dod-gear-foundation-placeholder-boundary`

The system **MUST NOT** add a DESIGN table, entity, component or sequence, a route for records, profiles or subjects, or a permission defined in the DESIGN or PRD. The route, the client operations, the table and the placeholder checks on `construct.foundation_note` (`create`, `get`) are a placeholder. Subject Settings replaces them together with the note.

**Implements**:
- `cpt-cf-construct-flow-gear-foundation-create-note`

**Verified by**: review only. No test checks this boundary.

## 6. Acceptance Criteria

- [ ] A caller with permission posts valid text and gets 201 with a new id, the caller's tenant id and the same text (`permitted_subject_gets_201_with_the_created_note`).
- [ ] A text that is blank or has a NUL character is refused with 400 on the route (`blank_text_gets_400`, `text_with_nul_character_gets_400`). No route test sends oversized text: the refusal of a text over the byte limit is shown by the service tests (`blank_and_oversized_text_is_rejected`, `multibyte_text_over_the_byte_limit_is_rejected`, `text_with_nul_character_is_rejected`) plus the error-mapping test `validation_maps_to_400_with_field_violation`. These tests do not assert what is stored.
- [ ] A text of exactly the byte limit is stored (`text_of_exactly_the_byte_limit_is_accepted`).
- [ ] A request without a JSON content type, whatever the body, gets 415 as `application/problem+json` (`missing_content_type_gets_a_problem_body`).
- [ ] An empty or invalid JSON body with a JSON content type gets 400 as `application/problem+json` (`malformed_json_gets_400`, `empty_body_gets_400`, `malformed_json_gets_a_problem_body`).
- [ ] Valid JSON of the wrong shape, for example `{"text":5}`, gets 422 as `application/problem+json` (`json_of_the_wrong_shape_gets_a_problem_body`).
- [ ] Not verified (gap): a body the server cannot read, or one over the framework body limit, gets the status of the extractor's rejection (for example 413) as `application/problem+json`. No test covers it.
- [ ] A caller the platform denies gets 403 (`subject_without_permission_is_denied`, `denied_subject_gets_403`). The tests do not assert that nothing is stored, and they do not check the message content of the 403 body.
- [ ] Not verified (gap): a request without an authenticated identity is refused by the gateway with 401 and the handler never runs. No test in this crate covers it, and the real gateway was not run.
- [ ] A note created in one tenant cannot be read from another tenant. The answer is not found, the same as for an unknown id (`note_of_another_tenant_is_not_found`, `unknown_note_is_not_found`).
- [ ] An insert whose scope does not cover the tenant of the note is refused and stores nothing (`insert_outside_scope_is_forbidden`, which looks the note up afterwards).
- [ ] The client creates and reads a note, and an unknown id comes back as a 404 problem (`local_client_round_trips_a_note_and_maps_not_found`).
- [ ] The config conversion rejects `max_text_length` below 1 or above 65535, and an unknown config key is rejected (`max_text_length_is_bounded_by_the_text_column`, `invalid_config_does_not_convert`, `unknown_keys_are_rejected`). `init` is not run in tests.
- [ ] The `detail` of a 500 answer does not contain the internal text, for an internal error and for a database error (`internal_maps_to_500_without_leaking_detail`, `database_maps_to_500_without_leaking_detail`).
- [ ] The migration names the table `construct__foundation_notes` and the index `idx_construct__foundation_notes__tenant` for each backend (`postgres_and_sqlite_ddl_use_namespaced_names`, `mysql_ddl_uses_binary_uuid_columns`).
- [ ] The tests named in this document pass. They run on an in-memory SQLite database.

## 7. Non-Functional Considerations

- **Security**: The caller and tenant come only from the security context that the platform supplies. The route is marked authenticated, and each operation has its own permission check. Another tenant's note answers as not found. The code sends no internal message in a 403 answer, and the tests check the `detail` of 500 answers (internal and database errors). The 403 body content is not checked. The unit tests use stub resolvers. The real gateway and the real AuthZ resolver were not exercised, so the 401 case is a gap.
- **Data integrity**: The id is generated by the gear. The text is stored as sent, after validation. The limit is in bytes and cannot be set above what a MySQL TEXT column holds. Inserts and reads go through the secure ORM, which applies the scope. A note is never updated or deleted by the shell.
- **Reliability**: A bad config, a missing database provider or a missing AuthZ resolver makes `init` fail, so errors show early. The migration uses create-if-not-exists. A failure of the AuthZ evaluation gives 500, not an open door (no test).
- **Observability**: The gear logs when it provides migrations and when it registers its route (info). It logs an enforcer denial (debug in the domain mapping, warn in the REST mapping), a compile failure (warn), an unknown scope-error variant (error), and internal or database errors (error). Metrics and traces were not checked on a running instance.
- **Rollback**: The migration has a `down` step that drops the table. It has no test, and it was not run. The placeholder is removed by the feature that replaces it.
- **Test layering**: At the time of writing there are 37 tests. All are unit-level, in process and on in-memory SQLite: config (4), migration DDL text (4), service and client (9), routes (9), DTO (2), error mapping (5), repository (4). There is no integration test with a real database, gateway or AuthZ resolver, and no end-to-end test.
- **Untested paths**: AuthZ evaluation failure gives 500 and compile failure gives 403 (`domain/error.rs`); the denial of `get_note` through the client (only the service is tested, with a stub resolver); the migration `down` step; running the migration twice; the OperationBuilder registration, the authenticated flag and the empty license feature list; the 401 case; an unreadable or oversized body; `init` and the startup order.
- **Compile-time gates**: The SDK forbids unsafe code. The workspace lints apply to both crates. The `domain_model` marker keeps infrastructure types out of the models and the domain at compile time. The config type rejects unknown keys when it is read. The secure ORM needs a scope before an insert or a select can run on the entity.
- **Performance, compliance and UX**: Performance and UX: not applicable. The shell has no performance target and no user interface. Compliance and personal data: the shell makes no personal-data decision. The note text is free-form, and the shell has no delete or retention path. Erasure and retention come with features 2.14 and 2.15 and with the replacement of the placeholder (see DECOMPOSITION).
- **Deviations from platform baselines**: None.
- **Not done by this feature** (see the out-of-scope paragraph in 1.2): Any DESIGN table, entity, component or sequence. Routes for records, profiles or subjects. The full Rust SDK interface `cpt-cf-construct-interface-rust-sdk`. A route to read a note: reading exists only through the client. Retiring the placeholder, which Subject Settings does.
