"""E2E tests for per-request secret references (ADR-0022)."""
import uuid

import httpx
import pytest

from . import mock_upstream as mock_module
from .helpers import (
    APIKEY_AUTH_PLUGIN_ID,
    create_route,
    create_upstream,
    unique_alias,
)

SECRET_REF_HEADER = "x-oagw-secret-ref"


def _record_mock_requests(monkeypatch) -> list[dict]:
    """Record the headers of every request the in-process mock upstream
    handles from now on. Returns the live list."""
    seen: list[dict] = []
    original = mock_module._handle

    async def recording_handle(method, target, headers, body, writer, reader=None):
        seen.append(dict(headers))
        await original(method, target, headers, body, writer, reader)

    monkeypatch.setattr(mock_module, "_handle", recording_handle)
    return seen


async def _provision_secret(client: httpx.AsyncClient, base_url: str, headers: dict,
                            reference: str, value: str, sharing: str = "tenant") -> None:
    """Create a secret through the credstore gateway API: tenant-wide by
    default, or ``shared`` with descendant tenants."""
    resp = await client.post(
        f"{base_url}/credstore/v1/secrets",
        headers={**headers, "content-type": "application/json"},
        json={"reference": reference, "value": value, "sharing": sharing},
    )
    # Report only the reference and status, never the body.
    assert resp.status_code in (200, 201, 204), (
        f"could not provision secret {reference!r}: HTTP {resp.status_code}"
    )


async def _delete_secret(client: httpx.AsyncClient, base_url: str, headers: dict,
                         reference: str) -> None:
    await client.delete(
        f"{base_url}/credstore/v1/secrets/{reference}",
        headers={**headers, "If-Match": "*"},
    )


REQUEST_REF_AUTH = {
    "type": APIKEY_AUTH_PLUGIN_ID,
    "sharing": "private",
    "config": {
        "header": "authorization",
        "prefix": "Bearer ",
        "credential_mode": "request_ref",
    },
}


async def _proxy(client: httpx.AsyncClient, base_url: str, headers: dict, alias: str,
                 reference: str) -> httpx.Response:
    return await client.post(
        f"{base_url}/oagw/v1/proxy/{alias}/echo",
        headers={
            **headers,
            "content-type": "application/json",
            SECRET_REF_HEADER: f"cred://{reference}",
        },
        json={"test": True},
    )


def _assert_gateway_problem(resp: httpx.Response, status: int, category: str) -> dict:
    assert resp.status_code == status, f"Expected {status}, got {resp.status_code}: {resp.text[:500]}"
    assert resp.headers.get("x-oagw-error-source") == "gateway"
    problem = resp.json()
    assert category in problem["type"], problem["type"]
    return problem


@pytest.mark.scenario("positive-31.1-inject-requested-secret")
@pytest.mark.scenario("negative-31.5-secret-ref-required")
@pytest.mark.asyncio
async def test_request_ref_injects_the_tenants_key(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
    monkeypatch,
):
    """SREF-REG-002, SREF-REQ-004: a `request_ref` upstream, a tenant secret,
    and a request naming it in `X-OAGW-Secret-Ref`: the provider receives that
    tenant's key and never the header, whatever the header name's case."""
    reference = f"e2e-sref-{uuid.uuid4().hex[:12]}"
    value = f"sk-e2e-sref-{uuid.uuid4().hex[:12]}"
    alias = unique_alias("sref")
    # The mock runs in-process unless E2E_MOCK_UPSTREAM_EXTERNAL is set; only
    # then can the test see which requests reached it.
    seen = _record_mock_requests(monkeypatch) if mock_upstream is not None else None
    auth_config = {
        "type": APIKEY_AUTH_PLUGIN_ID,
        "sharing": "private",
        "config": {
            "header": "authorization",
            "prefix": "Bearer ",
            "credential_mode": "request_ref",
        },
    }

    async with httpx.AsyncClient(timeout=10.0) as client:
        await _provision_secret(client, oagw_base_url, oagw_headers, reference, value)
        try:
            upstream = cleanup.upstream(oagw_headers, await create_upstream(
                client, oagw_base_url, oagw_headers, mock_upstream_url,
                alias=alias, auth=auth_config,
            ))
            await create_route(
                client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
            )

            before = len(seen) if seen is not None else 0
            resp = await client.post(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
                headers={
                    **oagw_headers,
                    "content-type": "application/json",
                    SECRET_REF_HEADER: f"cred://{reference}",
                },
                json={"test": True},
            )
            assert resp.status_code == 200, (
                f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
            )
            echoed = {k.lower(): v for k, v in resp.json()["headers"].items()}
            assert echoed.get("authorization") == f"Bearer {value}"
            assert SECRET_REF_HEADER not in echoed
            if seen is not None:
                # The recorder saw the forwarded request.
                assert len(seen) == before + 1

            # SREF-REQ-004: the header name is case-insensitive. httpx sends
            # these names as written over a real connection, so this is what
            # OAGW actually receives (in-process, `http` lowercases names
            # before any gateway code sees them).
            for name in ("X-OAGW-Secret-Ref", "X-OAGW-SECRET-REF"):
                resp = await client.post(
                    f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
                    headers={
                        **oagw_headers,
                        "content-type": "application/json",
                        name: f"cred://{reference}",
                    },
                    json={"test": True},
                )
                assert resp.status_code == 200, (
                    f"{name}: expected 200, got {resp.status_code}: {resp.text[:500]}"
                )
                echoed = {k.lower(): v for k, v in resp.json()["headers"].items()}
                assert echoed.get("authorization") == f"Bearer {value}", name
                assert SECRET_REF_HEADER not in echoed, name

            # Without the header and with no default: 400 SECRET_REF_REQUIRED.
            before = len(seen) if seen is not None else 0
            resp = await client.post(
                f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo",
                headers={
                    **oagw_headers,
                    "content-type": "application/json",
                },
                json={"test": True},
            )
            assert resp.status_code == 400, resp.text[:500]
            assert resp.headers.get("x-oagw-error-source") == "gateway"
            problem = resp.json()
            assert "invalid_argument" in problem["type"], problem["type"]
            violations = problem["context"]["field_violations"]
            assert violations[0]["field"] == SECRET_REF_HEADER
            assert violations[0]["reason"] == "SECRET_REF_REQUIRED"
            if seen is not None:
                # The provider received no request for this call.
                assert len(seen) == before
        finally:
            await _delete_secret(client, oagw_base_url, oagw_headers, reference)


