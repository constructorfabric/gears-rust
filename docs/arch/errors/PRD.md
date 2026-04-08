# PRD — Canonical Error System

## 1. Overview

### 1.1 Purpose

A universal error architecture for the CyberFabric platform that replaces the current ad-hoc error system (`Problem::new()` / `ErrDef` / `declare_errors!` / `ErrorCode`) with a canonical error model providing consistent, structured error responses across all modules and transport protocols.

### 1.2 Background / Problem Statement

CyberFabric modules currently define errors independently using a mix of `Problem::new()`, `ErrDef`, `declare_errors!`, and raw `ErrorCode` enums. This leads to inconsistent error shapes across modules, making it difficult for API consumers to write reliable error-handling code. There is no compile-time enforcement of error contracts, no structured context beyond free-form strings, and no mechanism to detect accidental breaking changes to error responses.

SDK clients (credstore, tenant-resolver, authn-resolver, etc.) maintain their own ad-hoc error enums that are lossy, manual reconstructions of server responses. Each SDK reinvents error mapping independently.

### 1.3 Goals (Business Outcomes)

- Consistent, predictable error responses across all modules and transports
- Simplified client-side error handling through a finite, well-documented error vocabulary
- Automated detection of accidental error contract changes before merge

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Canonical category | One of the 16 predefined error classifications (e.g., `not_found`, `invalid_argument`) |
| Context type | A structured payload associated with a canonical category providing machine-readable error details |
| Contract part | An error field that is fixed per category and part of the public API surface |
| Variable part | An error field that changes per occurrence and is not part of the contract |
| Resource-scoped error | An error automatically tagged with the resource's GTS type identity |

## 2. Actors

### 2.1 Human Actors

#### Module Developer

**ID**: `cpt-cf-errors-actor-module-developer`

- **Role**: Writes module handler code that constructs and returns errors.
- **Needs**: A simple, type-safe API for constructing errors without consulting transport-specific documentation.

#### API Consumer

**ID**: `cpt-cf-errors-actor-api-consumer`

- **Role**: Calls CyberFabric APIs and handles error responses programmatically.
- **Needs**: Consistent error structure across all modules to write reliable error-handling logic.

### 2.2 System Actors

#### CI Pipeline

**ID**: `cpt-cf-errors-actor-ci-pipeline`

- **Role**: Runs automated checks on every PR to detect accidental breaking changes.

#### LLM Agent

**ID**: `cpt-cf-errors-actor-llm-agent`

- **Role**: Generates handler code that constructs errors.
- **Needs**: A discoverable, finite error vocabulary with compile-time safety.

## 3. Operational Concept & Environment

No module-specific environment constraints. The canonical error system runs within the standard CyberFabric runtime environment.

## 4. Scope

### 4.1 In Scope

- Canonical error categories and their structured context types
- Transport-agnostic error model (REST first, gRPC future)
- REST wire format based on an existing standard
- Resource-scoped error construction via attribute macro
- Contract enforcement via compile-time checks, snapshot tests, and CI gates
- Automatic propagation of common library errors into canonical categories
- Round-trip serialization/deserialization (server → wire → SDK)
- Public vs private detail isolation (client-facing context vs internal debug info)
- Migration of all existing modules to the new error system
- Dylint-level rules enforcement

### 4.2 Out of Scope

- gRPC transport mapping — future work
- SSE error event format — future work
- W3C trace ID extraction — separate workstream
- Error middleware catch-all — depends on foundation phase
- Error extensibility rules (custom module-specific error types beyond the 16 categories) — future work

## 5. Functional Requirements

### 5.1 Error Model

#### Transport-Agnostic Error Representation

- [ ] `p1` - **ID**: `cpt-cf-errors-fr-transport-agnostic`

The system MUST provide a single error type that is independent of any transport protocol (HTTP, gRPC, SSE). Transport-specific representations MUST be derived from this single type at the transport boundary.

- **Rationale**: Modules must not embed transport details (HTTP status codes, gRPC status) in domain code. A unified internal model enables multi-transport support without per-module changes.
- **Actors**: `cpt-cf-errors-actor-module-developer`

#### Finite Error Vocabulary

- [ ] `p1` - **ID**: `cpt-cf-errors-fr-finite-vocabulary`

The system MUST define a fixed set of canonical error categories. Every error produced by any module MUST belong to exactly one canonical category.

