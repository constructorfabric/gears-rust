"""E2e aggregated query tests for the usage-collector module.

Covers GET /usage-collector/v1/aggregated against the timescaledb-backed gateway:
sum, count, avg, group-by-resource, and time-range scenarios.
All queries include resource_id to route to the raw hypertable path (no cagg dependency).
"""

from __future__ import annotations

import uuid
from datetime import datetime, timedelta, timezone

import pytest

from .helpers import encode_dt, wait_for_n_records, wait_for_record


MODULE = "e2e-test"
RESOURCE_TYPE = "e2e.resource"
TENANT_ID = "00000000-df51-5b42-9538-d2b56b7ee953"


@pytest.mark.asyncio
async def test_aggregated_sum(gateway_client):
    """Ingest 3 records with value=10.0; GET /aggregated fn=sum must return ~30.0."""
    resource_id = str(uuid.uuid4())
    from_dt = datetime.now(timezone.utc)
    to_dt = from_dt + timedelta(minutes=5)

    for _ in range(3):
        resp = await gateway_client.post(
            "/usage-collector/v1/records",
            json={
                "module": MODULE,
                "tenant_id": TENANT_ID,
                "resource_type": RESOURCE_TYPE,
                "resource_id": resource_id,
                "metric": "e2e.gauge",
                "value": 10.0,
                "timestamp": datetime.now(timezone.utc).isoformat(),
            },
        )
        assert resp.status_code == 204, f"expected 204, got {resp.status_code}: {resp.text}"

    await wait_for_n_records(gateway_client, from_dt, to_dt, 3, resource_id=resource_id)

    resp = await gateway_client.get(
        "/usage-collector/v1/aggregated",
        params={
            "from": encode_dt(from_dt),
            "to": encode_dt(to_dt),
            "fn": "sum",
            "resource_id": resource_id,
        },
    )
    resp.raise_for_status()
    items = resp.json()
    assert len(items) > 0, "expected at least one aggregated row"
    assert abs(items[0]["value"] - 30.0) < 0.001, (
        f"expected sum ~30.0, got {items[0]['value']}"
    )


@pytest.mark.asyncio
async def test_aggregated_count(gateway_client):
    """Ingest 5 records; GET /aggregated fn=count must return exactly 5."""
    resource_id = str(uuid.uuid4())
    from_dt = datetime.now(timezone.utc)
    to_dt = from_dt + timedelta(minutes=5)

    for _ in range(5):
        resp = await gateway_client.post(
            "/usage-collector/v1/records",
            json={
                "module": MODULE,
                "tenant_id": TENANT_ID,
                "resource_type": RESOURCE_TYPE,
                "resource_id": resource_id,
                "metric": "e2e.gauge",
                "value": 1.0,
                "timestamp": datetime.now(timezone.utc).isoformat(),
            },
        )
        assert resp.status_code == 204, f"expected 204, got {resp.status_code}: {resp.text}"

    await wait_for_n_records(gateway_client, from_dt, to_dt, 5, resource_id=resource_id)

    resp = await gateway_client.get(
        "/usage-collector/v1/aggregated",
        params={
            "from": encode_dt(from_dt),
            "to": encode_dt(to_dt),
            "fn": "count",
            "resource_id": resource_id,
        },
    )
    resp.raise_for_status()
    items = resp.json()
    assert len(items) > 0, "expected at least one aggregated row"
    assert items[0]["value"] == 5, f"expected count 5, got {items[0]['value']}"


@pytest.mark.asyncio
async def test_aggregated_avg(gateway_client):
    """Ingest 4 records with values [10, 20, 30, 40]; GET /aggregated fn=avg must return ~25.0."""
    resource_id = str(uuid.uuid4())
    from_dt = datetime.now(timezone.utc)
    to_dt = from_dt + timedelta(minutes=5)

    for value in [10.0, 20.0, 30.0, 40.0]:
        resp = await gateway_client.post(
            "/usage-collector/v1/records",
            json={
                "module": MODULE,
                "tenant_id": TENANT_ID,
                "resource_type": RESOURCE_TYPE,
                "resource_id": resource_id,
                "metric": "e2e.gauge",
                "value": value,
                "timestamp": datetime.now(timezone.utc).isoformat(),
            },
        )
        assert resp.status_code == 204, f"expected 204, got {resp.status_code}: {resp.text}"

    await wait_for_n_records(gateway_client, from_dt, to_dt, 4, resource_id=resource_id)

    resp = await gateway_client.get(
        "/usage-collector/v1/aggregated",
        params={
            "from": encode_dt(from_dt),
            "to": encode_dt(to_dt),
            "fn": "avg",
            "resource_id": resource_id,
        },
    )
    resp.raise_for_status()
    items = resp.json()
    assert len(items) > 0, "expected at least one aggregated row"
    assert abs(items[0]["value"] - 25.0) < 0.001, (
        f"expected avg ~25.0, got {items[0]['value']}"
    )


