---
cf: true
type: project-rule
topic: testing
generated-by: auto-config
version: 1.0
---
# Testing

<!-- toc -->

- [Test Strategy](#test-strategy)
- [E2E](#e2e)

<!-- /toc -->

Use this when writing or running tests.

## Test Strategy
- Follow the repository test pyramid and command split. Evidence: `docs/TESTING.md:8-38`
- Maintain the documented 80% line-coverage target. Evidence: `docs/TESTING.md:42-79`
- Use crate-level `tests/` suites for HTTP/router integration behavior. Evidence: `gears/system/account-management/account-management/tests/api_users_test.rs:1-60`
- Use `trybuild` compile-fail suites for type-system and security invariants. Evidence: `libs/toolkit/tests/typed_builder_compilefail.rs:1-21`, `libs/toolkit-db/tests/ui.rs:1-10`

## E2E
- Prefer deterministic E2E tests with isolated data and hard timeouts. Evidence: `docs/toolkit_unified_system/13_e2e_testing.md:127-195`
- Assert response body contracts, not only HTTP status codes. Evidence: `docs/toolkit_unified_system/13_e2e_testing.md:324-359`
