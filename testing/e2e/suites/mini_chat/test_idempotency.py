# Updated: 2026-04-16 by Constructor Tech
"""Tests for request_id idempotency — conflict detection, replay priority, quota invariance."""

import threading
import time
import uuid

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    expect_done,
    expect_stream_started,
    parse_sse,
    stream_message,
)
from .mock_provider.responses import MockEvent, Scenario


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def poll_turn_terminal(chat_id: str, request_id: str, timeout: float = 15.0) -> dict:
    """Poll GET /turns/{request_id} until the turn reaches a terminal state."""
    deadline = time.monotonic() + timeout
    body = None
    while time.monotonic() < deadline:
        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/turns/{request_id}", timeout=5
        )
        if resp.status_code == 200:
            body = resp.json()
            if body["state"] in ("done", "error", "cancelled"):
                return body
        time.sleep(0.3)
    state = body["state"] if body else "no response"
    raise AssertionError(
        f"Turn {request_id} did not reach terminal state within {timeout}s "
        f"(last state: {state})"
    )


def get_quota_used() -> int:
    """Return total daily used_credits_micro from GET /quota/status."""
    resp = httpx.get(f"{API_PREFIX}/quota/status", timeout=10)
    assert resp.status_code == 200
    for tier in resp.json()["tiers"]:
        if tier["tier"] == "total":
            for period in tier["periods"]:
                if period["period"] == "daily":
                    return period["used_credits_micro"]
    raise AssertionError("Could not find total/daily period in quota status")


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

