"""Tests for quota bucket accounting and policy version persistence.

Exhaustion, downgrade and warning flags need seeded usage and live in
test_quota_policy.py.
"""

from __future__ import annotations

import uuid

import pytest

from .conftest import (
    STANDARD_MODEL,
    USER_A_ID,
    assert_no_reserves,
    expect_done,
    find_period,
    get_quota_status,
    poll_turn,
    provider_usage,
    query_db,
    stream_message,
)

# Compares or seeds daily usage (conftest `same_utc_day`).
pytestmark = pytest.mark.usefixtures("same_utc_day")


def daily_used() -> tuple[int, int]:
    """(total daily, premium daily) used_credits_micro."""
    status = get_quota_status()
    return (
        find_period(status, "total", "daily")["used_credits_micro"],
        find_period(status, "premium", "daily")["used_credits_micro"],
    )


def send_first_message(chat_id: str, mock_provider) -> dict:
    """Send one message; return the usage the provider reported for it
    (conftest `provider_usage`)."""
    rid = str(uuid.uuid4())
    mock_provider.clear_captured_requests()
    status, events, _ = stream_message(chat_id, "Say OK.", request_id=rid)
    assert status == 200
    usage = provider_usage(mock_provider, expect_done(events).data["usage"])
    assert usage["input_tokens"] > 0 and usage["output_tokens"] > 0, usage
    poll_turn(chat_id, rid, ("done",))
    assert_no_reserves(USER_A_ID)
    return usage


def credits_micro(usage: dict, in_mult: int, out_mult: int) -> int:
    """ceil(tokens x multiplier_micro / 1e6) for input and for output."""
    return (
        -(-usage["input_tokens"] * in_mult // 1_000_000)
        + -(-usage["output_tokens"] * out_mult // 1_000_000)
    )


class TestQuotaEnforcement:
    """Bucket accounting for premium and standard models."""

    @pytest.mark.timeout(30)
    def test_bucket_model_premium_counts_total(self, chat, mock_provider):
        """A premium turn charges both `total` and `tier:premium` by its cost:
        the provider-reported usage times the azure-gpt-4.1 multipliers (3x
        input, 15x output, config/base.yaml)."""
        total_before, premium_before = daily_used()
        cost = credits_micro(
            send_first_message(chat["id"], mock_provider), 3_000_000, 15_000_000,
        )
        total_after, premium_after = daily_used()

        assert total_after - total_before == cost
        assert premium_after - premium_before == cost

    @pytest.mark.timeout(30)
    def test_bucket_model_standard_counts_total(self, chat_with_model, mock_provider):
        """A standard turn charges `total` only, by the provider-reported
        usage times the gpt-5.2 multipliers (1x input, 3x output,
        config/base.yaml)."""
        chat = chat_with_model(STANDARD_MODEL)
        total_before, premium_before = daily_used()
        cost = credits_micro(
            send_first_message(chat["id"], mock_provider), 1_000_000, 3_000_000,
        )
        total_after, premium_after = daily_used()

        assert total_after - total_before == cost
        assert premium_after == premium_before

    @pytest.mark.timeout(30)
    def test_policy_version_persisted_per_turn(self, chat):
        """The completed turn records `policy_version_applied` 1: the only
        version of the static model policy plugin (`SUPPORTED_POLICY_VERSION`
        in gears/mini-chat/mini-chat/src/infra/plugins/static_model_policy/service.rs)."""
        request_id = str(uuid.uuid4())
        status, events, _ = stream_message(chat["id"], "Say OK.", request_id=request_id)
        assert status == 200
        expect_done(events)
        poll_turn(chat["id"], request_id, ("done",))

        rows = query_db(
            "SELECT policy_version_applied FROM chat_turns WHERE request_id = ?",
            (request_id,),
        )
        assert rows == [{"policy_version_applied": 1}], rows
