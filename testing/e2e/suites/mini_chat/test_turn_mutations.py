"""Tests for turn mutation operations — retry, delete, concurrency, replaced_by tracking."""

import os
import sqlite3
import threading
import time
import uuid

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    DB_PATH,
    expect_done,
    expect_stream_started,
    parse_sse,
    stream_message,
)
from .mock_provider.responses import MockEvent, Scenario


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


def poll_turn_state(chat_id: str, request_id: str, target: str,
                    timeout: float = 15.0) -> dict:
    """Poll GET /turns/{request_id} until the target state or timeout."""
    deadline = time.monotonic() + timeout
    body = None
    while time.monotonic() < deadline:
        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/turns/{request_id}", timeout=5
        )
        if resp.status_code == 200:
            body = resp.json()
            if body["state"] == target:
                return body
        time.sleep(0.3)
    state = body["state"] if body else "no response"
    raise AssertionError(
        f"Turn {request_id} did not reach '{target}' within {timeout}s "
        f"(last state: {state})"
    )


def complete_turn(chat_id: str, content: str = "Say OK.") -> str:
    """Send a message and return its request_id after confirming done."""
    rid = str(uuid.uuid4())
    status, events, _ = stream_message(chat_id, content, request_id=rid)
    assert status == 200, f"stream_message failed: {status}"
    expect_done(events)
    return rid


# ---------------------------------------------------------------------------
# Tests: retry
# ---------------------------------------------------------------------------

class TestTurnRetry:
    """POST /turns/{request_id}/retry constraints."""

    @pytest.mark.xfail(reason="BUG: error_code not set on 400/409 turn mutation responses")
    def test_retry_running_turn_400(self, chat, mock_provider):
        """Cannot retry a turn that is still running."""
        chat_id = chat["id"]
        rid = str(uuid.uuid4())

        # Slow scenario keeps the turn in-flight
        many_deltas = [
            MockEvent("response.output_text.delta", {"delta": f"w{i} "})
            for i in range(20)
        ]
        many_deltas.append(
            MockEvent("response.output_text.done", {"text": "done"})
        )
        mock_provider.set_next_scenario(Scenario(slow=0.5, events=many_deltas))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Slow turn.", "request_id": rid}

        first_result = [None]

        def first_stream():
            first_result[0] = httpx.post(
                url, json=body,
                headers={"Accept": "text/event-stream"},
                timeout=90,
            )

        t = threading.Thread(target=first_stream)
        t.start()

        time.sleep(1.0)

        # Attempt retry while running
        retry_resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/turns/{rid}/retry",
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        assert retry_resp.status_code == 400
        assert retry_resp.json().get("title") == "invalid_turn_state"

        t.join(timeout=60)

    @pytest.mark.xfail(reason="BUG: error_code not set on 400/409 turn mutation responses")
    def test_retry_non_latest_turn_409(self, chat):
        """Cannot retry a turn that is not the latest in the chat."""
        chat_id = chat["id"]

        # Create 2 turns sequentially
        rid1 = complete_turn(chat_id, "First turn.")
        rid2 = complete_turn(chat_id, "Second turn.")

        # Retry the first turn (not latest)
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/turns/{rid1}/retry",
            headers={"Accept": "text/event-stream"},
            timeout=10,
        )
        assert resp.status_code == 409
        assert resp.json().get("code") == "not_latest_turn"

    def test_retry_cancelled_turn(self, request, chat, mock_provider):
        """08-12: Retrying a turn in error/cancelled state should succeed with a new generation."""
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (offline mode)")
        chat_id = chat["id"]

        # Drive the turn into an error state via the mock ERROR scenario
        mock_provider.set_next_scenario(Scenario(
            events=[MockEvent("response.output_text.delta", {"delta": "Partial"})],
            terminal="failed",
            error={"code": "server_error", "message": "Mock provider error"},
        ))

        rid = str(uuid.uuid4())
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Trigger error.", "request_id": rid},
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )
        assert resp.status_code == 200
        # Wait for the turn to reach 'error' state
        poll_turn_state(chat_id, rid, "error")

        # Retry the errored (latest) turn — should succeed
        retry_resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/turns/{rid}/retry",
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert retry_resp.status_code == 200, (
            f"Expected 200 on retry of error turn, got {retry_resp.status_code}: {retry_resp.text[:300]}"
        )
        retry_events = parse_sse(retry_resp.text)
        expect_done(retry_events)


# ---------------------------------------------------------------------------
# Tests: delete
# ---------------------------------------------------------------------------

