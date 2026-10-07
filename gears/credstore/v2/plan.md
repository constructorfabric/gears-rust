# credstore v2: temporary parallel version

`gears/credstore/v2/` is a temporary, complete second version of the credstore gear, built next to the current one (v1: `gears/credstore/credstore`, `credstore-sdk`, `plugins`). It implements the design in [`../docs`](../docs) (ADR-0004, ADR-0005, ADR-0006, DESIGN, [`out-of-scope.md`](../docs/out-of-scope.md)) and is merged into `main` piece by piece, each PR at most 6K changed lines including lock files.

## Nobody uses v2 yet

- v2 is not registered in the server and has no REST routes until the switch.
- No gear, plugin or app depends on the v2 crates; consumers keep using v1.
- The v2 crates are `publish = false` and never released.
- v1 stays as it is and keeps serving until the switch.

## Switch

When v2 is complete, one PR switches to it, and a single version remains:

1. v2 is registered under the gear name `credstore`.
2. The consumers (oagw, settings-service, keycloak-idp-plugin, configs, e2e) move to the v2 SDK.
3. v1 is deleted, and v2 moves to its place (`gears/credstore/credstore`, `gears/credstore/credstore-sdk`, `gears/credstore/plugins`); `gears/credstore/v2/` and this file disappear.

## Database and values

- The v2 gear carries the v1 schema migration (`m0001`) and the v1 → v2 schema migration (`m0002`), so after the switch it starts on the existing v1 database.
- The migration of stored secret values from v1 to v2 is not committed to `main`; it lives in a temporary branch and is run from there.

## Order of PRs

1. SDK contract (models, errors, types, client and plugin API) and the gear skeleton: domain model, repository contract, DB schema; `out-of-scope.md`.
2. Repository implementation and its tests.
3. Domain service: type rules, hierarchy reduction, authorization, write protocol, listing; split over several PRs with their tests.
4. Infrastructure adapters (metrics, audit, tenant resolver, types registry, plugin selection), client and config.
5. REST API and gear wiring (still not registered).
6. Plugin conformance suite and test utilities in the SDK.
7. Plugins: static and Vault.
8. Switch.
