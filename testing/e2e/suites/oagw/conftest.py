"""Pytest configuration and fixtures for OAGW E2E tests."""
import asyncio
import os
import re
import threading
from collections import defaultdict
from pathlib import Path

import httpx
import pytest

from .helpers import Cleanup
from .mock_upstream import MockUpstreamServer


# ---------------------------------------------------------------------------
# Scenario traceability
# ---------------------------------------------------------------------------
#
# A test that covers a scenario in gears/system/oagw/scenarios/ says so with
#
#     @pytest.mark.scenario("positive-2.1-create-minimal-http-upstream")
#     @pytest.mark.scenario("negative-7.4-well-known-header-validation-errors-400", part="B")
#
# naming the scenario file's stem (unique across the tree; the bare numbers
# are not: custom-header-routing restarts at 1.1, and 2.10 exists twice) and,
# for files split into "## Scenario A/B/C" sections, the part. A test may
# carry several markers. Tests with no scenario carry none; their docstring
# names the ADR or decision they check instead.
#
# This follows the `scenario` marker of suites/types_registry (one ID per
# marker, links recorded in `user_properties` so they reach JUnit XML), with
# file stems as IDs. Unknown stems or parts fail collection.
#   -m scenario            only tests linked to a scenario
#   --oagw-scenario STEM   only the tests of one scenario (stem or prefix,
#                          e.g. "positive-18.3")
#   --oagw-scenario-map    print scenario -> tests with this run's outcome,
#                          plus the scenarios no test covers

SCENARIOS_DIR = Path(__file__).resolve().parents[4] / "gears" / "system" / "oagw" / "scenarios"
_SKIP_DIRS = {"flows", "examples"}


def _scenario_index() -> dict[str, Path]:
    """Scenario stem -> file, excluding flow write-ups and protocol examples."""
    return {
        p.stem: p
        for p in SCENARIOS_DIR.rglob("*.md")
        if p.name != "INDEX.md" and not _SKIP_DIRS & set(p.relative_to(SCENARIOS_DIR).parts)
    }


def _scenario_parts(path: Path) -> set[str]:
    return set(re.findall(r"^## Scenario ([A-Z]):", path.read_text(), re.MULTILINE))


def _natural_key(path: Path) -> list:
    """Sort 2.2 before 2.12."""
    return [int(t) if t.isdigit() else t for t in re.split(r"(\d+)", str(path))]


def _scenario_label(stem: str, part: str | None) -> str:
    return f"{stem} [{part}]" if part else stem


def pytest_addoption(parser):
    group = parser.getgroup("oagw", "OAGW e2e scenario traceability")
    group.addoption(
        "--oagw-scenario-map", action="store_true",
        help="print scenario -> tests with outcomes, plus scenarios with no test",
    )
    group.addoption(
        "--oagw-scenario", metavar="STEM",
        help="run only tests marked with this scenario stem (or stem prefix)",
    )


def pytest_configure(config):
    config.addinivalue_line(
        "markers",
        "scenario(stem, part=None): the gears/system/oagw/scenarios file this test covers",
    )


SCENARIO_TESTS = pytest.StashKey[tuple[dict[str, Path], dict[str, list[str]]]]()


@pytest.hookimpl(tryfirst=True)
def pytest_collection_modifyitems(config, items):
    """Validate scenario markers and capture links before -k/-m deselection."""
    here = Path(__file__).resolve().parent
    ours = [i for i in items if here in Path(str(i.path)).resolve().parents]
    if not ours or not SCENARIOS_DIR.is_dir():
        # No OAGW tests, or the scenario docs aren't present (installed bundle).
        return

    index = _scenario_index()
    errors = []
    covered: dict[str, list[str]] = defaultdict(list)
    for item in ours:
        for mark in item.iter_markers("scenario"):
            part = mark.kwargs.get("part")
            if len(mark.args) != 1:
                errors.append(f"{item.nodeid}: one scenario per marker, got {mark.args}")
                continue
            stem = mark.args[0]
            path = index.get(stem)
            if path is None:
                errors.append(f"{item.nodeid}: unknown scenario {stem!r}")
            elif part and part not in _scenario_parts(path):
                errors.append(f"{item.nodeid}: {stem} has no '## Scenario {part}:' section")
            else:
                label = _scenario_label(stem, part)
                covered[label].append(item.nodeid)
                item.user_properties.append(("scenario", label))
    if errors:
        raise pytest.UsageError("invalid @pytest.mark.scenario:\n  " + "\n  ".join(errors))
    config.stash[SCENARIO_TESTS] = (index, covered)

    wanted = config.getoption("--oagw-scenario", default=None)
    if wanted:
        keep = {
            nodeid for label, nodeids in covered.items()
            if label.split(" ")[0].startswith(wanted) for nodeid in nodeids
        }
        deselected = [i for i in ours if i.nodeid not in keep]
        config.hook.pytest_deselected(items=deselected)
        items[:] = [i for i in items if i not in deselected]


