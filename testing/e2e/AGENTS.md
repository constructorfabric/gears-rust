# testing/e2e/

Python pytest suites (CI uses Python 3.11) that run against a live `cf-gears-example-server`. Mechanics are in `README.md` here; what to test is in `docs/toolkit_unified_system/13_e2e_testing.md`.

- Run through make rather than pytest directly (`tools/scripts/run_e2e.py --suite <s> --dry-run` shows what make would run): `make e2e-local SUITE=<suite>`, `make e2e-local GEAR=<gear>` (every suite whose features include the gear), `make e2e-local-smoke`, `make e2e-docker`. CI (`.github/workflows/e2e.yml`) runs `make e2e-local`, `make e2e-docker-smoke` and a few self-managed suites as separate steps.
- A suite is a directory under `suites/` with an `e2e.yaml`: `launcher: e2e-launcher` shares one server with other suites; `launcher: pytest` starts its own server (different features, ports or containers). Add a suite following README § Add a new E2E suite.
- Dependencies: `requirements.txt`; pytest settings (markers, timeout, asyncio mode): `pytest.ini`. Mark critical-path tests `@pytest.mark.smoke`.
- Test-writing rules (no sleeps, own data, assert the body contract): `13_e2e_testing.md` § Anti-Flaking Practices.
