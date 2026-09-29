---
status: proposed
date: 2026-09-24
---

# ADR-0007: Storage Key Layout & Backend/Bucket Placement Rules

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Now: a single computation point, no behavior change](#now-a-single-computation-point-no-behavior-change)
  - [Extension point: a Rust plugin](#extension-point-a-rust-plugin)
  - [Plugin contract](#plugin-contract)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Status quo: fixed path, single default backend](#status-quo-fixed-path-single-default-backend)
  - [Static tenant → `backend_id` map (config/policy)](#static-tenant--backend_id-map-configpolicy)
  - [Config template string (placeholders, static prefix)](#config-template-string-placeholders-static-prefix)
  - [Rust plugin (`StoragePlacementResolver`) with fallback to the fixed layout](#rust-plugin-storageplacementresolver-with-fallback-to-the-fixed-layout)
  - [Embedded runtime language (CEL / Starlark / Rhai / Lua / WASM)](#embedded-runtime-language-cel--starlark--rhai--lua--wasm)
- [More Information](#more-information)
  - [Open decisions and risks](#open-decisions-and-risks)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-file-storage-adr-storage-layout-rules`

## Context and Problem Statement

Every backend object this gear writes today sits at exactly one place: the object key (S3) / relative path
(`local-fs`) is `/{file_id}/{version_id}`, two server-generated UUIDs. Backend selection has exactly one branch:
every `create`/multipart-`initiate` call resolves to the deployment's single `default_backend_id`. `s3_backends`
lets an operator describe several S3 backends in config, but nothing today picks among them per tenant, per client,
or per request — that config surface only broadens what a deployment *can* be pointed at, not what any given write
is routed to.

Until PR #4708, this formula was computed independently in more than one place, including a fallback recomputation
cleanup used for an expired multipart session whose `file_versions` row — and with it the stored `backend_path` —
had already been reclaimed by an earlier phase of the same sweep. That fallback was harmless only because the
formula was a pure, stateless function of `(file_id, version_id)`; it would stop being harmless the moment placement
depended on anything beyond those two identifiers — see Decision Drivers below. PR #4708 closes that gap: a single
seam now computes the backend and path once, at version creation for `create` and at session creation for
multipart-`initiate`, and `multipart_uploads` carries its own `backend_id`/`backend_path` so cleanup reads them from
the session row instead of recomputing.

Two requests keep coming from clients, and they are the same missing capability under two names, not two separate
asks:

* **"A dedicated bucket / private storage for our tenant."** Not already available without operator action —
  correcting an earlier assumption. Today, giving one tenant an isolated bucket means either (a) a separate gear
  deployment with its own `default_backend_id`, or (b) writing to the shared default and running `migrate_backend`
  by hand, file by file, after the fact. Neither is "dynamically, per tenant, at write time."
* **"A hierarchical key layout"** — e.g. `/{tenant_id}/{yyyy}/{mm}/{file_id}/{version_id}`, grouped by
  `gts_file_type` (the mandatory immutable GTS type on every file) or by `(owner_kind, owner_id)` — instead of the
  flat `/{file_id}/{version_id}`. The owner's position, stated plainly: not worth building for on its own merits —
  see the database-is-the-index and dedicated-bucket drivers below. The request keeps coming up regardless, so this
  ADR compares real options rather than dismiss it by fiat.

Both reduce to the same gap: nothing today can compute a **non-default `(backend_id, path)` pair** for a new version
at write time. Closing it once also closes the per-tenant-bucket case and the hierarchical-path case, rather than
solving each independently.

This ADR chooses (1) whether and how the object key/backend-selection formula becomes extensible, and (2) the
mechanism used to express that extension, if any.

**Decision**: keep the flat `/{file_id}/{version_id}` layout and the single default backend as today's always-on
fallback — nothing changes until a client needs more. Add a Rust plugin extension point
(`StoragePlacementResolver`) that resolves `(backend_id, path)` once registered, with no other mechanism required.

## Decision Drivers

* **The database, not the storage layout, is the index.** Per-tenant/per-type/per-owner listing and reporting
  already exist as indexed Postgres queries; a hierarchical key is not required to serve that need and must not be
  sold as solving it.
* **A dedicated bucket per isolated client is not, in fact, already free.** `s3_backends` can describe many buckets,
  but nothing selects among them per tenant at write time; an extension point only earns its complexity budget once
  it does something a config entry cannot today — **dynamic, request-time** selection of both `backend_id` and path.
* **The extension point must be able to select `(backend_id, path)` together, not a path alone.** A per-tenant
  private bucket and a hierarchical key layout are the same underlying gap; a path-only mechanism cannot close the
  former.
* **Determinism, not just uniqueness, is load-bearing.** The backend-migration feature doc's race-safety argument
  rests on the canonical path being deterministic per version. Any extension must remain a pure, deterministic
  function of the same stable, already-persisted attributes for both racers, computed once and never re-derived, or
  this invariant silently breaks.
* **The result must always contain `version_id`.** Every uniqueness/immutability guarantee this gear has assumes the
  computed location is unique per version; an extension that can omit or collide on `version_id` breaks that
  silently, including breaking create-exclusive publish.
* **Only trusted, server-controlled attributes may drive placement.** User-supplied values (declared file name, MIME
  type, `custom_metadata`) must never flow unsanitized into a storage key: S3 key-traversal, PII leaking into audit
  tooling, and S3's own key-length ceiling (1024 bytes) are all real, not theoretical, risks.
* **A path, once computed for a version, is permanent for that version.** `file_versions.backend_path` records this
  per-version, and the sidecar only ever learns a path from the signed token — never by recomputing it. Any
  extension must preserve "compute once at version creation, store it, never recompute" as an absolute — exactly the
  assumption the Context section shows was already only *accidentally* true before PR #4708.
* **This platform already has one plugin pattern for exactly this kind of extensibility.** GTS-typed plugin
  registration plus scoped `ClientHub` resolution is the mechanism `credstore`, `authn-resolver`, `authz-resolver`,
  `tenant-resolver`, `usage-collector`, and `cluster`'s per-primitive backends already use; this ADR should reuse it
  rather than invent a second extension mechanism for this gear.
* **Operational blast radius of a misconfigured or failing extension.** Placement sits on the `create`/`multipart
  initiate` hot path; a plugin failure or timeout must fail closed with a clear error — never silently fall back to
  the default — since a silent fallback would place a file somewhere the caller did not intend.

## Considered Options

* Status quo: fixed path, single default backend
* Static tenant → `backend_id` map (config/policy)
* Config template string (placeholders, static prefix)
* Rust plugin (`StoragePlacementResolver`) with fallback to the fixed layout
* Embedded runtime language (CEL / Starlark / Rhai / Lua / WASM)

## Decision Outcome

Chosen option: "Rust plugin (`StoragePlacementResolver`) with fallback to the fixed layout", because it is the
only option that can select `(backend_id, path)` together rather than a path alone, it reuses this platform's
already-proven GTS/`ClientHub` plugin pattern instead of inventing a new one, and it changes nothing in today's
behavior until a client actually needs it.

* **Today, nothing changes.** Every `create`/multipart-`initiate` call still resolves to `default_backend_id` and
  `/{file_id}/{version_id}`, exactly as before.
* **When a client needs a dedicated bucket, private backend, or hierarchical key layout**, it implements a
  `StoragePlacementResolver` plugin that returns a `(backend_id, path)` pair for the gear to use and store.
* **If no plugin is registered**, the seam uses its built-in fallback unchanged — absence of a plugin is a
  supported, first-class state, not an error condition.
* **The other options are rejected**: the static tenant → `backend_id` map, the config path template, and the
  embedded runtime languages (CEL / Starlark / Rhai / Lua / WASM) — see Pros and Cons of the Options below.

### Now: a single computation point, no behavior change

Landed in PR #4708:

* One seam computes the backend and path once — at version creation for `create`, at session creation for
  multipart-`initiate` — byte-for-byte reproducing today's formula (`default_backend_id`,
  `/{file_id}/{version_id}`) when no plugin is bound. This is a refactor, not a behavior change: no config surface,
  no new dependency.
* `multipart_uploads` carries its own `backend_id` and `backend_path`, populated once at `initiate`. Cleanup of an
  expired multipart session reads them from the session row instead of recomputing the path from
  `(file_id, version_id)` and falling back to the default backend — removing the one place where placement was
  recomputed rather than read from a stored value.
* This is the prerequisite for the plugin extension point below: without a single seam, a resolver would have to be
  threaded through every call site that currently derives a path independently.

### Extension point: a Rust plugin

When a real client's requirement is confirmed — a dedicated bucket, private storage, or a hierarchical key layout —
the seam gains a second, opt-in implementation resolved through this platform's existing plugin mechanism: GTS-typed
registration plus scoped `ClientHub` resolution, the same pattern already used by `credstore`, `authn-resolver`,
`authz-resolver`, `tenant-resolver`, `usage-collector`, and `cluster`'s per-primitive backends.

* A plugin crate declares a GTS plugin spec, publishes an instance to the platform's type registry, and registers a
  scoped implementation of a `StoragePlacementResolver` trait in `ClientHub` at startup. Plugins are statically
  linked workspace members, resolved purely at runtime by GTS schema id (with vendor/priority selection when more
  than one instance is registered); no dynamic loading is involved, and this gear keeps no compile-time dependency
  on any concrete plugin crate.
* `StoragePlacementResolver` takes the trusted, server-controlled attributes for a new version (see Plugin contract)
  and returns a `(backend_id, path)` pair for the gear to use and store.
* **If no `StoragePlacementResolver` instance is registered, the seam must use its built-in fallback unchanged —
  `default_backend_id` and `/{file_id}/{version_id}`.** Absence of a plugin is a supported, first-class state, not
  an error condition.
* Why a Rust plugin rather than a config rule or an embedded language: it is the most flexible and the simplest
  option for the logic actually being asked for — a tenant → bucket map, a hierarchical path convention, a call into
  a private per-tenant credential/backend registry. Each client writes exactly the custom logic it needs, in a
  typed, testable, ordinarily-compiled crate, and this reuses machinery the platform already has rather than
  inventing a new one. See Pros and Cons for the comparison against the rejected options.

### Plugin contract

Binding on any registered `StoragePlacementResolver` — this restates and consolidates the Decision Drivers above
rather than duplicating them:

* The returned path **must** contain `version_id` as a whole path segment — delimited by `/` on both sides or
  ending the key — not merely as a substring; the gear validates every plugin result before use, and a result
  without such a segment **must** be rejected. A substring match could pass by accident and let two versions share a
  key, and create-exclusive publish and per-version immutability both depend on the key being unique per version.
* A plugin-produced path **must not** start with `/`: S3 consoles show a leading slash as an empty-named folder at
  the bucket root. The fixed `/{file_id}/{version_id}` layout keeps its leading slash, because objects already written
  under it cannot be renamed; the rule applies to layouts a plugin introduces, so they start clean.
* The returned `backend_id` **must** refer to a backend configured in this deployment; a result naming an
  unconfigured backend **must** be rejected.
* Placement **must** be computed exactly once — at version creation for `create`, at session creation for
  multipart-`initiate` — and stored in `file_versions.backend_path` and the `multipart_uploads` session row
  respectively. Nothing downstream may recompute it; per ADR-0003, the sidecar and cleanup only ever read the
  stored value.
* The resolver's input **must** be limited to trusted, server-controlled attributes — tenant, owner, `file_id`,
  `version_id`, `gts_file_type`, `created_at`, and the **target `backend_id`** the placement is being computed for
  (the default backend for `create`/`initiate`, the destination for `migrate_backend`) — and **must not** include
  user-supplied values (declared file name, MIME type, `custom_metadata`).
* A plugin **may** call trusted platform services to enrich those attributes — first of all `tenant-resolver`, to
  resolve a tenant's ancestors (for example project → workspace → organization, when a project is a tenant). Such a
  call is part of placement and falls under the same rules as the plugin itself: it runs under the placement timeout,
  and its failure fails the `create`/`initiate`/`migrate_backend` call instead of falling back to the default
  placement. It **must not** call services that return user-supplied data.
* A plugin error or timeout **must** be treated as a hard failure of the `create`/`initiate` call, with a clear
  error returned to the caller; it **must not** silently fall back to the default placement, since a silent fallback
  would place a file somewhere the caller did not ask for.
* A change to a plugin's own placement logic is prospective only: it affects versions created after the change, and
  existing objects are not moved. `migrate_backend` remains the only path-changing operation for existing content,
  and it **must** evaluate the resolver for the **destination** backend rather than copy the source path verbatim.
* Any plugin-produced path **must not** exceed S3's key-length ceiling (1024 bytes); the gear rejects an oversized
  result defensively rather than passing it on to a backend call.
* Every computed placement **must** record, at minimum, which plugin (and plugin version, once plugins are
  versioned) produced it and which attributes it consumed — sufficient for audit; this can live on the existing
  audit-entry mechanism rather than a new table.

### Consequences

* Good, because nothing user-visible changes until a client actually needs a plugin; there is no config surface or
  schema-visible behavior change from the `multipart_uploads` columns alone.
* Bad, because adopting the extension point commits this gear to the cross-crate plugin-resolution machinery (GTS
  spec, type registry publish, scoped `ClientHub` registration) it did not previously need — new infrastructure for
  this gear, even though the pattern itself is already proven elsewhere on this platform.
* Good, because a future per-tenant private-bucket or hierarchical-path requirement becomes a plugin to write, not
  an ADR to revisit.

### Confirmation

* Code review confirming every call site that computes a backend path goes through the single seam, byte-identical
  to today's formula for every existing test fixture, when no plugin is registered.
* Migration + code review confirming `multipart_uploads` carries its own `backend_path` and `backend_id`, populated
  at `initiate`, with no recomputation fallback left in expired-session cleanup.
* A test asserting the no-plugin-registered path reproduces exactly today's `default_backend_id` +
  `/{file_id}/{version_id}`.
* A test asserting a plugin result without `version_id` as a whole path segment (including one that contains it only
  as a substring) is rejected before it reaches storage, and that a plugin path with a leading `/` is rejected.
* A test asserting the resolver receives the target `backend_id`, and a different one for `migrate_backend`'s
  destination than for the source.
* A test asserting a plugin error or timeout produces a `create`/`initiate` failure, not a silent fallback to the
  default placement.
* An integration test asserting `migrate_backend` evaluates the **destination** backend's resolver (not a copy of
  the source path), and that concurrent migrations to the same destination remain a safe no-op.

## Pros and Cons of the Options

### Status quo: fixed path, single default backend

Fixed `/{file_id}/{version_id}`, single default backend; per-client isolation only via a dedicated deployment or a
manual, after-the-fact `migrate_backend`.

* Good, because it is exactly what ships today — zero new code, zero new risk.
* Bad, because it cannot serve a per-tenant private-bucket or hierarchical-path requirement at all: the only
  workarounds are a separate deployment or a manual, after-the-fact `migrate_backend`.
* Bad, because — until PR #4708 — it left a fragile recompute-on-fallback shape in cleanup; #4708 removes that
  regardless of which option this ADR chooses.

### Static tenant → `backend_id` map (config/policy)

An operator-maintained table in config/policy, resolved at write time; no path change.

* Good, because it is simple and directly closes the per-tenant-bucket routing case with no code per tenant.
* Bad, because it only selects `backend_id`, not a path — it does not serve the hierarchical-path request at all.
* Bad, because per-tenant private-backend credentials still need somewhere to be held and used; the map alone
  closes the routing half of the private-storage case, not the whole of it.
* In practice reduces to a special case of the Rust plugin (`StoragePlacementResolver`) with fallback to the fixed
  layout — a minimal plugin that reads a config map — so it is not treated as an independent long-term option.

### Config template string (placeholders, static prefix)

A per-backend/per-tenant path template with a fixed placeholder whitelist plus a static S3 key prefix.

* Good, because it is simple to implement and reason about, and trivially satisfies the deterministic-pure-function
  requirement.
* Good, because a placeholder whitelist is an easy place to enforce "trusted attributes only" — no expression
  language for user input to sneak into.
* Bad, because it **cannot select `backend_id`** — a template only ever produces a path string, so it cannot serve
  the per-tenant private-bucket requirement, the stronger of the two drivers in this ADR's Context.
* Bad, because "always include `version_id`" is enforceable only by a hand-rolled linter over the template, not by
  running the rule against representative inputs the way a plugin can be unit-tested.

### Rust plugin (`StoragePlacementResolver`) with fallback to the fixed layout

A `StoragePlacementResolver` trait, resolved through this codebase's existing GTS/`ClientHub` plugin pattern; when
no plugin is registered, the seam falls back unchanged to `default_backend_id` and `/{file_id}/{version_id}`.

* Good, because it is the most flexible option available: each client writes exactly the placement logic it needs —
  a tenant → bucket map, a hierarchical path convention, a call into a private per-tenant credential/backend
  registry — with no sandbox or expression-language ceiling to work around.
* Good, because it reuses this platform's existing GTS/type-registry/`ClientHub` plugin pattern, already proven by
  `credstore`, `authn-resolver`, `authz-resolver`, `tenant-resolver`, `usage-collector`, and `cluster` — no new
  resolution mechanism to design.
* Good, because it is ordinary, typed, testable Rust — no new runtime dependency, no sandbox, no new DSL for
  operators or plugin authors to learn.
* Bad, because a plugin change requires a compiled, versioned, redeployed artifact — higher friction than a config
  value for the simplest cases, which is exactly why the static tenant → `backend_id` map above exists as a
  plugin-shaped special case rather than a rejected idea.

### Embedded runtime language (CEL / Starlark / Rhai / Lua / WASM)

Considered as a group and rejected. Each would add a sandboxed expression/scripting runtime to the hot
`create`/multipart-`initiate` path for the sake of letting an operator change placement rules without a redeploy — a
benefit not needed here, since the confirmed requirement is client-specific logic a client writes once as a plugin,
not a rule an operator retunes repeatedly.

| Language | Termination guarantee | Cost vs. the chosen plugin |
|---|---|---|
| CEL | Not Turing-complete by construction | A new runtime dependency with its own security-review gate (the ADR-0005 precedent), for expressiveness a plugin already provides |
| Starlark | Turing-complete; this repo's own `serverless-runtime` treats it as heavy enough to need its own durability/scheduler substrate | Scoped elsewhere on this platform for full workflow execution, not a per-request lookup |
| Rhai | Turing-complete | No prior art anywhere in this codebase; a third embedded-language choice for a problem a plugin already solves |
| Lua (`mlua`) | Turing-complete; sandboxing is a nontrivial, ongoing hardening exercise | No prior art here; no expressiveness advantage over a plugin |
| WASM (`wasmtime`) | Strong sandbox, but general-purpose compute | Heaviest dependency of any candidate; needs its own build/authoring toolchain per rule, reintroducing most of the plugin option's redeploy-per-change friction while adding a VM runtime on top |

* Good, because an operator could in principle change placement rules without a rebuild — the one real advantage
  over a plugin, and not one this ADR needs.
* Bad, because every option in this group adds a sandbox, a dependency, or both, for expressiveness a typed Rust
  plugin already provides more simply and more safely.

## More Information

### Open decisions and risks

Flagged honestly, not resolved by this `proposed` ADR:

1. **Where a plugin gets per-tenant data.** Tenant hierarchy comes from `tenant-resolver` (allowed by the Plugin
   contract); other per-tenant data from the plugin's own config, and private-backend credentials from `credstore` —
   which of these a given plugin needs, and the placement timeout's value, are not decided here.
2. **`migrate_backend` interaction, precisely.** The Plugin contract above already requires evaluating the
   destination backend's resolver for new migrations; how that interacts with objects placed before any plugin
   existed needs to be threaded through the backend-migration feature doc's own definition-of-done — flagged here,
   not designed here.
3. **`AbortIncompleteMultipartUpload` and plugin-selected buckets.** Today's mandatory lifecycle rule is set
   per-bucket, once, ahead of time — it becomes a real risk only if a plugin can direct new files to a bucket that
   rule was never applied to, and nothing today automates applying it to a plugin-chosen bucket — unresolved.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Related**: [ADR-0003: Split the Data Plane into a Signed-URL Sidecar](./0003-cpt-cf-file-storage-adr-sidecar-data-plane.md) — the sidecar only ever reads `backend_path` from the signed token, never recomputes it; this ADR's "compute once, store, never recompute" applies the same posture to the control plane's own callers and to any registered plugin
- **Related**: [ADR-0005: S3 Client Selection](./0005-cpt-cf-file-storage-adr-s3-client-selection.md) — the security-review gate this ADR references when comparing the rejected embedded-language options
- **Related**: [ADR-0006: Content-Hash Modes](./0006-cpt-cf-file-storage-adr-content-hash-modes.md) — a sibling example of "compute once, store, never re-derive" applied to a different field (`hash_value` vs. `backend_path`)
- **Prior art**: `gears/serverless-runtime/docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md` — the Starlark-vs-CEL scoping precedent this ADR leans on when rejecting the embedded-language options
- **Feature doc**: [backend-migration.md](../features/backend-migration.md) — the deterministic-path race-safety argument this ADR's Decision Drivers depend on, and the destination-backend-resolver requirement in the Plugin contract
- **Feature doc**: [policy-engine.md](../features/policy-engine.md) — the existing tenant/user-scoped, DB-backed policy-resolution pattern relevant to Open Decision 1

This decision directly addresses the following requirements or design elements:

* `cpt-cf-file-storage-fr-backend-abstraction` — the placement seam is a new extension point alongside the
  storage-backend abstraction, determining both the path and the backend every backend call receives
* `cpt-cf-file-storage-fr-multipart-upload` — the `multipart_uploads.backend_path`/`backend_id` columns this ADR's
  "Now" phase adds close the recompute-on-fallback gap in expired-session cleanup
* `cpt-cf-file-storage-fr-file-type-classification` — `gts_file_type` is one of the trusted attributes a registered
  `StoragePlacementResolver` may key on
* `cpt-cf-file-storage-fr-tenant-boundary` — `tenant_id` is the trusted attribute a dynamic private-tenant-storage
  plugin would key on; this ADR does not implement that routing, only the mechanism that lets a plugin provide it
* `cpt-cf-file-storage-adr-sidecar-data-plane` (ADR-0003) — reaffirms the sidecar as a pure consumer of a
  token-carried `backend_path`, never a computer of one
