<!-- Updated: 2026-09-17 by Constructor Tech -->

# Types Registry e2e suite

Two sets of tests live here, against two different APIs:

| Path | Covers |
|---|---|
| `test_registration.py`, `test_deletion.py` | the asynchronous admission API (`202` + `GET /operations/{id}`), written as numbered scenarios |
| [`legacy/`](legacy/README.md) | the original synchronous `v1` API (`200` + a `results` array) |

Each topic of the asynchronous set is three siblings named after it: the tests,
its scenario document under `scenarios/`, and its JSON input items under
`fixtures/`. Today that is
[registration](scenarios/registration.md) (`TR-REG-*`) and
[deletion](scenarios/deletion.md) (`TR-DEL-*`).

`conftest.py` holds the shared HTTP client, the fixture loader and the
scenario-ID binding. `helpers.py` holds the submit-and-poll driver and the body
comparators. `e2e.yaml` is the suite manifest and points at this directory, so
pytest collects both sets recursively.

## Execution rules

Shared by every scenario file under `scenarios/`:

- `registry_api` is `{base_url}/types-registry/v2` today and `v1` from T24a.
  `TYPES_REGISTRY_API_VERSION` selects a version explicitly and there is no
  fallback; scenario IDs and fixtures are version-independent.
- Each test owns a namespace: its fixture loader rewrites the topic prefix in
  every fixture to a fresh `cf.e2e.r<uuid>.`, chained IDs and `$ref` targets
  included, and changes no schema constraint or value. `registration_fixture`
  rewrites `cf.e2e.registration.`, `deletion_fixture` rewrites
  `cf.e2e.deletion.`.
- Every submission carries a fresh `Idempotency-Key`.
- Item outcomes are matched by exact GTS ID for registration, whose response
  order carries no contract. Deletion is the exception: its outcomes are
  reported in request order so that a caller who deleted by Registry Reference
  can match identifier-keyed outcomes positionally, so those scenarios compare
  `items` as they arrived (`assert_operation(..., ordered=True)`).
- Complete expected bodies live in the tests as inline Python dictionaries; the
  scenario files state intent, not payloads.

## Run

From the repository root, with the existing Python environment:

```sh
# Whole suite: build, launch, run legacy + scenarios, stop the server.
.venv/bin/python tools/scripts/run_e2e.py --suite types-registry

# Same thing through make.
make e2e-local SUITE=types-registry

# Scenario tests only.
.venv/bin/python tools/scripts/run_e2e.py --suite types-registry -- -m scenario

# Legacy tests only.
.venv/bin/python tools/scripts/run_e2e.py --suite types-registry -- -m "not scenario"

# Collection only, no server: checks that every scenario ID binds to a test.
.venv/bin/python -m pytest testing/e2e/suites/types_registry --collect-only
```

Select a subset by marker, not by path: the runner appends whatever follows `--`
*after* the suite directory, so a bare path is added to it rather than replacing
it. The scenario tests carry the `scenario` marker; the legacy tests do not.

The co-hosted local launcher serves v2 with authentication, so these tests need
neither `.exposed()` nor an edge gateway.
