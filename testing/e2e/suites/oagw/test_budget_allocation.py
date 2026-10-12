"""E2E tests for OAGW budget allocation subsystem.

Tests cover:
- Category A: Budget config field validation (single-tenant)
- Category B: Budget allocation validation across tenant hierarchy
- Category C: Shared pool behaviour at runtime
- Category D: Unlimited / no-budget defaults

Allocation checks run at write time. Values sit on the exact boundary
(sum == total accepted, one more rejected) so an off-by-one fails, and a
test that relies on a check being skipped also runs the control where it
is not.
"""
import pytest
import httpx
from typing import Optional

from .helpers import (
    assert_problem,
    create_route,
    create_upstream,
    create_upstream_raw,
    unique_alias,
    update_upstream_raw,
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _rl(rate: int, window: str = "minute", budget: Optional[dict] = None,
        sharing: Optional[str] = None, **extra) -> dict:
    """Build a rate_limit payload."""
    rl: dict = {
        "algorithm": "token_bucket",
        "sustained": {"rate": rate, "window": window},
        "burst": {"capacity": rate},
        "scope": "tenant",
        "strategy": "reject",
    }
    if sharing is not None:
        rl["sharing"] = sharing
    if budget is not None:
        rl["budget"] = budget
    rl.update(extra)
    return rl


def _assert_budget_rejected(resp: httpx.Response, *fragments: str) -> None:
    assert_problem(resp, 400, esrc=None)
    for fragment in fragments:
        assert fragment in resp.text, f"{fragment!r} not in {resp.text[:500]}"


async def _parent(client, base, headers, mock_url, cleanup, prefix, rate_limit):
    alias = unique_alias(prefix)
    parent = cleanup.upstream(headers, await create_upstream(
        client, base, headers, mock_url, alias=alias, rate_limit=rate_limit,
    ))
    return alias, parent


async def _child(client, base, headers, mock_url, cleanup, alias, rate_limit=None) -> dict:
    kwargs = {} if rate_limit is None else {"rate_limit": rate_limit}
    return cleanup.upstream(headers, await create_upstream(
        client, base, headers, mock_url, alias=alias, **kwargs,
    ))


async def _child_raw(client, base, headers, mock_url, cleanup, alias, rate_limit=None) -> httpx.Response:
    kwargs = {} if rate_limit is None else {"rate_limit": rate_limit}
    resp = await create_upstream_raw(client, base, headers, mock_url, alias=alias, **kwargs)
    if resp.status_code == 201:
        cleanup.upstream(headers, resp.json())
    return resp


# ===================================================================
# Category A: Budget config validation (single-tenant, existing token)
# ===================================================================


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("budget", "message"),
    [
        ({"mode": "allocated"}, "budget.total is required"),
        ({"mode": "shared"}, "budget.total is required"),
        ({"mode": "allocated", "total": 0}, "budget.total must be at least 1"),
    ],
    ids=["allocated-no-total", "shared-no-total", "zero-total"],
)
async def test_budget_total_validation(
    budget, message, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Allocated and shared budgets need a positive total, on create and on update."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("ba-a1"), rate_limit=_rl(100, budget=budget),
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        _assert_budget_rejected(resp, message)

        alias = unique_alias("ba-a1-put")
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url, alias=alias,
            rate_limit=_rl(100),
        ))
        resp = await update_upstream_raw(
            client, oagw_base_url, oagw_headers, upstream["id"], mock_upstream_url,
            alias=alias, rate_limit=_rl(100, budget=budget),
        )
        _assert_budget_rejected(resp, message)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("ratio", "accepted"),
    [(0.99, False), (1.0, True), (2.0, True), (2.01, False)],
)
async def test_budget_overcommit_ratio_range(
    ratio, accepted, oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """The overcommit ratio accepts exactly [1.0, 2.0]."""
    budget = {"mode": "allocated", "total": 100, "overcommit_ratio": ratio}
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("ba-a3"), rate_limit=_rl(100, budget=budget),
        )
        if resp.status_code == 201:
            cleanup.upstream(oagw_headers, resp.json())
        if not accepted:
            _assert_budget_rejected(resp, "overcommit_ratio must be between")
            return
        assert resp.status_code == 201, resp.text[:500]
        assert resp.json()["rate_limit"]["budget"]["overcommit_ratio"] == ratio


@pytest.mark.asyncio
async def test_budget_unlimited_accepts_no_total(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Unlimited mode does not require total."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("ba-a5"), rate_limit=_rl(100, budget={"mode": "unlimited"}),
        ))
        assert upstream["rate_limit"]["budget"]["mode"] == "unlimited"