@pytest.mark.scenario("negative-31.4-header-not-allowed")
@pytest.mark.asyncio
async def test_header_on_a_config_mode_upstream_is_not_allowed(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
    monkeypatch,
):
    """SREF-REQ-001, SREF-ERR-005: the header on an upstream that is not in
    `request_ref` mode returns 400 `SECRET_REF_NOT_ALLOWED` over HTTP, and
    nothing is forwarded."""
    alias = unique_alias("sref-cfg")
    seen = _record_mock_requests(monkeypatch) if mock_upstream is not None else None
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
        )
        before = len(seen) if seen is not None else 0
        resp = await _proxy(client, oagw_base_url, oagw_headers, alias, "any-ref")
        problem = _assert_gateway_problem(resp, 400, "invalid_argument")
        violations = problem["context"]["field_violations"]
        assert violations[0]["field"] == SECRET_REF_HEADER
        assert violations[0]["reason"] == "SECRET_REF_NOT_ALLOWED"
        if seen is not None:
            assert len(seen) == before


@pytest.mark.scenario("negative-31.7-secret-not-accessible")
@pytest.mark.asyncio
async def test_missing_secret_is_not_accessible(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
    monkeypatch,
):
    """SREF-RES-005, SREF-ERR-005: a reference to a secret that does not
    exist returns 403 `SECRET_REF_NOT_ACCESSIBLE` over HTTP, and nothing is
    forwarded."""
    alias = unique_alias("sref-missing")
    seen = _record_mock_requests(monkeypatch) if mock_upstream is not None else None
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias, auth=REQUEST_REF_AUTH,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"], ["POST"], "/echo",
        )
        missing = f"e2e-sref-missing-{uuid.uuid4().hex[:12]}"
        before = len(seen) if seen is not None else 0
        resp = await _proxy(client, oagw_base_url, oagw_headers, alias, missing)
        problem = _assert_gateway_problem(resp, 403, "permission_denied")
        assert problem["context"]["reason"] == "SECRET_REF_NOT_ACCESSIBLE"
        assert missing not in resp.text
        if seen is not None:
            assert len(seen) == before


@pytest.mark.scenario("negative-31.8-owner-outside-chain")
@pytest.mark.asyncio
async def test_secret_owned_above_the_auth_owner_is_denied(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup, monkeypatch,
):
    """SREF-GUARD-002, SREF-ERR-005: the child tenant owns the `request_ref`
    upstream, so a secret the root shares with it is refused with 403
    `SECRET_REF_POLICY_DENIED` over HTTP and nothing is forwarded, while the
    child's own secret is injected."""
    shared_ref = f"e2e-sref-root-{uuid.uuid4().hex[:12]}"
    own_ref = f"e2e-sref-own-{uuid.uuid4().hex[:12]}"
    own_value = f"sk-e2e-own-{uuid.uuid4().hex[:12]}"
    alias = unique_alias("sref-chain")
    seen = _record_mock_requests(monkeypatch) if mock_upstream is not None else None
    async with httpx.AsyncClient(timeout=10.0) as client:
        await _provision_secret(
            client, oagw_base_url, hierarchy_root_headers, shared_ref,
            f"sk-e2e-root-{uuid.uuid4().hex[:12]}", sharing="shared",
        )
        await _provision_secret(client, oagw_base_url, hierarchy_l1a_headers, own_ref, own_value)
        try:
            upstream = cleanup.upstream(hierarchy_l1a_headers, await create_upstream(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias, auth=REQUEST_REF_AUTH,
            ))
            await create_route(
                client, oagw_base_url, hierarchy_l1a_headers, upstream["id"], ["POST"], "/echo",
            )

            before = len(seen) if seen is not None else 0
            resp = await _proxy(client, oagw_base_url, hierarchy_l1a_headers, alias, shared_ref)
            problem = _assert_gateway_problem(resp, 403, "permission_denied")
            assert problem["context"]["reason"] == "SECRET_REF_POLICY_DENIED"
            if seen is not None:
                assert len(seen) == before

            resp = await _proxy(client, oagw_base_url, hierarchy_l1a_headers, alias, own_ref)
            assert resp.status_code == 200, f"Expected 200, got {resp.status_code}: {resp.text[:500]}"
            echoed = {k.lower(): v for k, v in resp.json()["headers"].items()}
            assert echoed.get("authorization") == f"Bearer {own_value}"
        finally:
            await _delete_secret(client, oagw_base_url, hierarchy_l1a_headers, own_ref)
            await _delete_secret(client, oagw_base_url, hierarchy_root_headers, shared_ref)
