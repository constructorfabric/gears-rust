"""Tests for quota settlement — reserve release, actual settlement, cancel settlement.

Settlement internals are not visible over HTTP. These tests check the effects:
turn state, quota usage, and that `quota_usage.reserved_credits_micro` of the
user is back to 0 once the turn is terminal (read directly from the DB).
"""

import uuid

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    TENANT_A_ID,
    USER_A_ID,
    assert_no_reserves,
    expect_done,
    expect_stream_started,
    find_period,
    get_quota_status,
    open_stream,
    parse_sse,
    poll_turn,
    provider_usage,
    query_db,
    slow_scenario,
    stream_message,
    usage_events,
)
from .mock_provider.responses import MockEvent, Scenario, Usage

# Compares or seeds daily usage (conftest `same_utc_day`).
pytestmark = pytest.mark.usefixtures("same_utc_day")


def total_daily_used() -> int:
    return find_period(get_quota_status(), "total", "daily")["used_credits_micro"]


# ── Preflight estimate of a first message in a fresh azure-gpt-4.1 chat ──
#
# Estimation budgets come from the model's catalog entry. azure-gpt-4.1
# (base.yaml): bytes_per_token_conservative 4, fixed_overhead_tokens 500,
# safety_margin_pct 10, max_output_tokens 8192, 3 credits_micro per input token,
# 15 per output token. The generation floor is gear configuration
# (config.rs default minimal_generation_floor 50; the catalog value is not
# used), and the streaming cap is the default 32768.
# A first message has no prior context and no tools, so for `n` content bytes:
#   estimated input tokens = ceil((ceil(n / 4) + 500) * 110 / 100)
#   max_output_tokens_applied = min(8192, 32768) = 8192
#   reserve_tokens = estimated input tokens + 8192
#   reserved_credits_micro = estimated input * 3 + 8192 * 15
# The estimated settlement (DESIGN §5.8) charges the estimated input tokens
# plus the 50-token generation floor: estimated input * 3 + 50 * 15.
MAX_OUTPUT_TOKENS_APPLIED = 8192
MINIMAL_GENERATION_FLOOR = 50

# "Write a long essay." / "This should fail.": 19 / 17 bytes -> ceil(n/4) = 5
#   -> (5 + 500) * 1.1 = 555.5 -> 556 input tokens; charge 556 * 3 + 750 = 2418
# "Write slowly.": 13 bytes -> 4 -> (4 + 500) * 1.1 = 554.4 -> 555; 1665 + 750 = 2415
ESTIMATED_CHARGE = {
    "Write a long essay.": 2418,
    "Write slowly.": 2415,
    "This should fail.": 2418,
}


def assert_estimated_settlement(
    rid: str, used_before: int, billing_outcome: str, content: str,
) -> None:
    """The turn is settled on the estimate, never released (no-free-cancel rule):
    one usage event with `billing_outcome`, `settlement_method: estimated` and
    the estimated charge of `content` (ESTIMATED_CHARGE), which is added to
    the total daily usage."""
    assert_no_reserves(USER_A_ID)
    expected = ESTIMATED_CHARGE[content]
    events = usage_events(rid)
    assert len(events) == 1, events
    event = events[0]
    assert (event["billing_outcome"], event["settlement_method"]) == (
        billing_outcome, "estimated",
    ), event
    assert event["actual_credits_micro"] == expected, event
    assert total_daily_used() - used_before == expected


def _require_offline(request):
    if request.config.getoption("mode") == "online":
        pytest.skip("requires mock provider (offline mode)")


