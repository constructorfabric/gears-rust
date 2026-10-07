"""Tests for the quota status endpoint and quota_warnings in the SSE done event.

Covers:
- GET /v1/quota/status returns quota breakdown with warning flags
- Quota usage increases after sending a message
- remaining_percentage decreases after usage
- next_reset timestamps are correct
- SSE done event includes quota_warnings array
- quota_warnings in done event is consistent with GET /v1/quota/status
"""

from __future__ import annotations

from datetime import datetime, timezone

from .conftest import (
    USER_A_ID, assert_no_reserves, expect_done, find_period, get_quota_status, provider_usage,
    stream_message,
)

import pytest

# Compares or seeds daily usage (conftest `same_utc_day`).
pytestmark = pytest.mark.usefixtures("same_utc_day")

# Credit multipliers (credits_micro per token: input, output) of the default
# models (config/base.yaml, *_tokens_credit_multiplier_micro / 1e6).
CREDIT_MULTIPLIERS = {"gpt-5.2": (1, 3), "azure-gpt-4.1": (3, 15)}


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
        # QuotaConfig default (config.rs), not overridden in config/base.yaml.
        assert status["warning_threshold_pct"] == 80

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

@pytest.mark.multi_provider
class TestQuotaUsageTracking:
    """Quota usage increases after sending messages."""

    @pytest.mark.timeout(20)
    def test_used_credits_increase_after_send(self, provider_chat, mock_provider):
        """Each completed turn adds its cost to the total daily usage.

        cost = input_tokens * input multiplier + output_tokens * output
        multiplier, in credits_micro per token (base.yaml: gpt-5.2 1 and 3,
        azure-gpt-4.1 3 and 15); the token counts are the ones the provider
        reported (conftest `provider_usage`).
        """
        chat_id = provider_chat["id"]
        in_mult, out_mult = CREDIT_MULTIPLIERS[provider_chat["model"]]
        before = find_period(get_quota_status(), "total", "daily")["used_credits_micro"]

        cost = 0
        for content in ("Say A.", "Say B."):
            mock_provider.clear_captured_requests()
            status, events, _ = stream_message(chat_id, content)
            assert status == 200
            usage = provider_usage(mock_provider, expect_done(events).data["usage"])
            cost += usage["input_tokens"] * in_mult + usage["output_tokens"] * out_mult
        assert_no_reserves(USER_A_ID)

        after = find_period(get_quota_status(), "total", "daily")["used_credits_micro"]
        assert after - before == cost

    @pytest.mark.timeout(20)
    def test_remaining_credits_decrease_after_send(self, provider_chat):
        """remaining_credits_micro strictly decreases after a charged turn.

        (remaining_percentage is an integer and stays at 99 for one small turn.)
        """
        chat_id = provider_chat["id"]
        before = find_period(get_quota_status(), "total", "daily")

        status, events, _ = stream_message(chat_id, "Say hi.")
        assert status == 200
        expect_done(events)
        assert_no_reserves(USER_A_ID)

        after = find_period(get_quota_status(), "total", "daily")
        assert after["remaining_credits_micro"] < before["remaining_credits_micro"]
        assert after["remaining_credits_micro"] == after["limit_credits_micro"] - after["used_credits_micro"]


# ---------------------------------------------------------------------------
# Tests: SSE done event includes quota_warnings
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
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

    @pytest.mark.timeout(20)
    def test_quota_warnings_consistent_with_endpoint(self, provider_chat):
        """Every `quota_warnings` entry of `done` equals the same tier/period of
        GET /quota/status read right after the turn."""
        chat_id = provider_chat["id"]
        _, events, _ = stream_message(chat_id, "Say hello.")
        sse_warnings = expect_done(events).data["quota_warnings"]
        assert_no_reserves(USER_A_ID)
        endpoint_status = get_quota_status()

        assert len(sse_warnings) > 0
        for sw in sse_warnings:
            ep = find_period(endpoint_status, sw["tier"], sw["period"])
            assert (sw["remaining_percentage"], sw["warning"], sw["exhausted"]) == (
                ep["remaining_percentage"], ep["warning"], ep["exhausted"],
            ), (sw, ep)
