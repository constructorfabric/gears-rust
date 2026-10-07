# credstore v2: temporary parallel version

`gears/credstore/v2/` is a temporary, complete second version of the credstore gear, built next to the current one (v1: `gears/credstore/credstore`, `credstore-sdk`, `plugins`). It implements the design in [`../docs`](../docs) (ADRs, DESIGN, [`out-of-scope.md`](../docs/out-of-scope.md)) and is merged into `main` piece by piece, each PR at most 6K changed lines including lock files.

## Nobody uses v2 yet

- v2 is not registered in the server, has no REST routes and no database connection until the switch.
- No gear, plugin or app depends on the v2 crates; consumers keep using v1.
- v1 and v2 must never be linked into one binary: they share the plugin-spec GTS id, and later the gear name, routes and metric names.
- The v2 crates are `publish = false`; their package names differ from v1 until the switch.
- v1 and its database stay untouched until the switch.

## Database and values

- v2 carries the v1 schema migration (`m0001`) and the v1 → v2 schema migration (`m0002`), so after the switch it starts on the existing v1 database once its stored values have been moved.
- The migration of stored secret values from v1 to v2 is not committed to `main`; it lives in a temporary branch and is run from there. `m0002` refuses to run, and changes nothing, while the database holds an `active` row (`status = 2`) and the tool has not recorded that copying finished (or `discard_values`), so before the switch the operator runs the tool from that branch (DESIGN §8). A database with no such rows passes.

## Order of PRs

1. SDK contract (models, errors, types, client and plugin API) and the gear skeleton: domain model, repository contract, DB schema; `out-of-scope.md`.
2. Repository implementation and its tests, including the PostgreSQL migration test, `make test-credstore-pg` and its CI step (on the v2 package).
3. Domain service: type rules, hierarchy reduction, list filter, authorization, write intents, write protocol, listing; several PRs with their tests.
4. Infrastructure adapters (metrics, audit, tenant resolver, types registry, plugin selection, error mapping, GTS permissions), client and config.
5. REST API and gear wiring (still not registered).
6. Plugin conformance suite and test utilities in the SDK.
7. Plugins: static and Vault (Vault is new, v1 has none), with `-v2` package names.
8. Switch.

## Switch

Before it, diff `main` against the v1 baseline v2 was copied from (`bdf434ebd`) and port or drop every v1 change. Consumers cannot move earlier: the v2 SDK is a different crate, so their client lookup fails while v1 is registered. Afterwards a single version remains:

- **S1:** register v2 under the gear name `credstore`; move oagw, settings-service and keycloak-idp-plugin to the v2 SDK; update `config/*.yaml`, the e2e suites, `docs/api/api.json`, QUICKSTART, the example-server features, the `[workspace.dependencies]` entries and the `test-credstore-pg` lane. v1 stays in the tree, unlinked.
- **S2:** delete v1 (deletions only).
- **S3:** move v2 to `gears/credstore/{credstore,credstore-sdk,plugins}`; rename the packages to `cf-gears-credstore*` with versions above the published ones; drop `publish = false`; delete `gears/credstore/v2/` and this file.
- Operators reissue PDP grants before the deploy: the base type `secret.v1~` becomes `credential.v1~`, with six actions (ADR-0010).
