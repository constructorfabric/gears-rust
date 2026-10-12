<!-- Updated: 2026-09-24 by Constructor Tech -->

# OAGW e2e suite

HTTP tests against a running server: the Management API (`/oagw/v1/upstreams`,
`/routes`) and the Proxy API (`/oagw/v1/proxy/{alias}/...`), with
`mock_upstream.py` as the upstream.

## Scenario links

A test that covers a scenario in
[`gears/system/oagw/scenarios/`](../../../../gears/system/oagw/scenarios/INDEX.md)
says so with the scenario file's stem:

```python
@pytest.mark.scenario("positive-2.1-create-minimal-http-upstream")
@pytest.mark.scenario("negative-7.4-well-known-header-validation-errors-400", part="B")
```

- The stem is the ID because the bare numbers are not unique:
  `custom-header-routing/` restarts at 1.1, and 2.10 exists as both a
  positive and a negative scenario.
- `part` names a `## Scenario A/B/C` section, for files that have them.
- Use one marker per scenario. A parametrised test can put the marker on a
  single `pytest.param(..., marks=...)` when only that case covers the scenario.
- Tests with no scenario have no marker. Their docstring names the ADR or
  decision they check instead. So do tests whose nominal scenario they cannot
  actually reach: 5.1 (static-authz has no permission model), and 7.4-A
  (hyper answers a bad Content-Length before OAGW runs).
- Collection fails on an unknown stem or part, so the links can't go stale.
  The marker follows the `scenario` convention of
  [`types_registry`](../types_registry/README.md). Links are also written to
  `user_properties`, so they reach the JUnit XML.

## Tests for decided but unshipped fixes

Behaviour that a decision in the OAGW docs-vs-implementation review will
change is tested as the decision specifies, and marked
`@pytest.mark.xfail(strict=True, raises=AssertionError, reason="<ID>: ...")`:

- The test fails today, on an assertion about the behaviour itself.
- When the fix lands, the test XPASSes and strict mode turns that into a
  failure. The fixing PR removes the marker.
- Setup and preconditions use `pytest.fail` or helpers that raise HTTP errors.
  So an unrelated breakage fails the test, instead of passing as the expected
  failure.

## Conventions

- Register every created upstream with the `cleanup` fixture
  (`cleanup.upstream(headers, await create_upstream(...))`). Deletion runs at
  teardown, children before parents, even when the test fails.
- Pin the identity of errors with `helpers.assert_problem` (status,
  problem+json, `x-oagw-error-source`, and optionally `type`/`reason`/
  `resource_type`/`detail`). `esrc=None` asserts that a layer in front of
  OAGW produced the response.
- Page lists with `helpers.list_all`. The API caps a page at 100.
- A credstore provisioning failure in `conftest.py` errors every test. The
  auth tests assert the injected values and never skip.
- Rate-limit header names live in one place (`test_rate_limiting.py`), ready
  for the R-17 rename.

## Run

From the repository root:

```sh
make e2e-local SUITE=oagw

# Only the tests of one scenario (a stem or a stem prefix).
.venv/bin/python tools/scripts/run_e2e.py --suite oagw -- --oagw-scenario positive-18.3

# Scenario -> tests with this run's outcome, plus the scenarios no test covers.
.venv/bin/python tools/scripts/run_e2e.py --suite oagw -- --oagw-scenario-map

# The same map without a server; outcomes show "not run".
.venv/bin/python -m pytest testing/e2e/suites/oagw --collect-only -q --oagw-scenario-map
```

Against an already running server, set `E2E_OAGW_BASE_URL`. To run the mock
separately, start it with `python -m suites.oagw.mock_upstream` from
`testing/e2e` and set `E2E_MOCK_UPSTREAM_EXTERNAL=1`.
