"""Full end-to-end scenario test.

Creates a chat, exchanges multiple messages, verifies message history,
checks turn records and quota usage, then cleans up.

Uses REST API endpoints for verification where possible; falls back to
direct SQLite queries only for fields not exposed via API (reserve_tokens,
max_output_tokens_applied, reserved_credits_micro).
"""

import os
import sqlite3
import time
import uuid

import pytest
import httpx

from .conftest import API_PREFIX, DB_PATH, DEFAULT_MODEL, STANDARD_MODEL, expect_done, expect_stream_started, stream_message

pytestmark = pytest.mark.openai


# ── DB helpers (only for fields not exposed via REST) ────────────────────

def _to_blob(value):
    if isinstance(value, str):
        try:
            return uuid.UUID(value).bytes
        except ValueError:
            pass
    return value


def query_db(sql: str, params: tuple = ()) -> list[dict]:
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


# ── Quota endpoint helpers ───────────────────────────────────────────────

def _get_quota_status() -> dict:
    resp = httpx.get(f"{API_PREFIX}/quota/status", timeout=10)
    assert resp.status_code == 200
    return resp.json()


def _find_period(tiers: list, tier_name: str, period_name: str) -> dict | None:
    for t in tiers:
        if t["tier"] == tier_name:
            for p in t["periods"]:
                if p["period"] == period_name:
                    return p
    return None


class TestFullConversationScenario:
    """Complete conversation lifecycle: create → multi-turn → verify → delete."""

    def test_full_conversation(self, server):
        # ── 1. Create chat with default (premium) model ──────────────────
        resp = httpx.post(f"{API_PREFIX}/chats", json={"title": "Scenario Test"})
        assert resp.status_code == 201
        chat = resp.json()
        chat_id = chat["id"]
        assert chat["model"] == DEFAULT_MODEL
        assert chat["title"] == "Scenario Test"

        # ── 2. Turn 1: simple question ───────────────────────────────────
        rid1 = str(uuid.uuid4())
        s1, ev1, _ = stream_message(chat_id, "What is 2+2? Reply with just the number.", request_id=rid1)
        assert s1 == 200

        ss1 = expect_stream_started(ev1)
        msg_id1 = ss1.data["message_id"]
        assert ss1.data["is_new_turn"] is True

        done1 = expect_done(ev1)
        assert done1.data["quota_decision"] == "allow"
        assert done1.data["effective_model"] == DEFAULT_MODEL
        usage1 = done1.data["usage"]
        assert usage1["input_tokens"] > 0
        assert usage1["output_tokens"] > 0

        text1 = "".join(e.data["content"] for e in ev1 if e.event == "delta")
        assert len(text1.strip()) > 0

        # ── 3. Turn 2: follow-up referencing context ─────────────────────
        rid2 = str(uuid.uuid4())
        s2, ev2, _ = stream_message(chat_id, "Now multiply that result by 10.", request_id=rid2)
        assert s2 == 200

        ss2 = expect_stream_started(ev2)
        msg_id2 = ss2.data["message_id"]
        assert ss2.data["is_new_turn"] is True

        done2 = expect_done(ev2)
        usage2 = done2.data["usage"]

        assert usage2["input_tokens"] > usage1["input_tokens"], (
            f"Turn 2 input_tokens ({usage2['input_tokens']}) should exceed "
            f"turn 1 ({usage1['input_tokens']}) — context assembly sends history"
        )

        text2 = "".join(e.data["content"] for e in ev2 if e.event == "delta")
        assert len(text2.strip()) > 0

        # ── 4. Turn 3: third exchange ────────────────────────────────────
        rid3 = str(uuid.uuid4())
        s3, ev3, _ = stream_message(chat_id, "What was my first question?", request_id=rid3)
        assert s3 == 200

        ss3 = expect_stream_started(ev3)
        msg_id3 = ss3.data["message_id"]
        assert ss3.data["is_new_turn"] is True
        expect_done(ev3)

        # ── 5. Verify message history via API ────────────────────────────
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        msgs = resp.json()["items"]

        assert len(msgs) == 6
        roles = [m["role"] for m in msgs]
        assert roles == ["user", "assistant"] * 3

        timestamps = [m["created_at"] for m in msgs]
        assert timestamps == sorted(timestamps)

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

        # ── 9. Verify quota via REST endpoint ────────────────────────────
        time.sleep(0.5)  # settlement delay
        status = _get_quota_status()

        total_daily = _find_period(status["tiers"], "total", "daily")
        assert total_daily is not None
        assert total_daily["used_credits_micro"] > 0

        # No stuck reserves: remaining = limit - used
        for tier in status["tiers"]:
            for p in tier["periods"]:
                expected_remaining = p["limit_credits_micro"] - p["used_credits_micro"]
                assert p["remaining_credits_micro"] == expected_remaining, (
                    f"Stuck reserve in {tier['tier']}/{p['period']}"
                )

        # ── 10. Idempotency: replay turn 1 ───────────────────────────────
        s_replay, ev_replay, _ = stream_message(
            chat_id, "What is 2+2? Reply with just the number.", request_id=rid1,
        )
        assert s_replay == 200
        ss_replay = expect_stream_started(ev_replay)
        assert ss_replay.data["message_id"] == msg_id1
        assert ss_replay.data["is_new_turn"] is False

        # ── 11. Delete chat ──────────────────────────────────────────────
        resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 204

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 404


