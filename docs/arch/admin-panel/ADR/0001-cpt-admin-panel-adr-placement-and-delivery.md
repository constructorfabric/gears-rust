---
status: accepted
date: 2026-06-25
decision-makers: gears-rust admin-panel working group
---

# Embed the Admin Panel in the gears-rust monorepo, served by the example server


> **Revision (2026-06-30) — placement direction updated after review.**
> Reviewer feedback on [#4145](https://github.com/constructorfabric/gears-rust/pull/4145#discussion_r3495062634) reopened this decision: embedding ~all of the SPA's TypeScript under `apps/admin-panel/` forces every other gears-rust-based project to copy that code to get an admin panel (the Django-admin counter-example: install once, run against any project). The agreed target is a **generic, reusable admin SPA** driven at runtime by the aggregated `/cf/openapi.json` plus gear-emitted admin metadata, shipped as a **pre-built artifact** with **zero per-project TypeScript**, ultimately living in a **dedicated `constructorfabric/` repository** (Option B's home) and loaded by the existing thin Rust serving shim (`mount_admin_spa()` in the api-gateway gear).
>
> This does **not** revert to Option B wholesale. Option B's main rejection driver — *atomic changes spanning gear APIs and admin resources become cross-repo* — is mitigated by moving the admin metadata **next to each gear's API** (emitted server-side), so a gear's API and its admin descriptors still change together even after the SPA is extracted. Sequencing: keep the SPA in this monorepo until it is fully generic, then extract it as a follow-up (smaller, reviewable steps). The original Option A analysis below stands as the v0 record; the end-state is the generic/distributable shape.
>
> **Open (pending reviewer):** the transport for metadata OpenAPI can't express (custom actions, safety levels, tenant-scope, layout) — a config file shipped with the panel, `x-cf-admin-*` OpenAPI vendor extensions emitted via `OperationBuilder` (leaning), or a dedicated descriptor endpoint. Distribution format (release tarball / npm / container) and the exact extract trigger are a later round. See [ADR-0003](0003-cpt-admin-panel-adr-resource-discovery.md) for the discovery side of this pivot.

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A: Embed in the monorepo, served by the example server](#option-a-embed-in-the-monorepo-served-by-the-example-server)
  - [Option B: Separate repository + `make admin` sidecar](#option-b-separate-repository--make-admin-sidecar)
  - [Option C: SeaORM Pro as an integrated database-admin gear](#option-c-seaorm-pro-as-an-integrated-database-admin-gear)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-admin-panel-adr-placement-and-delivery`

## Context and Problem Statement

The Integrated Admin Panel (issue #4144) needs a home: where does its frontend (a browser SPA) and any backing code live, and how is it delivered to operators? The `gears-rust` repository is a pure-Rust monorepo with no existing JavaScript/TypeScript build tooling, and the example server (`apps/cf-gears-example-server`) does not currently serve static web assets. The issue explicitly leaves this open ("it could be even separate repo in `constructorfabric/` if needed"). Where should the admin panel be developed and from where should it be served?

## Decision Drivers

- **Single local bring-up** — `make example` should be able to start the platform and the admin panel together for development, demo, and e2e testing.
- **Authority boundary reuse** — the panel must talk to Gears APIs through the API Gateway prefix (`/cf`) and reuse the platform security model; co-location simplifies same-origin requests and auth.
- **Atomic changes** — admin resources track gear APIs; keeping them in the monorepo allows atomic refactors and consistent CI/tooling (a stated repo principle).
- **e2e coverage** — the existing Python/pytest e2e harness builds and starts the example server; an embedded panel slots into that flow.
- **Monorepo purity** — the repo is otherwise pure Rust; introducing a Node/JS toolchain adds a build step and dependency surface.
- **Issue intent** — the issue allows either an integrated gear/example-server delivery or a `make admin` sidecar.

## Considered Options

- **Option A**: Embed the panel in the gears-rust monorepo; build the SPA into static assets served by the example server under `/cf/admin`.
- **Option B**: Develop the panel in a separate `constructorfabric/` repository; run it as a sidecar via a `make admin` target.
- **Option C**: Adopt SeaORM Pro as an integrated database-admin gear.

## Decision Outcome

Chosen option: **Option A — embed in the monorepo, served by the example server**, because it gives a single local bring-up (`make example`), same-origin access to the API Gateway and security model, atomic changes alongside gear APIs, and a clean fit with the existing e2e harness. The added JS/TS toolchain is isolated to the admin subtree and gated behind a build step so the rest of the pure-Rust workflow is unaffected.

Implementation direction:

- The SPA lives under a dedicated admin subtree in the monorepo; its production build emits static assets.
- The example server serves those assets under the API Gateway prefix at `/cf/admin`, using `tower-http`'s `ServeDir` with an SPA fallback to `index.html` (the `tower-http` dependency is already available to the gateway).
- A `make admin` target builds the SPA and runs the example server with an `admin` feature/config so the panel can be brought up with one command; `make example` remains the plain-server path.
- The JS/TS build is confined to the admin directory with its own package manifest; CI builds it as a separate step and does not couple it to the Rust build graph.
- The exact serving gear (API Gateway vs a thin admin gear), the build-output location, and how assets are embedded (on-disk `ServeDir` vs compile-time embed) are settled in DESIGN.

### Consequences

- The monorepo gains a JavaScript/TypeScript toolchain (package manifest, lockfile, `node_modules`, a build step) confined to the admin subtree; CI must build the SPA and publish its assets before the server serves them.
- The example server must mount a static-asset route with SPA fallback under `/cf/admin` without shadowing existing gear routes; this requires a defined mount point in the gateway/router (specified in DESIGN).
- `.gitignore`, `Makefile`, and CI configuration must account for the SPA build output and `node_modules`; a decision on whether built assets are committed or built on demand is required in DESIGN.
- e2e tests can exercise the panel against a locally-started example server through the existing harness.
- The frontend framework choice (Refine vs alternatives) is a separate decision recorded in a later ADR; this ADR fixes only placement and delivery.
- Because the panel is co-located and same-origin, authentication can reuse the platform's bearer flow rather than a cross-origin scheme (auth flow detailed in DESIGN/later ADR).

### Confirmation

Confirmed by design review of DESIGN.md (static-asset mount point, build pipeline, `make admin` target), by the presence of a working `make admin` bring-up, and by e2e tests that load the panel and drive v0 flows against the example server.

## Pros and Cons of the Options

### Option A: Embed in the monorepo, served by the example server

The SPA is developed in the monorepo and built into static assets that the example server serves under `/cf/admin`.

- Good, because a single `make` bring-up starts the platform and panel together for dev, demo, and e2e.
- Good, because same-origin access to the API Gateway and security model avoids cross-origin auth/CORS complexity.
- Good, because admin resources can change atomically with the gear APIs they track, with consistent CI/tooling.
- Good, because it fits the existing Python/pytest e2e harness that builds and starts the example server.
- Neutral, because it matches one of the two delivery shapes the issue explicitly allows.
- Bad, because it introduces a JS/TS toolchain into an otherwise pure-Rust monorepo (build step, dependencies, `node_modules`).

### Option B: Separate repository + `make admin` sidecar

The panel lives in its own `constructorfabric/` repository and runs as a sidecar process wired in via a `make admin` target.

- Good, because the gears-rust monorepo stays pure Rust with no JS/TS toolchain.
- Good, because the frontend can have an independent release cycle.
- Bad, because two repositories must be checked out, versioned, and kept in sync for a single bring-up.
- Bad, because cross-origin/sidecar wiring complicates auth, CORS, and same-origin assumptions.
- Bad, because atomic changes spanning gear APIs and admin resources become cross-repo, harder to review and to test in CI/e2e.

### Option C: SeaORM Pro as an integrated database-admin gear

Adopt SeaORM Pro, a low-code admin over SeaORM entities driven by an auto-generated GraphQL layer.

- Good, because it is a native Rust/SeaORM stack and the fastest path to a raw database admin.
- Bad, because it is database/entity-driven via Seaography GraphQL, not OpenAPI/API-driven — it bypasses the Gears API authority boundary and business logic.
- Bad, because its frontend is closed-source and RBAC is paywalled, conflicting with the OSS and multi-tenant requirements.
- Bad, because it cannot drive the custom actions (suspend, approve, convert, etc.) and tenant-scoped authorization the panel requires.
- Neutral, because it remains acceptable only as an internal DB-inspection experiment, not the primary admin surface.

## More Information

- Issue: constructorfabric/gears-rust#4144
- The example server already depends on `tower-http`, which provides `ServeDir` for static-asset serving with SPA fallback.
- The API Gateway serves the aggregated OpenAPI document and proxies gear routes under the configurable prefix (default `/cf`); the panel is served alongside under `/cf/admin`.
- SeaORM Pro: https://www.sea-ql.org/sea-orm-pro/docs/introduction/sea-orm-pro/
- Frontend framework selection is recorded separately (see DESIGN.md and the frontend-framework ADR).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-admin-panel-fr-admin-shell` — fixes where the admin shell is built and from where it is served.
- `cpt-admin-panel-fr-openapi-discovery` — co-location gives same-origin access to the aggregated OpenAPI document via the gateway.
- `cpt-admin-panel-nfr-backend-authority` — same-origin delivery keeps the backend (API Gateway + resolvers) as the authority boundary.
- `cpt-admin-panel-contract-openapi` — the panel consumes the gateway-served OpenAPI contract under `/cf`.
