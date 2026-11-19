"""E2E tests for sharing modes across the tenant hierarchy (#4975).

`private` keeps a field with its owner: a descendant's request never gets an
ancestor's private auth, limit, plugins or route settings. `enforce` holds
down the whole chain: no tenant below, and no route, can drop or replace an
enforced plugin or limit, or override an enforced auth or CORS on write.

Tenants (static-tr-plugin, ``config/e2e-local.yaml``)::

    e2e-root          <- oagw_headers
      hierarchy-root  <- hierarchy_root_headers
        hierarchy-l1a <- hierarchy_l1a_headers
"""
import re

import httpx
import pytest

from .helpers import (
    APIKEY_AUTH_PLUGIN_ID,
    GUARD_PLUGIN_SCHEMA,
    NOOP_AUTH_PLUGIN_ID,
    PROXY_SCHEMA,
    REQUEST_ID_TRANSFORM_PLUGIN_ID,
    REQUIRED_HEADERS_GUARD_PLUGIN_ID,
    assert_problem,
    assert_problem_body,
    create_route,
    create_upstream,
    create_upstream_raw,
    err_type,
    traced,
    unique_alias,
    update_upstream_raw,
)

_PRIVATE = "negative-6.6-private-ancestor-configuration-never-reaches-descendant"
_AUTH_MODES = "positive-9.8-hierarchical-auth-sharing-modes-behave-specified"
_ANCESTOR_SHARING = "positive-5.3-upstream-sharing-ancestor-hierarchy"
_ENFORCED_LIMIT = "negative-6.2-enforced-ancestor-limits-still-apply-alias-shadowed"
_MIN_MERGE = "positive-18.6-hierarchical-min-merge-descendant-overrides"
_OVERRIDES = "negative-9.9-descendant-override-permissions-enforced"
_PLUGIN_SHARING = "positive-4.6-plugin-sharing-modes-merge-correctly-across-tenant-hierarchy"

GUARD_HEADER = "x-tenant-guard"
# The apikey secret conftest provisions in the e2e-root tenant.
SECRET_VALUE = "Bearer sk-test-e2e-fake-key"


def _rl(rate: int, sharing: str, scope: str = "tenant") -> dict:
    return {
        "sharing": sharing,
        "algorithm": "token_bucket",
        "sustained": {"rate": rate, "window": "minute"},
        "burst": {"capacity": rate},
        "scope": scope,
        "strategy": "reject",
    }


def _noop_auth(sharing: str) -> dict:
    return {"type": NOOP_AUTH_PLUGIN_ID, "sharing": sharing}


def _cors(sharing: str) -> dict:
    return {
        "sharing": sharing,
        "enabled": True,
        "allowed_origins": ["https://app.example.com"],
        "allowed_methods": ["GET"],
    }


def _guard(sharing: str, required: str = GUARD_HEADER) -> dict:
    return {
        "sharing": sharing,
        "items": [{
            "plugin_ref": REQUIRED_HEADERS_GUARD_PLUGIN_ID,
            "config": {"required_request_headers": required},
        }],
    }


def _request_id(sharing: str) -> dict:
    return {
        "sharing": sharing,
        "items": [{"plugin_ref": REQUEST_ID_TRANSFORM_PLUGIN_ID, "config": {}}],
    }


async def _post_echo(client, base, headers, alias, extra=None) -> httpx.Response:
    return await client.post(
        f"{base}/oagw/v1/proxy/{alias}/echo",
        headers={**headers, "content-type": "application/json", **(extra or {})},
        json={"isolation": True},
    )


async def _get_models(client, base, headers, alias) -> httpx.Response:
    return await client.get(f"{base}/oagw/v1/proxy/{alias}/v1/models", headers=headers)


def _assert_passed(resp: httpx.Response) -> dict:
    assert resp.status_code == 200, f"expected 200, got {resp.status_code}: {resp.text[:500]}"
    assert resp.headers.get("x-oagw-error-source") == "upstream"
    return resp.json()


