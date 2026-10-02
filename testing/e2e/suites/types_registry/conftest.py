"""Shared JSON scenario data and HTTP fixtures for the async admission API."""

import json
import os
from pathlib import Path
import re
import uuid

import httpx
import pytest

from .helpers import RECEIPT, TRACEPARENT, assert_operation, submit_and_poll


SCENARIO_TESTS = pytest.StashKey[dict[str, list[str]]]()


@pytest.fixture
def registry_api_path():
    """Use the configured API version without an automatic fallback."""
    return f"types-registry/{os.getenv('TYPES_REGISTRY_API_VERSION', 'v2')}"


@pytest.fixture
async def registry_http(base_url, auth_headers):
    async with httpx.AsyncClient(
        base_url=base_url.rstrip("/") + "/",
        headers={**auth_headers, "traceparent": TRACEPARENT},
        timeout=3.0,
    ) as client:
        yield client


def _topic_loader(topic):
    """Load `fixtures/<topic>/` with one namespace per test.

    Every fixture in a topic spells its IDs with the `cf.e2e.<topic>.` prefix;
    rewriting that prefix keeps chained IDs and `$ref` targets consistent while
    making the whole set unique to this test.

    One namespace per loader, so a test that asks for two topics gets two
    namespaces — fixtures of different topics cannot reference each other.
    """
    namespace = f"r{uuid.uuid4().hex}"
    directory = Path(__file__).parent / "fixtures" / topic
    prefix = f"cf.e2e.{topic}."

    def load(name):
        document = (directory / f"{name}.json").read_text(encoding="utf-8")
        assert prefix in document, (
            f"fixtures/{topic}/{name}.json must spell its identifiers with "
            f"'{prefix}' so the per-test namespace can isolate them"
        )
        return json.loads(document.replace(prefix, f"cf.e2e.{namespace}."))

    return load


@pytest.fixture
def registration_fixture():
    """Load the files linked from scenarios/registration.md."""
    return _topic_loader("registration")


@pytest.fixture
def deletion_fixture():
    """Load the files linked from scenarios/deletion.md."""
    return _topic_loader("deletion")


@pytest.fixture
def discovery_fixture():
    """Load the inheritance trees linked from scenarios/discovery.md."""
    return _topic_loader("discovery")


@pytest.fixture
def neighbour_discovery_fixture():
    """The same trees in a second namespace, which a target pattern must exclude."""
    return _topic_loader("discovery")


@pytest.fixture
def reading_fixture():
    """Load authored documents and expected artifacts for read scenarios."""
    return _topic_loader("reading")


@pytest.fixture
def given_registered(registry_http, registry_api_path):
    """Register prerequisites and prove that every one of them committed.

    The whole operation is compared, so a missing outcome fails the setup
    instead of leaving an exclusion test with nothing to exclude.
    """

    async def register(*items):
        operation = await submit_and_poll(
            registry_http, registry_api_path, list(items), RECEIPT
        )
        assert_operation(
            operation,
            {
                "operation_id": "<operation_id>",
                "kind": "registration",
                "dry_run": False,
                "status": "completed",
                "created_at": "<created_at>",
                "started_at": "<started_at>",
                "completed_at": "<completed_at>",
                "items": [
                    {
                        "gts_id": item["gts_id"],
                        "status": "succeeded",
                        "resource_version": 1,
                        "error": None,
                    }
                    for item in items
                ],
            },
        )
        return operation

    return register


def pytest_configure(config):
    config.addinivalue_line(
        "markers", "scenario(id): stable scenario ID from a scenarios/*.md document"
    )


@pytest.hookimpl(tryfirst=True)
def pytest_collection_modifyitems(config, items):
    """Capture associations before -k/-m deselection; permit planned scenarios."""
    associations = {}
    for document in sorted((Path(__file__).parent / "scenarios").glob("*.md")):
        for scenario_id in re.findall(
            r"^### (TR-[A-Z]+-\d{3}) —", document.read_text(encoding="utf-8"), re.M
        ):
            if scenario_id in associations:
                raise pytest.UsageError(f"Duplicate scenario ID {scenario_id} in {document}")
            associations[scenario_id] = []
    for item in items:
        if Path(__file__).parent not in item.path.parents:
            continue
        for marker in item.iter_markers("scenario"):
            if len(marker.args) != 1 or marker.args[0] not in associations:
                raise pytest.UsageError(
                    f"Unknown scenario on {item.nodeid}: {marker.args}"
                )
            scenario_id = marker.args[0]
            associations[scenario_id].append(item.nodeid)
            item.user_properties.append(("scenario", scenario_id))
    config.stash[SCENARIO_TESTS] = associations


def pytest_terminal_summary(terminalreporter):
    """Keep automation links separate from actual results, including deselection."""
    associations = terminalreporter.config.stash.get(SCENARIO_TESTS, {})
    if not associations:
        return
    terminalreporter.section("Types Registry scenarios")
    reports = [
        report
        for group in terminalreporter.stats.values()
        for report in group
        if isinstance(report, pytest.TestReport)
    ]
    for scenario_id, nodeids in associations.items():
        if not nodeids:
            terminalreporter.write_line(f"{scenario_id}: no test collected")
        for nodeid in nodeids:
            results = [report for report in reports if report.nodeid == nodeid]
            if any(report.failed for report in results):
                outcome = "failed"
            elif any(report.skipped for report in results):
                outcome = "skipped"
            elif any(report.when == "call" and report.passed for report in results):
                outcome = "passed"
            else:
                outcome = "not run"
            terminalreporter.write_line(f"{scenario_id}: {outcome} — {nodeid}")