@pytest.mark.asyncio
async def test_budget_allocated_valid_config_accepted(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """A valid allocated budget is stored field for field (unknown fields would be dropped)."""
    budget = {"mode": "allocated", "total": 1000, "overcommit_ratio": 1.5}
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = cleanup.upstream(oagw_headers, await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=unique_alias("ba-a6"), rate_limit=_rl(100, budget=budget),
        ))
        assert upstream["rate_limit"]["budget"] == budget

        stored = await client.get(
            f"{oagw_base_url}/oagw/v1/upstreams/{upstream['id']}", headers=oagw_headers,
        )
        assert stored.json()["rate_limit"]["budget"] == budget


# ===================================================================
# Category B: Budget allocation validation (multi-tenant hierarchy)
# ===================================================================


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="A")
@pytest.mark.asyncio
async def test_allocated_child_within_budget(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.7-A: a child using the whole budget exactly is accepted."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-b1",
            _rl(100, sharing="inherit", budget={"mode": "allocated", "total": 100}),
        )
        await _child(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup, alias, _rl(100),
        )


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="A")
@pytest.mark.asyncio
async def test_allocated_two_children_within_budget(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    hierarchy_l1b_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.7-A: siblings summing to the total fit; one more request/min does not.

    The rates are exact in binary after normalising to req/s (0.5 + 0.5 = 1.0),
    so the sum really equals the total instead of landing just under it.
    """
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-b2",
            _rl(60, sharing="inherit", budget={"mode": "allocated", "total": 60}),
        )
        await _child(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup, alias, _rl(30),
        )
        child_b = await _child(
            client, oagw_base_url, hierarchy_l1b_headers, mock_upstream_url, cleanup, alias, _rl(30),
        )

        resp = await update_upstream_raw(
            client, oagw_base_url, hierarchy_l1b_headers, child_b["id"], mock_upstream_url,
            alias=alias, rate_limit=_rl(31),
        )
        _assert_budget_rejected(resp, "budget allocation exceeded")


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="A")
@pytest.mark.asyncio
async def test_allocated_exceeded_rejected(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    hierarchy_l1b_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Second child that pushes the sum over budget is rejected."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-b3",
            _rl(100, sharing="inherit",
                budget={"mode": "allocated", "total": 100, "overcommit_ratio": 1.0}),
        )
        await _child(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup, alias, _rl(60),
        )

        # 60 + 50 = 110/min = 1.83 req/s > 100/min
        resp = await _child_raw(
            client, oagw_base_url, hierarchy_l1b_headers, mock_upstream_url, cleanup, alias, _rl(50),
        )
        _assert_budget_rejected(resp, "budget allocation exceeded", "children total 1.83 req/s")


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="A")
@pytest.mark.asyncio
async def test_allocated_overcommit_allows_excess(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    hierarchy_l1b_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Overcommit ratio 1.5 allows children to sum above nominal total."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-b4",
            _rl(100, sharing="inherit",
                budget={"mode": "allocated", "total": 100, "overcommit_ratio": 1.5}),
        )
        # 80 + 60 = 140 <= 100 * 1.5 = 150 → allowed
        await _child(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup, alias, _rl(80),
        )
        await _child(
            client, oagw_base_url, hierarchy_l1b_headers, mock_upstream_url, cleanup, alias, _rl(60),
        )


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="A")
@pytest.mark.asyncio
async def test_allocated_overcommit_still_has_limit(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    hierarchy_l1b_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Even with overcommit, exceeding total * ratio is rejected, at the computed ceiling."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-b5",
            _rl(100, sharing="inherit",
                budget={"mode": "allocated", "total": 100, "overcommit_ratio": 1.5}),
        )
        await _child(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup, alias, _rl(100),
        )

        # 100 + 60 = 160 > 100 * 1.5 = 150 → rejected
        resp = await _child_raw(
            client, oagw_base_url, hierarchy_l1b_headers, mock_upstream_url, cleanup, alias, _rl(60),
        )
        _assert_budget_rejected(
            resp, "budget allocation exceeded", "× 1.5 overcommit ratio (allowed: 2.50 req/s)",
        )


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="A")
@pytest.mark.asyncio
async def test_allocated_update_revalidates(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """A child's update is checked against the budget, without counting its old rate."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-b6",
            _rl(100, sharing="inherit", budget={"mode": "allocated", "total": 100}),
        )
        child = await _child(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup, alias, _rl(50),
        )
        child_url = f"{oagw_base_url}/oagw/v1/upstreams/{child['id']}"

        # 50 → 90 fits only if the child's current 50 is excluded from the sum.
        resp = await update_upstream_raw(
            client, oagw_base_url, hierarchy_l1a_headers, child["id"], mock_upstream_url,
            alias=alias, rate_limit=_rl(90),
        )
        assert resp.status_code == 200, resp.text[:500]

        resp = await update_upstream_raw(
            client, oagw_base_url, hierarchy_l1a_headers, child["id"], mock_upstream_url,
            alias=alias, rate_limit=_rl(120),
        )
        _assert_budget_rejected(resp, "budget allocation exceeded")
        stored = (await client.get(child_url, headers=hierarchy_l1a_headers)).json()
        assert stored["rate_limit"]["sustained"]["rate"] == 90


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="A")
@pytest.mark.asyncio
async def test_allocated_parent_cannot_shrink_below_children(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Lowering a parent's budget below what its children already use is rejected."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, parent = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-b9",
            _rl(100, sharing="inherit", budget={"mode": "allocated", "total": 100}),
        )
        await _child(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup, alias, _rl(80),
        )

        resp = await update_upstream_raw(
            client, oagw_base_url, hierarchy_root_headers, parent["id"], mock_upstream_url,
            alias=alias, rate_limit=_rl(100, sharing="inherit",
                                        budget={"mode": "allocated", "total": 50}),
        )
        _assert_budget_rejected(resp)


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="A")
@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("child_rate", "accepted"),
    [
        (7200, False),  # 2 req/s against a 1 req/s budget
        (3500, True),   # 0.97 req/s
    ],
)
async def test_allocated_different_windows_normalized(
    child_rate, accepted, oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Both sides are normalised to req/s: a per-hour child against a per-minute budget."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        # Parent: 60/min = 1 req/s budget
        alias, _ = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-b7",
            _rl(60, window="minute", sharing="inherit", budget={"mode": "allocated", "total": 60}),
        )
        resp = await _child_raw(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup,
            alias, _rl(child_rate, window="hour"),
        )
        if accepted:
            assert resp.status_code == 201, resp.text[:500]
        else:
            _assert_budget_rejected(resp, "budget allocation exceeded")


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="A")
@pytest.mark.asyncio
async def test_allocated_rejects_child_without_rate_limit(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Allocated budget rejects child that omits rate_limit entirely."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-b8",
            _rl(100, sharing="inherit", budget={"mode": "allocated", "total": 100}),
        )
        resp = await _child_raw(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup, alias,
        )
        _assert_budget_rejected(resp, "rate_limit is required")


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="A")
@pytest.mark.asyncio
@pytest.mark.xfail(
    strict=True,
    raises=AssertionError,
    reason="F-1 (review finding, no decision yet): changing a parent's "
           "sustained.window with the same budget skips the budget re-check",
)
async def test_allocated_parent_window_change_revalidates(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.7-A: minute → hour shrinks the budget 60×, so it must be re-checked."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, parent = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-f1",
            _rl(100, sharing="inherit", budget={"mode": "allocated", "total": 100}),
        )
        await _child(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup, alias, _rl(60),
        )

        resp = await update_upstream_raw(
            client, oagw_base_url, hierarchy_root_headers, parent["id"], mock_upstream_url,
            alias=alias, rate_limit=_rl(100, window="hour", sharing="inherit",
                                        budget={"mode": "allocated", "total": 100}),
        )
        _assert_budget_rejected(resp)


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="A")
@pytest.mark.asyncio
@pytest.mark.xfail(
    strict=True,
    raises=AssertionError,
    reason="R-15: allocated-budget checks count grandchildren against the top "
           "parent, so an unchanged PUT of a middle tenant is rejected",
)
async def test_allocated_three_levels_counts_direct_children_only(
    oagw_base_url, oagw_headers, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.7-A: each level's budget is checked against its direct children only.

    tenant-a (100/min) → hierarchy-root (60/min) → l1a (50/min): root's 60
    fits tenant-a's 100; l1a's 50 fits root's 60.
    """
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _parent(
            client, oagw_base_url, oagw_headers, mock_upstream_url, cleanup, "ba-r15",
            _rl(100, sharing="inherit", budget={"mode": "allocated", "total": 100}),
        )
        middle_rl = _rl(60, sharing="inherit", budget={"mode": "allocated", "total": 60})
        middle = await _child(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, alias, middle_rl,
        )
        await _child(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup, alias, _rl(50),
        )

        resp = await update_upstream_raw(
            client, oagw_base_url, hierarchy_root_headers, middle["id"], mock_upstream_url,
            alias=alias, rate_limit=middle_rl,
        )
        assert resp.status_code == 200, resp.text[:500]


# ===================================================================
# Category C: Shared pool
# ===================================================================


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="B")
@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("budget", "expected"),
    [
        # Children's own bindings draw on the pool owner's (parent's) bucket.
        ({"mode": "shared", "total": 2}, [200, 200, 429]),
        # Control: without the shared budget each binding has its own bucket.
        (None, [200, 200, 200]),
    ],
    ids=["shared", "no-budget"],
)
async def test_shared_budget_pools_child_bindings(
    budget, expected, oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    hierarchy_l1b_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.7-B: children that bind the alias themselves share the parent's pool.

    With `scope: global` the pool is keyed on the pool owner alone; the
    tenant-scoped case is R-09 (`test_shared_budget_pool_is_shared`).
    """
    rate_limit = _rl(2, sharing="inherit", scope="global", budget=budget)
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, parent = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-c0",
            rate_limit,
        )
        for headers in (hierarchy_l1a_headers, hierarchy_l1b_headers):
            await _child(client, oagw_base_url, headers, mock_upstream_url, cleanup, alias)
        await create_route(
            client, oagw_base_url, hierarchy_root_headers, parent["id"], ["GET"], "/v1/models",
        )

        statuses = [
            (await client.get(f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models", headers=h)).status_code
            for h in (hierarchy_l1a_headers, hierarchy_l1b_headers, hierarchy_l1a_headers)
        ]
        assert statuses == expected


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="B")
@pytest.mark.asyncio
@pytest.mark.xfail(
    strict=True,
    raises=AssertionError,
    reason="R-09: a shared budget is not shared at runtime (one bucket per "
           "tenant) and budget.total is ignored",
)
async def test_shared_budget_pool_is_shared(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    hierarchy_l1b_headers, mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.7-B: tenants draw on one pool of `budget.total` requests.

    The scope stays `tenant`, so any sharing has to come from the budget,
    and burst capacity is far above the total, so only the pool can refuse.
    """
    rate_limit = _rl(100, sharing="inherit", budget={"mode": "shared", "total": 3})
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, parent = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-c1",
            rate_limit,
        )
        for headers in (hierarchy_l1a_headers, hierarchy_l1b_headers):
            await _child(client, oagw_base_url, headers, mock_upstream_url, cleanup, alias)
        await create_route(
            client, oagw_base_url, hierarchy_root_headers, parent["id"], ["GET"], "/v1/models",
        )

        statuses = [
            (await client.get(f"{oagw_base_url}/oagw/v1/proxy/{alias}/v1/models", headers=h)).status_code
            for h in (hierarchy_l1a_headers, hierarchy_l1b_headers, hierarchy_l1a_headers, hierarchy_l1b_headers)
        ]
        assert statuses == [200, 200, 200, 429]


# ===================================================================
# Category D: Unlimited / no-budget defaults
# ===================================================================


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="C")
@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("mode", "child_accepted"),
    [("unlimited", True), ("allocated", False)],
)
async def test_unlimited_no_child_validation(
    mode, child_accepted, oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.7-C: unlimited mode skips allocation checks even when a total is set.

    The `allocated` case is the control: same parent and total, child rejected.
    """
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-d1",
            _rl(100, sharing="inherit", budget={"mode": mode, "total": 10}),
        )
        resp = await _child_raw(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup,
            alias, _rl(9999),
        )
        if child_accepted:
            assert resp.status_code == 201, resp.text[:500]
        else:
            _assert_budget_rejected(resp, "budget allocation exceeded")


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="C")
@pytest.mark.asyncio
async def test_no_budget_mode_defaults_unlimited(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """Scenario 18.7-C: a budget without `mode` defaults to unlimited."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, parent = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-d2",
            _rl(100, sharing="inherit", budget={"total": 100}),
        )
        assert parent["rate_limit"]["budget"]["mode"] == "unlimited"
        await _child(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup, alias, _rl(9999),
        )


@pytest.mark.scenario("positive-18.7-budget-modes-behave-specified", part="C")
@pytest.mark.asyncio
async def test_no_budget_field_accepts_any_child(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream, cleanup,
):
    """A parent limit with no budget at all puts no ceiling on children."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        alias, _ = await _parent(
            client, oagw_base_url, hierarchy_root_headers, mock_upstream_url, cleanup, "ba-d3",
            _rl(100, sharing="inherit"),
        )
        await _child(
            client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url, cleanup, alias, _rl(9999),
        )