def _assert_guard_rejected(resp: httpx.Response, alias: str) -> None:
    """The required-headers guard's request rejection, its body compared whole.

    Send the request with :func:`traced` headers.
    """
    assert_problem(resp, 400, category="invalid_argument")
    body = resp.json()
    trace_id = body.pop("trace_id", None)
    assert isinstance(trace_id, str) and re.fullmatch(r"[0-9a-f]{32}", trace_id), resp.text
    assert body == {
        "type": err_type("invalid_argument"),
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": f"/oagw/v1/proxy/{alias}/echo",
        "context": {
            "field_violations": [{
                "field": "guard",
                "reason": "REQUIRED_HEADER_MISSING",
                "description": f"Missing required header: {GUARD_HEADER}",
            }],
            "resource_type": GUARD_PLUGIN_SCHEMA,
        },
    }, resp.text


def _override_refused(field: str) -> dict:
    detail = f"cannot override {field}: ancestor upstream has sharing mode 'enforce'"
    return {
        "type": err_type("invalid_argument"),
        "title": "Invalid Argument",
        "status": 400,
        "detail": detail,
        "instance": "/oagw/v1/upstreams",
        "context": {"format": detail, "resource_type": PROXY_SCHEMA},
    }


# ---------------------------------------------------------------------------
# Private stays with its owner
# ---------------------------------------------------------------------------


@pytest.mark.scenario(_PRIVATE, part="A")
@pytest.mark.scenario(_AUTH_MODES, part="C")
@pytest.mark.asyncio
async def test_private_auth_is_not_sent_for_a_child(
    oagw_base_url, oagw_headers, hierarchy_root_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """The parent's private credential goes out on its own requests only: a
    child gets none, with or without an upstream of its own."""
    _ = mock_upstream
    alias = unique_alias("iso-auth")
    async with httpx.AsyncClient(timeout=10.0) as client:
        parent = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
            auth={
                "type": APIKEY_AUTH_PLUGIN_ID,
                "sharing": "private",
                "config": {
                    "header": "authorization",
                    "prefix": "Bearer ",
                    "secret_ref": "cred://openai-key",
                },
            },
            # Shared, so the child resolves to this upstream.
            rate_limit=_rl(1000, "inherit"),
        ))
        await create_route(client, oagw_base_url, oagw_headers, parent["id"], ["POST"], "/echo")

        echoed = _assert_passed(await _post_echo(client, oagw_base_url, oagw_headers, alias))
        assert echoed["headers"].get("authorization") == SECRET_VALUE

        # No upstream of its own: the alias resolves to the parent's.
        echoed = _assert_passed(
            await _post_echo(client, oagw_base_url, hierarchy_root_headers, alias),
        )
        assert "authorization" not in echoed["headers"], echoed["headers"]

        # Bound without auth of its own.
        cleanup.upstream(hierarchy_root_headers, await create_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, alias=alias,
        ))
        echoed = _assert_passed(
            await _post_echo(client, oagw_base_url, hierarchy_root_headers, alias),
        )
        assert "authorization" not in echoed["headers"], echoed["headers"]


@pytest.mark.scenario(_PRIVATE, part="B")
@pytest.mark.scenario(_ANCESTOR_SHARING)
@pytest.mark.asyncio
async def test_private_ancestor_limit_does_not_limit_a_child(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """The parent's private 1/min limit holds for the parent only."""
    _ = mock_upstream
    alias = unique_alias("iso-rl")
    async with httpx.AsyncClient(timeout=10.0) as client:
        parent = cleanup.upstream(hierarchy_root_headers, await create_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, alias=alias,
            auth=_noop_auth("inherit"), rate_limit=_rl(1, "private"),
        ))
        await create_route(
            client, oagw_base_url, hierarchy_root_headers, parent["id"], ["GET"], "/v1/models",
        )

        statuses = [
            (await _get_models(client, oagw_base_url, h, alias)).status_code
            for h in (hierarchy_l1a_headers, hierarchy_l1a_headers)
        ]
        assert statuses == [200, 200]

        assert (await _get_models(client, oagw_base_url, hierarchy_root_headers, alias)).status_code == 200
        assert_problem(
            await _get_models(client, oagw_base_url, hierarchy_root_headers, alias),
            429, category="resource_exhausted",
        )


