<!-- Updated: 2026-09-17 by Constructor Tech -->

# Legacy Types Registry e2e tests

These are the original end-to-end tests for the **synchronous** Types Registry
API: `POST /types-registry/v1/entities` returns `200` with a `results` array,
and every assertion reads that response body directly.

They are kept here, unchanged in behaviour, while the async admission API
(`202` + `GET /operations/{id}`) is built out next to them. The new
submit-then-poll tests live one level up, in the suite root
(`test_registration.py`, `scenarios/registration.md` and the JSON inputs
under `fixtures/`).

The directory is named `legacy`, not `v1`, on purpose: the async API is served
at `v2` today and takes over `v1` at cutover (T24a), so a version-based name
would become wrong.

## What is in here

| File | Covers |
|---|---|
| `test_types_registry_register.py` | registration through the synchronous `results` contract |
| `test_types_registry_get.py` | `GET /entities/{gts_id}` reads, UUID form, GTS ID segments |
| `test_types_registry_list.py` | `GET /entities` listing and filtering |
| `test_types_registry_validation.py` | instance-against-schema validation outcomes |
| `test_types_registry_error_handling.py` | RFC-9457 problem responses and edge cases |
| `test_registration_debug_logging.py` | debug log output on registration failures |
| `helpers.py` | GTS ID generators that belong to this older test era |

## Running

The commands are in the [suite README](../README.md): the suite manifest
(`../e2e.yaml`) points at the suite directory and pytest collects recursively,
so the default suite command runs both sets, and `-m "not scenario"` selects
these 58 tests alone. To collect just this directory, without a server:

```sh
.venv/bin/python -m pytest testing/e2e/suites/types_registry/legacy --collect-only
```

## When this directory can go away

Delete `legacy/` once **both** are true:

1. The coverage worth keeping — reads, listing and filtering, validation
   outcomes, error shapes, debug logging — has moved into the scenario-backed
   tests in the suite root. The four scenarios `TR-REG-001..004` that exist today
   cover registration only and do **not** replace these checks.
2. The synchronous v1 API itself is retired (T24a cutover, then removal), so
   nothing is left for these tests to exercise.

Until then, treat the files as frozen: fix them only when the old API's own
behaviour changes.
