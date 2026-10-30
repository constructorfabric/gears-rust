"""Transport-independent workflows keep their selected route and credentials."""

import httpx
import pytest

from ..helpers import RECEIPT, delete_one, await_deletion, submit_and_poll


@pytest.mark.parametrize(
    ("api_path", "credentials"),
    [
        ("types-registry/v2/platform", {"X-ToolKit-Internal-Token": "internal"}),
        ("types-registry/v2", {"Authorization": "Bearer tenant"}),
    ],
)
@pytest.mark.parametrize("kind", ["registration", "deletion"])
async def test_mutation_and_poll_use_the_supplied_client_and_api(api_path, credentials, kind):
    operation_id = "dc0e1b4a-5228-4c51-890c-c5c3c18c3d8e"
    operation = {"operation_id": operation_id, "kind": kind, "dry_run": True, "status": "completed"}
    requests = []

    def respond(request):
        requests.append(request)
        if request.method == "GET":
            return httpx.Response(200, json=operation)
        return httpx.Response(
            202,
            json={"operation_id": operation_id, "status": "pending", "replayed": False},
            headers={"Location": f"/gateway/{api_path}/operations/{operation_id}"},
        )

    async with httpx.AsyncClient(
        base_url="http://registry/gateway/", headers=credentials,
        transport=httpx.MockTransport(respond),
    ) as client:
        if kind == "registration":
            actual = await submit_and_poll(client, api_path, [], RECEIPT, dry_run=True)
        else:
            response = await delete_one(client, api_path, "entity", 1, dry_run=True)
            actual = await await_deletion(client, response, dry_run=True)

    assert actual == operation
    assert [r.url.path for r in requests] == [
        f"/gateway/{api_path}/entities" + ("/entity" if kind == "deletion" else ""),
        f"/gateway/{api_path}/operations/{operation_id}",
    ]
    for request in requests:
        assert request.headers.get("Authorization") == credentials.get("Authorization")
        assert request.headers.get("X-ToolKit-Internal-Token") == credentials.get("X-ToolKit-Internal-Token")


@pytest.fixture
def api_version(request, monkeypatch):
    monkeypatch.setenv("TYPES_REGISTRY_API_VERSION", request.param)
    return request.param


@pytest.mark.parametrize("api_version", ["v2/platform", "v2", "v1/platform", "v1"], indirect=True)
async def test_fixture_selects_credentials_for_its_api(
    api_version, registry_api_path, registry_http, auth_headers, platform_headers,
):
    assert registry_api_path == f"types-registry/{api_version}"
    expected = platform_headers if api_version.endswith("/platform") else auth_headers
    for name in ("Authorization", "X-ToolKit-Internal-Token"):
        assert registry_http.headers.get(name) == expected.get(name)
