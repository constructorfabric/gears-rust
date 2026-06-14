---
cf: true
type: project-rule
topic: api-contracts
generated-by: auto-config
version: 1.0
---
# API Contracts

<!-- toc -->

- [REST](#rest)
- [Errors](#errors)

<!-- /toc -->

Use this when adding REST endpoints, OpenAPI routes, DTOs, SDK errors, or Problem mappings.

## REST
- Register REST routes with `OperationBuilder` and explicit response metadata. Evidence: `libs/toolkit/src/api/operation_builder.rs:386-620`
- Use stateless Axum handlers with shared state/extensions. Evidence: `libs/toolkit/src/lib.rs:13-53`
- Put authenticated/public flags, rate limits, content-type policy, and vendor extensions in operation specs. Evidence: `libs/toolkit/src/api/operation_builder.rs:196-250`

## Errors
- Convert domain errors through canonical SDK/REST mappings before wire output. Evidence: `docs/toolkit_unified_system/05_errors_rfc9457.md:31-181`
- Emit RFC-9457 `application/problem+json` responses for canonical errors. Evidence: `libs/toolkit-canonical-errors/src/problem.rs:10-64`
- Do not expose internal diagnostics in production `Problem` responses. Evidence: `libs/toolkit-canonical-errors/src/problem.rs:67-88`
