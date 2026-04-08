---
status: accepted
date: 2026-02-28
---

# Use GTS Type System for Error Category Identification

**ID**: `cpt-cf-errors-adr-gts-error-identification`

## Context and Problem Statement

Each canonical error category needs a globally unique, machine-readable identifier that appears in wire responses and enables consumers to match errors precisely. The identifier must be validated before runtime to prevent typos and drift. What identification scheme should the platform use for error categories?

## Decision Drivers

* Platform consistency — error identifiers should use the same identity system as resource types, schemas, and other platform entities
* Compile-time validation — identifier format errors must be caught before code ships
* CI-time schema validation — schema definitions tied to identifiers must be diffable and auditable in PRs
* Machine-readability — identifiers must be parseable by consumers without human interpretation
* Uniqueness — no two error categories may share an identifier

## Considered Options

* **Option A**: Plain string constants (e.g., `"not_found"`, `"invalid_argument"`)
* **Option B**: GTS compound type identifiers (e.g., `gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~`)
* **Option C**: URN-style identifiers (e.g., `urn:cyberfabric:error:not_found:v1`)

## Decision Outcome

Chosen option: **Option B — GTS compound type identifiers**, because GTS is the platform's existing identity system, already provides compile-time format validation via macros, and integrates with the Types Registry for runtime schema discovery and CI-time schema diffing.

### Consequences

* Appropriate GTS base types and instance types must be defined for all 16 error categories and registered in the Types Registry
* Each context type must also have its own GTS schema registered, so the registry holds both error category types and their context schemas
* The error library must depend on GTS macros (`#[struct_to_gts_schema]`) for compile-time identifier validation — GTS becomes a build dependency of the error crate
* CI pipelines must export GTS schemas to JSON files and diff them against committed baselines to detect schema drift
* Wire responses carry GTS URIs in the `type` field — consumers must parse the compound type format (`base~instance~`) to extract the category name
* Any future error categories must follow the same GTS registration process before they can be used

### Confirmation

All 16 error categories have GTS identifiers defined in the PoC implementation. The `GtsSchema` trait generates JSON Schema from the Rust types, ensuring schema and code cannot diverge.

## Pros and Cons of the Options

### Option A: Plain String Constants

Use simple string constants like `"not_found"` as the error type identifier.

* Good, because short and human-readable
* Good, because no external dependency on GTS
* Bad, because no compile-time format validation — typos are runtime bugs
* Bad, because no connection to the Types Registry — schemas must be maintained separately
* Bad, because no namespace isolation — collision risk across modules

### Option B: GTS Compound Type Identifiers

Use the GTS compound type format: `gts.cf.core.errors.err.v1~cf.core.err.{category}.v1~`

* Good, because integrates with the platform's existing identity and schema infrastructure
* Good, because compile-time format validation via `#[struct_to_gts_schema]` macro
* Good, because `GtsSchema::gts_schema_with_refs()` generates JSON Schema from the same Rust types — single source of truth
* Good, because CI can export schemas to files and diff them, catching drift
* Neutral, because requires GTS as a build dependency for error crates
* Bad, because identifiers are long — 60+ characters per error type URI

### Option C: URN-Style Identifiers

Use URN format: `urn:cyberfabric:error:not_found:v1`

* Good, because standard URN format (RFC 8141)
* Good, because shorter than GTS identifiers
* Bad, because a separate identity system alongside GTS — two systems to maintain
* Bad, because no compile-time validation without a custom macro
* Bad, because no integration with the Types Registry or schema generation

## More Information

GTS identifier format for error categories:

```text
gts.cf.core.errors.err.v1~cf.core.err.{category}.v1~
└────── base type ──────┘ └───── instance type ────┘
```

The base type `gts.cf.core.errors.err.v1~` identifies "canonical error." The instance type `cf.core.err.{category}.v1~` identifies the specific category.

All 16 identifiers are defined as `const` values in the `CanonicalError` implementation. See [DESIGN.md](../DESIGN.md) § Category Reference.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements:

* `cpt-cf-errors-fr-gts-identification` — Defines GTS as the identification scheme for error categories
* `cpt-cf-errors-fr-schema-drift-prevention` — GTS schema export enables CI-time drift detection
* `cpt-cf-errors-fr-compile-time-safety` — GTS macros provide compile-time identifier validation