@pytest.mark.scenario(_PRIVATE, part="B")
@pytest.mark.asyncio
async def test_child_requests_do_not_reset_a_private_global_limit(
    oagw_base_url, oagw_headers, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Root `inherit` 1000/min, parent `private` 1/min, both global: the child
    resolves the parent's upstream under the root's limit. Its request between
    the parent's two must not reset the parent's bucket."""
    _ = mock_upstream
    alias = unique_alias("iso-rl-global")
    async with httpx.AsyncClient(timeout=10.0) as client:
        cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
            rate_limit=_rl(1000, "inherit", scope="global"),
        ))
        parent = cleanup.upstream(hierarchy_root_headers, await create_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, alias=alias,
            auth=_noop_auth("inherit"), rate_limit=_rl(1, "private", scope="global"),
        ))
        await create_route(
            client, oagw_base_url, hierarchy_root_headers, parent["id"], ["GET"], "/v1/models",
        )

        for headers in (hierarchy_root_headers, hierarchy_l1a_headers):
            resp = await _get_models(client, oagw_base_url, headers, alias)
            assert resp.status_code == 200, resp.text[:500]
        assert_problem(
            await _get_models(client, oagw_base_url, hierarchy_root_headers, alias),
            429, category="resource_exhausted",
        )


@pytest.mark.scenario(_PRIVATE, part="C")
@pytest.mark.scenario(_PLUGIN_SHARING, part="C")
@pytest.mark.asyncio
async def test_private_ancestor_plugins_do_not_apply_to_a_child(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """The parent's private guard checks the parent's requests only, also when
    the child binds the alias with `inherit` plugins of its own."""
    _ = mock_upstream
    alias = unique_alias("iso-plg")
    async with httpx.AsyncClient(timeout=10.0) as client:
        parent = cleanup.upstream(hierarchy_root_headers, await create_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, alias=alias,
            auth=_noop_auth("inherit"), plugins=_guard("private"),
        ))
        await create_route(
            client, oagw_base_url, hierarchy_root_headers, parent["id"], ["POST"], "/echo",
        )

        _assert_guard_rejected(
            await _post_echo(client, oagw_base_url, traced(hierarchy_root_headers), alias), alias,
        )
        _assert_passed(await _post_echo(client, oagw_base_url, hierarchy_l1a_headers, alias))

        cleanup.upstream(hierarchy_l1a_headers, await create_upstream(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, alias=alias,
            plugins=_request_id("inherit"),
        ))
        _assert_passed(await _post_echo(client, oagw_base_url, hierarchy_l1a_headers, alias))


@pytest.mark.scenario(_PRIVATE, part="D")
@pytest.mark.asyncio
async def test_private_settings_of_an_ancestor_route_do_not_apply_to_a_child(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """The parent's route serves the child without its private guard and limit."""
    _ = mock_upstream
    alias = unique_alias("iso-route")
    async with httpx.AsyncClient(timeout=10.0) as client:
        parent = cleanup.upstream(hierarchy_root_headers, await create_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, alias=alias,
            auth=_noop_auth("inherit"),
        ))
        await create_route(
            client, oagw_base_url, hierarchy_root_headers, parent["id"], ["POST"], "/echo",
            plugins=_guard("private"), rate_limit=_rl(1, "private"),
        )

        _assert_guard_rejected(
            await _post_echo(client, oagw_base_url, traced(hierarchy_root_headers), alias), alias,
        )
        for _ in range(2):
            _assert_passed(await _post_echo(client, oagw_base_url, hierarchy_l1a_headers, alias))


# ---------------------------------------------------------------------------
# Enforce holds down the chain
# ---------------------------------------------------------------------------


