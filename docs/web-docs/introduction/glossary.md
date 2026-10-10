---
title: Glossary
description: Quick reference for the core Gears and XaaS vocabulary used throughout the docs.
sidebar:
  label: Glossary
  order: 5
---

A quick reference for the terms used across these docs.

## Platform & XaaS terms

- **XaaS** — "anything-as-a-service"; software delivered as a running service for many customers. A broad term that includes SaaS, PaaS, and IaaS.
- **Multi-tenancy** — many customers (tenants) share the same infrastructure while their data stays isolated.
- **Entitlement / licensing** — the features or limits a tenant or user is allowed to use, checked per request.
- **Usage metering** — measuring usage such as API calls, compute, storage, or tokens; used for quotas and billing.
- **Deployment shape / profile** — how the system is deployed: single-node (all gears in one process), self-hosted (a `HostRuntime` process runs some gears in-process and spawns selected gears as REST-first OoP workers), or Kubernetes-native (gears as containerized services). gRPC remains available as an opt-in transport in all distributed profiles.
- **FIPS 140-3 / GDPR / HIPAA / SOC 2 / ISO 27001** — security, crypto, and data-protection standards or certifications often relevant to XaaS products.

## Gears core model

- **Gear** — a vertically-sliced, self-contained unit of capability with a public contract, lifecycle, and optional API.
- **Toolkit** — the reusable low-level substrate (`libs/`) every gear builds on.
- **Contract / SDK** — the stable public interface (a transport-agnostic trait plus models and errors) that other gears or apps depend on.
- **Implementation** — the API, business logic, and infrastructure behind a contract.
- **Application** — what assembles multiple gears into one runnable system.
- **Runtime** — what discovers, wires, starts, and stops the system (`HostRuntime`).
- **Plugin** — a replaceable implementation behind a stable public surface.
- **ClientHub** — the typed local registry gears use to find and call each other in-process.
- **OperationBuilder** — the standard way to declare a REST operation together with its OpenAPI documentation, auth posture, schemas, and errors.

## Security & data

- **SecurityContext** — identity and tenancy information about the caller, passed explicitly through the system.
- **AccessScope** — the compiled visibility rules controlling which rows can be read/written.
- **PDP / PEP** — Policy Decision Point / Policy Enforcement Point (the authorization model).
- **SecureConn / Scopable** — framework support for tenant-aware data access that is scoped automatically as SQL `WHERE` clauses.
- **CanonicalError / Problem** — the 16-category canonical error model, rendered at the REST boundary as an RFC-9457 problem document.

## Type system

- **GTS (Global Type System)** — a system for versioned, globally identified, schema-validated type definitions and instances; the basis of Gears' extension model.
- **Types Registry** — the system gear that stores GTS schemas and instances for discovery.

## Tooling

- **`cargo gears`** — the CLI used to scaffold, configure, build, run, lint, and test Gears projects. See the [CLI documentation](/cli/).
