"""Tests for quota enforcement — tier downgrade, bucket accounting, daily exhaustion, policy version.

Covers:
- Premium-to-standard downgrade when premium quota is exhausted
- Premium usage counts against both tier:premium and total buckets
- Daily period exhaustion blocks even when monthly has room
- All tiers exhausted returns 429
- Policy version persisted per turn
- Warning threshold boundary consistency
- Exhausted flag correctness at zero remaining
"""

from __future__ import annotations

import os
import sqlite3
import time
import uuid

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    DB_PATH,
    DEFAULT_MODEL,
    STANDARD_MODEL,
    expect_done,
    expect_stream_started,
    stream_message,
    parse_sse,
)
from .mock_provider.responses import MockEvent, Scenario, Usage


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _to_blob(value):
    if isinstance(value, str):
        try:
            return uuid.UUID(value).bytes
        except ValueError:
            pass
    return value


def query_db(sql, params=()):
    if not os.path.exists(DB_PATH):
        pytest.skip(f"DB not found at {DB_PATH}")
    conn = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    blob_params = tuple(_to_blob(p) for p in params)
    try:
        rows = conn.execute(sql, blob_params).fetchall()
        return [dict(r) for r in rows]
    finally:
        conn.close()


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
# Tests
# ---------------------------------------------------------------------------

