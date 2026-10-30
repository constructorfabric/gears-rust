"""One plane per route through the real api-gateway (D17, D20)."""

import uuid

import httpx
import pytest

from .helpers import PLATFORM_API, TRACEPARENT, get_entity


TENANT_API = "types-registry/v2"


@pytest.fixture
def registry_api_path():
    """Plane checks always prepare data through the fixed platform API."""
    return PLATFORM_API


@pytest.fixture
async def registry_tenant_http(base_url, auth_headers):
    """A bearer-only client used alongside the platform client in these checks."""
    async with httpx.AsyncClient(
        base_url=base_url.rstrip("/") + "/",
        headers={**auth_headers, "traceparent": TRACEPARENT},
        timeout=3.0,
    ) as client:
        yield client


def _reason(response):
    """The machine-readable reason of a canonical `401` Problem."""
    assert response.status_code == 401, response.text
    assert response.headers["content-type"].startswith("application/problem+json")
    return response.json()["context"]["reason"]


async def _send(base_url, method, path, headers, **kwargs):
    """A request carrying exactly `headers` as credentials, none from a fixture client."""
    async with httpx.AsyncClient(base_url=base_url, timeout=3.0) as bare:
        return await bare.request(
            method, path, headers={"traceparent": TRACEPARENT, **headers}, **kwargs
        )


@pytest.mark.scenario("TR-PLANE-001")
async def test_a_platform_route_serves_the_internal_token_and_refuses_a_bearer(
    registry_http, auth_headers, platform_headers, registration_fixture
):
    document = registration_fixture("person_schema")
    body = {"items": [document], "dry_run": True}
    forged = {"X-ToolKit-Internal-Token": f"forged-{uuid.uuid4()}"}
    for credentials, expected in (
        ({}, "MISSING_INTERNAL_TOKEN"),
        # The bearer is dropped unvalidated, so the token is what is missing.
        (auth_headers, "MISSING_INTERNAL_TOKEN"),
        (forged, "INTERNAL_AUTH_FAILED"),
    ):
        response = await _send(
            registry_http.base_url,
            "POST",
            f"{PLATFORM_API}/entities",
            {"Idempotency-Key": str(uuid.uuid4()), **credentials},
            json=body,
        )
        assert _reason(response) == expected, sorted(credentials)

    accepted = await _send(
        registry_http.base_url,
        "POST",
        f"{PLATFORM_API}/entities",
        {"Idempotency-Key": str(uuid.uuid4()), **platform_headers},
        json=body,
    )
    assert accepted.status_code == 202, accepted.text


@pytest.mark.scenario("TR-PLANE-002")
async def test_both_credentials_are_refused_on_either_route_set(
    registry_http,
    auth_headers,
    platform_headers,
    registration_fixture,
    given_registered,
):
    document = registration_fixture("person_schema")
    await given_registered(document)
    for path in (
        f"{PLATFORM_API}/entities/{document['gts_id']}",
        f"{TENANT_API}/entities/{document['gts_id']}",
    ):
        response = await _send(
            registry_http.base_url, "GET", path, {**auth_headers, **platform_headers}
        )
        assert _reason(response) == "MIXED_CREDENTIALS", path


@pytest.mark.scenario("TR-PLANE-003")
async def test_a_tenant_reads_with_its_bearer_and_cannot_mutate(
    registry_http,
    registry_tenant_http,
    auth_headers,
    platform_headers,
    registration_fixture,
    given_registered,
):
    document = registration_fixture("person_schema")
    await given_registered(document)

    tenant = await get_entity(registry_tenant_http, TENANT_API, document["gts_id"])
    platform = await get_entity(registry_http, PLATFORM_API, document["gts_id"])
    assert tenant.status_code == 200, tenant.text
    assert (tenant.json(), tenant.headers["etag"]) == (
        platform.json(),
        platform.headers["etag"],
    )

    token_only = await _send(
        registry_http.base_url,
        "GET",
        f"{TENANT_API}/entities/{document['gts_id']}",
        platform_headers,
    )
    assert token_only.status_code == 401, token_only.text

    write = await _send(
        registry_http.base_url,
        "POST",
        f"{TENANT_API}/entities",
        {"Idempotency-Key": str(uuid.uuid4()), **auth_headers},
        json={"items": [document]},
    )
    assert write.status_code in (404, 405), write.text
    after = await get_entity(
        registry_tenant_http, TENANT_API, document["gts_id"], select="origin"
    )
    assert after.json()["origin"]["resource_version"] == 1, after.text


