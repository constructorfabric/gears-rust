"""Shared helpers for usage-collector e2e tests."""

from __future__ import annotations

import asyncio
from datetime import datetime
from typing import Any


def encode_dt(dt: datetime) -> str:
    """Return an RFC-3339 / ISO-8601 string usable as a URL query parameter.

    Ensures the datetime has timezone info; naive datetimes are assumed UTC.
    """
    if dt.tzinfo is None:
        from datetime import timezone
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.isoformat()


async def wait_for_record(
    client,
    from_dt: datetime,
    to_dt: datetime,
    *,
    resource_id: str | None = None,
    timeout: float = 10.0,
    interval: float = 0.3,
) -> dict[str, Any]:
    """Poll GET /usage-collector/v1/raw until a matching record appears.

    Args:
        client: httpx.AsyncClient configured with the server base URL.
        from_dt: Start of the time range (inclusive).
        to_dt: End of the time range (inclusive).
        resource_id: Optional resource_id to filter by.
        timeout: Maximum seconds to wait before raising TimeoutError.
        interval: Seconds between polling attempts.

    Returns:
        The first matching record dict from the response items.

    Raises:
        TimeoutError: If no matching record appears within *timeout* seconds.
    """
    params: dict[str, str] = {
        "from": encode_dt(from_dt),
        "to": encode_dt(to_dt),
    }
    if resource_id is not None:
        params["resource_id"] = str(resource_id)

    loop = asyncio.get_running_loop()
    deadline = loop.time() + timeout

    while loop.time() < deadline:
        resp = await client.get("/usage-collector/v1/raw", params=params)
        resp.raise_for_status()
        data = resp.json()
        items = data.get("items", [])

        for item in items:
            if resource_id is None or str(item.get("resource_id")) == str(resource_id):
                return item

        await asyncio.sleep(interval)

    raise TimeoutError(
        f"No matching record appeared within {timeout}s "
        f"(from={encode_dt(from_dt)}, to={encode_dt(to_dt)}, resource_id={resource_id})"
    )


async def wait_for_n_records(
    client,
    from_dt: datetime,
    to_dt: datetime,
    n: int,
    *,
    resource_id: str | None = None,
    timeout: float = 10.0,
    interval: float = 0.3,
) -> list[dict[str, Any]]:
    """Poll GET /usage-collector/v1/raw until at least *n* matching records appear.

    Args:
        client: httpx.AsyncClient configured with the server base URL.
        from_dt: Start of the time range (inclusive).
        to_dt: End of the time range (inclusive).
        n: Minimum number of records to wait for.
        resource_id: Optional resource_id to filter by (sent as a server-side filter).
        timeout: Maximum seconds to wait before raising TimeoutError.
        interval: Seconds between polling attempts.

    Returns:
        The list of matching record dicts once at least *n* are present.

    Raises:
        TimeoutError: If fewer than *n* matching records appear within *timeout* seconds.
    """
    params: dict[str, str] = {
        "from": encode_dt(from_dt),
        "to": encode_dt(to_dt),
    }
    if resource_id is not None:
        params["resource_id"] = str(resource_id)

    loop = asyncio.get_running_loop()
    deadline = loop.time() + timeout

    while loop.time() < deadline:
        resp = await client.get("/usage-collector/v1/raw", params=params)
        resp.raise_for_status()
        data = resp.json()
        items = data.get("items", [])
        if resource_id is not None:
            items = [i for i in items if str(i.get("resource_id")) == str(resource_id)]
        if len(items) >= n:
            return items
        await asyncio.sleep(interval)

    raise TimeoutError(
        f"Expected at least {n} records but found fewer within {timeout}s "
        f"(from={encode_dt(from_dt)}, to={encode_dt(to_dt)}, resource_id={resource_id})"
    )
