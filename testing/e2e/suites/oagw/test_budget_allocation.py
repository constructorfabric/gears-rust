"""E2E tests for OAGW budget allocation subsystem.

Tests cover:
- Category A: Budget config field validation (single-tenant)
- Category B: Budget allocation validation across tenant hierarchy
- Category C: Shared pool config acceptance
- Category D: Unlimited / no-budget defaults
"""
import pytest
import httpx
from typing import Optional

from .helpers import (
    create_upstream,
    create_upstream_raw,
    delete_upstream,
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


# ===================================================================
# Category A: Budget config validation (single-tenant, existing token)
# ===================================================================


@pytest.mark.asyncio
async def test_budget_allocated_requires_total(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Allocated mode without total is rejected."""
    alias = unique_alias("ba-a1")
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            rate_limit=_rl(100, budget={"mode": "allocated"}),
        )
        assert resp.status_code == 400, f"Expected 400, got {resp.status_code}: {resp.text[:500]}"
        assert "budget.total is required" in resp.text


@pytest.mark.asyncio
async def test_budget_shared_requires_total(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Shared mode without total is rejected."""
    alias = unique_alias("ba-a2")
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            rate_limit=_rl(100, budget={"mode": "shared"}),
        )
        assert resp.status_code == 400, f"Expected 400, got {resp.status_code}: {resp.text[:500]}"
        assert "budget.total is required" in resp.text


@pytest.mark.asyncio
async def test_budget_overcommit_ratio_below_range(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Overcommit ratio below 1.0 is rejected."""
    alias = unique_alias("ba-a3")
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            rate_limit=_rl(100, budget={"mode": "allocated", "total": 100, "overcommit_ratio": 0.5}),
        )
        assert resp.status_code == 400, f"Expected 400, got {resp.status_code}: {resp.text[:500]}"
        assert "overcommit_ratio must be between" in resp.text


@pytest.mark.asyncio
async def test_budget_overcommit_ratio_above_range(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Overcommit ratio above 2.0 is rejected."""
    alias = unique_alias("ba-a4")
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await create_upstream_raw(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            rate_limit=_rl(100, budget={"mode": "allocated", "total": 100, "overcommit_ratio": 2.5}),
        )
        assert resp.status_code == 400, f"Expected 400, got {resp.status_code}: {resp.text[:500]}"
        assert "overcommit_ratio must be between" in resp.text


@pytest.mark.asyncio
async def test_budget_unlimited_accepts_no_total(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Unlimited mode does not require total."""
    alias = unique_alias("ba-a5")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            rate_limit=_rl(100, budget={"mode": "unlimited"}),
        )
        await delete_upstream(client, oagw_base_url, oagw_headers, upstream["id"])


@pytest.mark.asyncio
async def test_budget_allocated_valid_config_accepted(
    oagw_base_url, oagw_headers, mock_upstream_url, mock_upstream,
):
    """Valid allocated budget config is accepted."""
    alias = unique_alias("ba-a6")
    async with httpx.AsyncClient(timeout=10.0) as client:
        upstream = await create_upstream(
            client, oagw_base_url, oagw_headers, mock_upstream_url,
            alias=alias,
            rate_limit=_rl(100, budget={"mode": "allocated", "total": 1000, "overcommit_ratio": 1.5}),
        )
        await delete_upstream(client, oagw_base_url, oagw_headers, upstream["id"])


# ===================================================================
# Category B: Budget allocation validation (multi-tenant hierarchy)
# ===================================================================


@pytest.mark.asyncio
async def test_allocated_child_within_budget(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream,
):
    """Child allocation within parent budget succeeds."""
    alias = unique_alias("ba-b1")
    uids: list[tuple[dict, str]] = []
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            parent = await create_upstream(
                client, oagw_base_url, hierarchy_root_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(100, sharing="inherit",
                               budget={"mode": "allocated", "total": 100}),
            )
            uids.append((hierarchy_root_headers, parent["id"]))

            child = await create_upstream(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(50),
            )
            uids.append((hierarchy_l1a_headers, child["id"]))
        finally:
            for hdrs, uid in reversed(uids):
                await delete_upstream(client, oagw_base_url, hdrs, uid)


@pytest.mark.asyncio
async def test_allocated_two_children_within_budget(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    hierarchy_l1b_headers, mock_upstream_url, mock_upstream,
):
    """Two children whose rates sum within budget both succeed."""
    alias = unique_alias("ba-b2")
    uids: list[tuple[dict, str]] = []
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            parent = await create_upstream(
                client, oagw_base_url, hierarchy_root_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(100, sharing="inherit",
                               budget={"mode": "allocated", "total": 100}),
            )
            uids.append((hierarchy_root_headers, parent["id"]))

            child_a = await create_upstream(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(40),
            )
            uids.append((hierarchy_l1a_headers, child_a["id"]))

            child_b = await create_upstream(
                client, oagw_base_url, hierarchy_l1b_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(40),
            )
            uids.append((hierarchy_l1b_headers, child_b["id"]))
        finally:
            for hdrs, uid in reversed(uids):
                await delete_upstream(client, oagw_base_url, hdrs, uid)


@pytest.mark.asyncio
async def test_allocated_exceeded_rejected(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    hierarchy_l1b_headers, mock_upstream_url, mock_upstream,
):
    """Second child that pushes sum over budget is rejected."""
    alias = unique_alias("ba-b3")
    uids: list[tuple[dict, str]] = []
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            parent = await create_upstream(
                client, oagw_base_url, hierarchy_root_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(100, sharing="inherit",
                               budget={"mode": "allocated", "total": 100, "overcommit_ratio": 1.0}),
            )
            uids.append((hierarchy_root_headers, parent["id"]))

            child_a = await create_upstream(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(60),
            )
            uids.append((hierarchy_l1a_headers, child_a["id"]))

            # This should fail: 60 + 50 = 110 > 100
            resp = await create_upstream_raw(
                client, oagw_base_url, hierarchy_l1b_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(50),
            )
            assert resp.status_code == 400, (
                f"Expected 400 for over-budget, got {resp.status_code}: {resp.text[:500]}"
            )
            assert "budget allocation exceeded" in resp.text
        finally:
            for hdrs, uid in reversed(uids):
                await delete_upstream(client, oagw_base_url, hdrs, uid)


@pytest.mark.asyncio
async def test_allocated_overcommit_allows_excess(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    hierarchy_l1b_headers, mock_upstream_url, mock_upstream,
):
    """Overcommit ratio 1.5 allows children to sum above nominal total."""
    alias = unique_alias("ba-b4")
    uids: list[tuple[dict, str]] = []
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            parent = await create_upstream(
                client, oagw_base_url, hierarchy_root_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(100, sharing="inherit",
                               budget={"mode": "allocated", "total": 100, "overcommit_ratio": 1.5}),
            )
            uids.append((hierarchy_root_headers, parent["id"]))

            # 80 + 60 = 140 <= 100 * 1.5 = 150 → allowed
            child_a = await create_upstream(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(80),
            )
            uids.append((hierarchy_l1a_headers, child_a["id"]))

            child_b = await create_upstream(
                client, oagw_base_url, hierarchy_l1b_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(60),
            )
            uids.append((hierarchy_l1b_headers, child_b["id"]))
        finally:
            for hdrs, uid in reversed(uids):
                await delete_upstream(client, oagw_base_url, hdrs, uid)


@pytest.mark.asyncio
async def test_allocated_overcommit_still_has_limit(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    hierarchy_l1b_headers, mock_upstream_url, mock_upstream,
):
    """Even with overcommit, exceeding total * ratio is rejected."""
    alias = unique_alias("ba-b5")
    uids: list[tuple[dict, str]] = []
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            parent = await create_upstream(
                client, oagw_base_url, hierarchy_root_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(100, sharing="inherit",
                               budget={"mode": "allocated", "total": 100, "overcommit_ratio": 1.5}),
            )
            uids.append((hierarchy_root_headers, parent["id"]))

            child_a = await create_upstream(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(100),
            )
            uids.append((hierarchy_l1a_headers, child_a["id"]))

            # 100 + 60 = 160 > 100 * 1.5 = 150 → rejected
            resp = await create_upstream_raw(
                client, oagw_base_url, hierarchy_l1b_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(60),
            )
            assert resp.status_code == 400, (
                f"Expected 400 for over-budget, got {resp.status_code}: {resp.text[:500]}"
            )
            assert "budget allocation exceeded" in resp.text
        finally:
            for hdrs, uid in reversed(uids):
                await delete_upstream(client, oagw_base_url, hdrs, uid)


@pytest.mark.asyncio
async def test_allocated_update_revalidates(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream,
):
    """Updating a child's rate to exceed budget is rejected."""
    alias = unique_alias("ba-b6")
    uids: list[tuple[dict, str]] = []
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            parent = await create_upstream(
                client, oagw_base_url, hierarchy_root_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(100, sharing="inherit",
                               budget={"mode": "allocated", "total": 100}),
            )
            uids.append((hierarchy_root_headers, parent["id"]))

            child = await create_upstream(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(50),
            )
            uids.append((hierarchy_l1a_headers, child["id"]))

            # Update child to 120/min → exceeds budget of 100
            resp = await update_upstream_raw(
                client, oagw_base_url, hierarchy_l1a_headers,
                child["id"], mock_upstream_url,
                alias=alias,
                rate_limit=_rl(120),
            )
            assert resp.status_code == 400, (
                f"Expected 400 on update, got {resp.status_code}: {resp.text[:500]}"
            )
            assert "budget allocation exceeded" in resp.text
        finally:
            for hdrs, uid in reversed(uids):
                await delete_upstream(client, oagw_base_url, hdrs, uid)


@pytest.mark.asyncio
async def test_allocated_different_windows_normalized(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream,
):
    """Rates are normalized to req/s: child 2/s exceeds parent 60/min budget."""
    alias = unique_alias("ba-b7")
    uids: list[tuple[dict, str]] = []
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            # Parent: 60/min = 1 req/s budget
            parent = await create_upstream(
                client, oagw_base_url, hierarchy_root_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(60, window="minute", sharing="inherit",
                               budget={"mode": "allocated", "total": 60}),
            )
            uids.append((hierarchy_root_headers, parent["id"]))

            # Child: 2/second = 2 req/s → exceeds 1 req/s budget
            resp = await create_upstream_raw(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(2, window="second"),
            )
            assert resp.status_code == 400, (
                f"Expected 400 for cross-window over-budget, got {resp.status_code}: {resp.text[:500]}"
            )
            assert "budget allocation exceeded" in resp.text
        finally:
            for hdrs, uid in reversed(uids):
                await delete_upstream(client, oagw_base_url, hdrs, uid)


@pytest.mark.asyncio
async def test_allocated_rejects_child_without_rate_limit(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream,
):
    """Allocated budget rejects child that omits rate_limit entirely."""
    alias = unique_alias("ba-b8")
    uids: list[tuple[dict, str]] = []
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            parent = await create_upstream(
                client, oagw_base_url, hierarchy_root_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(100, sharing="inherit",
                               budget={"mode": "allocated", "total": 100}),
            )
            uids.append((hierarchy_root_headers, parent["id"]))

            # Child with no rate_limit → rejected under allocated budget.
            resp = await create_upstream_raw(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias,
            )
            assert resp.status_code == 400, (
                f"Expected 400 for missing rate_limit, got {resp.status_code}: {resp.text[:500]}"
            )
            assert "rate_limit is required" in resp.text
        finally:
            for hdrs, uid in reversed(uids):
                await delete_upstream(client, oagw_base_url, hdrs, uid)


# ===================================================================
# Category C: Shared pool config acceptance
# ===================================================================


@pytest.mark.asyncio
async def test_shared_budget_config_accepted(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream,
):
    """Shared budget mode: parent and child creation succeeds."""
    alias = unique_alias("ba-c1")
    uids: list[tuple[dict, str]] = []
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            parent = await create_upstream(
                client, oagw_base_url, hierarchy_root_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(100, sharing="inherit",
                               budget={"mode": "shared", "total": 100}),
            )
            uids.append((hierarchy_root_headers, parent["id"]))

            # Child binds without overriding rate_limit → no allocation check
            child = await create_upstream(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias,
            )
            uids.append((hierarchy_l1a_headers, child["id"]))
        finally:
            for hdrs, uid in reversed(uids):
                await delete_upstream(client, oagw_base_url, hdrs, uid)


# ===================================================================
# Category D: Unlimited / no-budget defaults
# ===================================================================


@pytest.mark.asyncio
async def test_unlimited_no_child_validation(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream,
):
    """Unlimited budget mode skips allocation validation for children."""
    alias = unique_alias("ba-d1")
    uids: list[tuple[dict, str]] = []
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            parent = await create_upstream(
                client, oagw_base_url, hierarchy_root_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(100, sharing="inherit",
                               budget={"mode": "unlimited"}),
            )
            uids.append((hierarchy_root_headers, parent["id"]))

            # Any rate is fine — no budget validation
            child = await create_upstream(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(9999),
            )
            uids.append((hierarchy_l1a_headers, child["id"]))
        finally:
            for hdrs, uid in reversed(uids):
                await delete_upstream(client, oagw_base_url, hdrs, uid)


@pytest.mark.asyncio
async def test_no_budget_defaults_unlimited(
    oagw_base_url, hierarchy_root_headers, hierarchy_l1a_headers,
    mock_upstream_url, mock_upstream,
):
    """No budget field on parent defaults to unlimited — child accepted."""
    alias = unique_alias("ba-d2")
    uids: list[tuple[dict, str]] = []
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            parent = await create_upstream(
                client, oagw_base_url, hierarchy_root_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(100, sharing="inherit"),  # no budget field
            )
            uids.append((hierarchy_root_headers, parent["id"]))

            child = await create_upstream(
                client, oagw_base_url, hierarchy_l1a_headers, mock_upstream_url,
                alias=alias,
                rate_limit=_rl(9999),
            )
            uids.append((hierarchy_l1a_headers, child["id"]))
        finally:
            for hdrs, uid in reversed(uids):
                await delete_upstream(client, oagw_base_url, hdrs, uid)
