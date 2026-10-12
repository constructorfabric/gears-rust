"""E2E tests verifying OAGW GTS types are registered in the types-registry after startup.

`is_schema` and the id format are computed and enforced by types-registry
(and OAGW aborts startup on a bad id), so only presence is OAGW's to test;
the id shape is covered by the `type_catalog.rs` unit tests.
"""
import httpx
import pytest

from .helpers import (
    ALL_OAGW_GTS_IDS,
    OAGW_INSTANCES,
    OAGW_SCHEMAS,
    list_oagw_types,
)


async def _registered(oagw_base_url, oagw_headers) -> dict[str, dict]:
    async with httpx.AsyncClient(timeout=10.0) as client:
        entities = await list_oagw_types(client, oagw_base_url, oagw_headers)
    return {e["gts_id"]: e for e in entities}


@pytest.mark.asyncio
async def test_all_oagw_schemas_registered(oagw_base_url, oagw_headers):
    """After platform startup, all 7 OAGW schemas are registered as schemas."""
    by_id = await _registered(oagw_base_url, oagw_headers)
    missing = set(OAGW_SCHEMAS) - by_id.keys()
    assert not missing, f"schemas not registered: {missing}"
    assert all(by_id[s]["is_schema"] is True for s in OAGW_SCHEMAS)


@pytest.mark.asyncio
async def test_all_oagw_instances_registered(oagw_base_url, oagw_headers):
    """After platform startup, all 14 built-in instances are registered as instances."""
    by_id = await _registered(oagw_base_url, oagw_headers)
    missing = set(OAGW_INSTANCES) - by_id.keys()
    assert not missing, f"instances not registered: {missing}"
    assert all(by_id[i]["is_schema"] is False for i in OAGW_INSTANCES)


@pytest.mark.asyncio
async def test_oagw_entity_count(oagw_base_url, oagw_headers):
    """OAGW registers exactly its 21 catalog entities, nothing extra.

    Only OAGW's own entities are compared: its schemas and `cf.core.oagw.*`
    built-in instances. Instances other gears register against OAGW schemas
    (upstreams, routes) are theirs and are ignored.
    """
    by_id = await _registered(oagw_base_url, oagw_headers)
    own = {
        gts_id for gts_id in by_id
        if gts_id.startswith("gts.cf.core.oagw.")
        and (gts_id.endswith("~") or "~cf.core.oagw." in gts_id)
    }
    assert own == set(ALL_OAGW_GTS_IDS), (
        f"missing: {set(ALL_OAGW_GTS_IDS) - own}, "
        f"unexpected: {own - set(ALL_OAGW_GTS_IDS)}"
    )