- **Rationale**: A finite vocabulary enables exhaustive client-side matching and prevents ad-hoc error types that break consumer expectations.
- **Actors**: `cpt-cf-errors-actor-api-consumer`, `cpt-cf-errors-actor-module-developer`

#### Structured Context per Category

- [ ] `p1` - **ID**: `cpt-cf-errors-fr-structured-context`

Each canonical category MUST have exactly one associated context type with a fixed set of fields. The context type schema (field names and types) is part of the public API contract. Context field values are variable per occurrence.

- **Rationale**: Fixed schemas allow consumers to parse error details without guessing. Structured context replaces free-form strings.
- **Actors**: `cpt-cf-errors-actor-api-consumer`

#### Mandatory Trace ID

- [ ] `p1` - **ID**: `cpt-cf-errors-fr-mandatory-trace-id`

Every error response MUST include a trace ID for request correlation.

- **Rationale**: Trace IDs enable end-to-end debugging across modules and support integration.
- **Actors**: `cpt-cf-errors-actor-api-consumer`, `cpt-cf-errors-actor-module-developer`

#### Public vs Private Detail Isolation

- [ ] `p1` - **ID**: `cpt-cf-errors-fr-public-private-isolation`

The system MUST separate client-facing error details (context, message) from internal diagnostic information (stack traces, query text). Internal details MUST NOT appear in production error responses. Internal details MUST be available in debug mode and MUST be logged for diagnostics.

- **Rationale**: Prevents information leakage in production while preserving debuggability.
- **Actors**: `cpt-cf-errors-actor-module-developer`

### 5.2 Construction & Ergonomics

#### Single-Line Error Construction

- [ ] `p1` - **ID**: `cpt-cf-errors-fr-single-line-construction`

A developer or LLM agent MUST be able to construct any canonical error in a single expression, without consulting transport documentation.

- **Rationale**: Low ceremony reduces errors and makes the API LLM-friendly.
- **Actors**: `cpt-cf-errors-actor-module-developer`, `cpt-cf-errors-actor-llm-agent`

#### Resource-Scoped Error Construction

- [ ] `p1` - **ID**: `cpt-cf-errors-fr-resource-scoped-construction`

Module vendors MUST be able to declare a resource type once and get error constructors that automatically tag every error with the resource's GTS identity, plus any additional details specific to that error occurrence.

- **Rationale**: Ensures resource identity is never forgotten and enables consumers to distinguish errors from different resource types.
- **Actors**: `cpt-cf-errors-actor-module-developer`

#### Library Error Propagation

- [ ] `p1` - **ID**: `cpt-cf-errors-fr-library-error-propagation`

Common library errors (database, serialization, IO) MUST propagate into canonical categories automatically without per-call-site mapping.

- **Rationale**: Eliminates boilerplate and ensures infrastructure errors are consistently categorized.
- **Actors**: `cpt-cf-errors-actor-module-developer`

### 5.3 Contract Enforcement

#### Compile-Time Category Safety

- [ ] `p1` - **ID**: `cpt-cf-errors-fr-compile-time-safety`

Using the wrong context type for a category, or forgetting to handle a category, MUST produce a compile error.

- **Rationale**: Compile-time enforcement is the strongest guarantee against contract violations.
- **Actors**: `cpt-cf-errors-actor-module-developer`, `cpt-cf-errors-actor-llm-agent`

#### Schema Drift Prevention

- [ ] `p1` - **ID**: `cpt-cf-errors-fr-schema-drift-prevention`

CI MUST detect changes to error response schemas (field names, types, status codes, GTS identifiers) before merge. Accidental changes MUST block the PR.

- **Rationale**: Prevents silent breaking changes to the error contract.
- **Actors**: `cpt-cf-errors-actor-ci-pipeline`

#### GTS-Based Error Identification

- [ ] `p1` - **ID**: `cpt-cf-errors-fr-gts-identification`

Each canonical error category MUST have a globally unique GTS identifier registered in the Types Registry. The GTS identifier MUST be validated at compile time and used as the error type URI in wire responses.

- **Rationale**: GTS provides a platform-wide identity system. Compile-time validation prevents identifier typos.
- **Actors**: `cpt-cf-errors-actor-api-consumer`

### 5.4 Standards Compliance

#### Existing Standard Adoption