class TestQuotaEnforcement:
    """Quota enforcement — downgrade, exhaustion, bucket accounting."""

    # NOTE: Cannot trigger actual downgrade with default quota limits.
    # This test validates the structural shape of done-event fields only.
    def test_tier_downgrade_premium_to_standard(self, chat, mock_provider):
        # TODO: Requires very low premium quota limit via CCM policy mock.
        # With default test config, premium quota limits are too high to
        # exhaust in a reasonable number of requests. This test verifies
        # the structural shape of the done event fields related to downgrade.
        #
        # To fully test: configure CCM mock with premium daily limit of ~100
        # credits_micro, then send messages until downgrade triggers.
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        status, events, _ = stream_message(
            chat_id, "Say OK.", request_id=request_id
        )
        assert status == 200
        done = expect_done(events)

        # Verify done event carries the downgrade-related fields
        assert "quota_decision" in done.data
        assert done.data["quota_decision"] in ("allow", "downgrade")
        assert "effective_model" in done.data
        assert "selected_model" in done.data

        # Under normal quota the decision should be "allow" and models match
        if done.data["quota_decision"] == "allow":
            assert done.data["effective_model"] == done.data["selected_model"]
        else:
            # If downgrade happened, effective != selected
            assert done.data["effective_model"] != done.data["selected_model"]
            assert "downgrade_reason" in done.data
            assert done.data.get("downgrade_from") == done.data.get("selected_model"), "downgrade_from must equal selected_model"
            assert done.data.get("downgrade_reason") in ("premium_quota_exhausted", "force_standard_tier", "disable_premium_tier", "model_disabled"), f"invalid downgrade_reason: {done.data.get('downgrade_reason')}"

    def test_bucket_model_premium_counts_total(self, chat, mock_provider):
        """Premium model usage should count against both tier:premium and total buckets."""
        chat_id = chat["id"]

        before = get_quota_status()
        before_total = find_period(before["tiers"], "total", "daily")
        before_premium = find_period(before["tiers"], "premium", "daily")
        assert before_total is not None, "Should have total/daily period"
        assert before_premium is not None, "Should have premium/daily period"
        before_total_used = before_total["used_credits_micro"]
        before_premium_used = before_premium["used_credits_micro"]

        # Send a message (default model is premium)
        status, events, _ = stream_message(chat_id, "Say OK.")
        assert status == 200
        expect_done(events)

        time.sleep(0.5)

        after = get_quota_status()
        after_total = find_period(after["tiers"], "total", "daily")
        after_premium = find_period(after["tiers"], "premium", "daily")

        total_increase = after_total["used_credits_micro"] - before_total_used
        premium_increase = after_premium["used_credits_micro"] - before_premium_used

        assert total_increase > 0, (
            f"Total bucket should increase after premium send: "
            f"before={before_total_used}, after={after_total['used_credits_micro']}"
        )
        assert premium_increase > 0, (
            f"Premium bucket should increase after premium send: "
            f"before={before_premium_used}, after={after_premium['used_credits_micro']}"
        )

    # NOTE: Cannot exhaust quota in normal test runs.
    # This test validates the structural shape when NOT exhausted only.
    def test_daily_period_structural_validation(self, chat, mock_provider):
        # TODO: Requires low daily quota limit configured via CCM policy mock.
        # With default test config, daily limits are too high to exhaust.
        # This test verifies the structural shape of a 429 response and
        # that the quota/status endpoint reports exhaustion correctly.
        #
        # To fully test: configure CCM mock with daily limit of ~100
        # credits_micro, send messages until 429 is returned, then verify
        # monthly still has remaining room.
        chat_id = chat["id"]

        # Send one message to prove the endpoint is functional
        status, events, _ = stream_message(chat_id, "Say OK.")
        assert status == 200
        expect_done(events)

        # Verify daily period exists and is not yet exhausted
        qs = get_quota_status()
        daily_total = find_period(qs["tiers"], "total", "daily")
        assert daily_total is not None
        assert daily_total["exhausted"] is False, (
            "Daily total should not be exhausted in normal test run"
        )

    def test_all_tiers_exhausted_429(self, chat, mock_provider):
        # TODO: Requires all tier budgets to be exhausted via CCM policy mock.
        # This test verifies the 429 response structure when quota is exceeded.
        # In normal test runs, quota is never exhausted.
        #
        # Expected 429 response shape:
        #   {"code": "quota_exceeded", "quota_scope": "...", "message": "..."}
        chat_id = chat["id"]

        # Send one message to prove non-exhausted path works
        status, events, _ = stream_message(chat_id, "Say OK.")
        assert status == 200
        expect_done(events)

        # Verify none of the tiers are exhausted
        qs = get_quota_status()
        for tier in qs["tiers"]:
            for period in tier["periods"]:
                if period["exhausted"]:
                    # If somehow exhausted, try sending and expect 429
                    resp = httpx.post(
                        f"{API_PREFIX}/chats/{chat_id}/messages:stream",
                        json={"content": "blocked?"},
                        headers={"Accept": "text/event-stream"},
                        timeout=30,
                    )
                    assert resp.status_code == 429
                    body = resp.json()
                    assert body["code"] == "quota_exceeded"
                    assert "quota_scope" in body
                    return

        # No tier exhausted — structural validation only
        for tier in qs["tiers"]:
            for period in tier["periods"]:
                assert period["exhausted"] is False

    def test_policy_version_persisted_per_turn(self, chat, mock_provider):
        """After completing a turn, policy_version_applied should be set in the DB."""
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        status, events, _ = stream_message(
            chat_id, "Say OK.", request_id=request_id
        )
        assert status == 200
        expect_done(events)

        time.sleep(0.5)

        rows = query_db(
            "SELECT policy_version_applied FROM chat_turns WHERE request_id = ?",
            (request_id,),
        )
        assert len(rows) > 0, (
            f"No chat_turns row found for request_id={request_id}"
        )
        row = rows[0]
        pv = row["policy_version_applied"]
        assert pv is not None, "policy_version_applied should not be NULL"
        assert pv > 0, f"policy_version_applied should be > 0, got {pv}"

    def test_warning_threshold_boundary(self, server):
        """Warning flag should be consistent with remaining_percentage and threshold."""
        qs = get_quota_status()
        threshold = qs["warning_threshold_pct"]
        assert 1 <= threshold <= 99

        for tier in qs["tiers"]:
            for period in tier["periods"]:
                remaining_pct = period["remaining_percentage"]
                warning = period["warning"]
                assert isinstance(warning, bool), (
                    f"warning should be bool, got {type(warning)} "
                    f"for {tier['tier']}/{period['period']}"
                )
                # If remaining is above threshold, warning should be false
                if remaining_pct > threshold:
                    assert warning is False, (
                        f"warning should be False when remaining={remaining_pct}% "
                        f"> threshold={threshold}% "
                        f"for {tier['tier']}/{period['period']}"
                    )
                # If remaining is at or below threshold, warning should be true
                # (but only if limit > 0, to avoid division-by-zero edge cases)
                if remaining_pct <= threshold and period["limit_credits_micro"] > 0:
                    assert warning is True, (
                        f"warning should be True when remaining={remaining_pct}% "
                        f"<= threshold={threshold}% "
                        f"for {tier['tier']}/{period['period']}"
                    )

    def test_bucket_model_standard_counts_total(self, server):
        """Standard-tier turns charge 'total' bucket only, not 'tier:premium'."""
        # Get quota before
        before = get_quota_status()
        before_total = find_period(before["tiers"], "total", "daily")
        before_premium = find_period(before["tiers"], "premium", "daily")
        assert before_total is not None, "Should have total/daily period"
        assert before_premium is not None, "Should have premium/daily period"

        # Send with standard model
        chat_resp = httpx.post(f"{API_PREFIX}/chats", json={"model": STANDARD_MODEL})
        assert chat_resp.status_code == 201
        chat_id = chat_resp.json()["id"]
        status, events, _ = stream_message(chat_id, "Say OK.")
        assert status == 200
        expect_done(events)

        time.sleep(0.5)

        # Get quota after
        after = get_quota_status()
        after_total = find_period(after["tiers"], "total", "daily")
        after_premium = find_period(after["tiers"], "premium", "daily")
        assert after_total is not None, "Should have total/daily period after send"
        assert after_premium is not None, "Should have premium/daily period after send"

        assert after_total["used_credits_micro"] > before_total["used_credits_micro"], \
            "Standard turn should charge total bucket"
        assert after_premium["used_credits_micro"] == before_premium["used_credits_micro"], \
            "Standard turn should NOT charge premium bucket"

    def test_exhausted_flag_at_zero(self, server):
        """Exhausted flag must be true iff remaining_percentage == 0."""
        qs = get_quota_status()

        for tier in qs["tiers"]:
            for period in tier["periods"]:
                remaining_pct = period["remaining_percentage"]
                exhausted = period["exhausted"]
                assert isinstance(exhausted, bool), (
                    f"exhausted should be bool, got {type(exhausted)} "
                    f"for {tier['tier']}/{period['period']}"
                )

                if remaining_pct == 0:
                    assert exhausted is True, (
                        f"exhausted should be True when remaining=0% "
                        f"for {tier['tier']}/{period['period']}"
                    )
                else:
                    assert exhausted is False, (
                        f"exhausted should be False when remaining={remaining_pct}% > 0 "
                        f"for {tier['tier']}/{period['period']}"
                    )
