"""E2E tests verifying OAGW GTS types are registered in the types-registry after startup."""
import httpx
import pytest

from .helpers import (
    ALL_OAGW_GTS_IDS,
    OAGW_INSTANCES,
    OAGW_SCHEMAS,
    list_oagw_types,
)


@pytest.mark.asyncio
async def test_all_oagw_schemas_registered(oagw_base_url, oagw_headers):
    """After platform startup, all 7 OAGW schemas should be present in types-registry."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        entities = await list_oagw_types(client, oagw_base_url, oagw_headers)
        registered_ids = {e["gts_id"] for e in entities}

        for schema_id in OAGW_SCHEMAS:
            assert schema_id in registered_ids, (
                f"Schema not registered: {schema_id}"
            )


@pytest.mark.asyncio
async def test_all_oagw_instances_registered(oagw_base_url, oagw_headers):
    """After platform startup, all 14 builtin instances should be present in types-registry."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        entities = await list_oagw_types(client, oagw_base_url, oagw_headers)
        registered_ids = {e["gts_id"] for e in entities}

        for instance_id in OAGW_INSTANCES:
            assert instance_id in registered_ids, (
                f"Instance not registered: {instance_id}"
            )


@pytest.mark.asyncio
async def test_oagw_entity_count(oagw_base_url, oagw_headers):
    """Types-registry should contain at least 21 OAGW entities (7 schemas + 14 instances)."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        entities = await list_oagw_types(client, oagw_base_url, oagw_headers)
        registered_ids = {e["gts_id"] for e in entities}

        oagw_ids = registered_ids & set(ALL_OAGW_GTS_IDS)
        assert len(oagw_ids) == 21, (
            f"Expected 21 OAGW entities, found {len(oagw_ids)}. "
            f"Missing: {set(ALL_OAGW_GTS_IDS) - registered_ids}"
        )


@pytest.mark.asyncio
async def test_schemas_have_is_schema_true(oagw_base_url, oagw_headers):
    """Schema entities should have is_schema=true."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        entities = await list_oagw_types(client, oagw_base_url, oagw_headers)
        schema_ids = set(OAGW_SCHEMAS)

        for entity in entities:
            if entity["gts_id"] in schema_ids:
                assert entity["is_schema"] is True, (
                    f"Schema should have is_schema=true: {entity['gts_id']}"
                )


@pytest.mark.asyncio
async def test_instances_have_is_schema_false(oagw_base_url, oagw_headers):
    """Instance entities should have is_schema=false."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        entities = await list_oagw_types(client, oagw_base_url, oagw_headers)
        instance_ids = set(OAGW_INSTANCES)

        for entity in entities:
            if entity["gts_id"] in instance_ids:
                assert entity["is_schema"] is False, (
                    f"Instance should have is_schema=false: {entity['gts_id']}"
                )


@pytest.mark.asyncio
async def test_gts_ids_have_valid_format(oagw_base_url, oagw_headers):
    """All OAGW entities should have valid 5-segment GTS identifiers."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        entities = await list_oagw_types(client, oagw_base_url, oagw_headers)

        for entity in entities:
            gts_id = entity["gts_id"]
            assert gts_id.startswith("gts."), f"GTS ID should start with 'gts.': {gts_id}"

            # Schema IDs end with ~, instance IDs have schema~instance
            if entity["is_schema"]:
                assert gts_id.endswith("~"), f"Schema should end with '~': {gts_id}"
                # 5 segments after 'gts.': vendor.package.namespace.name.version~
                segments = gts_id[4:].rstrip("~").split(".")
                assert len(segments) == 5, (
                    f"Schema should have 5 segments, got {len(segments)}: {gts_id}"
                )
            else:
                assert "~" in gts_id, f"Instance should contain '~': {gts_id}"
                schema_part, instance_part = gts_id.split("~", 1)
                # Schema portion: 5 segments after 'gts.'
                schema_segments = schema_part[4:].split(".")
                assert len(schema_segments) == 5, (
                    f"Schema portion should have 5 segments: {gts_id}"
                )
                # Instance portion: 5 segments
                instance_segments = instance_part.split(".")
                assert len(instance_segments) == 5, (
                    f"Instance portion should have 5 segments: {gts_id}"
                )