@pytest.mark.asyncio
async def test_aggregated_group_by_resource(gateway_client):
    """group_by=resource groups results by resource_id; both ingested resources must appear."""
    res_a = str(uuid.uuid4())
    res_b = str(uuid.uuid4())
    # Shared subject_id routes query through the raw hypertable path (no cagg dependency).
    shared_subject_id = str(uuid.uuid4())
    from_dt = datetime.now(timezone.utc)
    to_dt = from_dt + timedelta(minutes=5)

    for resource_id in (res_a, res_b):
        resp = await gateway_client.post(
            "/usage-collector/v1/records",
            json={
                "module": MODULE,
                "tenant_id": TENANT_ID,
                "resource_type": RESOURCE_TYPE,
                "resource_id": resource_id,
                "subject_id": shared_subject_id,
                "metric": "e2e.gauge",
                "value": 5.0,
                "timestamp": datetime.now(timezone.utc).isoformat(),
            },
        )
        assert resp.status_code == 204, f"expected 204, got {resp.status_code}: {resp.text}"

    await wait_for_record(gateway_client, from_dt, to_dt, resource_id=res_a)
    await wait_for_record(gateway_client, from_dt, to_dt, resource_id=res_b)

    resp = await gateway_client.get(
        "/usage-collector/v1/aggregated",
        params={
            "from": encode_dt(from_dt),
            "to": encode_dt(to_dt),
            "fn": "sum",
            "group_by": "resource",
            "subject_id": shared_subject_id,
        },
    )
    resp.raise_for_status()
    items = resp.json()
    result_resource_ids = {str(item.get("resource_id")) for item in items}
    assert res_a in result_resource_ids, (
        f"expected res_a {res_a} in grouped results, got resource_ids: {result_resource_ids}"
    )
    assert res_b in result_resource_ids, (
        f"expected res_b {res_b} in grouped results, got resource_ids: {result_resource_ids}"
    )


@pytest.mark.asyncio
async def test_aggregated_time_range(gateway_client):
    """Only records within the narrow query window contribute to the aggregate value."""
    resource_id = str(uuid.uuid4())
    now = datetime.now(timezone.utc)
    in_window_ts = now - timedelta(seconds=30)
    out_window_ts = now - timedelta(hours=2)

    # Ingest one record outside the narrow window (value 100.0 — must not be counted)
    resp = await gateway_client.post(
        "/usage-collector/v1/records",
        json={
            "module": MODULE,
            "tenant_id": TENANT_ID,
            "resource_type": RESOURCE_TYPE,
            "resource_id": resource_id,
            "metric": "e2e.gauge",
            "value": 100.0,
            "timestamp": out_window_ts.isoformat(),
        },
    )
    assert resp.status_code == 204, f"expected 204, got {resp.status_code}: {resp.text}"

    # Ingest one record inside the narrow window (value 7.0 — must be the only contribution)
    resp = await gateway_client.post(
        "/usage-collector/v1/records",
        json={
            "module": MODULE,
            "tenant_id": TENANT_ID,
            "resource_type": RESOURCE_TYPE,
            "resource_id": resource_id,
            "metric": "e2e.gauge",
            "value": 7.0,
            "timestamp": in_window_ts.isoformat(),
        },
    )
    assert resp.status_code == 204, f"expected 204, got {resp.status_code}: {resp.text}"

    narrow_from = now - timedelta(minutes=2)
    narrow_to = now + timedelta(minutes=1)
    await wait_for_record(gateway_client, narrow_from, narrow_to, resource_id=resource_id)

    resp = await gateway_client.get(
        "/usage-collector/v1/aggregated",
        params={
            "from": encode_dt(narrow_from),
            "to": encode_dt(narrow_to),
            "fn": "sum",
            "resource_id": resource_id,
        },
    )
    resp.raise_for_status()
    items = resp.json()
    assert len(items) > 0, "expected at least one aggregated row"
    assert abs(items[0]["value"] - 7.0) < 0.001, (
        f"expected sum ~7.0 (only in-window record), got {items[0]['value']}"
    )