@pytest.mark.scenario(_ENFORCED_LIMIT, part="B")
@pytest.mark.scenario(_MIN_MERGE)
@pytest.mark.asyncio
async def test_enforced_limit_holds_down_a_chain(
    oagw_base_url, oagw_headers, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Top `enforce` 3/min → middle `inherit` 2/min → leaf `private` 100/min:
    the leaf is held to 2/min.

    Bound bottom-up so no write is refused; the top's `enforce` is applied
    when the request is merged.
    """
    _ = mock_upstream
    alias = unique_alias("iso-chain")
    async with httpx.AsyncClient(timeout=10.0) as client:
        for headers, rate_limit in (
            (hierarchy_l1a_headers, _rl(100, "private")),
            (hierarchy_root_headers, _rl(2, "inherit")),
        ):
            cleanup.upstream(headers, await create_upstream(
                client, oagw_base_url, headers, mock_upstream_url, alias=alias,
                rate_limit=rate_limit,
            ))
        top = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
            rate_limit=_rl(3, "enforce"),
        ))
        await create_route(client, oagw_base_url, oagw_headers, top["id"], ["GET"], "/v1/models")

        for _ in range(2):
            resp = await _get_models(client, oagw_base_url, hierarchy_l1a_headers, alias)
            assert resp.status_code == 200, resp.text[:500]
        assert_problem(
            await _get_models(client, oagw_base_url, hierarchy_l1a_headers, alias),
            429, category="resource_exhausted",
        )


@pytest.mark.scenario(_ENFORCED_LIMIT, part="C")
@pytest.mark.asyncio
async def test_child_route_shadowing_the_parents_keeps_its_enforced_guard(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """The child's own `POST /echo` route wins over the parent's; the parent
    route's enforced guard still checks the child's requests."""
    _ = mock_upstream
    alias = unique_alias("iso-route-shadow")
    async with httpx.AsyncClient(timeout=10.0) as client:
        parent = cleanup.upstream(hierarchy_root_headers, await create_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, alias=alias,
            auth=_noop_auth("inherit"),
        ))
        await create_route(
            client, oagw_base_url, hierarchy_root_headers, parent["id"], ["POST"], "/echo",
            plugins=_guard("enforce"),
        )
        child = cleanup.upstream(hierarchy_l1a_headers, await create_upstream(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, alias=alias,
        ))
        await create_route(
            client, oagw_base_url, hierarchy_l1a_headers, child["id"], ["POST"], "/echo",
            plugins=_request_id("private"),
        )

        leaf = traced(hierarchy_l1a_headers)
        _assert_guard_rejected(await _post_echo(client, oagw_base_url, leaf, alias), alias)
        _assert_passed(await _post_echo(client, oagw_base_url, leaf, alias, {GUARD_HEADER: "1"}))


@pytest.mark.scenario(_ENFORCED_LIMIT, part="C")
@pytest.mark.asyncio
async def test_encoded_separator_cannot_reach_an_enforcing_parent_route(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """As sent, `/echo%2Fadmin` is under the child's `/echo` route; read with
    `/`, it is the parent's `/echo/admin`, whose guard is enforced. Refused
    with 400 `INVALID_PROXY_PATH`; `/echo/x` still reaches the child's route."""
    _ = mock_upstream
    alias = unique_alias("iso-separator")
    async with httpx.AsyncClient(timeout=10.0) as client:
        parent = cleanup.upstream(hierarchy_root_headers, await create_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, alias=alias,
            auth=_noop_auth("inherit"),
        ))
        await create_route(
            client, oagw_base_url, hierarchy_root_headers, parent["id"], ["POST"], "/echo/admin",
            plugins=_guard("enforce"),
        )
        child = cleanup.upstream(hierarchy_l1a_headers, await create_upstream(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, alias=alias,
        ))
        await create_route(
            client, oagw_base_url, hierarchy_l1a_headers, child["id"], ["POST"], "/echo",
        )

        leaf = traced(hierarchy_l1a_headers)
        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo%2Fadmin",
            headers={**leaf, "content-type": "application/json"},
            json={"isolation": True},
        )
        assert_problem(resp, 400, category="invalid_argument")
        body = resp.json()
        trace_id = body.pop("trace_id", None)
        assert isinstance(trace_id, str) and re.fullmatch(r"[0-9a-f]{32}", trace_id), resp.text
        detail = (
            "an encoded `/` or `\\` in the proxy path selects a different route "
            "than the decoded path; send the path decoded"
        )
        assert body == {
            "type": err_type("invalid_argument"),
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": f"/oagw/v1/proxy/{alias}/echo%2Fadmin",
            "context": {
                "field_violations": [{
                    "field": "path",
                    "reason": "INVALID_PROXY_PATH",
                    "description": detail,
                }],
                "resource_type": PROXY_SCHEMA,
            },
        }, resp.text

        resp = await client.post(
            f"{oagw_base_url}/oagw/v1/proxy/{alias}/echo/x",
            headers={**leaf, "content-type": "application/json"},
            json={"isolation": True},
        )
        assert resp.headers.get("x-oagw-error-source") == "upstream", resp.text[:500]


