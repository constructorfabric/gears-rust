"""Code interpreter usage verification tests.

Exercises code interpreter (XLSX upload) with the mock provider, then checks
quota usage via the REST quota endpoint (the `done` usage times the model's
multipliers), message tokens via the messages API, and the
code_interpreter_calls counter and reserves in the DB.
"""

from __future__ import annotations

import uuid
from datetime import datetime, timezone

import pytest
import httpx

from .conftest import (
    API_PREFIX, PROVIDER_DEFAULT_MODEL, USER_A_ID,
    assert_no_reserves, expect_done, find_period, get_quota_status, provider_usage, query_db,
    stream_message, usage_events,
)
from .test_attachments import _upload_ready
from .test_code_interpreter import XLSX_CONTENT_TYPE, _make_minimal_xlsx

# Compares or seeds daily usage (conftest `same_utc_day`).
pytestmark = pytest.mark.usefixtures("same_utc_day")


# ── DB helpers (quota_usage tool counters are not exposed via REST) ─────

def _query_ci_calls(user_id: str = USER_A_ID) -> int:
    """code_interpreter_calls of today's daily `total` quota_usage row."""
    rows = query_db(
        "SELECT code_interpreter_calls FROM quota_usage "
        "WHERE user_id = ? AND period_type = 'daily' "
        "AND period_start = ? AND bucket = 'total'",
        (user_id, datetime.now(timezone.utc).date().isoformat()),
    )
    return rows[0]["code_interpreter_calls"] if rows else 0


# (input, output) credit multipliers in credits_micro per token of each
# provider's default model (config/base.yaml).
MULTIPLIERS = {"openai": (1_000_000, 3_000_000), "azure": (3_000_000, 15_000_000)}


# ── Fixtures ─────────────────────────────────────────────────────────────

@pytest.fixture()
def xlsx_chat(provider):
    """Create a chat and upload a ready XLSX attachment."""
    model = PROVIDER_DEFAULT_MODEL[provider]
    resp = httpx.post(f"{API_PREFIX}/chats", json={"model": model})
    assert resp.status_code == 201
    chat = resp.json()
    chat_id = chat["id"]

    att_id = _upload_ready(chat_id, "data.xlsx", _make_minimal_xlsx(), XLSX_CONTENT_TYPE)

    return {"chat_id": chat_id, "att_id": att_id, "model": model}


@pytest.mark.multi_provider
@pytest.mark.usefixtures("offline_only")
class TestCodeInterpreterUsageAccounting:
    """Verify that code interpreter turns produce correct quota and message records.

    Offline only: the code interpreter call comes from the mock scenario."""

    @pytest.mark.timeout(20)
    def test_code_interpreter_usage_correct(self, provider, server, xlsx_chat, mock_provider):
        """Single CI turn: verify credits, messages, tool events, and turn state."""
        chat_id = xlsx_chat["chat_id"]
        att_id = xlsx_chat["att_id"]

        # Snapshot quota before
        spent_before = find_period(get_quota_status(), "total", "daily")["used_credits_micro"]

        rid = str(uuid.uuid4())
        mock_provider.clear_captured_requests()
        status, events, _ = stream_message(
            chat_id,
            "CODEINTERP: analyze the spreadsheet data",
            attachment_ids=[att_id],
            request_id=rid,
        )
        assert status == 200
        done = expect_done(events)

        sse_usage = provider_usage(mock_provider, done.data["usage"])
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
        # The CODEINTERP scenario has one code_interpreter call.
        assert len(ci_tool_dones) == 1, (
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
        assert len(asst_msgs) == 1, msgs  # a new chat with one turn
        m = asst_msgs[0]
        assert m["input_tokens"] == sse_input, (
            f"API input_tokens ({m['input_tokens']}) != SSE ({sse_input})"
        )
        assert m["output_tokens"] == sse_output, (
            f"API output_tokens ({m['output_tokens']}) != SSE ({sse_output})"
        )

        # ── Verify credits via quota endpoint ──
        assert sse_input > 0 and sse_output > 0, sse_usage
        assert_no_reserves(USER_A_ID)
        spent_after = find_period(get_quota_status(), "total", "daily")["used_credits_micro"]
        in_mult, out_mult = MULTIPLIERS[provider]
        cost = -(-sse_input * in_mult // 1_000_000) + -(-sse_output * out_mult // 1_000_000)
        assert spent_after - spent_before == cost

    @pytest.mark.timeout(20)
    def test_code_interpreter_calls_tracked_in_db(self, provider, server, xlsx_chat):
        """Verify code_interpreter_calls is incremented in quota_usage table."""
        chat_id = xlsx_chat["chat_id"]
        att_id = xlsx_chat["att_id"]

        # Get CI calls before
        ci_before = _query_ci_calls()

        status, events, _ = stream_message(
            chat_id,
            "CODEINTERP: what is the sum?",
            attachment_ids=[att_id],
        )
        assert status == 200
        expect_done(events)
        assert_no_reserves(USER_A_ID)

        # The CODEINTERP scenario emits exactly one completed code_interpreter call.
        assert _query_ci_calls() == ci_before + 1

    @pytest.mark.timeout(20)
    def test_non_ci_turn_has_zero_ci_calls(self, provider, server, mock_provider):
        """In a chat without an XLSX the same `CODEINTERP:` prompt does not
        offer the code_interpreter tool, so the mock (like the real API)
        runs no code: no code_interpreter tool events, no call counted in
        the turn's usage event or in quota_usage. The control with an XLSX
        is test_code_interpreter_calls_tracked_in_db."""
        model = PROVIDER_DEFAULT_MODEL[provider]

        resp = httpx.post(f"{API_PREFIX}/chats", json={"model": model})
        assert resp.status_code == 201
        chat_id = resp.json()["id"]

        ci_before = _query_ci_calls()

        rid = str(uuid.uuid4())
        mock_provider.clear_captured_requests()
        status, events, _ = stream_message(chat_id, "CODEINTERP: what is the sum?", request_id=rid)
        assert status == 200
        expect_done(events)
        assert_no_reserves(USER_A_ID)

        (req,) = mock_provider.get_captured_requests()
        assert "code_interpreter" not in [t.get("type") for t in req.get("tools") or []], req
        assert [e.data["name"] for e in events if e.event == "tool"] == [], events
        assert [u["code_interpreter_calls"] for u in usage_events(rid)] == [0]
        assert _query_ci_calls() == ci_before