class TestTurnDelete:
    """DELETE /turns/{request_id} constraints and behavior."""

    def test_delete_last_turn_204(self, chat):
        """Deleting the last (and only) turn returns 204 and removes messages."""
        chat_id = chat["id"]
        rid = complete_turn(chat_id, "Only turn.")

        # Delete
        resp = httpx.delete(
            f"{API_PREFIX}/chats/{chat_id}/turns/{rid}",
            timeout=10,
        )
        assert resp.status_code == 204

        # Messages should be empty
        msgs_resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages", timeout=10)
        assert msgs_resp.status_code == 200
        items = msgs_resp.json()["items"]
        assert len(items) == 0, f"Expected no messages after delete, got {len(items)}"

    @pytest.mark.xfail(reason="BUG: error_code not set on 400/409 turn mutation responses")
    def test_delete_running_turn_400(self, chat, mock_provider):
        """Cannot delete a turn that is still running."""
        chat_id = chat["id"]
        rid = str(uuid.uuid4())

        many_deltas = [
            MockEvent("response.output_text.delta", {"delta": f"w{i} "})
            for i in range(20)
        ]
        many_deltas.append(
            MockEvent("response.output_text.done", {"text": "done"})
        )
        mock_provider.set_next_scenario(Scenario(slow=0.5, events=many_deltas))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Slow turn.", "request_id": rid}

        first_result = [None]

        def first_stream():
            first_result[0] = httpx.post(
                url, json=body,
                headers={"Accept": "text/event-stream"},
                timeout=90,
            )

        t = threading.Thread(target=first_stream)
        t.start()

        time.sleep(1.0)

        # Attempt delete while running
        del_resp = httpx.delete(
            f"{API_PREFIX}/chats/{chat_id}/turns/{rid}",
            timeout=10,
        )
        assert del_resp.status_code == 400
        assert del_resp.json().get("title") == "invalid_turn_state"

        t.join(timeout=60)

    @pytest.mark.xfail(reason="BUG: error_code not set on 400/409 turn mutation responses")
    def test_delete_non_latest_turn_409(self, chat):
        """Cannot delete a turn that is not the latest."""
        chat_id = chat["id"]

        rid1 = complete_turn(chat_id, "First turn.")
        rid2 = complete_turn(chat_id, "Second turn.")

        resp = httpx.delete(
            f"{API_PREFIX}/chats/{chat_id}/turns/{rid1}",
            timeout=10,
        )
        assert resp.status_code == 409
        assert resp.json().get("code") == "not_latest_turn"

    def test_soft_deleted_turn_excluded_from_messages(self, chat):
        """After deleting the last turn, its messages disappear from GET /messages."""
        chat_id = chat["id"]

        rid1 = complete_turn(chat_id, "First turn.")
        rid2 = complete_turn(chat_id, "Second turn.")

        # Delete the last turn
        resp = httpx.delete(
            f"{API_PREFIX}/chats/{chat_id}/turns/{rid2}",
            timeout=10,
        )
        assert resp.status_code == 204

        # Only first turn's messages should remain
        msgs_resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages", timeout=10)
        assert msgs_resp.status_code == 200
        items = msgs_resp.json()["items"]
        # First turn produces user + assistant = 2 messages
        assert len(items) == 2, f"Expected 2 messages (first turn only), got {len(items)}"
        roles = [m["role"] for m in items]
        assert roles == ["user", "assistant"]


# ---------------------------------------------------------------------------
# Tests: concurrent retries
# ---------------------------------------------------------------------------

class TestConcurrentRetries:
    """Race condition: two concurrent retry requests — exactly one wins."""

    @pytest.mark.xfail(reason="BUG: concurrent retry returns 500 instead of 409")
    def test_concurrent_retries_one_wins(self, chat, mock_provider):
        """Send 2 concurrent retry requests; one gets 200, the other gets 409."""
        chat_id = chat["id"]

        rid = complete_turn(chat_id, "Retryable turn.")
        poll_turn_state(chat_id, rid, "done")

        url = f"{API_PREFIX}/chats/{chat_id}/turns/{rid}/retry"
        results = [None, None]

        def send_retry(idx):
            results[idx] = httpx.post(
                url,
                headers={"Accept": "text/event-stream"},
                timeout=90,
            )

        t1 = threading.Thread(target=send_retry, args=(0,))
        t2 = threading.Thread(target=send_retry, args=(1,))
        t1.start()
        t2.start()
        t1.join(timeout=60)
        t2.join(timeout=60)

        codes = sorted([results[0].status_code, results[1].status_code])
        assert codes == [200, 409], (
            f"Expected exactly one 200 and one 409, got {codes}"
        )


# ---------------------------------------------------------------------------
# Tests: replaced_by_request_id tracking
# ---------------------------------------------------------------------------

class TestReplacedByRequestId:
    """After retry, the original turn's replaced_by_request_id points to the new turn."""

    def test_replaced_by_request_id_set(self, chat, mock_provider):
        """DB column replaced_by_request_id is set on the original turn after retry."""
        chat_id = chat["id"]

        rid1 = complete_turn(chat_id, "Original turn.")
        poll_turn_state(chat_id, rid1, "done")

        # Retry
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/turns/{rid1}/retry",
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Retry failed: {resp.status_code} {resp.text}"
        retry_events = parse_sse(resp.text)
        ss = expect_stream_started(retry_events)
        assert ss.data.get("is_new_turn") is True
        new_rid = ss.data["request_id"]
        expect_done(retry_events)

        # TODO: This test queries the SQLite DB directly. If DB_PATH is not
        # populated in the test environment (e.g., because the server uses a
        # different home_dir), this assertion will fail. Adjust DB_PATH or
        # use an API endpoint if one becomes available.
        rows = query_db(
            "SELECT replaced_by_request_id FROM chat_turns WHERE request_id = ?",
            (rid1,),
        )

        assert len(rows) == 1, f"Turn {rid1} not found in DB"
        actual = rows[0]["replaced_by_request_id"]
        # DB stores UUIDs as bytes; normalise both sides to str for comparison.
        if isinstance(actual, (bytes, bytearray)):
            actual = str(uuid.UUID(bytes=bytes(actual)))
        assert actual == new_rid, (
            f"Expected replaced_by_request_id={new_rid}, "
            f"got {actual}"
        )
