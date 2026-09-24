# Settings Service

The gear that owns platform settings: a declaration registry, effective-value
resolution down the tenant tree, and validate-then-set writes that commit with
their audit record.

> Specifications ship with the code: [PRD](../docs/PRD.md),
> [DESIGN](../docs/DESIGN.md), [DECOMPOSITION](../docs/DECOMPOSITION.md) and the
> per-feature files under [`docs/features/`](../docs/features). The public
> contract lives in [`cf-gears-settings-service-sdk`](../settings-service-sdk);
> this crate is the implementation.

## Overview

- **Declarations, not free-form keys** — every setting is declared once, by an
  administrator or by a gear contributing its own, and carries a value type, a
  Schema Default and a Scope Class. The key is a GTS **type** identifier,
  `gts.cf.core.settings.setting_type.v1~<vendor>.<package>.<category>.<name>.vN~`,
  registered in `types-registry` when the declaration is created
  ([ADR-002](../docs/ADR/ADR-002-setting-key-gts-type-id.md)), so a policy can
  name one setting as its resource.
- **Scope Class decides inheritance** — `global` lives at platform scope only,
  `cascading` inherits down the tenant tree with the nearest override winning,
  `local` never inherits. Every read carries the value, its source, the
  inheritance trail and the **fallback**: what the scope would resolve to
  without a row of its own.
- **Validate, then set** — a value operation takes effect when the caller sets
  it. `POST …/validate` and `POST …/impact` preview without storing anything;
  `PUT …/value`, `POST …/batch`, revert, clone and remove commit each change in
  one transaction with its audit record, guarded by `If-Match`.
- **Two gates on a write** — authorization first, then credential **step-up**
  for an interactive caller on a declaration that requires it: the presented
  token is authenticated by the platform's AuthN resolver and its `auth_time`
  must fall inside a freshness window of at most five minutes, else `401` with
  the RFC 9470 challenge. A service principal is refused outright on such a
  setting rather than asked for a ceremony it cannot perform.
- **Tenant access is a sparse decision** — a `(setting, tenant)` row of
  `read_only` or `hidden`, recorded by a strict ancestor; absence means
  `overridable`, and the strictest value on the root-to-self chain wins. A
  `hidden` setting answers `404` on every administrative path, never `403`.
- **Secrets by reference** — a `secret`-trait value lives in the Credential
  Store under this gear's own principal; the row holds an opaque `secret_ref`,
  every administrative surface shows a mask, and the only path to plaintext is
  the SDK reader's `resolve_secret`, authorized per setting and audited. A
  secret may be staged ahead of the step-up redirect and claimed by the
  following batch through a single-use `pending_id`.
- **Gear-local audit** — every mutation writes a record in the mutation's own
  transaction; a change the platform could not record does not take effect.

## Running it

The gear is linked into the example server behind its own feature:

```sh
cargo run --bin cf-gears-example-server --features settings-service -- \
    --config config/quickstart.yaml
```

The end-to-end smoke lives in `testing/e2e/suites/settings_service` and runs
against the shared e2e server — `make e2e-local SUITE=settings-service` —
covering the gear's startup on its routes, the read surfaces, and the
canonical refusals. Writes that need a declaration to reach their gates are
covered by the crate's own surface tests until the fleet contributes one.

## Configuration

Bootstrap values are deployment-owned and read fail-closed: the `config:`
section is required, and an absent required value fails startup rather than
defaulting. Everything in it has a default today, so an empty section is valid:

```yaml
settings-service:
  database:
    server: "sqlite_users"
    file: "settings-service.db"
  config: {}
```

`cache_ttl_seconds` (30), `cache_max_entries` (500,000) and `audit_retention_days` (365) are fixed by the
design rather than by the operator: init refuses a TTL above 30 or of zero (a deployment may shorten
the backstop, never widen it), a cache of no entries, and a retention below twelve months. The optional `step_up` section carries
policy only — `max_age_seconds` (300, and its ceiling), `issuer`, `audience`,
`acr_values`, `amr_values` — and names no identity provider: the token is
validated by the platform's AuthN resolver. A blank or whitespace-padded pin
or assurance entry is refused at init: no token could carry
it, and it would refuse every step-up-gated write with no sign at boot.

## Dependencies and capabilities

`deps = [types_registry]` — the one gear this service calls during its own
init, to register the settings GTS schemas. The authorization resolver, the
AuthN resolver, the tenant resolver and the Credential Store are fetched from
`ClientHub` at first use on the request path, so they add no ordering edge: a
gear that reads settings during *its* init names `settings-service` in its own
`deps` and cannot close a cycle through this one.

Capabilities are `db`, `rest` and `stateful`; the managed lifecycle runs two
passes: the minute sweep that releases staged secrets nobody claimed, and a
daily audit retention pass that prunes records past their horizon.

## REST surface

Everything under `/settings-service/v1/`: `categories`, `declarations`,
`settings` (browse, read, history), `settings/{key}/value` and its `revert`,
`clone` and `secret-stage` actions, `settings/batch`, `settings/{key}/validate`
and `/impact`, and `settings/{key}/permissions`. The served document is at
`/openapi.json`; the platform contract is mirrored in TypeSpec under
`vhp-architecture`.
