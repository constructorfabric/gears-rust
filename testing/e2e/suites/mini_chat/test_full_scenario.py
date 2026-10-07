"""Full end-to-end scenario test.

Creates a chat, exchanges multiple messages, verifies message history,
checks turn records and quota usage, then cleans up.

Uses REST API endpoints for verification where possible; falls back to
direct SQLite queries only for fields not exposed via API (reserve_tokens,
max_output_tokens_applied, reserved_credits_micro).
"""

from __future__ import annotations

import uuid

import pytest
import httpx

from .conftest import (
    API_PREFIX,
    USER_A_ID,
    assert_no_reserves,
    expect_done,
    expect_stream_started,
    find_period,
    get_quota_status,
    parse_sse,
    query_db,
    stream_message,
)
from .test_quota_status import CREDIT_MULTIPLIERS

# Compares or seeds daily usage (conftest `same_utc_day`).
pytestmark = pytest.mark.usefixtures("same_utc_day")


def total_daily_used() -> int:
    return find_period(get_quota_status(), "total", "daily")["used_credits_micro"]


@pytest.mark.multi_provider
class TestFullConversationScenario:
    """Complete conversation lifecycle: create → multi-turn → verify → delete."""

    @pytest.mark.timeout(20)
    def test_full_conversation(self, server, provider_chat):
        # ── 1. Use provider-parameterized chat ───────────────────────────
        chat_id = provider_chat["id"]
        expected_model = provider_chat["model"]
        used_before = total_daily_used()

        # ── 2. Turn 1: simple question ───────────────────────────────────
        rid1 = str(uuid.uuid4())
        _url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        _resp1 = httpx.post(_url, json={"content": "What is 2+2? Reply with just the number.", "request_id": rid1}, headers={"Accept": "text/event-stream"}, timeout=90)
        s1 = _resp1.status_code
        ev1 = parse_sse(_resp1.text) if s1 == 200 else []
        assert s1 == 200

        ss1 = expect_stream_started(ev1)
        msg_id1 = ss1.data["message_id"]
        assert ss1.data["is_new_turn"] is True

        done1 = expect_done(ev1)
        assert done1.data["quota_decision"] == "allow"
        assert done1.data["effective_model"] == expected_model
        assert done1.data["selected_model"] == expected_model
        usage1 = done1.data["usage"]
        assert usage1["input_tokens"] > 0
        assert usage1["output_tokens"] > 0

        text1 = "".join(e.data["content"] for e in ev1 if e.event == "delta")
        assert len(text1.strip()) > 0

        # ── 3. Turn 2: follow-up referencing context ─────────────────────
        rid2 = str(uuid.uuid4())
        _resp2 = httpx.post(_url, json={"content": "Now multiply that result by 10.", "request_id": rid2}, headers={"Accept": "text/event-stream"}, timeout=90)
        s2 = _resp2.status_code
        ev2 = parse_sse(_resp2.text) if s2 == 200 else []
        assert s2 == 200

        ss2 = expect_stream_started(ev2)
        assert ss2.data["is_new_turn"] is True

        done2 = expect_done(ev2)
        assert done2.data["quota_decision"] == "allow"
        assert done2.data["effective_model"] == expected_model
        assert done2.data["selected_model"] == expected_model
        usage2 = done2.data["usage"]
        assert usage2["input_tokens"] > 0
        assert usage2["output_tokens"] > 0

        text2 = "".join(e.data["content"] for e in ev2 if e.event == "delta")
        assert len(text2.strip()) > 0

        # ── 4. Turn 3: third exchange ────────────────────────────────────
        rid3 = str(uuid.uuid4())
        _resp3 = httpx.post(_url, json={"content": "What was my first question?", "request_id": rid3}, headers={"Accept": "text/event-stream"}, timeout=90)
        s3 = _resp3.status_code
        ev3 = parse_sse(_resp3.text) if s3 == 200 else []
        assert s3 == 200

        ss3 = expect_stream_started(ev3)
        assert ss3.data["is_new_turn"] is True
        done3 = expect_done(ev3)
        assert done3.data["quota_decision"] == "allow"
        assert done3.data["effective_model"] == expected_model
        assert done3.data["selected_model"] == expected_model
        usage3 = done3.data["usage"]
        assert usage3["input_tokens"] > 0
        assert usage3["output_tokens"] > 0

        # ── 5. Verify message history via API ────────────────────────────
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        msgs = resp.json()["items"]

        assert len(msgs) == 6
        roles = [m["role"] for m in msgs]
        assert roles == ["user", "assistant"] * 3

        timestamps = [m["created_at"] for m in msgs]
        assert timestamps == sorted(timestamps)

        first_msg = msgs[0]
        assert first_msg.get("request_id") is not None, "request_id must be non-null"
        assert isinstance(first_msg.get("attachments"), list), "attachments must be an array"

        # ── 6. Verify chat metadata updated ──────────────────────────────
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 200
        assert resp.json()["message_count"] == 6

        # ── 7. Verify turn status via API ────────────────────────────────
        for rid in [rid1, rid2, rid3]:
            resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/turns/{rid}")
            assert resp.status_code == 200
            turn = resp.json()
            assert turn["state"] == "done"
            assert turn["assistant_message_id"] is not None

        # ── 8. Verify assistant messages have token counts (REST API) ────
        asst_msgs = [m for m in msgs if m["role"] == "assistant"]
        assert len(asst_msgs) == 3
        for m in asst_msgs:
            assert m.get("input_tokens") is not None and m["input_tokens"] > 0
            assert m.get("output_tokens") is not None and m["output_tokens"] > 0
            assert len(m["content"]) > 0

        # ── 9. Verify quota: each turn charged its cost, no reserve left ─
        # cost = input_tokens * input multiplier + output_tokens * output
        # multiplier (credits_micro per token, base.yaml).
        assert_no_reserves(USER_A_ID)
        in_mult, out_mult = CREDIT_MULTIPLIERS[expected_model]
        cost = sum(
            u["input_tokens"] * in_mult + u["output_tokens"] * out_mult
            for u in (usage1, usage2, usage3)
        )
        assert total_daily_used() - used_before == cost

        # ── 10. Idempotency: replay turn 1 ───────────────────────────────
        _resp_replay = httpx.post(_url, json={"content": "What is 2+2? Reply with just the number.", "request_id": rid1}, headers={"Accept": "text/event-stream"}, timeout=90)
        s_replay = _resp_replay.status_code
        ev_replay = parse_sse(_resp_replay.text) if s_replay == 200 else []
        assert s_replay == 200
        ss_replay = expect_stream_started(ev_replay)
        assert ss_replay.data["message_id"] == msg_id1
        assert ss_replay.data["is_new_turn"] is False

        # The replay charged nothing.
        assert_no_reserves(USER_A_ID)
        assert total_daily_used() - used_before == cost

        # ── 11. Delete chat ──────────────────────────────────────────────
        resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 204

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 404


@pytest.mark.multi_provider
class TestTurnDetailsInDb:
    """Verify turn-level DB fields not exposed via REST API.

    These fields (reserve_tokens, max_output_tokens_applied, reserved_credits_micro)
    are internal to the quota/reservation system and have no REST equivalent.
    """

    def test_max_output_tokens_applied(self, server, provider_chat):
        """max_output_tokens_applied = min(catalog max_output_tokens, streaming cap).

        Both default models have max_output_tokens 8192 (base.yaml); the
        streaming cap defaults to 32768, so 8192 applies.
        """
        chat_id = provider_chat["id"]
        rid = str(uuid.uuid4())
        status, events, _ = stream_message(chat_id, "Say hi.", request_id=rid)
        assert status == 200
        expect_done(events)

        turns = query_db(
            "SELECT max_output_tokens_applied, reserve_tokens FROM chat_turns "
            "WHERE chat_id = ? AND request_id = ?",
            (chat_id, rid),
        )
        assert len(turns) == 1
        assert turns[0]["max_output_tokens_applied"] == 8192
        # reserve_tokens = estimated input tokens + max_output_tokens_applied
        assert turns[0]["reserve_tokens"] > 8192
