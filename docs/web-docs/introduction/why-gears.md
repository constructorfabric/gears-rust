---
title: Why Gears
description: Why Rust and Constructor Fabric Gears are a strong foundation for secure, composable XaaS systems.
sidebar:
  label: Why Gears
  order: 2
---

Constructor Fabric Gears combines **Rust as a language** with a **secure, modular XaaS framework**. Rust moves many defects into compile-time checks; Gears adds the platform layer around Rust: tenancy, authorization, consistent APIs, lifecycle, observability, extension points, and governance.

For the longer rationale, see [WHY_GEARS.md](https://github.com/constructorfabric/gears-rust/blob/main/docs/WHY_GEARS.md) (for technical decision-makers) and [WHY_RUST.md](https://github.com/constructorfabric/gears-rust/blob/main/docs/WHY_RUST.md) (a technical comparison with Go and C#).

## Why Rust

Rust is useful for platform and backend code because it makes correctness visible in types and compilation instead of relying only on runtime checks or review discipline.

- **Errors are explicit** — fallible operations return `Result<T, E>`, so callers must handle or propagate failures.
- **Absence is explicit** — `Option<T>` replaces null-like values and forces code to handle missing data.
- **Memory and data-race safety** — ownership, borrowing, and `Send` / `Sync` prevent broad classes of unsafe shared-memory bugs at compile time.
- **Illegal states can be unrepresentable** — enums and strong domain types model valid states directly, instead of relying on comments and optional fields.
- **State evolution is safer** — exhaustive `match` forces code to handle new enum variants when workflows or domain states change.
- **Predictable performance** — no garbage collector, small runtime footprint, and zero-cost abstractions are a good fit for local, edge, on-prem, and high-throughput deployments.
- **Scoped resources** — RAII and `Drop` make cleanup deterministic for transactions, locks, spans, file handles, and pooled connections.
- **Zero-cost polymorphism** — traits and generics support strongly typed SDKs and framework abstractions without requiring runtime reflection for the common path.
- **Compile-time framework rules** — macros and lints can generate or validate route metadata, schemas, security metadata, and layer boundaries as checked Rust code.
- **Typed identities** — newtypes make `TenantId`, `UserId`, `ResourceId`, and other domain identifiers distinct even when they share the same underlying representation.

## Why Gears on top of Rust

Rust gives a safe language. It does not automatically provide a SaaS or XaaS platform. Gears supplies the reusable platform layer.

- **Pre-integrated XaaS backbone** — common concerns such as tenancy, permissions, licensing, quota, usage, events, credentials, and outbound traffic are modeled as reusable gears and SDKs.
- **Spec-driven development** — requirements, architecture, design, decomposition, feature docs, code, and tests can stay traceable through repository artifacts instead of drifting in separate documents.
- **Tenant isolation by default** — `SecurityContext`, `AccessScope`, `Scopable`, and `SecureConn` make scoped data access the normal path.
- **AuthN and AuthZ architecture** — the API Gateway validates identity; domain gears act as PEPs; the AuthZ resolver acts as PDP and returns decisions plus row-level constraints.
- **Architecture lints** — custom Dylint rules enforce layer boundaries, versioned REST paths, `OperationBuilder` metadata, [GTS](https://github.com/GlobalTypeSystem/gts-rust) identifier rules, and restrictions on direct SQL.
- **Runtime capabilities** — gears can own migrations, background tasks, REST APIs, gRPC services, SSE streams, typed configuration, transactional outbox flows, and lifecycle hooks.
- **Consistent API dialect** — `OperationBuilder` declares method, path, auth posture, schemas, errors, license posture, and OpenAPI metadata in one place.
- **Composable deployment** — the same gear code can run in-process, out-of-process over gRPC, or as containerized services.
- **Extensible domain model** — the [Global Type System (GTS)](https://github.com/GlobalTypeSystem/gts-rust) gives globally identified, versioned, schema-validated contracts for plugins, events, settings, permissions, and other extensible data.
- **Canonical errors** — Gears uses a 16-category error vocabulary aligned with gRPC status categories and renders HTTP errors as RFC-9457 `Problem` documents.
- **Operational defaults** — tracing, request IDs, structured logs, health endpoints, timeouts, body limits, rate limiting, and inflight protection are shared platform concerns.
- **FIPS-aware TLS strategy** — builds can route TLS through supported FIPS-capable providers with `--features fips`, depending on platform and provider validation status.
- **Supply-chain policy as code** — lockfiles, pinned toolchains, `cargo-deny`, advisory checks, CI scans, and FIPS dependency policy make dependency risk reviewable.
- **Build-gated safety** — workspace lint policy forbids unsafe shortcuts such as unchecked panics, unsafe code, direct unscoped ORM methods, and common async or numeric mistakes.
- **Local-first testing** — multiple gears can run together locally in one process, so cross-gear behavior can be tested before a distributed deployment.

## When Gears is a good fit

Gears is designed for teams building long-lived, governed platforms and services where consistency matters across many components.

Choose it when you need:

- **Multi-tenant SaaS or XaaS foundations** with shared security, tenancy, usage, and governance.
- **Composable product capabilities** that can be reused across services and deployment shapes.
- **AI-ready platform architecture** for chat, retrieval, model access, agents, tools, and serverless workflows.
- **Edge, on-prem, or regulated deployments** where local execution, small footprint, and supply-chain controls matter.
- **Enterprise integration** where platform services such as identity, policy, licensing, and credentials may be provided by existing systems.

Gears is deliberately not optimized for minimalism or the absolute lowest learning curve. It is a structured framework for secure, evolvable systems, not a tiny web framework or a ready-made end-user SaaS catalog.

## Where to go next

- Read the [Architecture Manifest](https://github.com/constructorfabric/gears-rust/blob/main/docs/ARCHITECTURE_MANIFEST.md) for the full architectural rationale.
- Browse [Capabilities](../../capabilities/) for the detailed component catalog and status.
- Start with [Build with Gears](../../build-with-gears/) to run the example server locally.
- See [Where Gears fits](../where-gears-fits/) to choose a greenfield or existing-platform adoption path.