class TestIdempotency:
    """Request-id idempotency conflict detection and replay semantics."""

    @pytest.mark.xfail(reason="BUG: 409 missing error_code request_id_conflict")
    def test_running_turn_same_request_id_409(self, chat, mock_provider):
        """Sending the same request_id while a turn is still running returns 409."""
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        # Slow scenario so the first stream stays in-flight
        many_deltas = [
            MockEvent("response.output_text.delta", {"delta": f"word{i} "})
            for i in range(20)
        ]
        many_deltas.append(
            MockEvent("response.output_text.done", {"text": "done"})
        )
        mock_provider.set_next_scenario(Scenario(slow=0.2, events=many_deltas))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Hello slow.", "request_id": request_id}

        # Start first stream in a background thread
        first_result = [None]

        def first_stream():
            first_result[0] = httpx.post(
                url, json=body,
                headers={"Accept": "text/event-stream"},
                timeout=90,
            )

        t = threading.Thread(target=first_stream)
        t.start()

        # Give the first request time to start processing
        time.sleep(1.0)

        # Second request with SAME request_id while first is running
        second_resp = httpx.post(
            url, json=body,
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )

        t.join(timeout=60)

        assert second_resp.status_code == 409
        assert second_resp.json().get("title") == "request_id_conflict"

    @pytest.mark.xfail(reason="BUG: 409 missing error_code request_id_conflict")
    def test_failed_turn_same_request_id_409(self, request, chat, mock_provider):
        """Resending request_id of a failed turn returns 409."""
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (offline mode)")
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        mock_provider.set_next_scenario(Scenario(
            terminal="failed",
            error={"code": "server_error", "message": "fail"},
            events=[MockEvent("response.output_text.delta", {"delta": "x"})],
        ))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Fail please.", "request_id": request_id}
        resp = httpx.post(
            url, json=body,
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )
        # Stream completes (with error terminal)
        assert resp.status_code == 200

        # Wait for turn to reach error state
        poll_turn_terminal(chat_id, request_id)

        # Resend with same request_id
        second_resp = httpx.post(
            url, json=body,
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )
        assert second_resp.status_code == 409
        assert second_resp.json().get("title") == "request_id_conflict"

    @pytest.mark.xfail(reason="BUG: 409 missing error_code request_id_conflict")
    def test_cancelled_turn_same_request_id_409(self, chat, mock_provider):
        """Resending request_id of a cancelled turn returns 409."""
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())

        # Slow scenario — we will disconnect early
        many_deltas = [
            MockEvent("response.output_text.delta", {"delta": f"chunk{i} "})
            for i in range(30)
        ]
        many_deltas.append(
            MockEvent("response.output_text.done", {"text": "done"})
        )
        mock_provider.set_next_scenario(Scenario(slow=0.5, events=many_deltas))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Write slowly.", "request_id": request_id}

        # Read partial then disconnect
        with httpx.stream(
            "POST", url, json=body,
            headers={"Accept": "text/event-stream"},
            timeout=30,
        ) as resp:
            assert resp.status_code == 200
            for _ in resp.iter_bytes(chunk_size=256):
                break  # disconnect after first chunk

        # Poll until cancelled
        poll_turn_terminal(chat_id, request_id)

        # Resend with same request_id
        second_resp = httpx.post(
            url, json=body,
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )
        assert second_resp.status_code == 409
        assert second_resp.json().get("title") == "request_id_conflict"

    @pytest.mark.timeout(30)
    def test_replay_priority_over_parallel_check(self, chat, mock_provider):
        """Replay of a completed turn returns 200 even while another turn is running."""
        chat_id = chat["id"]
        rid_a = str(uuid.uuid4())

        # Complete turn A (normal speed)
        status_a, events_a, _ = stream_message(
            chat_id, "Turn A.", request_id=rid_a
        )
        assert status_a == 200
        expect_done(events_a)

        # Start slow turn B with a different request_id
        rid_b = str(uuid.uuid4())
        many_deltas = [
            MockEvent("response.output_text.delta", {"delta": f"slow{i} "})
            for i in range(20)
        ]
        many_deltas.append(
            MockEvent("response.output_text.done", {"text": "done"})
        )
        mock_provider.set_next_scenario(Scenario(slow=0.5, events=many_deltas))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body_b = {"content": "Turn B slow.", "request_id": rid_b}

        b_result = [None]

        def stream_b():
            b_result[0] = httpx.post(
                url, json=body_b,
                headers={"Accept": "text/event-stream"},
                timeout=90,
            )

        t = threading.Thread(target=stream_b)
        t.start()

        # Wait for B to start processing
        time.sleep(1.0)

        # Replay turn A while B is running — should get replay, not 409
        replay_resp = httpx.post(
            url,
            json={"content": "Turn A.", "request_id": rid_a},
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )
        assert replay_resp.status_code == 200

        replay_events = parse_sse(replay_resp.text)
        ss = expect_stream_started(replay_events)
        assert ss.data["is_new_turn"] is False

        t.join(timeout=60)

    def test_replay_does_not_modify_quota(self, chat, mock_provider):
        """Replaying a completed turn does not increase quota usage."""
        chat_id = chat["id"]
        rid = str(uuid.uuid4())

        # Complete a turn
        status, events, _ = stream_message(
            chat_id, "Say OK.", request_id=rid
        )
        assert status == 200
        expect_done(events)

        # Small delay for quota settlement
        time.sleep(0.5)
        used_before = get_quota_used()

        # Replay 3 times
        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        body = {"content": "Say OK.", "request_id": rid}
        for _ in range(3):
            resp = httpx.post(
                url, json=body,
                headers={"Accept": "text/event-stream"},
                timeout=30,
            )
            assert resp.status_code == 200

        time.sleep(0.5)
        used_after = get_quota_used()

        assert used_after == used_before, (
            f"Quota should not change on replay: before={used_before}, after={used_after}"
        )


@pytest.mark.multi_provider
@pytest.mark.online_only
class TestReplayProviderFidelity:
    """Verify replay SSE fidelity across providers (Azure vs OpenAI envelope differences)."""

    def test_replay_stream_started_fields(self, provider_chat):
        """Replay should produce identical stream_started fields regardless of provider."""
        chat_id = provider_chat["id"]
        request_id = str(uuid.uuid4())

        # First send
        resp1 = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Say OK.", "request_id": request_id},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp1.status_code == 200
        events1 = parse_sse(resp1.text)
        expect_done(events1)
        ss1 = expect_stream_started(events1)

        # Replay
        resp2 = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Say OK.", "request_id": request_id},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp2.status_code == 200
        events2 = parse_sse(resp2.text)
        ss2 = expect_stream_started(events2)

        assert ss2.data["is_new_turn"] is False
        assert ss2.data["message_id"] == ss1.data["message_id"]
        assert ss2.data["request_id"] == request_id
