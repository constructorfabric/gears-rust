---
title: Toolkit (libs/)
description: The low-level substrate every gear builds on — runtime, secure ORM, errors, transport, security, and observability crates.
sidebar:
  label: Toolkit (libs/)
  order: 2
---

The toolkit (`libs/`) is the low-level substrate every gear builds on.

![Libraries dependency graph](../assets/libs-dependencies.drawio.svg)

| Crate | What it provides | Key types / macros |
| --- | --- | --- |
| `toolkit` | Core runtime: gear lifecycle (`HostRuntime`), in-process composition (`ClientHub`), REST/OpenAPI wiring, SSE, transactional outbox, telemetry | `#[toolkit::gear]`, `GearCtx`, `ClientHub`, `OperationBuilder`, `SseBroadcaster` |
| `toolkit-macros` | Proc-macros for gear discovery and domain validation | `#[domain_model]`, `#[gear(...)]` |
| `toolkit-db` | Secure ORM over SeaORM: scoped access, transactions | `SecureConn`, `SecureTx`, `DBProvider`, `AccessScope` |
| `toolkit-db-macros` | Entity security-dimension derive | `#[derive(Scopable)]`, `#[secure(...)]` |
| `toolkit-security` | Core security types | `SecurityContext`, `AccessScope`, `ScopableEntity` |
| `toolkit-canonical-errors` | 16-category canonical errors + RFC-9457 rendering | `CanonicalError`, `Problem` |
| `toolkit-auth` | AuthN/AuthZ integration types | `PolicyEnforcer`, `AuthZResolverApi` |
| `toolkit-http` | HTTP client with OpenTelemetry tracing | `HttpClient`, `.with_otel()` |
| `toolkit-odata` | OData `$filter` / `$orderby` / `$select` + cursor pagination | `Page<T>`, OData extractors |
| `toolkit-odata-macros` | OData-filterable DTO derive | `#[derive(ODataFilterable)]` |
| `toolkit-gts` / `-macros` | [Global Type System](https://github.com/GlobalTypeSystem/gts-rust): schema collection & generation | GTS schema registration |
| `toolkit-sdk` | SDK-pattern helpers and transport-agnostic contracts | facade/query helpers |
| `toolkit-transport-grpc` | gRPC transport for out-of-process gears | gRPC client/connect helpers |
| `toolkit-node-info`, `toolkit-utils` | Node/deployment info; shared utilities | — |
| `rustls-corecrypto-provider`, `rustls-fips-shim` | FIPS 140-3 crypto routing per platform | TLS provider shims |

## See also

- [Cross-cutting features](../cross-cutting-features/) — how you use these crates while building.
- [Concepts](../../concepts/) — the mental model behind the runtime, secure ORM, and errors.
