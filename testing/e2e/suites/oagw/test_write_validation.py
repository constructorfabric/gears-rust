"""E2E tests for write-time validation of upstreams and routes (#4974).

Values the gateway could never serve are rejected with 400 on create and
update, and nothing is stored. HTTP-only upstreams are not covered here: the
suite runs with `allow_http_upstream` on for its mock upstream.
"""
import httpx
import pytest

from .helpers import (
    APIKEY_AUTH_PLUGIN_ID,
    AWS_SIGV4_AUTH_PLUGIN_ID,
    BASIC_AUTH_PLUGIN_ID,
    LOGGING_TRANSFORM_PLUGIN_ID,
    NOOP_AUTH_PLUGIN_ID,
    OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID,
    OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID,
    PROXY_SCHEMA,
    REQUEST_ID_TRANSFORM_PLUGIN_ID,
    assert_problem,
    assert_problem_body,
    create_route_raw,
    create_upstream,
    create_upstream_raw,
    err_type,
    list_all,
    traced,
    unique_alias,
    update_upstream_raw,
)

_SCENARIO = "negative-2.15-write-validation-rejected-400"
_SECRET_ACCESS = "negative-9.7-secret-access-control-cred-store"
_CUSTOM_GUARD = "gts.cf.core.oagw.guard_plugin.v1~550e8400-e29b-41d4-a716-446655440000"
_UNKNOWN_TRANSFORM = "gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.nonexistent.v1"
_IMPLEMENTED_AUTH = [
    NOOP_AUTH_PLUGIN_ID,
    APIKEY_AUTH_PLUGIN_ID,
    OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID,
    OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID,
    AWS_SIGV4_AUTH_PLUGIN_ID,
]


def _rl(rate: int = 10, capacity: int = 10, **extra) -> dict:
    return {
        "algorithm": "token_bucket",
        "sustained": {"rate": rate, "window": "minute"},
        "burst": {"capacity": capacity},
        "scope": "tenant",
        "strategy": "reject",
        **extra,
    }


_COST_OVER_BURST = "rate_limit.cost (6) must not exceed rate_limit.burst.capacity (5)"


def _plugins(plugin_ref: str) -> dict:
    return {"items": [{"plugin_ref": plugin_ref}]}


def _format(detail: str) -> dict:
    """A rule without a field: `detail` and `context.format` carry its text."""
    return {"format": detail}


def _violation(field: str, reason: str, text: str) -> dict:
    """One field violation on `field` with `reason`, whose description contains `text`."""
    return {"field": field, "reason": reason, "text": text}


def _assert_rejected(resp: httpx.Response, expected: dict) -> None:
    """A 400 with exactly the expected cause and nothing else.

    Descriptions of field violations quote ids and the plugin lists, so only
    their text is matched here; `test_rejection_bodies_are_complete` compares
    one body of each shape whole.
    """
    body = assert_problem(resp, 400, esrc=None, category="invalid_argument")
    assert body["title"] == "Invalid Argument", body
    assert body.get("instance"), body
    context = body["context"]
    if "format" in expected:
        assert body["detail"] == expected["format"], body
        assert context == {"format": expected["format"], "resource_type": PROXY_SCHEMA}, body
    else:
        assert body["detail"] == "Request validation failed", body
        assert set(context) == {"field_violations", "resource_type"}, body
        assert context["resource_type"] == PROXY_SCHEMA, body
        violations = context["field_violations"]
        assert len(violations) == 1, body
        violation = violations[0]
        assert violation["field"] == expected["field"], body
        assert violation["reason"] == expected["reason"], body
        assert expected["text"] in violation["description"], body


