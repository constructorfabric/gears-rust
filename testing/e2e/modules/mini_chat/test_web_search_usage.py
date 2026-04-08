"""Web search usage verification tests.

Exercises web search with mock/real LLM, then verifies quota usage via
the REST quota endpoint and message tokens via the messages API.

Falls back to direct SQLite only for turn-level fields not exposed via REST
(reserve_tokens, reserved_credits_micro).

Provider-parameterized — runs against both OpenAI and Azure.
"""

import os
import sqlite3
import time
import uuid

import pytest
import httpx

from .conftest import (
    API_PREFIX, DB_PATH, DEFAULT_MODEL, STANDARD_MODEL, PROVIDER_DEFAULT_MODEL,
    expect_done, stream_message,
)


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


# ── Credit math helpers ──────────────────────────────────────────────────

def ceil_div(a: int, b: int) -> int:
    if a == 0 or b == 0:
        return 0
    return (a + b - 1) // b


def expected_credits_micro(input_tokens: int, output_tokens: int, in_mult: int, out_mult: int) -> int:
    divisor = 1_000_000
    return ceil_div(input_tokens * in_mult, divisor) + ceil_div(output_tokens * out_mult, divisor)


MODEL_MULTIPLIERS = {
    "gpt-5.2": (3_000_000, 15_000_000),
    "gpt-5-mini": (1_000_000, 3_000_000),
    "gpt-5-nano": (500_000, 1_500_000),
    "azure-gpt-4.1-mini": (1_000_000, 3_000_000),
}


class TestWebSearchUsageAccounting:
    """Verify that web search turns produce correct quota and message records."""

    def test_web_search_usage_correct(self, provider, server):
        """Single web-search turn: verify credits, messages, and turn state."""
        model = PROVIDER_DEFAULT_MODEL[provider]

        # Snapshot quota before via REST
        before = _get_quota_status()
        before_td = _find_period(before["tiers"], "total", "daily")
        spent_before = before_td["used_credits_micro"]

        resp = httpx.post(f"{API_PREFIX}/chats", json={"model": model})
        assert resp.status_code == 201
        chat = resp.json()
        chat_id = chat["id"]

        rid = str(uuid.uuid4())
        status, events, _ = stream_message(
            chat_id,
            "SEARCH: current population of Tokyo",
            web_search={"enabled": True},
            request_id=rid,
        )
        assert status == 200
        done = expect_done(events)

        sse_usage = done.data["usage"]
        sse_input = sse_usage["input_tokens"]
        sse_output = sse_usage["output_tokens"]

        tool_events = [e for e in events if e.event == "tool"]
        ws_tool_dones = [
            e for e in tool_events
            if isinstance(e.data, dict)
            and e.data.get("phase") == "done"
            and e.data.get("name") in ("web_search", "web_search_preview")
        ]

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
        time.sleep(0.5)  # settlement delay
        after = _get_quota_status()
        after_td = _find_period(after["tiers"], "total", "daily")
        spent_after = after_td["used_credits_micro"]

        # Credit formula with overshoot cap
        in_mult, out_mult = MODEL_MULTIPLIERS[model]
        formula_credits = expected_credits_micro(sse_input, sse_output, in_mult, out_mult)

        # Need turn DB data for overshoot check (reserve_tokens not in REST)
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

        print(f"  CREDIT VERIFICATION ({provider}/{model}):")
        print(f"    SSE tokens: input={sse_input}, output={sse_output}")
        print(f"    actual_tokens={actual_tokens}, reserve_tokens={reserve_tokens}")
        print(f"    Overshoot capped: {capped}")
        print(f"    Expected: {expected_credits}, Actual delta: {spent_delta}")

        assert spent_delta == expected_credits, (
            f"Credit mismatch: delta={spent_delta} != expected={expected_credits} "
            f"(formula={formula_credits}, capped={capped})"
        )

        # ws_delta via tool events
        if ws_tool_dones:
            # Can't check ws_calls via API (not exposed), but tool events confirm it happened
            pass

        # No stuck reserves
        for tier in after["tiers"]:
            for p in tier["periods"]:
                expected_remaining = p["limit_credits_micro"] - p["used_credits_micro"]
                assert p["remaining_credits_micro"] == expected_remaining, (
                    f"Stuck reserve in {tier['tier']}/{p['period']}"
                )

    def test_non_websearch_has_zero_ws_calls(self, provider, server):
        """A normal turn (no web_search) should not change credits more than expected."""
        model = PROVIDER_DEFAULT_MODEL[provider]

        before = _get_quota_status()
        before_td = _find_period(before["tiers"], "total", "daily")
        spent_before = before_td["used_credits_micro"]

        resp = httpx.post(f"{API_PREFIX}/chats", json={"model": model})
        chat_id = resp.json()["id"]

        status, events, _ = stream_message(chat_id, "What is 2+2? Answer in one word.")
        assert status == 200
        done = expect_done(events)

        time.sleep(0.5)

        after = _get_quota_status()
        after_td = _find_period(after["tiers"], "total", "daily")
        spent_after = after_td["used_credits_micro"]

        # Credits should increase (message was processed)
        assert spent_after > spent_before

        # Verify no tool events (no web search happened)
        tool_events = [e for e in events if e.event == "tool"]
        assert len(tool_events) == 0, (
            f"Unexpected tool events without web_search: {[t.data for t in tool_events]}"
        )

    @pytest.mark.online_only
    def test_web_search_credits_match_model_multipliers(self, provider, chat_with_model, server):
        """Verify credit formula with real provider web search (overshoot-cap logic)."""
        model = PROVIDER_DEFAULT_MODEL[provider]
        chat = chat_with_model(model)
        chat_id = chat["id"]

        before = _get_quota_status()
        before_td = _find_period(before["tiers"], "total", "daily")
        spent_before = before_td["used_credits_micro"]

        rid = str(uuid.uuid4())
        status, events, _ = stream_message(
            chat_id,
            "Search the web: who invented the telephone?",
            web_search={"enabled": True},
            request_id=rid,
        )
        assert status == 200
        done = expect_done(events)

        sse_input = done.data["usage"]["input_tokens"]
        sse_output = done.data["usage"]["output_tokens"]

        time.sleep(0.5)

        after = _get_quota_status()
        after_td = _find_period(after["tiers"], "total", "daily")
        spent_after = after_td["used_credits_micro"]
        spent_delta = spent_after - spent_before

        in_mult, out_mult = MODEL_MULTIPLIERS[model]
        formula_credits = expected_credits_micro(sse_input, sse_output, in_mult, out_mult)

        # Overshoot check via DB (reserve_tokens not in REST)
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

        print(f"  CREDIT VERIFICATION ({provider}/{model}):")
        print(f"    actual_tokens={actual_tokens}, reserve_tokens={reserve_tokens}")
        print(f"    Overshoot capped: {capped}")
        print(f"    Expected: {expected}, Actual delta: {spent_delta}")

        assert spent_delta == expected, (
            f"Credit mismatch for {provider}/{model}: "
            f"delta={spent_delta} != expected={expected}"
        )
