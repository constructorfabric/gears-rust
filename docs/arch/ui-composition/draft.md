# UI Composition — Draft

> **Status:** draft, for discussion. Sections 1–2 state intent; sections 3–5 document how the
> FrontX frontend actually works today, verified by running it.
>
> **Evidence base:** [constructorfabric/gears-frontx](https://github.com/constructorfabric/gears-frontx)
> at commit `910a4ca4` (branch `develop`, 2026-07-01) — the commit was built and run locally, and
> every path and line number below was checked against it. Package layout changed in `develop`
> HEAD; the second column of each table gives the current path.

---

## 1. Service goal

Provide the UI with a structure that describes how content is presented — for example `Header`,
`Menu`, `Footer`, `Content`.

When a new Gear is registered it must appear in the UI automatically, gaining some or all of the
following capabilities without bespoke frontend work:

- **Navigation entry** — visible in the relevant navigation surfaces (admin console; possibly
  teacher-facing consoles).
- **Gear settings editing** — admin-only screens for the gear's own configuration.
- **Generic CRUD tables** — a list view over the gear's entities / domains / API, without a
  hand-written screen per entity.
- **Generic input overrides** — Django-style forms, where a gear may override the generic
  rendering via a *well-known instance*.
- **Generic table overrides** — Django-style filters and columns, again overridable via a
  *well-known instance*.

The unit of extension is therefore a declaration, not code: a gear declares *what* it offers, and
the shell decides *where* and *how* it is rendered.

## 2. Delivery priorities

`P0` and `P1` are priority bands, not sequential steps: items within a band are worked in
parallel, and `P1` only starts once `P0` gives it something to build on. Status markers follow the
repository convention — `[x]` means the capability exists today, `[ ]` means nothing is
implemented yet.

### P0 — Basic navigation

| Deliverable | Status | Scope | Owned by |
|---|---|---|---|
| Schemas & types convention | `[ ]` | Naming and versioning rules for navigation types; PRD and Design must land before implementation | architecture |
| Navigation shell | `[ ]` | Web UI that renders navigation and mounts gear-provided screens | FrontX |
| Type schemas & instances | `[ ]` | Register, validate, notify | TypeRegistry |
| Navigation API assembly | `[ ]` | Logic that composes the navigation endpoints (Starlark, CEL) | Serverless |

### P1 — Generic admin (Django-like)

| Deliverable | Status | Scope | Owned by |
|---|---|---|---|
| Schemas & types convention | `[ ]` | Same as P0, extended to forms and tables; PRD and Design first | architecture |
| Gear settings storage & API | `[ ]` | Persistence and API for per-gear configuration | Setting Service |
| Per-gear declaration | `[ ]` | Entities, screens and overrides a gear contributes | Gear Manifest |
| Entity contract for CRUD screens | `[x]` | Source of truth for generic CRUD screens — **done**, already available per gear | OpenAPI |

---

## 3. FrontX today: how navigation actually works

### 3.1 The left menu is registry-driven — there is no static menu list

The shell does not hold a list of navigation entries. It reads the MFE registry and renders
whatever is registered in the *screen* domain.

[`src/app/layout/Menu.tsx`](https://github.com/constructorfabric/gears-frontx/blob/910a4ca4/src/app/layout/Menu.tsx)
(`develop`: `template-standard/src-app/app/layout/Menu.tsx`):

| Line | What happens |
|---|---|
| 55 | `mfeRegistry.getExtensionsForDomain(FRONTX_SCREEN_DOMAIN)` — the only source of menu entries |
| 57 | sort by `presentation.order` (missing order defaults to `999`, i.e. last) |
| 70 | click handler emits `executeActionsChain({ action: { type: FRONTX_ACTION_MOUNT_EXT, target: <screen domain>, payload: { subject: <extension id> } } })` |

Navigation is **not** URL-driven. `presentation.route` exists in the schema but the shell does not
consume it — switching screens is a mount action against a domain, not a route transition.

One caveat that applies to `910a4ca4` specifically: a project scaffolded with `frontx create` got a
**static** `Menu.tsx` with a single hardcoded `Home` entry and no registry lookup. The CLI's
template (`packages/cli/template-sources/project/src/app/layout/Menu.tsx`) had simply fallen behind
the shell it was supposed to mirror — both files exist in that same commit, one registry-driven and
one not. Resolved upstream since: `develop` keeps a single registry-driven `Menu.tsx` under
`template-standard/src-app/`, with no separate shell to drift from.

### 3.2 Where the menu instances live

The source of truth is one `mfe.json` per MFE package, sitting next to that package's code:

| File (`910a4ca4`) | extensions | entries | domains |
|---|---|---|---|
| [`src/mfe_packages/demo-mfe/mfe.json`](https://github.com/constructorfabric/gears-frontx/blob/910a4ca4/src/mfe_packages/demo-mfe/mfe.json) | 5 | 5 | 1 |
| [`src/mfe_packages/_blank-mfe/mfe.json`](https://github.com/constructorfabric/gears-frontx/blob/910a4ca4/src/mfe_packages/_blank-mfe/mfe.json) | 1 | 1 | 0 |
| [`src/mfe_packages/widgets-fixture-a/mfe.json`](https://github.com/constructorfabric/gears-frontx/blob/910a4ca4/src/mfe_packages/widgets-fixture-a/mfe.json) | 2 | 1 | 0 |
| [`src/mfe_packages/widgets-fixture-b/mfe.json`](https://github.com/constructorfabric/gears-frontx/blob/910a4ca4/src/mfe_packages/widgets-fixture-b/mfe.json) | 1 | 1 | 0 |

In `develop`: `template-standard/src-app/mfe_packages/<pkg>/mfe.json`.

A single extension declaration looks like this — this is the whole contract for "appear in the
menu":

```json
{
  "id": "gts.frontx.mfes.ext.extension.v1~frontx.screensets.layout.screen.v1~frontx.demo.screens.profile.v1",
  "domain": "gts.frontx.mfes.ext.domain.v1~frontx.screensets.layout.screen.v1",
  "entry": "gts.frontx.mfes.mfe.entry.v1~frontx.mfes.mfe.entry_mf.v1~frontx.demo.mfe.profile.v1",
  "presentation": {
    "label": "Profile",
    "icon": "lucide:user",
    "route": "/profile",
    "order": 20
  }
}
```

The menu that these files actually produce (every extension carrying `presentation`):

| order | label | icon | route | package |
|---|---|---|---|---|
| 10 | Hello World | `lucide:globe` | /hello-world | demo-mfe |
| 20 | Profile | `lucide:user` | /profile | demo-mfe |
| 30 | Current Theme | `lucide:palette` | /current-theme | demo-mfe |
| 40 | UIKit Elements | `lucide:component` | /uikit-elements | demo-mfe |
| 100 | Blank Home | `lucide:home` | /blank-home | _blank-mfe |
| 200 | Widgets Host | `lucide:layout-grid` | /widgets-host | demo-mfe |

`widgets-fixture-a` and `widgets-fixture-b` do not appear in the menu: their extensions target a
`widgets` domain that `demo-mfe` declares itself — `demo-mfe` acts as a **nested FrontX
application** owning its own domain, and the two fixtures mount concurrently inside the Widgets
Host screen. Composition is therefore recursive, not just shell → MFE.

### 3.3 From repository to browser

```text
src/mfe_packages/<pkg>/mfe.json           # hand-written; source of truth
  → <pkg>/dist/mfe-manifest.json          # build: frontxMfGts copies mfe.json and enriches it
                                          #   with data from Module Federation's mf-manifest.json
  → public/generated-mfe-manifests.json   # aggregate over all MFEs (generate-mfe-manifests.ts)
  → fetch('/generated-mfe-manifests.json')  # runtime (bootstrap.ts:321)
  → gtsPlugin.register(...) + registry.registerExtension(...)
  → Menu.tsx reads the registry
```

| Stage | File (`910a4ca4`) | `develop` HEAD |
|---|---|---|
| Build-time enrichment | [`packages/screensets/src/build/mf-gts.ts`](https://github.com/constructorfabric/gears-frontx/blob/910a4ca4/packages/screensets/src/build/mf-gts.ts) — reads `mfe.json` (line 885), writes enriched `dist/mfe-manifest.json`, and emits shared deps as standalone ESM | `template-standard/src/build/mf-gts.ts` |
| Aggregation | [`scripts/generate-mfe-manifests.ts`](https://github.com/constructorfabric/gears-frontx/blob/910a4ca4/scripts/generate-mfe-manifests.ts) — output at `public/generated-mfe-manifests.json` (line 385) | `template-standard/scripts/generate-mfe-manifests.ts` |
| Runtime registration | [`src/app/mfe/bootstrap.ts`](https://github.com/constructorfabric/gears-frontx/blob/910a4ca4/src/app/mfe/bootstrap.ts) — strict order: schemas → manifest → domains → entries → extensions | `template-standard/src-app/app/mfe/bootstrap.ts` |
| MF loading | `packages/screensets/src/mfe/handler/MfeHandlerMF.ts` | `packages/mfes/src/handler/MfeHandlerMF.ts` |

**No service and no database sit anywhere in this chain.** Between build and browser the instances
live in a static JSON file served by Vite. That is precisely the step a Type Registry replaces.

### 3.4 GTS schemas

Core types — [`packages/screensets/src/mfe/gts/frontx.mfes/schemas/`](https://github.com/constructorfabric/gears-frontx/tree/910a4ca4/packages/screensets/src/mfe/gts/frontx.mfes/schemas)
(`develop`: `packages/gts-plugin/src/frontx.mfes/schemas/`), JSON Schema draft 2020-12:

| Schema | `$id` | Purpose |
|---|---|---|
| `ext/extension.v1.json` | `gts://gts.frontx.mfes.ext.extension.v1~` | extension: `id`, `domain`, `entry`, optional `lifecycle` |
| `ext/domain.v1.json` | `…ext.domain.v1~` | extension domain: actions, shared properties, lifecycle stages, `extensionsTypeId` |
| `mfe/entry.v1.json`, `mfe/entry_mf.v1.json` | `…mfe.entry.v1~`, `…entry_mf.v1~` | MFE entry point; the MF variant adds `exposedModule` |
| `mfe/mf_manifest.v1.json` | `…mfe.mf_manifest.v1~` | Module Federation manifest (remoteEntry, publicPath) |
| `comm/action.v1.json`, `comm/actions_chain.v1.json` | `…comm.action.v1~` | actions and action chains |
| `comm/shared_property.v1.json` | `…comm.shared_property.v1~` | shared property (theme, language) |
| `ext/load_ext.v1.json`, `ext/mount_ext.v1.json`, `ext/unmount_ext.v1.json` | derived from `action.v1` | load / mount / unmount an extension |
| `lifecycle/stage.v1.json`, `lifecycle/hook.v1.json` | `…lifecycle.stage.v1~` | lifecycle stages and hooks |

They are loaded as a batch by
[`packages/screensets/src/mfe/gts/loader.ts`](https://github.com/constructorfabric/gears-frontx/blob/910a4ca4/packages/screensets/src/mfe/gts/loader.ts)
(lines 14–33 — `loadSchemas()` returns 13 schemas, plus 4 lifecycle-stage instances).

Application-level derived schemas live one layer up, in
[`packages/framework/src/gts/schemas/`](https://github.com/constructorfabric/gears-frontx/tree/910a4ca4/packages/framework/src/gts/schemas)
(`develop`: `template-standard/src/gts/schemas/`): `extension_screen.v1.json`, `theme.v1.json`,
`language.v1.json`. The application registers them explicitly — `src/app/main.tsx:23-25`,
`gtsPlugin.registerSchema(...)`.

**`presentation` is not part of the base extension type.** It is added by the derived screen type,
[`extension_screen.v1.json`](https://github.com/constructorfabric/gears-frontx/blob/910a4ca4/packages/framework/src/gts/schemas/extension_screen.v1.json):

```json
{
  "$id": "gts://gts.frontx.mfes.ext.extension.v1~frontx.screensets.layout.screen.v1~",
  "allOf": [{ "$ref": "gts://gts.frontx.mfes.ext.extension.v1~" }],
  "properties": {
    "presentation": {
      "type": "object",
      "properties": {
        "label": { "type": "string" },
        "icon":  { "type": "string" },
        "route": { "type": "string" },
        "order": { "type": "number" }
      },
      "required": ["label", "route"]
    }
  },
  "required": ["presentation"]
}
```

Only `label` and `route` are mandatory; `icon` and `order` are optional.

Well-known domain instances — [`packages/framework/src/plugins/microfrontends/gts/frontx.screensets/instances/domains/`](https://github.com/constructorfabric/gears-frontx/tree/910a4ca4/packages/framework/src/plugins/microfrontends/gts/frontx.screensets/instances/domains):
`screen.v1.json`, `sidebar.v1.json`, `popup.v1.json`, `overlay.v1.json`. The screen domain pins the
extension type it accepts (line 12):

```json
"extensionsTypeId": "gts.frontx.mfes.ext.extension.v1~frontx.screensets.layout.screen.v1~"
```

So an extension that targets the screen domain but omits `presentation` is rejected at registration
time by schema validation — not silently ignored later in the UI. That is distinct from an
extension targeting a domain this registry does not own (e.g. the `widgets` domain owned by
`demo-mfe`'s child app): those are skipped deliberately, without validation, and delivered to the
owning runtime instead — `bootstrap.ts:283`, the recursive-composition path from §3.2. Rejection
means "malformed contribution to my slot"; skipping means "not my slot".

The domain instance also declares its shared properties (`theme`, `language`), the actions it
accepts (`load_ext`, `mount_ext`), a `defaultActionTimeout` of 30 s, and the lifecycle stages it
drives.

### 3.5 ID notation

```text
gts.frontx.mfes.ext.extension.v1~frontx.screensets.layout.screen.v1~frontx.demo.screens.profile.v1
└─ base type ─────────────────┘ └─ derived type ────────────────┘ └─ instance ─────────────────┘
```

The chain reads left to right as an inheritance path. A trailing `~` means "schema (type)"; no
trailing `~` means "instance" — enforced in
[`extract-package.ts:66`](https://github.com/constructorfabric/gears-frontx/blob/910a4ca4/packages/screensets/src/mfe/gts/extract-package.ts).
`x-gts-ref` inside a schema is a typed reference to another GTS type, with `…domain.v1~*` meaning
"any instance of this type".

### 3.6 Runtime sequence

1. `src/app/main.tsx` registers the application-level schemas and creates the app via
   `createFrontXApp` with the `MfeHandlerMF` handler installed.
2. `MfeScreenContainer` calls `bootstrapMFE(app)` on mount.
3. `bootstrap.ts` registers the well-known domains — `registry.registerDomain(screenDomain, …)`
   at line 310, screen using `ExclusiveMountStrategy` (one screen mounted at a time) — then
   fetches the manifest aggregate and registers everything it contains.
4. `Menu.tsx` renders entries from the registry.
5. A click issues a mount action; `MfeHandlerMF` fetches `remoteEntry.js` from that MFE's
   `publicPath`, rewrites bare specifiers of shared dependencies to per-load blob URLs, and mounts
   the screen **into a Shadow DOM** — MFE styles cannot leak into the shell or vice versa.

Verified at runtime: the mounted screen's shadow root carries its own Tailwind bundle (~145 KB),
fully isolated from the host.

### 3.7 How MFEs are served in development

Each MFE is served by its own preview server, and the manifests hardcode those origins:

| Server | Port |
|---|---|
| shell (host) | 5173 |
| demo-mfe | 3001 |
| _blank-mfe | 3099 |
| widgets-fixture-a | 3201 |
| widgets-fixture-b | 3202 |

`npm run dev:all` starts all five. Plain `npm run dev` is not enough — it builds the MFEs but does
not serve them, and the first mount fails with
`Failed to load MFE 'http://localhost:3001/shared/react.js'`.

---

## 4. Implications for the navigation service

### 4.1 Where instances should come from

Today the instances that drive navigation live **in the MFE's own repository** (`mfe.json`) and are
frozen into a static JSON at build time. TypeRegistry replaces exactly one link of that chain: the
shell needs a single endpoint instead of `/generated-mfe-manifests.json`, returning the same shape
it already consumes — an array of `{ manifest, entries, extensions, domains?, schemas? }`. The
runtime contract does not have to change; only the source in `bootstrap.ts` does.

That also implies a registration flow: whatever publishes a gear must push its extension instances
into the registry, and the registry must validate them against the derived type the target domain
pins via `extensionsTypeId` — the same validation the frontend does locally today.

### 4.2 Gaps in the current schemas

| Gap | Detail | Disposition |
|---|---|---|
| No i18n for labels | `presentation.label` is a raw string, not a dictionary key. Localized menus are not expressible today. | Runtime transformation — out of scope for this phase (see below) |
| No audience / device / scenario targeting | Nothing in the schemas expresses "admin only", "teacher", "mobile", "tv". The only available filter is which instances reached the registry at all. | Runtime transformation — out of scope for this phase (see below) |
| `route` is declared but unused | The shell mounts by action, not by URL. Deep links, browser history and bookmarking are therefore not supported by the current shell. | Open |
| Ordering is a flat number | `order` is a global integer per domain; no grouping, sections or nesting. A gear cannot say "put me under Administration". | Open |
| Template lagged behind the shell | On `910a4ca4` the registry-driven `Menu.tsx` lived in the monorepo shell (`src/app/`) while the CLI's own template shipped a static one (`packages/cli/template-sources/project/src/app/layout/Menu.tsx`, hardcoded `Home`) — two implementations in the same commit, so a scaffolded project did not inherit the documented behaviour. | Resolved upstream: in `develop` there is a single, registry-driven `Menu.tsx` under `template-standard/src-app/`, and the separate shell is gone |

**Localization and filtering are deliberately not schema concerns.** Both are runtime
transformations over the instance set, and they belong to a *serverless runtime* — a function
resolver that evaluates transformation functions, on the server or on the client, before the shell
sees the result. Baking them into the navigation schemas would push per-tenant, per-role and
per-locale logic into static declarations. Out of scope for this phase; it does, however, mean the
navigation API must stay a *computed* surface rather than a straight dump of stored instances.

### 4.3 Composition model worth reusing

Two properties of the current design are worth keeping in whatever we build:

- **Domains as named slots with a pinned extension type.** The slot declares the contract
  (`extensionsTypeId`), so a malformed contribution fails at registration. This maps cleanly onto
  "a gear registers and appears in navigation" — the shell never special-cases a gear.
- **Recursive composition.** An MFE can own domains and accept extensions from other MFEs
  (`demo-mfe` ↔ `widgets-fixture-a/b`). A gear's screen can therefore host contributions from
  other gears without shell involvement.

### 4.4 Distribution direction in FrontX itself

Worth accounting for in planning: the FrontX CLI was rewritten between `0.2.x` and `0.3.x`.
The `0.2.x` line scaffolds a project from bundled templates (`frontx create <name>`). The `0.3.x`
line drops `create` entirely and becomes a template-resolution tool — `frontx install
github:owner/repo@ref`, then `seed` / `add` / `upgrade` against a repository, with ownership
boundaries and per-template provenance under `.frontx/`. The shell and demo application moved into
`template-standard/` in `develop`. Any integration we design should target the template model
rather than the `create` model.

---

## 5. Appendix — reproduction notes

Running `910a4ca4` locally required four workarounds; useful for anyone repeating the exercise.

1. **`npm ci` fails** on parts of this branch — `package-lock.json` is out of sync with
   `package.json`. Use `npm install`.
2. **`@module-federation/vite` must be pinned to 1.14.5.** The root lock resolves 1.14.4, on which
   `demo-mfe` builds only partially (187 modules, no `mf-manifest.json`) and the build dies with
   `[frontx-mf-gts] ENOENT ... dist/mf-manifest.json`. With 1.14.5 the same sources produce 3934
   modules and a valid manifest. Version 1.20.0 (2026-07-27) fails the same way.
3. **`generate-mfe-manifests` runs before the build**, so a first-time `dev:all` needs every MFE
   built beforehand — including `_blank-mfe`, which `dev-all.ts` excludes from serving but the
   generator still requires.
4. **Copying `src/mfe_packages` into a scaffolded project is not sufficient.** Package paths there
   are relative to the monorepo (`file:../../../packages/*`, and `tsconfig.json` `extends` a path
   under `packages/cli/template-sources/`). A broken `extends` is especially deceptive: esbuild
   silently loses the JSX/target settings, the MFE builds to 4 modules, and the failure surfaces as
   the same `ENOENT mf-manifest.json`. The shell must also come from the same commit — `910a4ca4`
   renamed the API from `hai3` to `frontx` (`createFrontXApp`, `FrontXProvider`, `useFrontX`,
   `FRONTX_*`), so an older scaffolded host fails dependency scanning outright.
