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

This builds the SPA (`npm install && npm run build`), starts the example server with the
Gears APIs at `http://localhost:8087/cf`, and **serves the built panel at
`http://localhost:8087/cf/admin`** — no separate dev server needed. Two **non-production**
dev tokens (a platform admin and a tenant admin) are exposed via the static-auth stub; open
`/cf/admin` and pick a role to sign in.

For frontend development with hot reload, run the Vite dev server instead and let it proxy
the API:

```sh
cd apps/admin-panel
npm install
npm run dev
```

### How the SPA is served

The api-gateway serves the built SPA from disk when its `admin_spa_dir` config points at the
`dist/` directory (set in `config/admin.yaml`):

```yaml
gears:
  api-gateway:
    config:
      prefix_path: "/cf"
      admin_spa_dir: "apps/admin-panel/dist"   # serves the SPA at /cf/admin
```

The static assets are mounted **outside the auth middleware** — the SPA itself is public,
while its API calls carry a bearer token. Unmatched paths fall back to `index.html` so
client-side routes deep-link and survive a refresh. Leave `admin_spa_dir` unset to disable
serving the panel (e.g. when running Vite separately).

## Resources are described, not hardcoded

Every manageable object is one entry in
**`apps/admin-panel/src/resources/admin.config.json`** — declarative data, not TypeScript.
Discovery is split by concern:

- **API-intrinsic facts come from OpenAPI.** At startup the panel reads the gateway-aggregated
  `/cf/openapi.json` and derives each resource's field types / `required` / `readOnly`, its
  CRUD verb→path mapping, custom operations, and tenant scope. The API is the single source of
  truth for what exists.
- **Presentation and policy come from the config.** Each entry supplies only what the spec
  can't express: list columns and labels, the component `schema` name, an irregular list path,
  verbs the panel intentionally withholds (`suppressVerbs`, or a `read-only` safety), custom
  actions, and field option sources.

Adding an admin object — here, or in another `gears-rust` project that ships the pre-built
panel — is therefore a JSON edit with **no TypeScript and no core changes**. A read-only
resource backed by a list endpoint:

```json
{
  "key": "gears",
  "label": "Gears",
  "owningGear": "gear-orchestrator",
  "tenantScope": "global",
  "safety": "read-only",
  "basePath": "/gear-orchestrator/v1/gears",
  "idField": "name",
  "capabilities": { "read": "gears:read" },
  "fields": [
    { "name": "name", "inList": true, "readOnly": true },
    { "name": "deployment_mode", "label": "Mode", "inList": true }
  ]
}
```

The panel derives the available verbs from the spec under `basePath` — here only a list
endpoint exists, so only a list screen renders. Custom (non-CRUD) actions — tenant `suspend` /
`unsuspend`, conversion `approve` / `reject` / `cancel` — are declared declaratively too: a
path template (`{tenant}` / `{id}` placeholders), an optional static body, a capability gate,
a safety level, and a `visibleWhen` predicate; destructive ones get a confirmation prompt.

## v0 coverage

The first version covers the admin shell, the session/context view, the enabled-gears
summary, tenants (list, detail, create/update and lifecycle actions), resource groups
(CRUD), conversions (list/detail + actions), and read-only types/GTS and gear status.
Runtime OpenAPI discovery (fields **and** routes) and declarative JSON registration are in
place. See `docs/arch/admin-panel/` (PRD, DESIGN, ADRs) for the full design and the deferred
items (extraction to a dedicated `constructorfabric/` repo as a pre-built artifact, a
production admin-role model beyond the dev stub, and the raw-database operator fallback).
