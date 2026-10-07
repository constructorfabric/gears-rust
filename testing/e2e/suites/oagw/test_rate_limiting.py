"""E2E tests for OAGW rate limiting — token bucket, sliding window, scoping.

Each test varies one thing (algorithm, scope, cost, refill) and pins the
values that identify it: statuses in order, `Retry-After`, and the
rate-limit header values, so a limiter that ignores scope, never refills,
runs the wrong algorithm or reports wrong numbers fails.
"""
import asyncio

import httpx
import pytest

from .helpers import assert_problem, create_route, create_upstream, unique_alias

# Response header names. R-17 (IETF RateLimit headers) renames these; keep
# the names here so that change is one edit and absence checks can't go
# vacuous by testing a name nobody sends.
RL_LIMIT = "x-ratelimit-limit"
RL_REMAINING = "x-ratelimit-remaining"
RL_RESET = "x-ratelimit-reset"
RL_HEADER_PREFIX = "x-ratelimit-"


def _rl(rate: int, window: str = "minute", capacity: int | None = None, **extra) -> dict:
    """Build a token-bucket rate_limit payload; `extra` overrides any field."""
    rl: dict = {
        "algorithm": "token_bucket",
        "sustained": {"rate": rate, "window": window},
        "burst": {"capacity": rate if capacity is None else capacity},
        "scope": "tenant",
        "strategy": "reject",
    }
    rl.update(extra)
    return rl


def _rl_headers(resp: httpx.Response) -> list[str]:
    return sorted(h for h in resp.headers if h.lower().startswith(RL_HEADER_PREFIX))


def _assert_rate_limited(resp: httpx.Response) -> int:
    """Assert an OAGW 429 and return its Retry-After in seconds."""
    assert_problem(resp, 429, category="resource_exhausted")
    return int(resp.headers["retry-after"])


async def _limited_upstream(client, base, headers, mock_url, cleanup, prefix, rate_limit, paths=("/v1/models",)):
    alias = unique_alias(prefix)
    upstream = cleanup.upstream(headers, await create_upstream(
        client, base, headers, mock_url, alias=alias, rate_limit=rate_limit,
    ))
    for path in paths:
        await create_route(client, base, headers, upstream["id"], ["GET"], path)
    return alias, upstream


async def _get(client, base, alias, headers, path="/v1/models"):
    return await client.get(f"{base}/oagw/v1/proxy/{alias}{path}", headers=headers)


@pytest.mark.scenario("positive-18.1-token-bucket-sustained-burst")
@pytest.mark.asyncio
async def test_rate_limit_exceeded_returns_429(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.1: the request past capacity gets a gateway 429 with a real Retry-After."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _limited_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "rl-429", _rl(1, cost=1),
        )

        first = await _get(client, oagw_base_url, alias, oagw_headers)
        assert first.status_code == 200
        assert first.headers.get("x-oagw-error-source") == "upstream"
        assert first.headers.get(RL_LIMIT) == "1"
        assert first.headers.get(RL_REMAINING) == "0"

        second = await _get(client, oagw_base_url, alias, oagw_headers)
        # One token per minute: the next one is about a minute away.
        assert 55 <= _assert_rate_limited(second) <= 60


@pytest.mark.scenario("positive-18.1-token-bucket-sustained-burst")
@pytest.mark.asyncio
async def test_token_bucket_burst_capacity_and_headers(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.1: burst allows `capacity` requests, counting down in the headers."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _limited_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "rl-burst", _rl(5, window="hour", capacity=10, response_headers=True),
        )

        for i in range(10):
            resp = await _get(client, oagw_base_url, alias, oagw_headers)
            assert resp.status_code == 200, f"request {i + 1}/10: {resp.status_code}"
            assert resp.headers.get(RL_LIMIT) == "10"
            assert resp.headers.get(RL_REMAINING) == str(9 - i)
            assert RL_RESET in resp.headers

        resp = await _get(client, oagw_base_url, alias, oagw_headers)
        # 5 tokens per hour: one token every 720 s, minus the time the burst took.
        assert 715 <= _assert_rate_limited(resp) <= 720