def _case(case_id, part, body, expected):
    return pytest.param(
        body, expected, id=case_id, marks=pytest.mark.scenario(_SCENARIO, part=part),
    )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("body", "expected"),
    [
        _case(
            "port-0", "A",
            {"server": {"endpoints": [{"host": "127.0.0.1", "port": 0, "scheme": "http"}]}},
            _format("endpoint[0] port must be between 1 and 65535"),
        ),
        _case("uppercase-tag", "A", {"tags": ["LLM"]}, _format("tags[0] must match ^[a-z0-9_-]+$")),
        _case("dotted-tag", "A", {"tags": ["a.b"]}, _format("tags[0] must match ^[a-z0-9_-]+$")),
        _case(
            "alias-leading-dash", "A", {"alias": "-api"},
            _format("alias must not start or end with '.', '-' or ':'"),
        ),
        _case(
            "alias-trailing-colon", "A", {"alias": "api:"},
            _format("alias must not start or end with '.', '-' or ':'"),
        ),
        _case(
            "rate-0", "A", {"rate_limit": _rl(rate=0)},
            _format("rate_limit.sustained.rate must be at least 1"),
        ),
        _case(
            "capacity-0", "A", {"rate_limit": _rl(capacity=0)},
            _format("rate_limit.burst.capacity must be at least 1"),
        ),
        _case("cost-0", "A", {"rate_limit": _rl(cost=0)}, _format("rate_limit.cost must be at least 1")),
        _case(
            "cost-over-burst", "A", {"rate_limit": _rl(capacity=5, cost=6)},
            _format(_COST_OVER_BURST),
        ),
        _case(
            "cost-over-rate", "A",
            {"rate_limit": {k: v for k, v in _rl(rate=5, cost=6).items() if k != "burst"}},
            _format("rate_limit.cost (6) must not exceed rate_limit.sustained.rate (5)"),
        ),
        _case(
            "sliding-cost-over-rate", "A",
            # A sliding window ignores `burst`.
            {"rate_limit": _rl(rate=5, capacity=50, cost=6, algorithm="sliding_window")},
            _format("rate_limit.cost (6) must not exceed rate_limit.sustained.rate (5)"),
        ),
        _case(
            "unknown-auth-type", "C",
            {"auth": {"type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.nonexistent.v1"}},
            _violation("auth.type", "NOT_IMPLEMENTED", "is not implemented"),
        ),
        _case(
            "catalog-only-auth-type", "C", {"auth": {"type": BASIC_AUTH_PLUGIN_ID}},
            _violation("auth.type", "NOT_IMPLEMENTED", "is not implemented"),
        ),
        _case(
            "catalog-only-transform", "C", {"plugins": _plugins(LOGGING_TRANSFORM_PLUGIN_ID)},
            _violation("plugins.items.plugin_ref", "NOT_IMPLEMENTED", "is not implemented"),
        ),
        _case(
            "custom-uuid-guard", "C", {"plugins": _plugins(_CUSTOM_GUARD)},
            _violation("plugins.items.plugin_ref", "NOT_IMPLEMENTED", "is not implemented"),
        ),
        _case(
            "auth-plugin-in-plugins", "C", {"plugins": _plugins(NOOP_AUTH_PLUGIN_ID)},
            _violation("plugins.items.plugin_ref", "VALIDATION", "is an auth plugin"),
        ),
        _case(
            "transform-as-auth-type", "C", {"auth": {"type": REQUEST_ID_TRANSFORM_PLUGIN_ID}},
            _violation("auth.type", "VALIDATION", "is not an auth plugin"),
        ),
    ],
)
async def test_upstream_write_rejected(
    body, expected, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.15 A/C: an upstream the gateway could not serve is a 400 and is not stored."""
    _ = mock_upstream
    body = {"alias": unique_alias("write-check"), **body}
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(client, oagw_base_url, oagw_headers, mock_upstream_url, **body)
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        _assert_rejected(resp, expected)
        upstreams = await list_all(client, oagw_base_url, oagw_headers, "upstreams")
        assert not [u for u in upstreams if u["alias"] == body["alias"]]


@pytest.mark.scenario(_SCENARIO, part="A")
@pytest.mark.asyncio
async def test_upstream_update_rejected_and_row_kept(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.15-A on update: a PUT that breaks a rule is a 400 and the stored row is unchanged."""
    _ = mock_upstream
    alias = unique_alias("write-check-put")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias, tags=["llm"],
        ))
        url = f"{oagw_base_url}/oagw/v1/upstreams/{upstream['id']}"
        before = await client.get(url, headers=oagw_headers)
        assert before.status_code == 200, before.text[:500]

        resp = await update_upstream_raw(
            client, oagw_base_url, oagw_headers, upstream["id"], mock_upstream_url,
            alias=alias, tags=["LLM"],
        )
        _assert_rejected(resp, _format("tags[0] must match ^[a-z0-9_-]+$"))

        after = await client.get(url, headers=oagw_headers)
        assert after.status_code == 200, after.text[:500]
        assert after.json() == before.json()


