# UI Composition — Draft

> **Status:** draft, for discussion. This document keeps only what belongs to the platform side:
> the service goal, delivery priorities, and the implications for the navigation service. The
> detailed description of how the FrontX frontend composes navigation lives in FrontX itself, as
> two code-verified guidelines shipped with the templates (links in §3) — maintained next to the
> code they describe, so they cannot silently drift from it.
>
> **Evidence base:** [constructorfabric/gears-frontx](https://github.com/constructorfabric/gears-frontx)
> at `develop` (verified 2026-08-10, commit `4d06931e`).

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
- **Generic input overrides** — schema-driven forms rendered generically, where a gear may
  override the generic rendering via a *well-known instance*.
- **Generic table overrides** — generically rendered filters and columns, again overridable via
  a *well-known instance*.

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

### P1 — Generic admin

| Deliverable | Status | Scope | Owned by |
|---|---|---|---|
| Schemas & types convention | `[ ]` | Same as P0, extended to forms and tables; PRD and Design first | architecture |
| Gear settings storage & API | `[ ]` | Persistence and API for per-gear configuration | Setting Service |
| Per-gear declaration | `[ ]` | Entities, screens and overrides a gear contributes | Gear Manifest |
| Entity contract for CRUD screens | `[x]` | Source of truth for generic CRUD screens — **done**, already available per gear | OpenAPI |

---

## 3. How FrontX composes navigation — summary and references

The FrontX shell holds no static menu: microfrontends declare *screen extensions* in their own
`mfe.json`, a build step aggregates the declarations into one static JSON asset, and the shell
registers them at runtime and renders the menu from its registry. Declarations are validated at
registration against the extension type the target domain pins — a malformed contribution fails
loudly, and composition is recursive (an MFE can own domains and host contributions from other
MFEs).

FrontX distributes the frontend as two templates, and the detailed mechanics are documented
there, one guideline per side of the contract:

- **Consumer side (shell)** — how the menu is derived from the registry, the
  build → aggregate → runtime pipeline, registration order, validation vs skip, mounting and
  isolation:
  [`template-shell` → `navigation-composition`](https://github.com/constructorfabric/gears-frontx/blob/develop/template-shell/.frontx/ai/%40gears-frontx/frontx-template-shell/guidelines/navigation-composition.md)
- **Producer side (MFE)** — what a package declares to appear in the menu, `presentation`
  semantics, and non-menu (widget) contributions:
  [`template-mfe` → `navigation-contribution`](https://github.com/constructorfabric/gears-frontx/blob/develop/template-mfe/.frontx/ai/%40gears-frontx/frontx-template-mfe/guidelines/navigation-contribution.md)

The contract this service cares about is the shape the shell ingests. Per MFE package, the
aggregated manifest entry is:

```ts
{ manifest, entries, extensions?, domains?, schemas? }
```

## 4. Implications for the navigation service

### 4.1 Where instances should come from

Today the declarations that drive navigation live in the MFE's own repository (`mfe.json`) and
are frozen into a static JSON at build time. **No service and no database sit anywhere in that
chain** — which is precisely the link a Type Registry replaces: the shell needs a single endpoint
instead of the static aggregate, returning the same shape it already consumes (§3). The runtime
contract does not have to change; only the source URL in the shell's bootstrap does.

That also implies a registration flow: whatever publishes a gear must push its extension
instances into the registry, and the registry must validate them against the derived type the
target domain pins — the same validation the frontend performs locally today.

### 4.2 Gaps in the current schemas

| Gap | Detail | Disposition |
|---|---|---|
| No i18n for labels | `presentation.label` is a raw string, not a dictionary key. Localized menus are not expressible today. | Runtime transformation — out of scope for this phase (see below) |
| No audience / device / scenario targeting | Nothing in the schemas expresses "admin only", "teacher", "mobile", "tv". The only available filter is which instances reached the registry at all. | Runtime transformation — out of scope for this phase (see below) |
| `route` is declared but unused | The shell mounts by action, not by URL. Deep links, browser history and bookmarking are therefore not supported by the current shell. | Open |
| Ordering is a flat number | `order` is a global integer per domain; no grouping, sections or nesting. A gear cannot say "put me under Administration". | Open |

**Localization and filtering are deliberately not schema concerns.** Both are runtime
transformations over the instance set, and they belong to a *serverless runtime* — a function
resolver that evaluates transformation functions, on the server or on the client, before the
shell sees the result. Baking them into the navigation schemas would push per-tenant, per-role
and per-locale logic into static declarations. Out of scope for this phase; it does, however,
mean the navigation API must stay a *computed* surface rather than a straight dump of stored
instances.

### 4.3 Composition model worth reusing

Two properties of the current design are worth keeping in whatever we build:

- **Domains as named slots with a pinned extension type.** The slot declares the contract, so a
  malformed contribution fails at registration. This maps cleanly onto "a gear registers and
  appears in navigation" — the shell never special-cases a gear.
- **Recursive composition.** An MFE can own domains and accept extensions from other MFEs. A
  gear's screen can therefore host contributions from other gears without shell involvement.

### 4.4 Distribution direction in FrontX

FrontX distributes the frontend via a template-resolution CLI (`frontx install` / `seed` / `add` /
`upgrade`), not project scaffolding: `template-shell` establishes the host application, and
`template-mfe` adds the microfrontend workspace on top — the two sides of the navigation contract
ship, and are documented, separately. Any integration we design should target this template
model, and anything we add to the contract lands in those templates' guidelines alongside the
code.