@pytest.mark.scenario("TR-PLANE-004")
async def test_the_full_auth_matrix_on_every_route(
    registry_http,
    auth_headers,
    platform_headers,
    registration_fixture,
    given_registered,
):
    document = registration_fixture("person_schema")
    await given_registered(document)
    gts_id = document["gts_id"]
    dry_run = {"items": [document], "dry_run": True}
    read = {"items": [{"entity_key": gts_id}]}
    operation = "00000000-0000-0000-0000-000000000001"
    platform_routes = (
        ("POST", f"{PLATFORM_API}/entities", dry_run),
        (
            "POST",
            f"{PLATFORM_API}/entities:batchDelete",
            {
                "items": [{"entity_key": gts_id, "expected_resource_version": 1}],
                "dry_run": True,
            },
        ),
        (
            "DELETE",
            f"{PLATFORM_API}/entities/{gts_id}?expected_resource_version=1&dry_run=true",
            None,
        ),
        ("GET", f"{PLATFORM_API}/operations/{operation}", None),
        ("GET", f"{PLATFORM_API}/entities/{gts_id}", None),
        ("POST", f"{PLATFORM_API}/entities:batchGet", read),
        ("GET", f"{PLATFORM_API}/entities", None),
    )
    tenant_routes = (
        ("GET", f"{TENANT_API}/entities/{gts_id}", None),
        ("POST", f"{TENANT_API}/entities:batchGet", read),
        ("GET", f"{TENANT_API}/entities", None),
    )
    forged_token = {"X-ToolKit-Internal-Token": f"forged-{uuid.uuid4()}"}
    forged_bearer = {"Authorization": f"Bearer forged-{uuid.uuid4()}"}
    # `None` is served: the handler answers, whatever the answer.
    platform_rows = (
        ({}, "MISSING_INTERNAL_TOKEN"),
        (platform_headers, None),
        (forged_token, "INTERNAL_AUTH_FAILED"),
        # A bearer alone is dropped unvalidated, so the token is what is missing.
        (auth_headers, "MISSING_INTERNAL_TOKEN"),
        (forged_bearer, "MISSING_INTERNAL_TOKEN"),
    )
    tenant_rows = (
        ({}, "MISSING_BEARER"),
        (auth_headers, None),
        (forged_bearer, "AUTHN_FAILED"),
        # A token alone is dropped unvalidated, so the bearer is what is missing.
        (platform_headers, "MISSING_BEARER"),
        (forged_token, "MISSING_BEARER"),
    )
    both = [
        {**bearer, **token}
        for bearer in (auth_headers, forged_bearer)
        for token in (platform_headers, forged_token)
    ]

    async def answer(method, path, body, credentials):
        return await _send(
            registry_http.base_url,
            method,
            path,
            {"Idempotency-Key": str(uuid.uuid4()), **credentials},
            json=body,
        )

    for routes, rows in ((platform_routes, platform_rows), (tenant_routes, tenant_rows)):
        for method, path, body in routes:
            for credentials, reason in rows:
                response = await answer(method, path, body, credentials)
                where = f"{method} {path} with {sorted(credentials)}"
                if reason is None:
                    assert response.status_code in (200, 202, 404), (where, response.text)
                else:
                    assert _reason(response) == reason, where
            for credentials in both:
                response = await answer(method, path, body, credentials)
                assert _reason(response) == "MIXED_CREDENTIALS", (method, path)
