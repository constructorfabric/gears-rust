"""Web search usage verification tests.

Exercises web search with the mock provider, then checks quota usage via the
REST quota endpoint (literal expected credits, see EXPECTED_CREDITS), message
tokens via the messages API, and that no quota reserve is left in the DB.

Provider-parameterized — runs against both OpenAI and Azure mock endpoints.
"""

from __future__ import annotations

import dataclasses
import uuid
from datetime import datetime, timezone

import pytest
import httpx

from .conftest import (
    API_PREFIX, PROVIDER_DEFAULT_MODEL, USER_A_ID,
    assert_no_reserves, expect_done, find_period, get_quota_status, parse_sse, query_db,
    stream_message,
)
from .mock_provider.responses import SCENARIOS, Usage

# Compares or seeds daily usage (conftest `same_utc_day`).
pytestmark = pytest.mark.usefixtures("same_utc_day")


def _query_ws_calls(user_id: str = USER_A_ID) -> int:
    """web_search_calls of today's daily `total` quota_usage row (not exposed
    over REST)."""
    rows = query_db(
        "SELECT web_search_calls FROM quota_usage "
        "WHERE user_id = ? AND period_type = 'daily' "
        "AND period_start = ? AND bucket = 'total'",
        (user_id, datetime.now(timezone.utc).date().isoformat()),
    )
    assert len(rows) == 1, rows
    return rows[0]["web_search_calls"]


# ── Expected charges ─────────────────────────────────────────────────────
#
# The test sends the mock's "SEARCH:*" answer with this usage, taken as is
# (`scale_input=False`: not the mock's per-input-item estimate).
# credits_micro = ceil(input * in_mult / 1e6) + ceil(output * out_mult / 1e6)
# with the base.yaml multipliers:
#   gpt-5.2        (openai): 400 * 1.0 + 50 * 3.0  =  400 + 150 =  550
#   azure-gpt-4.1  (azure):  400 * 3.0 + 50 * 15.0 = 1200 + 750 = 1950
SEARCH_USAGE = Usage(input_tokens=400, output_tokens=50, scale_input=False)
EXPECTED_USAGE = {"input_tokens": 400, "output_tokens": 50}
EXPECTED_CREDITS = {"openai": 550, "azure": 1950}


@pytest.mark.multi_provider
@pytest.mark.usefixtures("offline_only")
class TestWebSearchUsageAccounting:
    """Verify that web search turns produce correct quota and message records.

    Offline only: the test sets the provider usage (SEARCH_USAGE)."""

    @pytest.mark.timeout(20)
    def test_web_search_usage_correct(self, provider, server, mock_provider):
        """Single web-search turn: verify credits, messages, and turn state."""
        model = PROVIDER_DEFAULT_MODEL[provider]
        mock_provider.set_next_scenario(
            dataclasses.replace(SCENARIOS["SEARCH:*"], usage=SEARCH_USAGE),
        )

        # Snapshot quota before via REST
        spent_before = find_period(get_quota_status(), "total", "daily")["used_credits_micro"]

        resp = httpx.post(f"{API_PREFIX}/chats", json={"model": model})
        assert resp.status_code == 201
        chat = resp.json()
        chat_id = chat["id"]

        rid = str(uuid.uuid4())
        _url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        _resp = httpx.post(_url, json={"content": "SEARCH: current population of Tokyo", "web_search": {"enabled": True}, "request_id": rid}, headers={"Accept": "text/event-stream"}, timeout=90)
        status = _resp.status_code
        events = parse_sse(_resp.text) if status == 200 else []
        assert status == 200
        done = expect_done(events)

        sse_usage = done.data["usage"]
        sse_input = sse_usage["input_tokens"]
        sse_output = sse_usage["output_tokens"]

        tools = [(e.data["name"], e.data["phase"]) for e in events if e.event == "tool"]

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
        assert len(asst_msgs) == 1, msgs  # a new chat with one turn
        m = asst_msgs[0]
        assert m["input_tokens"] == sse_input, (
            f"API input_tokens ({m['input_tokens']}) != SSE ({sse_input})"
        )
        assert m["output_tokens"] == sse_output, (
            f"API output_tokens ({m['output_tokens']}) != SSE ({sse_output})"
        )

        # ── Verify credits via quota endpoint ──
        assert sse_usage == EXPECTED_USAGE
        assert_no_reserves(USER_A_ID)
        spent_after = find_period(get_quota_status(), "total", "daily")["used_credits_micro"]
        assert spent_after - spent_before == EXPECTED_CREDITS[provider]

        assert tools == [("web_search", "start"), ("web_search", "done")], tools

    @pytest.mark.timeout(20)
    def test_web_search_calls_counted(self, provider, chat_with_model):
        """The daily `total` usage row counts the completed web searches of a
        turn: the mock `SEARCH:*` answer has one."""
        chat_id = chat_with_model(PROVIDER_DEFAULT_MODEL[provider])["id"]
        # A turn creates today's row if it is missing.
        status, events, _ = stream_message(chat_id, "Say OK.")
        assert status == 200
        expect_done(events)
        assert_no_reserves(USER_A_ID)
        before = _query_ws_calls()

        status, events, _ = stream_message(
            chat_id, "SEARCH: population of Oslo", web_search={"enabled": True},
        )
        assert status == 200
        expect_done(events)
        assert_no_reserves(USER_A_ID)
        assert _query_ws_calls() == before + 1
