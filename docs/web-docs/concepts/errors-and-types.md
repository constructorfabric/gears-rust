---
title: Errors & the type system
description: The canonical error model (RFC-9457) and the Global Type System (GTS).
sidebar:
  label: Errors & types
  order: 5
---

## One canonical error model

All errors use a single canonical taxonomy of **16 categories**, aligned with gRPC status
codes, each with a fixed HTTP mapping and a stable type identity:

`cancelled`, `unknown`, `invalid_argument`, `deadline_exceeded`, `not_found`,
`already_exists`, `permission_denied`, `resource_exhausted`, `failed_precondition`,
`aborted`, `out_of_range`, `unimplemented`, `internal`, `service_unavailable`, `data_loss`,
`unauthenticated`.

Internally you work with a typed `CanonicalError`; at the REST boundary it renders as an
**RFC-9457** problem document (`application/problem+json`), so clients get consistent,
machine-readable errors across every gear regardless of which one produced them.

```rust
// Domain errors map to the canonical model at the API boundary:
impl From<DomainError> for CanonicalError {
    fn from(e: DomainError) -> Self {
        match &e {
            DomainError::UserNotFound { id } =>
                UserResourceError::not_found(format!("User {id} not found"))
                    .with_resource(id.to_string()).create(),
            DomainError::Forbidden =>
                UserResourceError::permission_denied().with_reason("ACCESS_DENIED").create(),
            _ => CanonicalError::internal("An internal error occurred").create(),
        }
    }
}
```

A single `From<CanonicalError> for Problem` at the REST layer handles all 16 categories, so
gears never hand-roll a wire format. The error type carries a GTS identifier (below), which
makes errors routable and lets clients branch on a stable machine code rather than a string.

## The Global Type System (GTS)

GTS gives the platform a **versioned, schema-validated type identity** for domain objects.
A GTS identifier looks like:

```text
gts.<vendor>.<package>.<namespace>.<type>.v<MAJOR>[.<MINOR>]~
```

New data types — event formats, document schemas, permission types, custom attributes — can
be introduced by registering new GTS instances, **without modifying existing endpoints or
storage**. In Rust, schemas are derived from source types and registered in the Types
Registry (the same way OpenAPI is generated from route metadata), so the catalog of types
stays in sync with the code by construction.

This is the mechanism behind extensibility: error identities, plugin contracts, and
domain-object schemas are all GTS-typed, which is what lets vendors extend the domain model
without forking the framework.