@pytest.mark.scenario(_OVERRIDES, part="D")
@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("auth", _noop_auth("private")),
        ("cors", _cors("private")),
    ],
    ids=["auth", "cors"],
)
async def test_override_refused_under_an_enforcing_grand_ancestor(
    field, value, oagw_base_url, oagw_headers, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """The middle tenant's `inherit` auth and CORS do not lift the top's
    `enforce`: the leaf's override is refused, a plain binding is not."""
    _ = mock_upstream
    alias = unique_alias("iso-bind")
    async with httpx.AsyncClient(timeout=10.0) as client:
        cleanup.upstream(hierarchy_root_headers, await create_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, alias=alias,
            auth=_noop_auth("inherit"), cors=_cors("inherit"),
        ))
        cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
            auth=_noop_auth("enforce"), cors=_cors("enforce"),
        ))

        resp = await create_upstream_raw(
            client, oagw_base_url, traced(hierarchy_l1a_headers), mock_upstream_url,
            alias=alias, **{field: value},
        )
        if resp.status_code == 201:
            cleanup.upstream(hierarchy_l1a_headers, resp.json())
        assert_problem_body(resp, _override_refused(field))

        cleanup.upstream(hierarchy_l1a_headers, await create_upstream(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, alias=alias,
        ))


@pytest.mark.scenario(_OVERRIDES, part="E")
@pytest.mark.asyncio
async def test_allocated_rate_limit_accepted_under_an_enforcing_grand_ancestor(
    oagw_base_url, oagw_headers, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Top `enforce` 2/min → middle `inherit` 500/min with an allocated budget
    of 500 → leaf allocates 100/min. The leaf must set a limit to allocate, so
    the top's `enforce` does not refuse it; create and update are accepted and
    the leaf is still held to the top's 2/min."""
    _ = mock_upstream
    alias = unique_alias("iso-alloc")
    async with httpx.AsyncClient(timeout=10.0) as client:
        mid_limit = {**_rl(500, "inherit"), "budget": {"mode": "allocated", "total": 500}}
        cleanup.upstream(hierarchy_root_headers, await create_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, alias=alias,
            rate_limit=mid_limit,
        ))
        top = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
            rate_limit=_rl(2, "enforce"),
        ))
        await create_route(client, oagw_base_url, oagw_headers, top["id"], ["GET"], "/v1/models")

        resp = await create_upstream_raw(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
            alias=alias, rate_limit=_rl(100, "inherit"),
        )
        assert resp.status_code == 201, resp.text[:500]
        leaf = cleanup.upstream(hierarchy_l1a_headers, resp.json())

        resp = await update_upstream_raw(
            client, oagw_base_url, hierarchy_l1a_headers, leaf["id"], mock_upstream_url,
            alias=alias, rate_limit=_rl(200, "inherit"),
        )
        assert resp.status_code == 200, resp.text[:500]
        assert resp.json()["rate_limit"]["sustained"]["rate"] == 200, resp.text[:500]

        for _ in range(2):
            resp = await _get_models(client, oagw_base_url, hierarchy_l1a_headers, alias)
            assert resp.status_code == 200, resp.text[:500]
        assert_problem(
            await _get_models(client, oagw_base_url, hierarchy_l1a_headers, alias),
            429, category="resource_exhausted",
        )


@pytest.mark.scenario(_PLUGIN_SHARING, part="B")
@pytest.mark.asyncio
async def test_enforced_plugin_cannot_be_replaced_by_the_same_ref(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """A child binding the parent's enforced guard with another config does not
    replace it: the parent's required header is still checked."""
    _ = mock_upstream
    alias = unique_alias("iso-plg-ref")
    async with httpx.AsyncClient(timeout=10.0) as client:
        parent = cleanup.upstream(hierarchy_root_headers, await create_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, alias=alias,
            plugins=_guard("enforce"),
        ))
        await create_route(
            client, oagw_base_url, hierarchy_root_headers, parent["id"], ["POST"], "/echo",
        )
        cleanup.upstream(hierarchy_l1a_headers, await create_upstream(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, alias=alias,
            plugins=_guard("inherit", required="x-child-header"),
        ))

        child = traced(hierarchy_l1a_headers)
        _assert_guard_rejected(
            await _post_echo(client, oagw_base_url, child, alias, {"x-child-header": "1"}),
            alias,
        )
        _assert_passed(
            await _post_echo(client, oagw_base_url, child, alias, {GUARD_HEADER: "1"}),
        )


