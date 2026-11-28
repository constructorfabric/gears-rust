"""E2E tests verifying OAGW publishes its GTS catalog to the types registry.

The runtime publishes OAGW's declarations before its `init` and the registry
admits them; only presence and kind are OAGW's to test here. The declarations
themselves are covered by the `type_catalog` unit tests.
"""
import pytest

from .helpers import (
    ALL_OAGW_GTS_IDS,
    OAGW_INSTANCES,
    OAGW_SCHEMAS,
    list_oagw_types,
)


async def _registered(oagw_base_url, oagw_headers) -> dict[str, dict]:
    _ = oagw_headers  # the platform plane authenticates with the internal token
    return {e["gts_id"]: e for e in await list_oagw_types(oagw_base_url)}


@pytest.mark.asyncio
async def test_all_oagw_schemas_registered(oagw_base_url, oagw_headers):
    """After platform startup, all 7 OAGW schemas are registered as schemas."""
    by_id = await _registered(oagw_base_url, oagw_headers)
    missing = set(OAGW_SCHEMAS) - by_id.keys()
    assert not missing, f"schemas not registered: {missing}"
    assert all(by_id[s]["kind"] == "type_schema" for s in OAGW_SCHEMAS)


@pytest.mark.asyncio
async def test_all_oagw_instances_registered(oagw_base_url, oagw_headers):
    """After platform startup, all 15 built-in instances are registered as instances."""
    by_id = await _registered(oagw_base_url, oagw_headers)
    missing = set(OAGW_INSTANCES) - by_id.keys()
    assert not missing, f"instances not registered: {missing}"
    assert all(by_id[i]["kind"] == "instance" for i in OAGW_INSTANCES)


@pytest.mark.asyncio
async def test_oagw_entity_count(oagw_base_url, oagw_headers):
    """OAGW registers exactly its 22 catalog entities, nothing extra.

    Only OAGW's own entities are compared: its schemas and `cf.core.oagw.*`
    built-in instances. Instances others publish against OAGW schemas
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