- [ ] `p1` - **ID**: `cpt-cf-errors-fr-standard-adoption`

The REST wire format MUST be based on an existing industry standard for error responses.

- **Rationale**: Using an established standard reduces learning curve for external consumers and aligns with industry best practices.
- **Actors**: `cpt-cf-errors-actor-api-consumer`

## 6. Non-Functional Requirements

### 6.1 Module-Specific NFRs

#### Error Construction Performance

- [ ] `p2` - **ID**: `cpt-cf-errors-nfr-error-construction-perf`

Error construction MUST be O(1) enum + struct allocation with no heap allocation beyond the context payload.

- **Rationale**: Errors are on the hot path for rejected requests; construction must not add latency.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### CanonicalError Crate

- [ ] `p1` - **ID**: `cpt-cf-errors-interface-canonical-error-crate`

- **Type**: Library
- **Stability**: stable
- **Description**: Public library exporting the canonical error type, all context types, wire format conversion, and the resource-scoped error macro.
- **Breaking Change Policy**: Major version bump required for any change to category set, context type schemas, GTS identifiers, or HTTP status mappings.

### 7.2 External Integration Contracts

#### REST Error Response

- [ ] `p1` - **ID**: `cpt-cf-errors-contract-problem-response`

- **Direction**: provided by library
- **Protocol/Format**: HTTP/REST, structured JSON error response based on an industry standard
- **Compatibility**: Backward-compatible; new optional fields may be added (minor version)

## 8. Use Cases

#### Module Developer Constructs Resource Error

- [ ] `p2` - **ID**: `cpt-cf-errors-usecase-construct-resource-error`

**Actor**: `cpt-cf-errors-actor-module-developer`

**Preconditions**:
- Module has declared a resource-scoped error type for its resource

**Main Flow**:
1. Developer constructs a resource-scoped error (e.g., not_found for a given resource ID)
2. System returns a canonical error with category, context, and resource type set
3. Framework converts to the transport-specific wire format at the boundary

**Postconditions**:
- API consumer receives a structured error response with context details and trace ID

#### API Consumer Handles Error Programmatically

- [ ] `p2` - **ID**: `cpt-cf-errors-usecase-handle-error`

**Actor**: `cpt-cf-errors-actor-api-consumer`

**Preconditions**:
- Consumer knows the 16 canonical categories

**Main Flow**:
1. Consumer receives error response with `type` field containing GTS URI
2. Consumer matches on the category extracted from the GTS URI
3. Consumer parses the `context` field according to the category's schema

**Postconditions**:
- Consumer has structured error details for display or retry logic

## 9. Acceptance Criteria

- [ ] A developer can construct any canonical error in a single line of code
- [ ] Common library errors propagate into canonical categories without manual mapping at every call site
- [ ] The error vocabulary is finite and discoverable via code completion
- [ ] Changing a category, context field, or GTS identifier is detected automatically before merge
- [ ] No error reaches API consumers outside the canonical vocabulary
- [ ] Production error responses for `internal`/`unknown` contain no stack traces, query text, or file paths
- [ ] Every error response includes a trace ID
- [ ] Dylint static analysis rules enforce correct error construction patterns (no bypassing canonical errors)

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| Error library | Library where the canonical error type, context types, and automatic error propagation are defined | p1 |
| GTS Type System | Provides globally unique identifiers for error categories | p1 |
| Semver checks tool | CI tool for detecting breaking changes in public APIs | p1 |
| CI schema validation | Automated verification that error response schemas have not changed unintentionally | p1 |

## 11. Assumptions

- API consumers parse error responses programmatically (not just display the `detail` string)
- The 16 canonical categories cover all current error scenarios across all current and planned modules
- CI tooling can detect field-level changes in public error type definitions

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| CI schema checks become maintenance burden | Devs skip updates, reducing trust | One check per category; auto-generate from error definitions |
| 16 categories insufficient long-term | Ad-hoc types outside canonical set | Additive categories (minor version bump) |
| LLM agents bypass compile checks | Contract violated despite CI gates | Dylint lint rules (`dylint_lints/`) that enforce canonical error construction patterns |

## 13. Open Questions

- How should modules define custom error types outside of the 16 canonical categories, if at all?
- Should error schema specialisation (inheritance) within the 16 canonical categories be supported, and if so, how?

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