@pytest.mark.scenario("positive-18.1-token-bucket-sustained-burst")
@pytest.mark.asyncio
@pytest.mark.timeout(15)
async def test_token_bucket_refills(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.1 step 3: an exhausted bucket serves again after the refill interval."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _limited_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "rl-refill", _rl(1, window="second", capacity=3),
        )

        # 1/second: the burst only has to finish within a second to exhaust it.
        statuses = [(await _get(client, oagw_base_url, alias, oagw_headers)).status_code for _ in range(4)]
        assert statuses == [200] * 3 + [429]

        await asyncio.sleep(1.2)
        assert (await _get(client, oagw_base_url, alias, oagw_headers)).status_code == 200


@pytest.mark.scenario("positive-18.1.1-rate-limit-response-headers-can-be-disabled")
@pytest.mark.asyncio
@pytest.mark.parametrize("response_headers", [True, False])
async def test_response_headers_toggle(
    response_headers, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.1.1: `response_headers` controls the rate-limit headers on 200 and 429.

    `Retry-After` is always sent on a 429. The `True` case is the control
    that shows the absence check means something.
    """
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _limited_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "rl-hdr", _rl(1, response_headers=response_headers),
        )
        expected = [RL_LIMIT, RL_REMAINING, RL_RESET] if response_headers else []

        resp = await _get(client, oagw_base_url, alias, oagw_headers)
        assert resp.status_code == 200
        assert _rl_headers(resp) == sorted(expected)

        resp = await _get(client, oagw_base_url, alias, oagw_headers)
        _assert_rate_limited(resp)
        assert _rl_headers(resp) == sorted(expected)


@pytest.mark.scenario("negative-18.2-sliding-window-strictness")
@pytest.mark.asyncio
async def test_sliding_window_basic_enforcement(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.2: a sliding window allows `rate` per window, then waits out the window.

    A token bucket with the same numbers would answer `Retry-After: 30`
    (one of two tokens refills halfway through the minute).
    """
    rate_limit = {
        "algorithm": "sliding_window",
        "sustained": {"rate": 2, "window": "minute"},
        "scope": "tenant",
        "strategy": "reject",
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _limited_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "rl-sw", rate_limit,
        )

        for i in range(2):
            resp = await _get(client, oagw_base_url, alias, oagw_headers)
            assert resp.status_code == 200, f"request {i + 1}/2: {resp.status_code}"

        resp = await _get(client, oagw_base_url, alias, oagw_headers)
        assert 59 <= _assert_rate_limited(resp) <= 60


@pytest.mark.scenario("negative-18.2-sliding-window-strictness")
@pytest.mark.asyncio
async def test_sliding_window_no_boundary_burst(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario N18.2: the window slides, so waiting part of it earns nothing back.

    At 2/second, a token bucket refills 1.2 tokens in 0.6 s and serves the
    third request; the sliding window still counts both earlier requests
    (the three requests then have ~400 ms of latency budget together).
    """
    rate_limit = {
        "algorithm": "sliding_window",
        "sustained": {"rate": 2, "window": "second"},
        "scope": "tenant",
        "strategy": "reject",
    }
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _limited_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "rl-sw-edge", rate_limit,
        )

        for _ in range(2):
            assert (await _get(client, oagw_base_url, alias, oagw_headers)).status_code == 200
        await asyncio.sleep(0.6)
        resp = await _get(client, oagw_base_url, alias, oagw_headers)
        assert _assert_rate_limited(resp) == 1


@pytest.mark.scenario("positive-18.3-rate-limit-scope-variants")
@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("scope", "expected"),
    [
        # One bucket for everyone: l1b is refused once l1a used the token.
        ("global", [200, 429, 429]),
        # One bucket per tenant: l1b still has its own token.
        ("tenant", [200, 429, 200]),
    ],
)
async def test_scope_across_tenants(
    scope, expected, oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    hierarchy_l1b_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.3 (`global`, `tenant`): the scope alone decides bucket sharing.

    Both children call the root's upstream directly, with no bindings or
    budget of their own, so the scope is the only thing that differs.
    """
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _limited_upstream(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup,
            f"rl-{scope}", _rl(1, scope=scope, sharing="inherit"),
        )

        statuses = [
            (await _get(client, oagw_base_url, alias, h)).status_code
            for h in (hierarchy_l1a_headers, hierarchy_l1a_headers, hierarchy_l1b_headers)
        ]
        assert statuses == expected


@pytest.mark.scenario("positive-18.3-rate-limit-scope-variants")
@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("scope", "expected"),
    [
        ("user", [200, 429, 200]),
        ("tenant", [200, 429, 429]),
    ],
)
async def test_scope_user_within_tenant(
    scope, expected, oagw_base_url, oagw_headers, tenant_a_reviewer_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.3 (`user`): two subjects in one tenant get separate buckets."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _limited_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            f"rl-{scope}", _rl(1, scope=scope),
        )

        statuses = [
            (await _get(client, oagw_base_url, alias, h)).status_code
            for h in (oagw_headers, oagw_headers, tenant_a_reviewer_headers)
        ]
        assert statuses == expected


@pytest.mark.asyncio
async def test_route_level_limits_are_per_route(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A limit set on each route gives each route its own bucket, whatever the scope."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = unique_alias("rl-route")
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        for path in ("/v1/models", "/health"):
            await create_route(
                client, oagw_base_url, oagw_headers, upstream["id"],
                ["GET"], path, rate_limit=_rl(1, scope="route"),
            )

        statuses = [
            (await _get(client, oagw_base_url, alias, oagw_headers, path)).status_code
            for path in ("/v1/models", "/health", "/v1/models", "/health")
        ]
        assert statuses == [200, 200, 429, 429]


@pytest.mark.scenario("positive-18.3-rate-limit-scope-variants")
@pytest.mark.asyncio
@pytest.mark.xfail(
    strict=True,
    raises=AssertionError,
    reason="R-08: scope=route on an upstream-level limit acts as one bucket "
           "for every route",
)
async def test_scope_route_on_upstream_limit(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.3 (`route`): an upstream-level limit with scope=route is per route."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _limited_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "rl-up-route", _rl(1, scope="route"), paths=("/v1/models", "/health"),
        )

        statuses = [
            (await _get(client, oagw_base_url, alias, oagw_headers, path)).status_code
            for path in ("/v1/models", "/health", "/v1/models")
        ]
        assert statuses == [200, 200, 429]


@pytest.mark.scenario("positive-18.4-weighted-cost-per-route")
@pytest.mark.asyncio
async def test_weighted_cost(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.4: each request draws `cost` tokens."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _limited_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup,
            "rl-cost", _rl(10, cost=4),
        )

        remaining = []
        for _ in range(2):
            resp = await _get(client, oagw_base_url, alias, oagw_headers)
            assert resp.status_code == 200
            remaining.append(resp.headers.get(RL_REMAINING))
        assert remaining == ["6", "2"]
        _assert_rate_limited(await _get(client, oagw_base_url, alias, oagw_headers))


@pytest.mark.scenario("positive-18.4-weighted-cost-per-route")
@pytest.mark.asyncio
@pytest.mark.xfail(
    strict=True,
    raises=AssertionError,
    reason="R-14: a route rate_limit carrying only `cost` is rejected (422)",
)
async def test_weighted_cost_per_route(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.4: routes with different costs draw on the upstream's one bucket."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = unique_alias("rl-cost-rt")
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
            rate_limit=_rl(10),
        ))
        for path, cost in (("/v1/models", 10), ("/health", 1)):
            resp = await client.post(
                f"{oagw_base_url}/oagw/v1/routes",
                headers=oagw_headers,
                json={
                    "upstream_id": upstream["id"],
                    "match": {"http": {"methods": ["GET"], "path": path}},
                    "enabled": True,
                    "tags": [],
                    "priority": 0,
                    "rate_limit": {"cost": cost},
                },
            )
            # Today's R-14 failure: the cost-only route is refused (422).
            assert resp.status_code == 201, resp.text[:300]

        assert (await _get(client, oagw_base_url, alias, oagw_headers, "/v1/models")).status_code == 200
        _assert_rate_limited(await _get(client, oagw_base_url, alias, oagw_headers, "/health"))


@pytest.mark.asyncio
async def test_route_level_rate_limit(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A limit on the route alone (none on the upstream) is enforced."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias = unique_alias("rl-rtlvl")
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
        ))
        await create_route(
            client, oagw_base_url, oagw_headers, upstream["id"],
            ["GET"], "/v1/models", rate_limit=_rl(1),
        )

        assert (await _get(client, oagw_base_url, alias, oagw_headers)).status_code == 200
        resp = await _get(client, oagw_base_url, alias, oagw_headers)
        assert 55 <= _assert_rate_limited(resp) <= 60
