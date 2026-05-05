"""E2e ingestion tests for the usage-collector module.

Covers the local ingest path: direct POST to gateway and idempotency deduplication
via the timescaledb plugin.
"""

from __future__ import annotations

import uuid
from datetime import datetime, timedelta, timezone

import pytest

from .helpers import encode_dt, wait_for_record


MODULE = "e2e-test"
RESOURCE_TYPE = "e2e.resource"
TENANT_ID = "00000000-df51-5b42-9538-d2b56b7ee953"


@pytest.mark.asyncio
async def test_local_ingest(gateway_client):
    """POST directly to gateway; record must appear in GET /raw on the gateway."""
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
    assert str(record.get("resource_id")) == resource_id, (
        f"returned record resource_id mismatch: {record}"
    )


@pytest.mark.asyncio
async def test_local_ingest_idempotency(gateway_client):
    """Two POSTs with the same idempotency_key to gateway must yield exactly one record."""
    resource_id = str(uuid.uuid4())
    idempotency_key = str(uuid.uuid4())
    from_dt = datetime.now(timezone.utc)
    to_dt = from_dt + timedelta(minutes=5)

    payload = {
        "module": MODULE,
        "tenant_id": TENANT_ID,
        "resource_type": RESOURCE_TYPE,
        "resource_id": resource_id,
        "metric": "e2e.counter",
        "value": 1.0,
        "idempotency_key": idempotency_key,
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }

    resp1 = await gateway_client.post("/usage-collector/v1/records", json=payload)
    assert resp1.status_code == 204, f"first POST expected 204, got {resp1.status_code}: {resp1.text}"

    resp2 = await gateway_client.post("/usage-collector/v1/records", json=payload)
    assert resp2.status_code == 204, f"second POST expected 204, got {resp2.status_code}: {resp2.text}"

    await wait_for_record(gateway_client, from_dt, to_dt, resource_id=resource_id)

    raw_resp = await gateway_client.get(
        "/usage-collector/v1/raw",
        params={"from": encode_dt(from_dt), "to": encode_dt(to_dt)},
    )
    raw_resp.raise_for_status()
    matching = [
        item for item in raw_resp.json().get("items", [])
        if str(item.get("resource_id")) == resource_id
    ]
    assert len(matching) == 1, (
        f"expected exactly 1 deduplicated record for resource_id {resource_id}, got {len(matching)}"
    )


