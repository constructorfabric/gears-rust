"""Code interpreter usage verification tests.

Exercises code interpreter (XLSX upload) with mock/real LLM, then verifies
quota usage via the REST quota endpoint and message tokens via the messages API.

Falls back to direct SQLite only for turn-level fields not exposed via REST
(reserve_tokens, reserved_credits_micro).

Follows the same patterns as test_web_search_usage.py.
"""

from __future__ import annotations

import io
import os
import sqlite3
import time
import uuid
from datetime import datetime, timezone

import pytest
import httpx

from .conftest import (
    API_PREFIX, DB_PATH, DEFAULT_MODEL, PROVIDER_DEFAULT_MODEL,
    expect_done, poll_until, stream_message,
)
from .test_code_interpreter import XLSX_CONTENT_TYPE, _make_minimal_xlsx


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


def _wait_for_quota_settled(before_used: int, *, timeout: float = 5.0, interval: float = 0.1):
    """Poll quota status until used_credits_micro changes (settlement landed)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        status = _get_quota_status()
        td = _find_period(status["tiers"], "total", "daily")
        if td and td["used_credits_micro"] != before_used:
            return status
        time.sleep(interval)
    # Return last snapshot even if unchanged (caller will assert)
    return _get_quota_status()


# ── DB helpers (only for turn-level fields not in REST API) ──────────────

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


def _get_chat_owner(chat_id: str) -> tuple[str, str]:
    """Return (tenant_id, user_id) for a chat from the DB."""
    rows = query_db(
        "SELECT tenant_id, user_id FROM chats WHERE id = ?",
        (chat_id,),
    )
    assert rows, f"Chat {chat_id} not found in DB"
    return rows[0]["tenant_id"], rows[0]["user_id"]


def _today_str() -> str:
    """Return the current UTC date as ISO string (matches quota period_start)."""
    return datetime.now(timezone.utc).date().isoformat()


def _query_ci_calls(chat_id: str) -> int:
    """Query code_interpreter_calls for the exact active quota_usage row."""
    tenant_id, user_id = _get_chat_owner(chat_id)
    rows = query_db(
        "SELECT code_interpreter_calls FROM quota_usage "
        "WHERE tenant_id = ? AND user_id = ? AND period_type = 'daily' "
        "AND period_start = ? AND bucket = 'total' LIMIT 1",
        (tenant_id, user_id, _today_str()),
    )
    return rows[0]["code_interpreter_calls"] if rows else 0


# ── Credit math helpers ──────────────────────────────────────────────────

def ceil_div(a: int, b: int) -> int:
    if a == 0 or b == 0:
        return 0
    return (a + b - 1) // b


def expected_credits_micro(input_tokens: int, output_tokens: int, in_mult: int, out_mult: int) -> int:
    divisor = 1_000_000
    return ceil_div(input_tokens * in_mult, divisor) + ceil_div(output_tokens * out_mult, divisor)


MODEL_MULTIPLIERS = {
    "gpt-5.2": (1_000_000, 3_000_000),
    "gpt-5-mini": (1_000_000, 3_000_000),
    "gpt-5-nano": (500_000, 1_500_000),
    "azure-gpt-4.1": (3_000_000, 15_000_000),
}


# ── Fixtures ─────────────────────────────────────────────────────────────

@pytest.fixture()
def xlsx_chat(provider):
    """Create a chat and upload a ready XLSX attachment."""
    model = PROVIDER_DEFAULT_MODEL[provider]
    resp = httpx.post(f"{API_PREFIX}/chats", json={"model": model})
    assert resp.status_code == 201
    chat = resp.json()
    chat_id = chat["id"]

    xlsx = _make_minimal_xlsx()
    resp = httpx.post(
        f"{API_PREFIX}/chats/{chat_id}/attachments",
        files={"file": ("data.xlsx", io.BytesIO(xlsx), XLSX_CONTENT_TYPE)},
        timeout=60,
    )
    assert resp.status_code == 201
    att_id = resp.json()["id"]
    resp = poll_until(
        lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
        until=lambda r: r.json()["status"] in ("ready", "failed"),
    )
    assert resp.json()["status"] == "ready", "Attachment did not become ready"

    return {"chat_id": chat_id, "att_id": att_id, "model": model}


@pytest.mark.openai
class TestCodeInterpreterUsageAccounting:
    """Verify that code interpreter turns produce correct quota and message records."""

    def test_code_interpreter_usage_correct(self, provider, server, xlsx_chat):
        """Single CI turn: verify credits, messages, tool events, and turn state."""
        chat_id = xlsx_chat["chat_id"]
        att_id = xlsx_chat["att_id"]
        model = xlsx_chat["model"]

        # Snapshot quota before
        before = _get_quota_status()
        before_td = _find_period(before["tiers"], "total", "daily")
        spent_before = before_td["used_credits_micro"]

        rid = str(uuid.uuid4())
        status, events, _ = stream_message(
            chat_id,
            "CODEINTERP: analyze the spreadsheet data",
            attachment_ids=[att_id],
            request_id=rid,
        )
        assert status == 200
        done = expect_done(events)

        sse_usage = done.data["usage"]
        sse_input = sse_usage["input_tokens"]
        sse_output = sse_usage["output_tokens"]

        # Verify code_interpreter tool events
        tool_events = [e for e in events if e.event == "tool"]
        ci_tool_dones = [
            e for e in tool_events
            if isinstance(e.data, dict)
            and e.data.get("phase") == "done"
            and e.data.get("name") == "code_interpreter"
        ]
        assert len(ci_tool_dones) >= 1, (
            f"Expected code_interpreter done event. "
            f"Tool events: {[t.data for t in tool_events]}"
        )

        # ── Verify turn state via REST ──
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/turns/{rid}")
        assert resp.status_code == 200
        turn = resp.json()
        assert turn["state"] == "done"
        assert turn["assistant_message_id"] is not None

        # ── Verify message tokens via REST ──
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        asst_msgs = [m for m in msgs if m["role"] == "assistant"]
        assert len(asst_msgs) >= 1
        m = asst_msgs[-1]
        assert m["input_tokens"] == sse_input, (
            f"API input_tokens ({m['input_tokens']}) != SSE ({sse_input})"
        )
        assert m["output_tokens"] == sse_output, (
            f"API output_tokens ({m['output_tokens']}) != SSE ({sse_output})"
        )

        # ── Verify credits via quota endpoint ──
        after = _wait_for_quota_settled(spent_before)
        after_td = _find_period(after["tiers"], "total", "daily")
        spent_after = after_td["used_credits_micro"]

        in_mult, out_mult = MODEL_MULTIPLIERS[model]
        formula_credits = expected_credits_micro(sse_input, sse_output, in_mult, out_mult)

        # Overshoot check via DB
        turns_db = query_db(
            "SELECT reserve_tokens, reserved_credits_micro FROM chat_turns WHERE chat_id = ? AND request_id = ?",
            (chat_id, rid),
        )
        assert len(turns_db) == 1
        reserve_tokens = turns_db[0]["reserve_tokens"]
        reserved_credits = turns_db[0]["reserved_credits_micro"]
        actual_tokens = sse_input + sse_output
        overshoot_tolerance = 1.1

        if actual_tokens > reserve_tokens and actual_tokens / max(reserve_tokens, 1) > overshoot_tolerance:
            expected_credits = reserved_credits
            capped = True
        else:
            expected_credits = formula_credits
            capped = False

        spent_delta = spent_after - spent_before

        print(f"  CI CREDIT VERIFICATION ({provider}/{model}):")
        print(f"    SSE tokens: input={sse_input}, output={sse_output}")
        print(f"    actual_tokens={actual_tokens}, reserve_tokens={reserve_tokens}")
        print(f"    Overshoot capped: {capped}")
        print(f"    Expected: {expected_credits}, Actual delta: {spent_delta}")

        assert spent_delta == expected_credits, (
            f"Credit mismatch: delta={spent_delta} != expected={expected_credits} "
            f"(formula={formula_credits}, capped={capped})"
        )

        # No stuck reserves
        for tier in after["tiers"]:
            for p in tier["periods"]:
                expected_remaining = p["limit_credits_micro"] - p["used_credits_micro"]
                assert p["remaining_credits_micro"] == expected_remaining, (
                    f"Stuck reserve in {tier['tier']}/{p['period']}"
                )

    def test_code_interpreter_calls_tracked_in_db(self, provider, server, xlsx_chat):
        """Verify code_interpreter_calls is incremented in quota_usage table."""
        chat_id = xlsx_chat["chat_id"]
        att_id = xlsx_chat["att_id"]

        # Get CI calls before
        ci_before = _query_ci_calls(chat_id)

        status, events, _ = stream_message(
            chat_id,
            "CODEINTERP: what is the sum?",
            attachment_ids=[att_id],
        )
        assert status == 200
        expect_done(events)

        # Poll until quota settles
        before_status = _get_quota_status()
        before_td = _find_period(before_status["tiers"], "total", "daily")
        _wait_for_quota_settled(before_td["used_credits_micro"])

        ci_after = _query_ci_calls(chat_id)

        # Mock provider emits exactly 1 code_interpreter completed event per CODEINTERP scenario
        assert ci_after > ci_before, (
            f"code_interpreter_calls not incremented: before={ci_before}, after={ci_after}"
        )

    def test_non_ci_turn_has_zero_ci_calls(self, provider, server):
        """A normal turn (no XLSX) should not increment code_interpreter_calls."""
        model = PROVIDER_DEFAULT_MODEL[provider]

        resp = httpx.post(f"{API_PREFIX}/chats", json={"model": model})
        assert resp.status_code == 201
        chat_id = resp.json()["id"]

        ci_before = _query_ci_calls(chat_id)

        before_status = _get_quota_status()
        before_td = _find_period(before_status["tiers"], "total", "daily")
        spent_before = before_td["used_credits_micro"]

        status, events, _ = stream_message(chat_id, "What is 2+2? Answer in one word.")
        assert status == 200
        expect_done(events)

        _wait_for_quota_settled(spent_before)

        ci_after = _query_ci_calls(chat_id)

        assert ci_after == ci_before, (
            f"code_interpreter_calls changed without CI: before={ci_before}, after={ci_after}"
        )

        # Verify no code_interpreter tool events
        tool_events = [e for e in events if e.event == "tool"]
        ci_events = [
            e for e in tool_events
            if isinstance(e.data, dict) and e.data.get("name") == "code_interpreter"
        ]
        assert len(ci_events) == 0, (
            f"Unexpected code_interpreter events: {[t.data for t in ci_events]}"
        )


@pytest.mark.openai
@pytest.mark.online_only
class TestCodeInterpreterUsageOnline:
    """Online test: verify code interpreter credit accounting with real provider."""

    def test_ci_credits_match_model_multipliers(self, provider, server, xlsx_chat):
        """Verify credit formula with real provider code interpreter."""
        chat_id = xlsx_chat["chat_id"]
        att_id = xlsx_chat["att_id"]
        model = xlsx_chat["model"]

        before = _get_quota_status()
        before_td = _find_period(before["tiers"], "total", "daily")
        spent_before = before_td["used_credits_micro"]

        rid = str(uuid.uuid4())
        status, events, _ = stream_message(
            chat_id,
            "Read the spreadsheet and tell me the value in cell B1.",
            attachment_ids=[att_id],
            request_id=rid,
        )
        assert status == 200
        done = expect_done(events)

        # Verify the turn actually invoked code_interpreter
        tool_events = [e for e in events if e.event == "tool"]
        ci_events = [
            e for e in tool_events
            if isinstance(e.data, dict) and e.data.get("name") == "code_interpreter"
            and e.data.get("phase") == "done"
        ]
        assert len(ci_events) >= 1, (
            f"Expected at least one code_interpreter done event, got {len(ci_events)}; "
            f"tool events: {[t.data for t in tool_events]}"
        )

        sse_input = done.data["usage"]["input_tokens"]
        sse_output = done.data["usage"]["output_tokens"]

        after = _wait_for_quota_settled(spent_before)
        after_td = _find_period(after["tiers"], "total", "daily")
        spent_after = after_td["used_credits_micro"]
        spent_delta = spent_after - spent_before

        in_mult, out_mult = MODEL_MULTIPLIERS[model]
        formula_credits = expected_credits_micro(sse_input, sse_output, in_mult, out_mult)

        turns_db = query_db(
            "SELECT reserve_tokens, reserved_credits_micro FROM chat_turns WHERE chat_id = ? AND request_id = ?",
            (chat_id, rid),
        )
        assert len(turns_db) == 1
        reserve_tokens = turns_db[0]["reserve_tokens"]
        reserved_credits = turns_db[0]["reserved_credits_micro"]
        actual_tokens = sse_input + sse_output
        overshoot_tolerance = 1.1

        if actual_tokens > reserve_tokens and actual_tokens / max(reserve_tokens, 1) > overshoot_tolerance:
            expected = reserved_credits
            capped = True
        else:
            expected = formula_credits
            capped = False

        print(f"  CI CREDIT VERIFICATION ({provider}/{model}):")
        print(f"    actual_tokens={actual_tokens}, reserve_tokens={reserve_tokens}")
        print(f"    Overshoot capped: {capped}")
        print(f"    Expected: {expected}, Actual delta: {spent_delta}")

        assert spent_delta == expected, (
            f"Credit mismatch for {provider}/{model}: "
            f"delta={spent_delta} != expected={expected}"
        )
