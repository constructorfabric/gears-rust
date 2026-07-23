---
status: accepted
date: 2026-06-25
decision-makers: gears-rust admin-panel working group
---

# Build the Admin Panel frontend with Refine (React + TypeScript)


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A: Refine + TypeScript](#option-a-refine--typescript)
  - [Option B: React Admin](#option-b-react-admin)
  - [Option C: SeaORM Pro frontend](#option-c-seaorm-pro-frontend)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-admin-panel-adr-frontend-framework`

## Context and Problem Statement

The admin panel is delivered as a browser SPA embedded in the monorepo (see [ADR-0001](./0001-cpt-admin-panel-adr-placement-and-delivery.md)). It must be metadata-driven and OpenAPI-discovered, support resources with partial CRUD, drive custom actions (suspend, unsuspend, approve, reject, cancel, retry, resolve, deprovision), enforce capability-driven navigation, and respect backend-authoritative multi-tenant isolation. Which frontend framework and stack should we build it on?

## Decision Drivers

- **OpenAPI-driven** — the panel discovers resources and operations from the aggregated OpenAPI document; the framework must allow a custom data layer over arbitrary REST.
- **Custom-action-heavy** — tenant lifecycle and conversions are action-driven, not plain CRUD; the framework must support non-CRUD operations cleanly.
- **Capability-driven access control** — navigation and write actions are gated by backend capabilities; the framework should offer a first-class access-control hook wired to our own policy.
- **Multi-tenant** — nested tenant routes and tenant-scoped views are required.
- **Licensing** — the result must remain OSS/MIT-compatible; RBAC and access control must not be paywalled.
- **Maturity & ecosystem** — active project, stable releases, usable UI components for admin CRUD.
- **Generated-but-overridable** — screens generated from metadata, with per-resource overrides for custom workflows.

## Considered Options

- **Option A**: Refine (React meta-framework) + TypeScript, Vite build, Ant Design UI.
- **Option B**: React Admin (Material-UI based).
- **Option C**: SeaORM Pro frontend.

## Decision Outcome

Chosen option: **Option A — Refine + TypeScript**, because it is MIT across the board (including RBAC and access control), provides a first-class `accessControlProvider.can()` hook that maps cleanly onto our backend capability model, exposes a `custom()` data-provider method purpose-built for our custom-action-heavy API, and is headless so it does not fight the generated/overridable screen requirement. No framework ships a production-grade generic OpenAPI generator, so a hand-written data provider is required regardless; Refine's data-provider contract is the smallest and most flexible.

Stack:

- **Language**: TypeScript.
- **Framework**: Refine (`@refinedev/core`) with React.
- **Build**: Vite, emitting a static `dist/` bundle served by the example server under `/cf/admin`.
- **UI kit**: Ant Design (`@refinedev/antd`) for batteries-included tables, forms, and filters suited to admin CRUD.
- **Routing**: React Router, with nested routes for tenant scope.
- **Data**: a custom Refine **data provider** that maps list/read/create/update/delete and `custom()` actions onto Gears API requests, consuming `x-odata-*` extensions and cursor pagination, and normalizing RFC-9457 errors. It issues requests through a small hand-written `apiFetch` helper (`src/httpClient.ts`) rather than a generated client; the spec is consumed **at runtime** for discovery — fields from component schemas, CRUD routes and custom actions from paths — so resources stay in sync with `/openapi.json` without a codegen build step.
- **Auth**: Refine **auth provider** (bearer token) and **access-control provider** (`can()`), both wired to the admin-context endpoint and the platform security model.

UI kit choice (Ant Design vs MUI) and the exact OpenAPI client tooling are refined in DESIGN; the framework decision (Refine) is fixed here.

### Consequences

- A TypeScript/React/Vite/Refine toolchain is added under the admin subtree (package manifest, lockfile), built as a separate CI step per ADR-0001.
- A custom Gears data provider must be implemented against the Refine data-provider contract, including OData filter/order mapping, cursor pagination, and RFC-9457 error normalization.
- An auth provider and an access-control provider must be implemented, both driven by the admin-context endpoint's principal, mode, and capabilities.
- A typed OpenAPI client generation step (from the gateway-served `/openapi.json`) is introduced to keep the data provider thin and spec-synced.
- Generated screens use Ant Design components by default; per-resource overrides are implemented as custom Refine pages/components.
- Resources lacking `getOne`/`update`/`delete` are handled by omitting those provider methods/actions and exposing the gap in the UI.

### Confirmation

Confirmed by design review of DESIGN.md (data-provider, auth-provider, access-control-provider contracts), by a working Refine app served under `/cf/admin`, and by e2e tests exercising list/detail/create/edit and custom actions for v0 resources.

## Pros and Cons of the Options

### Option A: Refine + TypeScript

Headless React meta-framework for internal tools/admin, with provider-based data, auth, and access control. MIT.

- Good, because it is MIT in full — RBAC and access control are not paywalled.
- Good, because `accessControlProvider.can()` maps directly onto our backend capability model for capability-driven navigation.
- Good, because the `custom()` data-provider method serves custom actions (suspend, approve, convert, etc.) directly.
- Good, because headless design leaves UI freedom and supports generated-but-overridable screens.
- Good, because it is mature and active with a large ecosystem and a UI integration for Ant Design.
- Neutral, because it has no first-party OpenAPI generator — a custom data provider is needed (true of all options).
- Bad, because headless means more upfront UI wiring than a batteries-included Material admin.

### Option B: React Admin

Mature, Material-UI based admin framework with a standardized data-provider interface.

- Good, because very mature, with a low-effort custom data provider ("a couple of hours") and batteries-included Material UI.
- Good, because basic access control moved into the OSS core (v5.3).
- Neutral, because no generic OpenAPI generator either (API Platform Admin targets Hydra/OpenAPI conventions, not arbitrary specs).
- Bad, because fine-grained RBAC (`ra-rbac`) and audit logging are in the paid Enterprise Edition.
- Bad, because custom actions are more manual (no dedicated `custom()` verb; call the HTTP client inside provider methods).
- Bad, because it is Material-centric and less headless, constraining the generated/overridable design.

### Option C: SeaORM Pro frontend

A low-code admin frontend over SeaORM entities via an auto-generated GraphQL layer.

- Good, because it is the fastest path to a raw database admin in a Rust/SeaORM stack.
- Bad, because it is entity/DB + GraphQL driven, not OpenAPI/API driven — it bypasses the Gears API authority boundary.
- Bad, because the frontend is closed-source and RBAC is paywalled, conflicting with OSS and multi-tenant requirements.
- Bad, because its low-code ceiling cannot express the custom actions and tenant-scoped authorization the panel needs.

## More Information

- Issue: constructorfabric/gears-rust#4144
- Refine: https://refine.dev/docs/ — data provider: https://refine.dev/docs/data/data-provider/ , access control: https://refine.dev/docs/authorization/access-control-provider/
- React Admin: https://marmelab.com/react-admin/ — data provider: https://marmelab.com/react-admin/DataProviderWriting.html ; Enterprise (RBAC): https://react-admin-ee.marmelab.com/
- SeaORM Pro: https://www.sea-ql.org/sea-orm-pro/docs/introduction/sea-orm-pro/
- OpenAPI typed client: https://github.com/OpenAPITools/openapi-generator , https://github.com/drwpow/openapi-typescript

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-admin-panel-fr-custom-actions` — Refine's `custom()` provider method drives non-CRUD actions.
- `cpt-admin-panel-fr-admin-shell` — capability-driven navigation via the access-control provider.
- `cpt-admin-panel-fr-generated-screens` — generated, per-resource-overridable screens via Refine + Ant Design.
- `cpt-admin-panel-fr-partial-crud` — provider methods omitted for resources lacking CRUD operations.
- `cpt-admin-panel-interface-data-provider` — fixes the framework the data-provider contract is built on.
- `cpt-admin-panel-nfr-backend-authority` — the access-control provider defers to backend capability decisions.
