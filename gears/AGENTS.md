# gears/

`gears/system/<name>/` holds platform gears, `gears/<name>/` service gears. Reference implementation: `examples/toolkit/users-info`.

## Layout and naming

- Usual shape: `<name>/<name>/` (gear crate, `cf-gears-<name>`), `<name>/<name>-sdk/` (public API, `cf-gears-<name>-sdk`), `<name>/plugins/<plugin>/`, `<name>/docs/` (`PRD.md`, `DESIGN.md`, `ADR/`), and a `gear.gdl` descriptor next to each gear crate (`docs/GEARBOX.md`). A few older system gears (grpc-hub, service-discovery, api-gateway) keep the crate at `<name>/src/`.
- Gear and directory names are kebab-case; `make validate-gear-names` checks them.
- Other gears call only the `-sdk` API. A Cargo dependency on the gear crate itself exists only to list it in `#[toolkit::gear(deps = [...])]`. REST DTOs stay in the gear crate under `src/api/rest/`.

## Working on a gear

- `make gear-ci GEAR=<name>` runs what gear-scoped CI runs; `make test GEAR=<name>` also tests reverse dependencies. E2E: `make e2e-local GEAR=<name>`. Database tests: `make test-integration LANE=<lane>` (lanes in `gears-rust.mk` `INTEGRATION_LANES`) or a gear-specific target such as `make test-eb-pg`; `make help` lists them.
- Config lives at `gears.<name>.config` (and `gears.<name>.database`). Environment override: `APP__GEARS__<NAME>__CONFIG__<FIELD>`, with `_` in the gear name mapped back to `-`.
- Code rules: routes through `OperationBuilder`; handlers return `ApiResult<T>` (`CanonicalError`); data access through `DBProvider` with queries scoped by `.secure().scope_with(&scope)`, the scope coming from `PolicyEnforcer`. Details: `docs/toolkit_unified_system/README.md`.

## Adding a gear

Follow the checklist in `docs/toolkit_unified_system/10_checklists_and_templates.md`. Steps that are easy to miss:

- workspace `members` in the root `Cargo.toml`;
- `[workspace.metadata.cargo-shear] ignored` (by dependency key, e.g. `authz-resolver`) when another gear lists the crate in `#[toolkit::gear(deps = [...])]`;
- a Cargo feature in `apps/cf-gears-example-server/Cargo.toml` and a `use <crate> as _;` line in its `src/registered_gears.rs`;
- an entry in `docs/GEARS.md`.

Gears get their own changelog section and release from the workspace defaults, so they need no `release-plz.toml` entry; the only exceptions are an entry that sets just `semver_check`, or `git_only = true` for an unpublished crate that ships an image. `make check-release-config` enforces this.

## System gears

`gears/system/` gears with `capabilities = [system]` (types-registry, service-discovery, cluster, grpc-hub) start before application gears; read `docs/toolkit_unified_system/08_lifecycle_stateful_tasks.md` § The system capability before adding `system` to a gear. Changes to authn, authz or tenant contracts affect every gear's PEP; check `docs/arch/authorization/DESIGN.md`.