class TestSettlement:
    """Quota settlement after various turn outcomes."""

    @pytest.mark.multi_provider
    @pytest.mark.timeout(30)
    def test_completed_turn_releases_reserve(self, provider, provider_chat, mock_provider):
        """A completed turn leaves no reserve behind, its turn is done, and it
        is settled on the token counts the provider reported (offline the
        mock's `response.usage`, online the `done` usage): one usage event,
        `actual`, with their credits (base.yaml multipliers in credits_micro
        per token: gpt-5.2 1 in / 3 out, azure-gpt-4.1 3 in / 15 out)."""
        rid = str(uuid.uuid4())
        mock_provider.clear_captured_requests()
        status, events, raw = stream_message(provider_chat["id"], "Say OK.", request_id=rid)
        assert status == 200, raw
        usage = provider_usage(mock_provider, expect_done(events).data["usage"])

        assert poll_turn(provider_chat["id"], rid)["state"] == "done"
        assert_no_reserves(USER_A_ID)
        in_mult, out_mult = {"openai": (1, 3), "azure": (3, 15)}[provider]
        (event,) = usage_events(rid)
        assert (event["billing_outcome"], event["settlement_method"]) == (
            "completed", "actual",
        ), event
        assert event["actual_credits_micro"] == (
            usage["input_tokens"] * in_mult + usage["output_tokens"] * out_mult
        ), (event, usage)

    @pytest.mark.timeout(30)
    def test_reservation_snapshot_persisted(self, request, chat, mock_provider):
        """09-04, 14-01: the preflight reservation snapshot is on the turn row
        while the turn is still running, and completion leaves it unchanged.

        "Say OK." is 7 bytes: ceil(7 / 4) = 2 -> (2 + 500) * 1.1 = 552.2 -> 553
        estimated input tokens (see the estimate notes above), so
        reserve_tokens = 553 + 8192 = 8745 and
        reserved_credits_micro = 553 * 3 + 8192 * 15 = 124539."""
        _require_offline(request)
        chat_id = chat["id"]  # azure-gpt-4.1, no prior context
        rid = str(uuid.uuid4())
        expected = {
            "state": "running",
            "reserve_tokens": 8745,
            "max_output_tokens_applied": MAX_OUTPUT_TOKENS_APPLIED,
            "reserved_credits_micro": 124_539,
            "minimal_generation_floor_applied": MINIMAL_GENERATION_FLOOR,
            "effective_model": "azure-gpt-4.1",
        }
        columns = ", ".join(expected)

        def snapshot() -> dict:
            rows = query_db(f"SELECT {columns} FROM chat_turns WHERE request_id = ?", (rid,))
            assert len(rows) == 1, rows
            return rows[0]

        mock_provider.set_next_scenario(slow_scenario(5, slow=0.3))
        with open_stream(chat_id, "Say OK.", request_id=rid) as s:
            s.read_until_started()
            assert snapshot() == expected
            expect_done(s.drain())

        assert poll_turn(chat_id, rid, ("done",))["state"] == "done"
        assert snapshot() == {**expected, "state": "completed"}
        assert_no_reserves(USER_A_ID)

    @pytest.mark.timeout(30)
    def test_web_search_surcharge_in_reserve(self, request, chat, chat_with_model):
        """14-12: web search adds exactly `web_search_surcharge_tokens` to the
        reserve of the same first message.

        Each turn is the first message of its own azure-gpt-4.1 chat, so
        neither has prior context. The surcharge is added after the safety
        margin (gears/mini-chat/mini-chat/src/domain/service/token_estimator.rs,
        `estimate_tokens`), and the output part of the reserve is the same:
          "SEARCH: weather" is 15 bytes: ceil(15 / 4) = 4
          -> (4 + 500) * 110 / 100 = 554.4 -> 555 estimated input tokens
          plain:      reserve_tokens = 555 + 8192 = 8747,
                      credits = 555 * 3 + 8192 * 15 = 124545
          web search: +500 web_search_surcharge_tokens (base.yaml) ->
                      reserve_tokens = 9247, credits = 124545 + 500 * 3 = 126045
        """
        _require_offline(request)
        content = "SEARCH: weather"

        def reserve(chat_id: str, **extra) -> dict:
            rid = str(uuid.uuid4())
            status, events, raw = stream_message(chat_id, content, request_id=rid, **extra)
            assert status == 200, raw
            expect_done(events)
            poll_turn(chat_id, rid, ("done",))
            rows = query_db(
                "SELECT reserve_tokens, reserved_credits_micro FROM chat_turns "
                "WHERE request_id = ?", (rid,),
            )
            assert len(rows) == 1, rows
            return rows[0]

        ws = reserve(chat["id"], web_search={"enabled": True})
        plain = reserve(chat_with_model("azure-gpt-4.1")["id"])
        assert plain == {"reserve_tokens": 8747, "reserved_credits_micro": 124_545}, plain
        assert ws == {"reserve_tokens": 9247, "reserved_credits_micro": 126_045}, ws

    @pytest.mark.timeout(30)
    def test_cancelled_with_content(self, request, chat, mock_provider):
        """Disconnect after some deltas (the provider reported no usage): the
        turn is cancelled, the reserve released and the estimate charged
        (billing outcome `aborted`)."""
        _require_offline(request)
        rid = str(uuid.uuid4())
        used_before = total_daily_used()
        mock_provider.set_next_scenario(slow_scenario(20, slow=0.3))

        with open_stream(chat["id"], "Write a long essay.", request_id=rid) as s:
            seen = []
            s.read_until(lambda e: e.event == "delta" and (seen.append(e) or len(seen) == 3))

        assert poll_turn(chat["id"], rid)["state"] == "cancelled"
        assert_estimated_settlement(rid, used_before, "aborted", "Write a long essay.")

    @pytest.mark.timeout(30)
    def test_cancelled_without_content(self, request, chat, mock_provider):
        """Disconnect before any delta: the turn is cancelled, the reserve
        released and the estimate charged (billing outcome `aborted`)."""
        _require_offline(request)
        rid = str(uuid.uuid4())
        used_before = total_daily_used()
        scenario = slow_scenario(3, slow=0.5)
        scenario.initial_delay = 3.0
        mock_provider.set_next_scenario(scenario)

        with open_stream(chat["id"], "Write slowly.", request_id=rid) as s:
            s.read_until_started()

        assert poll_turn(chat["id"], rid)["state"] == "cancelled"
        assert_estimated_settlement(rid, used_before, "aborted", "Write slowly.")

    @pytest.mark.timeout(30)
    def test_provider_http_error_releases_reserve(self, request, chat, mock_provider):
        """A provider HTTP 500 ends the stream with `error` and fails the turn;
        the reserve is released and the estimate charged (billing outcome
        `failed`: the provider call had started)."""
        _require_offline(request)
        used_before = total_daily_used()
        mock_provider.set_next_scenario(Scenario(
            http_error_status=500,
            http_error_body={"error": {"message": "Internal server error", "type": "server_error"}},
        ))
        rid = str(uuid.uuid4())
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat['id']}/messages:stream",
            json={"content": "This should fail.", "request_id": rid},
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        assert [e.event for e in events] == ["stream_started", "error"]

        assert poll_turn(chat["id"], rid)["state"] == "error"
        assert_estimated_settlement(rid, used_before, "failed", "This should fail.")

    @pytest.mark.usefixtures("offline_only")
    @pytest.mark.timeout(30)
    @pytest.mark.parametrize("with_usage", [True, False], ids=["with_usage", "usage_null"])
    def test_response_failed_settles_on_reported_usage(self, chat, mock_provider, with_usage):
        """A `response.failed` that carries `response.usage` (the provider
        billed the tokens it produced) ends in SSE `error` and fails the
        turn; the turn is settled on that usage (billing outcome `failed`,
        `actual`): azure-gpt-4.1 charges 700 * 3 + 40 * 15 = 2700. With
        `usage: null` the estimate is charged, as for an HTTP error."""
        used_before = total_daily_used()
        mock_provider.set_next_scenario(Scenario(
            events=[MockEvent("response.output_text.delta", {"delta": "Partial"})],
            terminal="failed",
            error={"code": "server_error", "message": "Mock fail"},
            usage=Usage(input_tokens=700, output_tokens=40, scale_input=False),
            failed_with_usage=with_usage,
        ))
        rid = str(uuid.uuid4())
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat['id']}/messages:stream",
            json={"content": "This should fail.", "request_id": rid},
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )
        assert resp.status_code == 200
        events = parse_sse(resp.text)
        assert [e.event for e in events] == ["stream_started", "delta", "error"], events
        assert events[-1].data["code"] == "provider_error", events[-1].data
        assert poll_turn(chat["id"], rid)["state"] == "error"

        if not with_usage:
            assert_estimated_settlement(rid, used_before, "failed", "This should fail.")
            return
        assert_no_reserves(USER_A_ID)
        (event,) = usage_events(rid)
        assert (event["billing_outcome"], event["settlement_method"]) == (
            "failed", "actual",
        ), event
        assert event["actual_credits_micro"] == 700 * 3 + 40 * 15, event
        assert total_daily_used() - used_before == 700 * 3 + 40 * 15

    @pytest.mark.timeout(30)
    def test_incomplete_response_is_done_and_settled_on_actual_usage(
        self, request, chat, mock_provider,
    ):
        """A provider `response.incomplete` (mock `TRUNCATE`, reason
        max_output_tokens) ends in `done`, not `error`: the turn is completed
        with no error code, the truncated text is persisted, and it is settled
        on the actual usage of `response.usage` (DESIGN, response.incomplete)."""
        _require_offline(request)
        rid = str(uuid.uuid4())
        used_before = total_daily_used()
        mock_provider.clear_captured_requests()
        status, events, raw = stream_message(chat["id"], "TRUNCATE", request_id=rid)
        assert status == 200, raw
        usage = provider_usage(mock_provider, expect_done(events).data["usage"])
        assert usage["output_tokens"] == 100, usage

        assert poll_turn(chat["id"], rid)["state"] == "done"
        rows = query_db(
            "SELECT t.state, t.error_code, m.content FROM chat_turns t "
            "JOIN messages m ON m.id = t.assistant_message_id WHERE t.request_id = ?",
            (rid,),
        )
        assert rows == [{"state": "completed", "error_code": None, "content": "Truncated text"}], rows

        assert_no_reserves(USER_A_ID)
        # azure-gpt-4.1 (base.yaml): 3 and 15 credits_micro per token.
        cost = usage["input_tokens"] * 3 + usage["output_tokens"] * 15
        (event,) = usage_events(rid)
        assert (
            event["terminal_state"], event["billing_outcome"], event["settlement_method"],
        ) == ("completed", "completed", "actual"), event
        assert event["actual_credits_micro"] == cost, event
        assert total_daily_used() - used_before == cost

    @pytest.mark.usefixtures("offline_only")
    @pytest.mark.timeout(30)
    def test_cached_and_reasoning_tokens_recorded_not_billed(self, chat, mock_provider):
        """The provider reports 300 cached of 400 input tokens and 40
        reasoning of 60 output tokens (`input_tokens_details.cached_tokens`,
        `output_tokens_details.reasoning_tokens`). They are subsets of the
        totals, stored on the assistant message and sent in the usage event;
        the `done` usage carries only the totals. Credits use only the totals
        (DESIGN, "Credit computation in P1 uses only total input_tokens and
        output_tokens"): azure-gpt-4.1 (base.yaml, 3 and 15 credits_micro per
        token) charges 400 * 3 + 60 * 15 = 2100."""
        mock_provider.set_next_scenario(Scenario(
            events=[
                MockEvent("response.output_text.delta", {"delta": "Thought it through."}),
                MockEvent("response.output_text.done", {"text": "Thought it through."}),
            ],
            usage=Usage(
                input_tokens=400, output_tokens=60, cached_tokens=300,
                reasoning_tokens=40, scale_input=False,
            ),
        ))
        rid = str(uuid.uuid4())
        used_before = total_daily_used()
        status, events, raw = stream_message(chat["id"], "Think.", request_id=rid)
        assert status == 200, raw
        assert expect_done(events).data["usage"] == {"input_tokens": 400, "output_tokens": 60}

        assert poll_turn(chat["id"], rid)["state"] == "done"
        rows = query_db(
            "SELECT m.input_tokens, m.output_tokens, m.cache_read_input_tokens, "
            "m.cache_write_input_tokens, m.reasoning_tokens FROM chat_turns t "
            "JOIN messages m ON m.id = t.assistant_message_id WHERE t.request_id = ?",
            (rid,),
        )
        assert rows == [{
            "input_tokens": 400, "output_tokens": 60, "cache_read_input_tokens": 300,
            "cache_write_input_tokens": 0, "reasoning_tokens": 40,
        }], rows

        assert_no_reserves(USER_A_ID)
        (event,) = usage_events(rid)
        assert event["usage"] == {
            "input_tokens": 400, "output_tokens": 60, "cache_read_input_tokens": 300,
            "cache_write_input_tokens": 0, "reasoning_tokens": 40,
        }, event
        assert event["settlement_method"] == "actual", event
        assert event["actual_credits_micro"] == 2100, event
        assert total_daily_used() - used_before == 2100

    @pytest.mark.timeout(30)
    def test_one_usage_outbox_event_per_turn(self, chat):
        """A completed turn enqueues exactly one usage event, even after a
        replay (the replay itself succeeds: `done`, `is_new_turn` false)."""
        rid = str(uuid.uuid4())
        status, events, _ = stream_message(chat["id"], "Say hello.", request_id=rid)
        assert status == 200
        expect_done(events)
        poll_turn(chat["id"], rid, ("done",))

        status, replay, raw = stream_message(chat["id"], "Say hello.", request_id=rid)
        assert status == 200, raw
        assert expect_stream_started(replay).data["is_new_turn"] is False
        expect_done(replay)

        events = usage_events(rid)
        assert len(events) == 1, events
        event = events[0]
        assert event["terminal_state"] == "completed"
        # A user's turn; the thread summary's usage event is "system"
        # (test_thread_summary.py).
        assert event["requester_type"] == "user", event
        assert event["user_id"] == USER_A_ID, event
        # dedupe_key = {tenant_id}/{turn_id}/{request_id} (UUIDs in simple form).
        expected = "/".join(uuid.UUID(v).hex for v in (TENANT_A_ID, event["turn_id"], rid))
        assert event["dedupe_key"] == expected
