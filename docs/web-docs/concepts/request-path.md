---
title: Request path
description: How a request flows through Gears — gateway authentication and licensing, gear-level authorization, scoped data access, and response.
sidebar:
  label: Request path
  order: 5
---

A request flows through clearly separated responsibilities. Each stage has one job, and no stage can be skipped by accident.

```text
Client
  → API Gateway        validates the token → SecurityContext; checks license
  → Gear handler       passes SecurityContext to the domain service
  → domain service     calls PolicyEnforcer (PEP); runs business logic
      → AuthZ Resolver (PDP) returns decision + row-level constraints → AccessScope
  → SecureConn         applies AccessScope as automatic WHERE clauses
  → response           (RFC-9457 problem on error)
```

## Who owns what

- **The API Gateway** owns authentication and license validation. It validates the token and injects a `SecurityContext` (subject id, subject tenant, token scopes). Gear code never parses tokens.
- **Gear domain services** own authorization. Each operation asks the `PolicyEnforcer` (the PEP) for an `AccessScope` before touching data.
- **The AuthZ Resolver** (the PDP) makes the decision and returns row-level constraints that compile into the `AccessScope`.
- **`SecureConn`** applies the `AccessScope` as automatic `WHERE` clauses, so tenant isolation and ABAC happen at the query layer.
- **The domain service** runs business logic over already-scoped data.

## Why it is structured this way

Separating these responsibilities means a handler cannot accidentally read another tenant's data, skip authorization, or hand-roll an error format. Identity flows as explicit data (`SecurityContext` as the first argument), authorization is fail-closed, and errors render as consistent [RFC-9457 problems](../error-model/).

## See also

- [API Gateway and OpenAPI](../api-gateway-openapi/) — the ingress and contract layer.
- [Security and multi-tenancy](../security-and-tenancy/) — authn/authz and the tenant tree.
- [Secure data path](../secure-data-path/) — the full defense-in-depth layering.
