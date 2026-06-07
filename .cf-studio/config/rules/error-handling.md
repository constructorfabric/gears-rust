---
cf: true
type: project-rule
topic: error-handling
generated-by: auto-config
version: 1.0
---
# Error Handling

<!-- toc -->

- [Error Flow](#error-flow)
  - [Keep domain errors internal](#keep-domain-errors-internal)
  - [Map domain errors to canonical categories](#map-domain-errors-to-canonical-categories)
  - [Return RFC 9457 Problem responses](#return-rfc-9457-problem-responses)

<!-- /toc -->

## Error Flow
### Keep domain errors internal
Represent business failures as module `DomainError` values and convert at REST/SDK boundaries.
Evidence: `modules/system/account-management/account-management/src/domain/error.rs:1` - domain error layering contract.

### Map domain errors to canonical categories
Map errors to `CanonicalError` categories with resource/field context before wire serialization.
Evidence: `modules/file-parser/src/api/rest/error.rs:5` - domain-to-canonical mapping.

### Return RFC 9457 Problem responses
Use the shared Problem conversion path for wire errors; do not hand-roll response bodies.
Evidence: `libs/modkit-canonical-errors/src/problem.rs:10` - Problem shape and conversion.
