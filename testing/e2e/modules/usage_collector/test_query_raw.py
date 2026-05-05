"""E2e raw query tests for the usage-collector module.

Covers GET /usage-collector/v1/raw against the timescaledb-backed gateway instance:
basic retrieval, time-range exclusion, cursor pagination, ascending sort, multi-metric.
"""

from __future__ import annotations

import asyncio
import uuid
from datetime import datetime, timedelta, timezone

import pytest

from .helpers import encode_dt, wait_for_record


MODULE = "e2e-test"
RESOURCE_TYPE = "e2e.resource"
TENANT_ID = "00000000-df51-5b42-9538-d2b56b7ee953"


@pytest.mark.asyncio
async def test_raw_query_basic(gateway_client):
    """Ingest one record; it must appear in GET /raw filtered by resource_id."""
    resource_id = str(uuid.uuid4())
    from_dt = datetime.now(timezone.utc)
    to_dt = from_dt + timedelta(minutes=5)

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

    record = await wait_for_record(gateway_client, from_dt, to_dt, resource_id=resource_id)
    assert str(record.get("resource_id")) == resource_id
    assert record.get("metric") == "e2e.gauge"


@pytest.mark.asyncio
async def test_raw_query_time_range_excludes_outside(gateway_client):
    """Record whose timestamp is outside the query window must not appear in items."""
    resource_id = str(uuid.uuid4())
    past_ts = datetime.now(timezone.utc) - timedelta(hours=2)

    resp = await gateway_client.post(
        "/usage-collector/v1/records",
        json={
            "module": MODULE,
            "tenant_id": TENANT_ID,
            "resource_type": RESOURCE_TYPE,
            "resource_id": resource_id,
            "metric": "e2e.gauge",
            "value": 1.0,
            "timestamp": past_ts.isoformat(),
        },
    )
    assert resp.status_code == 204, f"expected 204, got {resp.status_code}: {resp.text}"

    # Confirm the record is stored in the wide window around past_ts
    wide_from = past_ts - timedelta(minutes=1)
    wide_to = past_ts + timedelta(minutes=1)
    await wait_for_record(gateway_client, wide_from, wide_to, resource_id=resource_id)

    # Query a future window that does not include past_ts
    future_from = datetime.now(timezone.utc) + timedelta(hours=1)
    future_to = future_from + timedelta(hours=1)
    resp2 = await gateway_client.get(
        "/usage-collector/v1/raw",
        params={"from": encode_dt(future_from), "to": encode_dt(future_to)},
    )
    resp2.raise_for_status()
    items = resp2.json().get("items", [])
    matching = [item for item in items if str(item.get("resource_id")) == resource_id]
    assert len(matching) == 0, (
        f"resource_id {resource_id} must not appear in future window, found: {matching}"
    )


@pytest.mark.asyncio
async def test_raw_query_pagination_cursor(gateway_client):
    """GET /raw with page_size=1 must return a cursor; second page must be non-empty."""
    resource_id_a = str(uuid.uuid4())
    resource_id_b = str(uuid.uuid4())
    from_dt = datetime.now(timezone.utc)
    to_dt = from_dt + timedelta(minutes=5)
    # Use distinct timestamps so pagination sort order is deterministic.
    ts_a = from_dt + timedelta(seconds=1)
    ts_b = from_dt + timedelta(seconds=2)

    for resource_id, ts in ((resource_id_a, ts_a), (resource_id_b, ts_b)):
        resp = await gateway_client.post(
            "/usage-collector/v1/records",
            json={
                "module": MODULE,
                "tenant_id": TENANT_ID,
                "resource_type": RESOURCE_TYPE,
                "resource_id": resource_id,
                "metric": "e2e.gauge",
                "value": 1.0,
                "timestamp": ts.isoformat(),
            },
        )
        assert resp.status_code == 204, f"expected 204, got {resp.status_code}: {resp.text}"

    await wait_for_record(gateway_client, from_dt, to_dt, resource_id=resource_id_a)
    await wait_for_record(gateway_client, from_dt, to_dt, resource_id=resource_id_b)

    # First page
    resp1 = await gateway_client.get(
        "/usage-collector/v1/raw",
        params={
            "from": encode_dt(from_dt),
            "to": encode_dt(to_dt),
            "page_size": 1,
        },
    )
    resp1.raise_for_status()
    data1 = resp1.json()
    items1 = data1.get("items", [])
    assert len(items1) == 1, "first page must have exactly 1 item"
    first_page_resource_id = str(items1[0].get("resource_id", ""))
    next_cursor = data1.get("page_info", {}).get("next_cursor")
    assert next_cursor, "page_info.next_cursor must be present after page 1"

    # Second page via cursor
    resp2 = await gateway_client.get(
        "/usage-collector/v1/raw",
        params={
            "from": encode_dt(from_dt),
            "to": encode_dt(to_dt),
            "page_size": 1,
            "cursor": next_cursor,
        },
    )
    resp2.raise_for_status()
    data2 = resp2.json()
    items2 = data2.get("items", [])
    assert len(items2) > 0, "second page must be non-empty"
    assert str(items2[0].get("resource_id", "")) != first_page_resource_id, (
        "second page must contain a different record than first page (cursor must advance)"
    )


