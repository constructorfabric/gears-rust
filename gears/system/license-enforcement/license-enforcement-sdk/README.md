# License Enforcement SDK

The contract of the `license_enforcement` gear: the management API, the grant model, the admission
plugin trait, and the evaluator trait. The design is in [`../docs/DESIGN.md`](../docs/DESIGN.md).

## Management API

`LicenseEnforcementApi` is a `#[toolkit::contract]`. In process, resolve it from `ClientHub`:

```rust,ignore
let api = hub.get::<dyn LicenseEnforcementApi>()?;
let grant = api.get_grant(ctx, grant_id).await?;
```

Over REST, `LicenseEnforcementApiRest` maps the same methods under `/license-enforcement/v1`.
Feature `rest-client` emits `LicenseEnforcementApiRestClient`; `rest-server` emits the route
registration for the gear. Each verb on a grant, a pack, or a delegation is its own method and
route, for example `bind_grant` is `POST /grants/{id}/bind`:

```json
{ "subject": { "type": "gts.cf.core.lic.subj.v1~cf.core.lic.tenant.v1~", "id": "550e8400-e29b-41d4-a716-446655440000" },
  "idempotency_token": "8b1c…", "expected_version": 4 }
```

Errors are `CanonicalError`. `error` lists the `reason` and `subject` values to match on.

## Admission plugin and evaluator

The binary links exactly one admission plugin crate and one evaluator crate. Each registers itself
at link time:

```rust,ignore
license_enforcement_sdk::register_admission_plugin!(|| std::sync::Arc::new(MyPlugin::new()));
license_enforcement_sdk::register_evaluator!(|| std::sync::Arc::new(MyEvaluator));
```

The binary pulls the crates in with `use my_plugin as _;` and `use my_evaluator as _;`. The gear
calls `linked_admission_plugin()` and `linked_evaluator()` at init and does not start when either
finds zero or more than one. No configuration, registry entry, or request selects them.

A default comes from the binary, not from the gear. The binary gates each implementation behind
its own Cargo feature and lists the defaults in `default`:

```toml
[features]
default = ["le-default-plugin", "le-default-evaluator"]
le-default-plugin = ["dep:default-plugin"]
le-default-evaluator = ["dep:default-evaluator"]
```

```rust,ignore
#[cfg(feature = "le-default-plugin")]
use default_plugin as _;
#[cfg(feature = "le-default-evaluator")]
use default_evaluator as _;
```

To replace a default, build with `--no-default-features` and enable the other crate. If both
are linked, the gear does not start. Two approaches are not used:

- A Cargo feature on the gear crate that pulls in an implementation. Cargo unifies features
  across the dependency graph, so one dependency that keeps default features brings the default
  back next to the operator's crate.
- A fallback in `linked_admission_plugin()` or `linked_evaluator()` that picks the default when
  nothing else is linked. A missing `use … as _;` would then start the gear on the default
  without an error.
