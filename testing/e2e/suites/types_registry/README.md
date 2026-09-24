<!-- Updated: 2026-09-17 by Constructor Tech -->

# Types Registry e2e suite

The suite covers two APIs:

| Path | Covers |
|---|---|
| `test_registration.py`, `test_deletion.py` | the asynchronous admission API (`202` + `GET /operations/{id}`), written as numbered scenarios |
| [`legacy/`](legacy/README.md) | the original synchronous `v1` API (`200` + a `results` array) |

Async scenarios pair tests, `scenarios/` documentation and `fixtures/` data:
[registration](scenarios/registration.md) (`TR-REG-*`) and
[deletion](scenarios/deletion.md) (`TR-DEL-*`).

`conftest.py` provides HTTP/fixture setup and scenario-ID binding; `helpers.py`
provides submit-and-poll helpers and comparators. Pytest collects both sets.

## Execution rules

- `registry_api` uses `v2` today and `v1` after T24a. Set
  `TYPES_REGISTRY_API_VERSION` explicitly; there is no fallback.
- Each test gets a fresh `cf.e2e.r<uuid>.` namespace. Fixture loaders rewrite
  IDs and `$ref` targets, not schema constraints or values.
- Every submission carries a fresh `Idempotency-Key`.
- Registration outcomes match by GTS ID. Deletion outcomes preserve request
  order and use `assert_operation(..., ordered=True)`.
- Tests contain complete expected bodies; scenario docs state intent.

## Run

From the repository root, with the existing Python environment:

```sh
# Whole suite.
.venv/bin/python tools/scripts/run_e2e.py --suite types-registry

# Through make.
make e2e-local SUITE=types-registry

# Scenario tests only.
.venv/bin/python tools/scripts/run_e2e.py --suite types-registry -- -m scenario

# Legacy tests only.
.venv/bin/python tools/scripts/run_e2e.py --suite types-registry -- -m "not scenario"

# Collection only; verifies scenario-ID bindings.
.venv/bin/python -m pytest testing/e2e/suites/types_registry --collect-only
```

Select subsets by marker: arguments after `--` are appended after the suite path.
Scenario tests carry the `scenario` marker; legacy tests do not.

The local launcher serves authenticated v2 directly; no edge gateway is required.
