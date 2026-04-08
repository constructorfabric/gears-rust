"""Tests for the quota status endpoint and quota_warnings in the SSE done event.

Covers:
- GET /v1/quota/status returns quota breakdown with warning flags
- Quota usage increases after sending a message
- remaining_percentage decreases after usage
- next_reset timestamps are correct
- SSE done event includes quota_warnings array
- quota_warnings in done event is consistent with GET /v1/quota/status
"""

import time
from datetime import datetime, timezone

import httpx

from .conftest import API_PREFIX, expect_done, stream_message

import pytest


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def get_quota_status() -> dict:
    """Call GET /v1/quota/status and return the JSON response."""
    resp = httpx.get(f"{API_PREFIX}/quota/status", timeout=10)
    assert resp.status_code == 200, (
        f"GET /quota/status failed: {resp.status_code} {resp.text}"
    )
    return resp.json()


def find_period(tiers: list, tier_name: str, period_name: str) -> dict | None:
    """Find a specific tier+period entry in the quota status response."""
    for t in tiers:
        if t["tier"] == tier_name:
            for p in t["periods"]:
                if p["period"] == period_name:
                    return p
    return None


# ---------------------------------------------------------------------------
# Tests: GET /v1/quota/status endpoint
# ---------------------------------------------------------------------------

class TestQuotaStatusEndpoint:
    """GET /v1/quota/status returns quota breakdown."""

    def test_returns_200_with_tiers_and_threshold(self, server):
        status = get_quota_status()
        assert "tiers" in status
        assert isinstance(status["tiers"], list)
        assert len(status["tiers"]) > 0
        assert "warning_threshold_pct" in status
        assert 1 <= status["warning_threshold_pct"] <= 99

    def test_each_tier_has_periods(self, server):
        status = get_quota_status()
        for tier in status["tiers"]:
            assert "tier" in tier
            assert tier["tier"] in ("premium", "total")
            assert "periods" in tier
            assert len(tier["periods"]) > 0
            for period in tier["periods"]:
                assert period["period"] in ("daily", "monthly")
                assert "limit_credits_micro" in period
                assert "used_credits_micro" in period
                assert "remaining_credits_micro" in period
                assert "remaining_percentage" in period
                assert "next_reset" in period
                assert "warning" in period
                assert "exhausted" in period

    def test_remaining_percentage_is_valid(self, server):
        status = get_quota_status()
        for tier in status["tiers"]:
            for period in tier["periods"]:
                pct = period["remaining_percentage"]
                assert 0 <= pct <= 100, f"Invalid percentage: {pct}"

    def test_next_reset_is_future(self, server):
        status = get_quota_status()
        now = datetime.now(timezone.utc)
        for tier in status["tiers"]:
            for period in tier["periods"]:
                reset_str = period["next_reset"]
                # Parse ISO 8601 / RFC 3339
                reset = datetime.fromisoformat(reset_str.replace("Z", "+00:00"))
                assert reset > now, (
                    f"next_reset {reset_str} is not in the future (now: {now})"
                )


# ---------------------------------------------------------------------------
# Tests: Quota changes after sending a message
# ---------------------------------------------------------------------------

class TestQuotaUsageTracking:
    """Quota usage increases after sending messages."""

    def test_used_credits_increase_after_send(self, provider_chat):
        chat_id = provider_chat["id"]

        before = get_quota_status()
        before_total_daily = find_period(before["tiers"], "total", "daily")
        assert before_total_daily is not None, "Should have total/daily period"
        before_used = before_total_daily["used_credits_micro"]

        # Send a message
        status, events, _ = stream_message(chat_id, "Say OK.")
        assert status == 200
        expect_done(events)

        # Small delay for settlement
        time.sleep(0.5)

        after = get_quota_status()
        after_total_daily = find_period(after["tiers"], "total", "daily")
        after_used = after_total_daily["used_credits_micro"]

        assert after_used > before_used, (
            f"used_credits_micro should increase after send: "
            f"before={before_used}, after={after_used}"
        )

    def test_remaining_percentage_decreases_after_send(self, provider_chat):
        chat_id = provider_chat["id"]

        before = get_quota_status()
        before_total_daily = find_period(before["tiers"], "total", "daily")
        before_pct = before_total_daily["remaining_percentage"]

        # Send a message
        stream_message(chat_id, "Say hi.")

        time.sleep(0.5)

        after = get_quota_status()
        after_total_daily = find_period(after["tiers"], "total", "daily")
        after_pct = after_total_daily["remaining_percentage"]

        assert after_pct <= before_pct, (
            f"remaining_percentage should decrease: "
            f"before={before_pct}, after={after_pct}"
        )


# ---------------------------------------------------------------------------
# Tests: SSE done event includes quota_warnings
# ---------------------------------------------------------------------------

class TestQuotaWarningsInDoneEvent:
    """SSE done event carries quota_warnings array."""

    def test_done_event_has_quota_warnings(self, provider_chat):
        chat_id = provider_chat["id"]
        _, events, _ = stream_message(chat_id, "Say OK.")
        done = expect_done(events)

        warnings = done.data.get("quota_warnings")
        assert warnings is not None, (
            f"done event should have quota_warnings, got: {done.data.keys()}"
        )
        assert isinstance(warnings, list)
        assert len(warnings) > 0

        for w in warnings:
            assert w["tier"] in ("premium", "total")
            assert w["period"] in ("daily", "monthly")
            assert 0 <= w["remaining_percentage"] <= 100
            assert isinstance(w["warning"], bool)
            assert isinstance(w["exhausted"], bool)

    def test_quota_warnings_consistent_with_endpoint(self, provider_chat):
        chat_id = provider_chat["id"]
        _, events, _ = stream_message(chat_id, "Say hello.")
        done = expect_done(events)

        sse_warnings = done.data.get("quota_warnings", [])

        # Small delay then fetch endpoint
        time.sleep(0.3)
        endpoint_status = get_quota_status()

        # Compare each SSE warning entry with the endpoint
        for sw in sse_warnings:
            ep = find_period(
                endpoint_status["tiers"], sw["tier"], sw["period"]
            )
            assert ep is not None, (
                f"SSE warning tier={sw['tier']} period={sw['period']} "
                f"not found in endpoint response"
            )
            # Percentages should be close (may differ slightly due to timing)
            diff = abs(ep["remaining_percentage"] - sw["remaining_percentage"])
            assert diff <= 2, (
                f"remaining_percentage mismatch for {sw['tier']}/{sw['period']}: "
                f"SSE={sw['remaining_percentage']}, endpoint={ep['remaining_percentage']}"
            )
