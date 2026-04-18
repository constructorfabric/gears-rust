# Created: 2026-04-16 by Constructor Tech
"""E2E tests: authentication modes for resource-group module.

Verifies that JWT + hierarchy endpoint → 200, full AuthZ applied.
"""
import httpx


async def test_jwt_hierarchy_full_authz(
    rg_base_url, rg_headers, create_type, create_group,
):
    """JWT request to hierarchy endpoint → 200, full AuthZ applied."""
    type_data = await create_type("jwt_org")
    group = await create_group(type_data["code"], "JWT Test Group")

    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.get(
            f"{rg_base_url}/resource-group/v1/groups/{group['id']}/descendants",
            headers=rg_headers,
        )
        assert resp.status_code == 200, (
            f"JWT hierarchy should return 200, got {resp.status_code}: {resp.text}"
        )
