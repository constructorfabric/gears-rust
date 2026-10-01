"""Tests for architectural principles — model lock and streaming without buffering.

Tenant and owner isolation are covered in test_isolation.py. Kill switches
are fixed plugin configuration in this rig (all off); their behaviour is
covered by unit tests of the quota cascade and the stream/upload guards.
"""

from __future__ import annotations

import time

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    STANDARD_MODEL,
    expect_done,
    open_stream,
)
from .mock_provider.responses import MockEvent, Scenario, Usage


class TestPrinciples:
    """Architectural principles."""

    def test_model_locked_per_chat(self, chat):
        """PATCH with title and model: 200, the title changes, the model does not."""
        chat_id = chat["id"]
        assert chat["model"] != STANDARD_MODEL

        resp = httpx.patch(
            f"{API_PREFIX}/chats/{chat_id}",
            json={"title": "Renamed", "model": STANDARD_MODEL},
            timeout=10,
        )
        assert resp.status_code == 200, resp.text
        assert resp.json()["model"] == chat["model"]

        fetched = httpx.get(f"{API_PREFIX}/chats/{chat_id}", timeout=10).json()
        assert fetched["title"] == "Renamed"
        assert fetched["model"] == chat["model"]

    @pytest.mark.timeout(20)
    def test_no_buffering(self, request, chat, mock_provider):
        """Deltas are relayed as the provider produces them, not after it finishes.

        The mock sends one event every 0.5 s (3 deltas, output_text.done,
        response.completed). The first delta must reach the client well before
        the terminal `done`.
        """
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (offline mode)")
        mock_provider.set_next_scenario(Scenario(
            events=[
                MockEvent("response.output_text.delta", {"delta": "chunk1 "}),
                MockEvent("response.output_text.delta", {"delta": "chunk2 "}),
                MockEvent("response.output_text.delta", {"delta": "chunk3"}),
                MockEvent("response.output_text.done", {"text": "chunk1 chunk2 chunk3"}),
            ],
            usage=Usage(input_tokens=30, output_tokens=6),
            slow=0.5,
        ))

        with open_stream(chat["id"], "Stream test.") as s:
            s.read_until(lambda e: e.event == "delta")
            first_delta_at = time.monotonic()
            done = s.read_until(lambda e: e.event == "done")
            done_at = time.monotonic()

        assert done_at - first_delta_at >= 1.0, (
            f"done arrived {done_at - first_delta_at:.2f}s after the first delta: "
            "the response was buffered"
        )
        assert expect_done(s.events) is done
