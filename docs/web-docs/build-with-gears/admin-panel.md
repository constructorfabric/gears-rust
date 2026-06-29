---
title: Admin panel
description: The descriptor-driven admin console — platform vs tenant modes, the admin-context endpoint, and how to add a resource.
sidebar:
  label: Admin panel
  order: 11
---

Gears ships an optional **admin console** — a Django-admin-style management UI that
talks to the platform exclusively through the existing Gears HTTP APIs. It is a React
single-page app (`apps/admin-panel`, built on [Refine](https://refine.dev) + Ant Design)
served beside the example server; there is no privileged back door, so anything the panel
can do, an API client can do with the same token.

## Two admin modes

The panel renders the same screens in two postures, decided entirely by the backend:

- **Platform (operator) admin** — manages all authorized tenants and gear-owned
  resources, runs cross-tenant and destructive operations (with confirmation).
- **Tenant admin** — manages only the current tenant or its authorized subtree, sees
  tenant-owned objects only, and never the global tenant list.

Tenant isolation is **not** a UI concern: it is enforced server-side (the account-management
`SecureORM` `InTenantSubtree` predicate). The panel only hides controls the caller has no
capability for; the backend re-authorizes every request.

## Startup context

On login the panel calls one endpoint to discover who it is talking to:

```
GET /account-management/v1/admin/context
```

It returns the principal, home tenant, an `admin_mode` (`platform` | `tenant`), a list of
coarse `capabilities` hints used for capability-driven navigation, and
`non_production_auth` — `true` whenever the demo static-auth stub is in effect, so the UI
can surface a "non-production" banner. The capabilities are advisory for UI gating only.

## Run it locally

Start the example server with the admin feature set and its config:

```sh
make admin
```

This serves the Gears APIs at `http://localhost:8087/cf` and exposes two **non-production**
dev tokens (a platform admin and a tenant admin) via the static-auth stub. Then run the SPA:

```sh
cd apps/admin-panel
npm install
npm run dev
```

Open the printed URL and pick a role to sign in. In production the built SPA is served by
the example server under `/cf/admin`.

## Resources are described, not hardcoded

Every manageable object is one **resource descriptor** in
`apps/admin-panel/src/resources/registry.ts`. A descriptor declares the resource key, owning
gear, the API paths for each CRUD verb, the fields (with per-view visibility and
create-time immutability), required capabilities, a safety level, tenant scope, and any
custom actions. The data provider and the generated List / Show / Create / Edit screens are
driven entirely off these descriptors — a verb is offered only when its path is declared, so
resources without full CRUD degrade gracefully instead of rendering broken controls.

Adding a new admin object is therefore additive: append a descriptor, no core changes. For
example, a read-only resource backed by a list endpoint:

```ts
{
  key: "gears",
  label: "Gears",
  owningGear: "gear-orchestrator",
  tenantScope: "global",
  safety: "read-only",
  idField: "name",
  capabilities: { read: "gears:read" },
  paths: { list: () => `/gear-orchestrator/v1/gears` },
  fields: [
    { name: "name", inList: true, readOnly: true },
    { name: "deployment_mode", label: "Mode", inList: true },
  ],
}
```

Custom (non-CRUD) actions — tenant `suspend` / `unsuspend` / soft-delete, conversion
`approve` / `reject` / `cancel` — are declared the same way, with a capability gate and a
confirmation prompt for destructive ones.

## v0 coverage

The first version covers the admin shell, the session/context view, the enabled-gears
summary, tenants (list, detail, create/update and lifecycle actions), resource groups
(CRUD), conversions (list/detail + actions), and read-only types/GTS and gear status. See
`docs/arch/admin-panel/` (PRD, DESIGN, ADRs) for the full design and the deferred items
(runtime OpenAPI discovery, gear-contributed descriptors, raw-database operator fallback).
