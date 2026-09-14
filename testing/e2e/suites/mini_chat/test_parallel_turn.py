"""Tests for parallel turn rejection — only one generation at a time per chat."""

import threading
import time
import uuid

import httpx
import pytest

from .conftest import API_PREFIX, expect_done, parse_sse, stream_message
from .mock_provider.responses import MockEvent, Scenario


class TestParallelTurn:
    """Only one active generation per chat at a time."""

    @pytest.mark.xfail(reason="BUG: 409 missing error_code generation_in_progress")
    def test_second_stream_409_generation_in_progress(self, chat, mock_provider, request):
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (slow scenario)")
        """A second stream request to the same chat while one is running returns 409."""
        chat_id = chat["id"]

        # Slow scenario keeps the first stream in-flight
        many_deltas = [
            MockEvent("response.output_text.delta", {"delta": f"word{i} "})
            for i in range(20)
        ]
        many_deltas.append(
            MockEvent("response.output_text.done", {"text": "done"})
        )
        mock_provider.set_next_scenario(Scenario(slow=0.5, events=many_deltas))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"

        first_result = [None]

        def first_stream():
            first_result[0] = httpx.post(
                url,
                json={"content": "First turn.", "request_id": str(uuid.uuid4())},
                headers={"Accept": "text/event-stream"},
                timeout=90,
            )

        t = threading.Thread(target=first_stream)
        t.start()

        # Give the first request time to start processing
        time.sleep(1.0)

        # Second request with a DIFFERENT request_id
        second_resp = httpx.post(
            url,
            json={"content": "Second turn.", "request_id": str(uuid.uuid4())},
            headers={"Accept": "text/event-stream"},
            timeout=30,
        )

        t.join(timeout=60)
        assert not t.is_alive(), "First stream did not complete within 60s"
        assert first_result[0] is not None
        assert first_result[0].status_code == 200

        assert second_resp.status_code == 409
        assert second_resp.json().get("title") == "generation_in_progress"

    @pytest.mark.multi_provider
    def test_new_stream_succeeds_after_terminal(self, chat, mock_provider):
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