class TestQuotaAccumulation:
    """Verify quota accumulates correctly across multiple turns."""

    def test_quota_credits_accumulate(self, server):
        """Each turn should add to used_credits_micro via quota status endpoint."""
        before = _get_quota_status()
        before_total_daily = _find_period(before["tiers"], "total", "daily")
        assert before_total_daily is not None
        spent_before = before_total_daily["used_credits_micro"]

        resp = httpx.post(f"{API_PREFIX}/chats", json={})
        chat_id = resp.json()["id"]

        stream_message(chat_id, "Say A.")
        stream_message(chat_id, "Say B.")

        time.sleep(0.5)

        after = _get_quota_status()
        after_total_daily = _find_period(after["tiers"], "total", "daily")
        assert after_total_daily is not None
        spent_after = after_total_daily["used_credits_micro"]

        assert spent_after > spent_before, (
            f"used_credits_micro should increase: before={spent_before}, after={spent_after}"
        )

    def test_no_stuck_reserves_after_completion(self, server):
        """After all turns complete, remaining should equal limit - used."""
        resp = httpx.post(f"{API_PREFIX}/chats", json={})
        chat_id = resp.json()["id"]

        stream_message(chat_id, "Hello.")
        time.sleep(0.5)

        status = _get_quota_status()
        for tier in status["tiers"]:
            for p in tier["periods"]:
                expected_remaining = p["limit_credits_micro"] - p["used_credits_micro"]
                assert p["remaining_credits_micro"] == expected_remaining, (
                    f"Stuck reserve in {tier['tier']}/{p['period']}: "
                    f"remaining={p['remaining_credits_micro']} != "
                    f"limit({p['limit_credits_micro']}) - used({p['used_credits_micro']})"
                )


class TestTurnDetailsInDb:
    """Verify turn-level DB fields not exposed via REST API.

    These fields (reserve_tokens, max_output_tokens_applied, reserved_credits_micro)
    are internal to the quota/reservation system and have no REST equivalent.
    """

    def test_max_output_tokens_applied(self, server):
        """max_output_tokens_applied should reflect min(catalog, config_cap)."""
        resp = httpx.post(f"{API_PREFIX}/chats", json={})
        chat_id = resp.json()["id"]
        rid = str(uuid.uuid4())

        stream_message(chat_id, "Say hi.", request_id=rid)

        turns = query_db(
            "SELECT * FROM chat_turns WHERE chat_id = ? AND request_id = ?",
            (chat_id, rid),
        )
        assert len(turns) == 1
        t = turns[0]

        assert t["max_output_tokens_applied"] is not None
        assert t["max_output_tokens_applied"] > 0
        assert t["max_output_tokens_applied"] <= 8192

    def test_reserve_tokens_formula(self, server):
        """reserve_tokens = estimated_input_tokens + max_output_tokens_applied."""
        resp = httpx.post(f"{API_PREFIX}/chats", json={})
        chat_id = resp.json()["id"]
        rid = str(uuid.uuid4())

        stream_message(chat_id, "Hello.", request_id=rid)

        turns = query_db(
            "SELECT * FROM chat_turns WHERE chat_id = ? AND request_id = ?",
            (chat_id, rid),
        )
        t = turns[0]

        assert t["reserve_tokens"] > t["max_output_tokens_applied"]

    def test_credits_settled_after_completion(self, server):
        """After completion, no stuck reserves in quota (verified via REST)."""
        resp = httpx.post(f"{API_PREFIX}/chats", json={})
        chat_id = resp.json()["id"]

        stream_message(chat_id, "Say OK.")
        time.sleep(0.5)

        status = _get_quota_status()
        for tier in status["tiers"]:
            for p in tier["periods"]:
                expected_remaining = p["limit_credits_micro"] - p["used_credits_micro"]
                assert p["remaining_credits_micro"] == expected_remaining, (
                    f"Stuck reserve in {tier['tier']}/{p['period']}"
                )
