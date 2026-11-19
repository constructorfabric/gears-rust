"""Schema revisions in Types Registry govern writes to existing Resource Groups."""
from copy import deepcopy
from uuid import uuid4

import httpx
import pytest

from .conftest import (
    REQUEST_TIMEOUT,
    membership_schema,
    membership_schema_id,
    rg_type_schema,
    unique_type_code,
)
from ..types_registry.helpers import PLATFORM_API, RECEIPT, submit_and_poll


async def register_schema(registry, code, schema, *, version=None, refusal_reasons=()):
    candidate = {"gts_id": code, "content": schema}
    if version is not None:
        candidate["expected_resource_version"] = version
    operation = await submit_and_poll(registry, PLATFORM_API, [candidate], RECEIPT)
    [item] = operation["items"]
    assert item["gts_id"] == code, item
    if refusal_reasons:
        assert item["status"] == "failed", item
        assert item["error"]["reason"] in refusal_reasons, item
    else:
        assert item["status"] == "succeeded", item
        assert item["resource_version"] == (version or 0) + 1, item


@pytest.fixture
async def registry(rg_base_url, platform_headers):
    async with httpx.AsyncClient(
        base_url=rg_base_url.rstrip("/") + "/", headers=platform_headers, timeout=REQUEST_TIMEOUT,
    ) as client:
        yield client


async def test_existing_group_can_move_after_placement_schema_widens(
    registry, rg_base_url, rg_headers, create_type, create_group,
):
    department = await create_type("department")
    team = unique_type_code(f"team{uuid4().hex}")
    members = membership_schema_id(team)
    await register_schema(registry, members, membership_schema(team, {"const": department}))
    original = rg_type_schema(team, {"anyOf": [
        {"type": "null"}, {"type": "string", "x-gts-type-ref": team},
    ]})
    await register_schema(registry, team, original)

    parent = await create_group(department, "Engineering")
    group = await create_group(team, "Platform")
    path = f"{rg_base_url}/resource-group/v1/groups/{group['id']}"
    move = {"name": group["name"], "parent_id": parent["id"]}
    async with httpx.AsyncClient(headers=rg_headers, timeout=REQUEST_TIMEOUT) as rg:
        denied = await rg.put(path, json=move)
        assert denied.status_code == 400, denied.text

        widened = deepcopy(original)
        widened["allOf"][1]["properties"]["parent_type"]["anyOf"].append(
            {"type": "string", "x-gts-type-ref": department}
        )
        await register_schema(registry, team, widened, version=1)
        await register_schema(
            registry, team, original, version=2,
            refusal_reasons=("incompatible_with_baseline", "compatibility_undecidable"),
        )

        changed_binding = deepcopy(widened)
        changed_binding["allOf"][1]["properties"]["membership_type"] = {
            "type": "string", "const": membership_schema_id(department),
        }
        await register_schema(
            registry, team, changed_binding, version=2, refusal_reasons=("incompatible_with_baseline",),
        )

        moved = await rg.put(path, json=move)
        assert moved.status_code == 200, moved.text
        assert moved.json()["hierarchy"]["parent_id"] == parent["id"]

        del widened["allOf"][1]["properties"]["membership_type"]
        await register_schema(registry, team, widened, version=2)
        new_group = await rg.post(
            f"{rg_base_url}/resource-group/v1/groups", json={"type": team, "name": "Unbound"},
        )
        assert new_group.status_code == 400, new_group.text
        existing = await rg.post(
            f"{rg_base_url}/resource-group/v1/memberships/{group['id']}/{department}/member-1",
        )
        assert existing.status_code == 201, existing.text


async def test_existing_group_uses_widened_membership_schema(
    registry, rg_base_url, rg_headers, create_type, create_group,
):
    user_type = await create_type("member")
    dept_type = await create_type("dept")
    team = unique_type_code(f"team{uuid4().hex}")
    members = membership_schema_id(team)
    original = membership_schema(team, {"type": "string", "const": user_type})
    await register_schema(registry, members, original)
    await register_schema(registry, team, rg_type_schema(team))
    group = await create_group(team, "Platform")
    links = f"{rg_base_url}/resource-group/v1/memberships/{group['id']}"

    async with httpx.AsyncClient(headers=rg_headers, timeout=REQUEST_TIMEOUT) as rg:
        added = await rg.post(f"{links}/{user_type}/user-42")
        assert added.status_code == 201, added.text
        denied = await rg.post(f"{links}/{dept_type}/department-42")
        assert denied.status_code == 400, denied.text

        user_family = deepcopy(original)
        user_family["allOf"][1]["properties"]["resource_type"] = {
            "anyOf": [{"type": "string", "x-gts-type-ref": user_type}],
        }
        await register_schema(registry, members, user_family, version=1)
        widened = deepcopy(user_family)
        widened["allOf"][1]["properties"]["resource_type"]["anyOf"].append(
            {"type": "string", "x-gts-type-ref": dept_type}
        )
        await register_schema(registry, members, widened, version=2)
        await register_schema(
            registry, members, user_family, version=3,
            refusal_reasons=("incompatible_with_baseline", "compatibility_undecidable"),
        )

        added = await rg.post(f"{links}/{dept_type}/department-42")
        assert added.status_code == 201, added.text