@pytest.mark.scenario(_SCENARIO, part="A")
@pytest.mark.asyncio
async def test_alias_may_start_and_end_with_underscore(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.15-A: `_` is allowed anywhere in an alias."""
    _ = mock_upstream
    alias = f"_{unique_alias('underscore')}_"
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        assert upstream["alias"] == alias


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("body", "expected"),
    [
        _case(
            "empty-path", "B", {"match": {"http": {"methods": ["GET"], "path": ""}}},
            _violation("match.http.path", "INVALID_PROXY_PATH", "route path must start with `/`"),
        ),
        _case(
            "relative-path", "B", {"match": {"http": {"methods": ["GET"], "path": "v1/chat"}}},
            _violation("match.http.path", "INVALID_PROXY_PATH", "route path must start with `/`"),
        ),
        _case(
            "no-methods", "B", {"match": {"http": {"methods": [], "path": "/v1"}}},
            _format("match.http.methods must list at least one method"),
        ),
        _case(
            "route-rate-0", "B",
            {"match": {"http": {"methods": ["GET"], "path": "/v1"}}, "rate_limit": _rl(rate=0)},
            _format("rate_limit.sustained.rate must be at least 1"),
        ),
        _case(
            "route-cost-over-burst", "B",
            {
                "match": {"http": {"methods": ["GET"], "path": "/v1"}},
                "rate_limit": _rl(capacity=5, cost=6),
            },
            _format(_COST_OVER_BURST),
        ),
        _case(
            "unknown-route-transform", "C",
            {"match": {"http": {"methods": ["GET"], "path": "/v1"}}, "plugins": _plugins(_UNKNOWN_TRANSFORM)},
            _violation("plugins.items.plugin_ref", "NOT_IMPLEMENTED", "is not implemented"),
        ),
    ],
)
async def test_route_write_rejected(
    body, expected, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.15 B/C: a route the gateway could not serve is a 400 and is not stored."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("write-check-r"),
        ))
        resp = await create_route_raw(
            client, oagw_base_url, oagw_headers, upstream["id"], [], "", **body,
        )
        _assert_rejected(resp, expected)
        routes = await list_all(
            client, oagw_base_url, oagw_headers, "routes", upstream_id=upstream["id"],
        )
        assert routes == []


@pytest.mark.scenario(_SCENARIO, part="C")
@pytest.mark.asyncio
async def test_implemented_plugin_is_accepted(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 2.15-C: an implemented plugin id is accepted on a route."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("write-check-ok"),
        ))
        resp = await create_route_raw(
            client, oagw_base_url, oagw_headers, upstream["id"], ["GET"], "/v1",
            plugins=_plugins(REQUEST_ID_TRANSFORM_PLUGIN_ID),
        )
        assert resp.status_code == 201, resp.text[:500]


def _invalid_argument(context: dict, detail: str = "Request validation failed") -> dict:
    return {
        "type": err_type("invalid_argument"),
        "title": "Invalid Argument",
        "status": 400,
        "detail": detail,
        "instance": "/oagw/v1/upstreams",
        "context": context,
    }


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("body", "expected"),
    [
        pytest.param(
            {"server": {"endpoints": [{"host": "127.0.0.1", "port": 0, "scheme": "http"}]}},
            _invalid_argument(
                {
                    "format": "endpoint[0] port must be between 1 and 65535",
                    "resource_type": PROXY_SCHEMA,
                },
                detail="endpoint[0] port must be between 1 and 65535",
            ),
            id="format-error",
            marks=pytest.mark.scenario(_SCENARIO, part="A"),
        ),
        pytest.param(
            {"rate_limit": _rl(capacity=5, cost=6)},
            _invalid_argument(
                {"format": _COST_OVER_BURST, "resource_type": PROXY_SCHEMA},
                detail=_COST_OVER_BURST,
            ),
            id="cost-over-capacity",
            marks=pytest.mark.scenario(_SCENARIO, part="A"),
        ),
        pytest.param(
            {"auth": {"type": BASIC_AUTH_PLUGIN_ID}},
            _invalid_argument({
                "field_violations": [{
                    "field": "auth.type",
                    "reason": "NOT_IMPLEMENTED",
                    "description": (
                        f"auth plugin '{BASIC_AUTH_PLUGIN_ID}' is not implemented; use one of ["
                        + ", ".join(f'"{p}"' for p in _IMPLEMENTED_AUTH)
                        + "]"
                    ),
                }],
                "resource_type": PROXY_SCHEMA,
            }),
            id="field-violation",
            marks=pytest.mark.scenario(_SCENARIO, part="C"),
        ),
        pytest.param(
            {"auth": {
                "type": APIKEY_AUTH_PLUGIN_ID,
                "config": {"header": "authorization", "secret_ref": "cred://not a ref"},
            }},
            _invalid_argument({
                "field_violations": [{
                    "field": "auth.config.secret_ref",
                    "reason": "VALIDATION",
                    "description": (
                        "auth.config.secret_ref: invalid credential reference: "
                        "contains invalid characters; only [a-zA-Z0-9_-] are allowed"
                    ),
                }],
                "resource_type": PROXY_SCHEMA,
            }),
            id="malformed-credential-ref",
            marks=pytest.mark.scenario(_SECRET_ACCESS, part="C"),
        ),
    ],
)
async def test_rejection_bodies_are_complete(
    body, expected, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenarios 2.15 and 9.7-C: one rejected write of each shape, its body compared whole."""
    _ = mock_upstream
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, traced(oagw_headers), mock_upstream_url,
            alias=unique_alias("write-check-body"), **body,
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        assert_problem_body(resp, expected)
        assert "not a ref" not in resp.text


@pytest.mark.scenario(_SECRET_ACCESS, part="C")
@pytest.mark.asyncio
@pytest.mark.parametrize(
    "plugin_id", [NOOP_AUTH_PLUGIN_ID, OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID], ids=["noop", "oauth2"],
)
async def test_secret_ref_is_checked_whatever_the_plugin(
    plugin_id, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 9.7-C: a malformed `secret_ref` is refused on every auth plugin, not only apikey."""
    _ = mock_upstream
    config = {"secret_ref": "cred://not a ref"}
    if plugin_id == OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID:
        config |= {
            "token_endpoint": f"{mock_upstream_url}/oauth2/token",
            "client_id_ref": "cred://test-oauth2-client-id",
            "client_secret_ref": "cred://test-oauth2-client-secret",
        }
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("write-check-ref"), auth={"type": plugin_id, "config": config},
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        _assert_rejected(
            resp, _violation("auth.config.secret_ref", "VALIDATION", "invalid credential reference"),
        )
        assert "not a ref" not in resp.text