@pytest.mark.asyncio
async def test_raw_query_ascending_order(gateway_client):
    """Records returned by GET /raw must be in ascending timestamp order."""
    resource_id = str(uuid.uuid4())
    now = datetime.now(timezone.utc)
    ts_early = now - timedelta(seconds=30)
    ts_late = now - timedelta(seconds=10)
    from_dt = now - timedelta(minutes=2)
    to_dt = now + timedelta(minutes=1)

    for ts in (ts_late, ts_early):  # ingest out of order
        resp = await gateway_client.post(
            "/usage-collector/v1/records",
            json={
                "module": MODULE,
                "tenant_id": TENANT_ID,
                "resource_type": RESOURCE_TYPE,
                "resource_id": resource_id,
                "metric": "e2e.gauge",
                "value": 1.0,
                "timestamp": ts.isoformat(),
            },
        )
        assert resp.status_code == 204, f"expected 204, got {resp.status_code}: {resp.text}"

    # Poll until both records are visible before asserting order
    loop = asyncio.get_running_loop()
    deadline = loop.time() + 10.0
    items = []
    while loop.time() < deadline:
        resp = await gateway_client.get(
            "/usage-collector/v1/raw",
            params={
                "from": encode_dt(from_dt),
                "to": encode_dt(to_dt),
                "resource_id": resource_id,
            },
        )
        resp.raise_for_status()
        items = resp.json().get("items", [])
        if len(items) >= 2:
            break
        await asyncio.sleep(0.3)

    assert len(items) == 2, f"expected exactly 2 records, got {len(items)}: {items}"
    timestamps = [item["timestamp"] for item in items]
    assert timestamps == sorted(timestamps), (
        f"expected ascending timestamp order, got: {timestamps}"
    )


@pytest.mark.asyncio
async def test_raw_query_multiple_metrics(gateway_client):
    """Records with different metric names must both appear in GET /raw."""
    resource_id_gauge = str(uuid.uuid4())
    resource_id_counter = str(uuid.uuid4())
    from_dt = datetime.now(timezone.utc)
    to_dt = from_dt + timedelta(minutes=5)

    resp = await gateway_client.post(
        "/usage-collector/v1/records",
        json={
            "module": MODULE,
            "tenant_id": TENANT_ID,
            "resource_type": RESOURCE_TYPE,
            "resource_id": resource_id_gauge,
            "metric": "e2e.gauge",
            "value": 1.0,
            "timestamp": datetime.now(timezone.utc).isoformat(),
        },
    )
    assert resp.status_code == 204, f"expected 204, got {resp.status_code}: {resp.text}"

    resp = await gateway_client.post(
        "/usage-collector/v1/records",
        json={
            "module": MODULE,
            "tenant_id": TENANT_ID,
            "resource_type": RESOURCE_TYPE,
            "resource_id": resource_id_counter,
            "metric": "e2e.counter",
            "value": 1.0,
            "idempotency_key": str(uuid.uuid4()),
            "timestamp": datetime.now(timezone.utc).isoformat(),
        },
    )
    assert resp.status_code == 204, f"expected 204, got {resp.status_code}: {resp.text}"

    await wait_for_record(gateway_client, from_dt, to_dt, resource_id=resource_id_gauge)
    await wait_for_record(gateway_client, from_dt, to_dt, resource_id=resource_id_counter)

    resp = await gateway_client.get(
        "/usage-collector/v1/raw",
        params={"from": encode_dt(from_dt), "to": encode_dt(to_dt)},
    )
    resp.raise_for_status()
    items = resp.json().get("items", [])
    id_to_metric = {str(item.get("resource_id")): item.get("metric") for item in items}
    assert id_to_metric.get(resource_id_gauge) == "e2e.gauge", (
        f"e2e.gauge record not found for resource_id {resource_id_gauge}"
    )
    assert id_to_metric.get(resource_id_counter) == "e2e.counter", (
        f"e2e.counter record not found for resource_id {resource_id_counter}"
    )
