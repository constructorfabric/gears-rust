"""Tests for turn lifecycle: running → terminal, and null assistant_message_id on
content-less cancel/failure."""

import uuid

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    expect_done,
    list_messages,
    open_stream,
    poll_turn,
    slow_scenario,
    stream_message,
)
from .mock_provider.responses import Scenario


class TestTurnLifecycle:
    """Terminal turn states and assistant_message_id."""

    @pytest.fixture(autouse=True)
    def _offline_only(self, request):
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (offline mode)")

    @pytest.mark.timeout(30)
    def test_cancelled_without_content_null_message_id(self, chat, mock_provider):
        """Disconnect before the first delta: cancelled, no assistant_message_id, no message."""
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())
        scenario = slow_scenario(3, slow=0.5)
        scenario.initial_delay = 3.0  # provider silent for 3 s after the headers
        mock_provider.set_next_scenario(scenario)

        with open_stream(chat_id, "Write slowly.", request_id=request_id) as s:
            s.read_until_started()

        turn = poll_turn(chat_id, request_id)
        assert turn["state"] == "cancelled"
        assert turn.get("assistant_message_id") is None
        assert [m["role"] for m in list_messages(chat_id)] == ["user"]

    @pytest.mark.timeout(30)
    def test_failed_turn_null_message_id(self, chat, mock_provider):
        """A provider failure without content: error, no assistant_message_id, no message."""
        chat_id = chat["id"]
        request_id = str(uuid.uuid4())
        mock_provider.set_next_scenario(Scenario(
            terminal="failed",
            error={"code": "server_error", "message": "fail"},
            events=[],
        ))

        status, events, _ = stream_message(chat_id, "Fail now.", request_id=request_id)
        assert status == 200
        # The stream ends with one `error` event, no `done`.
        assert [e.event for e in events if e.event in ("error", "done")] == ["error"], events
        assert events[-1].event == "error", [e.event for e in events]
        assert (events[-1].data["code"], events[-1].data["message"]) == ("provider_error", "fail")

        turn = poll_turn(chat_id, request_id)
        assert (turn["state"], turn["error_code"]) == ("error", "provider_error"), turn
        assert turn.get("assistant_message_id") is None
        assert [m["role"] for m in list_messages(chat_id)] == ["user"]

    @pytest.mark.timeout(30)
    def test_turn_running_then_done(self, chat, mock_provider):
        """09-11: GET turn reports `running` while the stream is open and `done`
        after it ended, with the assistant message announced in stream_started."""
        chat_id = chat["id"]
        rid = str(uuid.uuid4())
        mock_provider.set_next_scenario(slow_scenario(5, slow=0.3))

        with open_stream(chat_id, "Answer slowly.", request_id=rid) as s:
            started = s.read_until_started()
            resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/turns/{rid}", timeout=5)
            assert resp.status_code == 200
            assert resp.json()["state"] == "running"
            expect_done(s.drain())

        turn = poll_turn(chat_id, rid)
        assert turn["state"] == "done"
        assert turn["assistant_message_id"] == started.data["message_id"]