def _outcomes(terminalreporter) -> dict[str, str]:
    """nodeid -> outcome of this run (passed, failed, xfailed, skipped...)."""
    outcomes: dict[str, str] = {}
    for key, reports in terminalreporter.stats.items():
        if not key:  # passing setup/teardown phases
            continue
        for report in reports:
            if isinstance(report, pytest.TestReport) and outcomes.get(report.nodeid) not in ("failed", "error"):
                outcomes[report.nodeid] = key
    return outcomes


def pytest_terminal_summary(terminalreporter, config):
    if not config.getoption("--oagw-scenario-map", default=False):
        return
    index, covered = config.stash.get(SCENARIO_TESTS, ({}, {}))
    if not index:
        return
    tr = terminalreporter
    outcomes = _outcomes(tr)
    tr.section("OAGW scenarios")
    covered_stems = {label.split(" ")[0] for label in covered}
    for stem in sorted(index, key=lambda s: _natural_key(index[s].relative_to(SCENARIOS_DIR))):
        labels = sorted(label for label in covered if label.split(" ")[0] == stem)
        if not labels:
            continue
        tr.write_line(str(index[stem].relative_to(SCENARIOS_DIR)))
        for label in labels:
            part = label[len(stem):].strip()
            for nodeid in covered[label]:
                outcome = outcomes.get(nodeid, "not run")
                tr.write_line(f"    {part + ' ' if part else ''}{outcome}: {nodeid}")
    uncovered = sorted(
        (index[s].relative_to(SCENARIOS_DIR) for s in index if s not in covered_stems),
        key=_natural_key,
    )
    tr.write_line("")
    tr.write_line(f"{len(covered_stems)} of {len(index)} scenarios have tests; none for:")
    for rel in uncovered:
        tr.write_line(f"    {rel}")


# ---------------------------------------------------------------------------
# Environment-driven fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def oagw_base_url():
    """OAGW service base URL."""
    return os.getenv("E2E_OAGW_BASE_URL", "http://localhost:8086")


@pytest.fixture
def mock_upstream_url():
    """Mock upstream base URL (must be reachable by the OAGW service)."""
    return os.getenv("E2E_MOCK_UPSTREAM_URL", "http://127.0.0.1:19876")


@pytest.fixture
def oagw_headers():
    """Standard headers for OAGW requests (auth only — tenant comes from the token)."""
    token = os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a")
    return {
        "Authorization": f"Bearer {token}",
    }


# ---------------------------------------------------------------------------
# Hierarchy tenant headers (for multi-tenant budget allocation tests)
# ---------------------------------------------------------------------------

@pytest.fixture
def tenant_a_reviewer_headers():
    """A second subject inside the ``oagw_headers`` tenant (for ``scope: user``).

    Override together with ``E2E_AUTH_TOKEN`` so both stay in one tenant.
    """
    token = os.getenv("E2E_AUTH_TOKEN_SAME_TENANT", "e2e-token-tenant-a-reviewer")
    return {"Authorization": f"Bearer {token}"}


@pytest.fixture
async def cleanup(oagw_base_url):
    """Deletes registered upstreams at teardown, even when the test fails."""
    tracker = Cleanup()
    yield tracker
    await tracker.run(oagw_base_url)


@pytest.fixture
def hierarchy_root_headers():
    """Headers for hierarchy-root tenant (00000000-...001)."""
    return {"Authorization": "Bearer e2e-token-hierarchy-root"}


@pytest.fixture
def hierarchy_l1a_headers():
    """Headers for hierarchy-l1a tenant (00000000-...002), child of root."""
    return {"Authorization": "Bearer e2e-token-hierarchy-l1a"}


@pytest.fixture
def hierarchy_l1b_headers():
    """Headers for hierarchy-l1b tenant (00000000-...005), child of root."""
    return {"Authorization": "Bearer e2e-token-hierarchy-l1b"}


# ---------------------------------------------------------------------------
# Session-scoped mock upstream server
# ---------------------------------------------------------------------------

@pytest.fixture(scope="session")
def mock_upstream():
    """Start the mock upstream server for the entire test session."""
    url = os.getenv("E2E_MOCK_UPSTREAM_URL", "http://127.0.0.1:19876")

    # If a custom URL is set, assume the mock is managed externally.
    if os.getenv("E2E_MOCK_UPSTREAM_EXTERNAL"):
        yield
        return

    # Parse port from URL.
    port = int(url.rsplit(":", 1)[-1].split("/")[0])
    server = MockUpstreamServer(host="127.0.0.1", port=port)

    # Run the mock server in a background thread with its own event loop
    # so it can actually serve requests while tests run.
    loop = asyncio.new_event_loop()
    loop.run_until_complete(server.start())

    thread = threading.Thread(target=loop.run_forever, daemon=True)
    thread.start()

    yield server

    async def _shutdown() -> None:
        if server._server:
            server._server.close()
        current = asyncio.current_task()
        pending = [
            t for t in asyncio.all_tasks()
            if t is not current and not t.done()
        ]
        for task in pending:
            task.cancel()
        if pending:
            await asyncio.wait(pending, timeout=2)

    fut = asyncio.run_coroutine_threadsafe(_shutdown(), loop)
    try:
        fut.result(timeout=5)
    except (TimeoutError, Exception):
        pass  # Best-effort; the daemon thread will die with the process.
    loop.call_soon_threadsafe(loop.stop)
    thread.join(timeout=5)