@pytest.mark.scenario(_PLUGIN_SHARING, part="B")
@pytest.mark.asyncio
async def test_enforced_plugin_survives_a_private_level_below_an_inherit_one(
    oagw_base_url, oagw_headers, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Top `enforce` guard → middle `inherit` plugins → leaf `private` plugins:
    the guard still checks the leaf's requests."""
    _ = mock_upstream
    alias = unique_alias("iso-plg-chain")
    async with httpx.AsyncClient(timeout=10.0) as client:
        top = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
            plugins=_guard("enforce"),
        ))
        await create_route(client, oagw_base_url, oagw_headers, top["id"], ["POST"], "/echo")
        for headers, plugins in (
            (hierarchy_root_headers, _request_id("inherit")),
            (hierarchy_l1a_headers, _request_id("private")),
        ):
            cleanup.upstream(headers, await create_upstream(
                client, oagw_base_url, headers, mock_upstream_url, alias=alias, plugins=plugins,
            ))

        leaf = traced(hierarchy_l1a_headers)
        _assert_guard_rejected(await _post_echo(client, oagw_base_url, leaf, alias), alias)
        _assert_passed(await _post_echo(client, oagw_base_url, leaf, alias, {GUARD_HEADER: "1"}))


@pytest.mark.scenario(_PLUGIN_SHARING, part="D")
@pytest.mark.asyncio
async def test_private_route_keeps_enforced_upstream_plugins(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """A child's route with `private` plugins keeps the parent's enforced guard."""
    _ = mock_upstream
    alias = unique_alias("iso-plg-route")
    async with httpx.AsyncClient(timeout=10.0) as client:
        cleanup.upstream(hierarchy_root_headers, await create_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, alias=alias,
            plugins=_guard("enforce"),
        ))
        child = cleanup.upstream(hierarchy_l1a_headers, await create_upstream(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, alias=alias,
        ))
        await create_route(
            client, oagw_base_url, hierarchy_l1a_headers, child["id"], ["POST"], "/echo",
            plugins=_request_id("private"),
        )

        leaf = traced(hierarchy_l1a_headers)
        _assert_guard_rejected(await _post_echo(client, oagw_base_url, leaf, alias), alias)
        _assert_passed(await _post_echo(client, oagw_base_url, leaf, alias, {GUARD_HEADER: "1"}))


@pytest.mark.scenario(_PLUGIN_SHARING, part="E")
@pytest.mark.asyncio
async def test_child_enforcing_the_same_plugin_keeps_the_parent_routes_guard(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """The child enforces the parent route's guard `plugin_ref` with an empty
    config on its own upstream: the parent route's guard still checks the
    child's requests, on the parent's route and on the child's own route
    shadowing it."""
    _ = mock_upstream
    alias = unique_alias("iso-plg-collide")
    async with httpx.AsyncClient(timeout=10.0) as client:
        parent = cleanup.upstream(hierarchy_root_headers, await create_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, alias=alias,
            auth=_noop_auth("inherit"),
        ))
        await create_route(
            client, oagw_base_url, hierarchy_root_headers, parent["id"], ["POST"], "/echo",
            plugins=_guard("enforce"),
        )
        child = cleanup.upstream(hierarchy_l1a_headers, await create_upstream(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, alias=alias,
            plugins={
                "sharing": "enforce",
                "items": [{"plugin_ref": REQUIRED_HEADERS_GUARD_PLUGIN_ID, "config": {}}],
            },
        ))

        leaf = traced(hierarchy_l1a_headers)
        # The parent's route is matched.
        _assert_guard_rejected(await _post_echo(client, oagw_base_url, leaf, alias), alias)
        _assert_passed(await _post_echo(client, oagw_base_url, leaf, alias, {GUARD_HEADER: "1"}))

        # The child's own route shadows it.
        await create_route(
            client, oagw_base_url, hierarchy_l1a_headers, child["id"], ["POST"], "/echo",
            plugins=_request_id("private"),
        )
        _assert_guard_rejected(await _post_echo(client, oagw_base_url, leaf, alias), alias)
        _assert_passed(await _post_echo(client, oagw_base_url, leaf, alias, {GUARD_HEADER: "1"}))
