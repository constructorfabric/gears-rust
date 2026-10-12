"""Tests for parallel turn rejection — only one generation at a time per chat."""

import uuid

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    OpenStream,
    assert_problem,
    expect_done,
    list_messages,
    open_stream,
    parse_sse,
    slow_scenario,
    stream_message,
)


class TestParallelTurn:
    """Only one active generation per chat at a time."""

    @pytest.mark.timeout(30)
    def test_second_stream_409_turn_already_running(self, request, chat, mock_provider):
        """A second stream into a chat with a running turn gets 409 turn_already_running."""
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (slow scenario)")
        chat_id = chat["id"]
        mock_provider.set_next_scenario(slow_scenario(10, slow=0.3))

        with open_stream(chat_id, "First turn.", request_id=str(uuid.uuid4())) as first:
            first.read_until_started()

            second = httpx.post(
                f"{API_PREFIX}/chats/{chat_id}/messages:stream",
                json={"content": "Second turn.", "request_id": str(uuid.uuid4())},
                headers={"Accept": "text/event-stream"},
                timeout=30,
            )
            assert_problem(
                second, 409, "aborted",
                reason="turn_already_running",
            )

            expect_done(first.drain())

        # Only the first turn was persisted: one user + one assistant message.
        messages = list_messages(chat_id)
        assert [m["role"] for m in messages] == ["user", "assistant"]
        assert messages[0]["content"] == "First turn."

    @pytest.mark.timeout(30)
    @pytest.mark.parametrize("mutation", ["retry", "edit"])
    def test_send_while_mutation_streams_409(self, request, chat, mock_provider, mutation):
        """A send into a chat whose retry or edit is streaming gets 409
        turn_already_running; the mutation completes and is the only turn."""
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (slow scenario)")
        chat_id = chat["id"]
        rid = str(uuid.uuid4())
        status, events, _ = stream_message(chat_id, "Original.", request_id=rid)
        assert status == 200
        expect_done(events)

        mock_provider.set_next_scenario(slow_scenario(10, slow=0.3))
        url = f"{API_PREFIX}/chats/{chat_id}/turns/{rid}"
        if mutation == "retry":
            stream = OpenStream(f"{url}/retry", None)
        else:
            stream = OpenStream(url, {"content": "Edited."}, method="PATCH")
        with stream as s:
            new_rid = s.read_until_started().data["request_id"]
            second = httpx.post(
                f"{API_PREFIX}/chats/{chat_id}/messages:stream",
                json={"content": "Meanwhile.", "request_id": str(uuid.uuid4())},
                headers={"Accept": "text/event-stream"},
                timeout=30,
            )
            assert_problem(
                second, 409, "aborted",
                reason="turn_already_running",
            )
            expect_done(s.drain())

        messages = list_messages(chat_id)
        assert [(m["role"], m["request_id"]) for m in messages] == [
            ("user", new_rid), ("assistant", new_rid),
        ]

    def test_new_stream_succeeds_after_terminal(self, chat):
        """A new stream request succeeds after the previous turn completed."""
        chat_id = chat["id"]

        # Complete first turn (normal speed)
        status1, events1, _ = stream_message(
            chat_id, "First turn.", request_id=str(uuid.uuid4())
        )
        assert status1 == 200
        expect_done(events1)

        # Immediately send another turn
        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        resp2 = httpx.post(
            url,
            json={"content": "Second turn.", "request_id": str(uuid.uuid4())},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        raw2 = resp2.text
        events2 = parse_sse(raw2) if resp2.status_code == 200 else []
        assert resp2.status_code == 200
        expect_done(events2)