# ---------------------------------------------------------------------------
# Session-scoped OAGW reachability check
# ---------------------------------------------------------------------------

@pytest.fixture(scope="session", autouse=True)
def _check_oagw_reachable():
    """Skip all OAGW tests if the service is not reachable."""
    url = os.getenv("E2E_OAGW_BASE_URL", "http://localhost:8086")
    try:
        resp = httpx.get(f"{url}/oagw/v1/upstreams", timeout=5.0)
        # Any response (even 401/403) means the service is up.
    except httpx.ConnectError:
        pytest.skip(f"OAGW service not running at {url}", allow_module_level=True)
    except Exception:
        # Timeout or other transient error — still try to run tests.
        pass


# ---------------------------------------------------------------------------
# Provision the secrets the auth-injection tests resolve via `cred://`
# ---------------------------------------------------------------------------

# Secrets the apikey / oauth2 auth plugins resolve via `cred://<ref>`.
# Values mirror the historical static-credstore-plugin seed in
# config/e2e-local.yaml.
_CREDSTORE_SECRETS = {
    "openai-key": "sk-test-e2e-fake-key",
    "test-oauth2-client-id": "test-client-id",
    "test-oauth2-client-secret": "test-client-secret",
}


@pytest.fixture(scope="session", autouse=True)
def _provision_credstore_secrets(_check_oagw_reachable):
    """Write the auth-injection secrets through the credstore gateway API.

    A provisioning failure errors every OAGW test instead of letting the auth
    tests skip.

    The credstore gateway is now stateful: a GET resolves the secret's
    metadata from the gateway's own database first, then reads the value from
    the backend plugin. A secret merely pre-seeded in the static plugin config
    has no gateway metadata row and is therefore unreachable. So we create the
    secrets via the gateway API (which writes the metadata row *and* the
    backend value).

    We POST (create-only) and on 409 — a rerun against an already-provisioned
    rig — PUT with the explicit ``If-Match: *`` overwrite (PUT no longer
    creates and requires a precondition), with the same token the proxied
    requests carry (``E2E_AUTH_TOKEN`` -> tenant ``00000000-df51-...``). Sharing is
    ``tenant`` so any subject in that tenant resolves the value — this matches
    the proxied-request context regardless of subject_id (the historical seed
    used owner-bound ``private``; the gateway lookup does not depend on the
    OAGW auth-config ``sharing`` label).
    """
    base_url = os.getenv("E2E_OAGW_BASE_URL", "http://localhost:8086")
    token = os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a")
    headers = {
        "Authorization": f"Bearer {token}",
        "content-type": "application/json",
    }
    # Iterate over a literal tuple of reference names (not the value mapping)
    # so nothing derived from the secret values flows into log messages.
    for ref in ("openai-key", "test-oauth2-client-id", "test-oauth2-client-secret"):
        try:
            resp = httpx.post(
                f"{base_url}/credstore/v1/secrets",
                headers=headers,
                json={
                    "reference": ref,
                    "value": _CREDSTORE_SECRETS[ref],
                    "sharing": "tenant",
                },
                timeout=5.0,
            )
            if resp.status_code == 409:
                # Already provisioned (rerun): overwrite in place.
                resp = httpx.put(
                    f"{base_url}/credstore/v1/secrets/{ref}",
                    headers={**headers, "If-Match": "*"},
                    json={"value": _CREDSTORE_SECRETS[ref], "sharing": "tenant"},
                    timeout=5.0,
                )
        except httpx.RequestError as exc:
            # A connection refusal was already turned into a skip by
            # `_check_oagw_reachable`; anything else (timeout, reset) means
            # the secrets are missing, so fail like a bad status does.
            pytest.fail(f"[e2e] could not provision credstore secret {ref!r}: {exc!r}")
        if resp.status_code not in (200, 201, 204):
            # Error every OAGW test (this fixture is session-scoped and
            # autouse): the auth-injection tests assert the injected value, so
            # a missing secret must not surface as a skip or a misleading
            # plugin failure. `fail`, not `exit`, so other suites in the same
            # run are unaffected. Report only the ref name and status, never
            # the body, which could reflect the secret value.
            pytest.fail(
                f"[e2e] could not provision credstore secret {ref!r}: "
                f"HTTP {resp.status_code}"
            )
    yield
