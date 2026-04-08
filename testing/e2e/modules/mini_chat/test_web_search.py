"""Tests for web search integration.

Offline mode: mock provider returns canned web_search tool events.
Online mode: real LLM provider performs actual web search.

Provider-parameterized — runs against both OpenAI and Azure.
Use ``-m openai`` or ``-m azure`` to target a single provider.
"""

import uuid

import pytest
import httpx

from .conftest import API_PREFIX, STANDARD_MODEL, expect_done, stream_message


class TestWebSearchBasic:
    """Web search happy path — tool events, usage, deltas."""

    def test_web_search_returns_tool_events(self, provider_chat):
        """Streaming with web_search enabled should produce tool events."""
        status, events, _ = stream_message(
            provider_chat["id"],
            "SEARCH: current weather in Berlin",
            web_search={"enabled": True},
        )
        assert status == 200
        expect_done(events)

        tool_events = [e for e in events if e.event == "tool"]
        assert len(tool_events) > 0, (
            f"Expected tool events for web search but got none. "
            f"Event types: {[e.event for e in events]}"
        )

    def test_web_search_done_has_usage(self, provider_chat):
        """Done event after web search should include usage with tokens."""
        status, events, _ = stream_message(
            provider_chat["id"],
            "SEARCH: population of Tokyo",
            web_search={"enabled": True},
        )
        assert status == 200
        done = expect_done(events)
        usage = done.data.get("usage")
        assert usage is not None, f"Done event missing usage: {done.data}"
        assert usage["input_tokens"] > 0
        assert usage["output_tokens"] > 0

    def test_web_search_has_delta_events(self, provider_chat):
        """Web search stream should still have delta text events."""
        status, events, _ = stream_message(
            provider_chat["id"],
            "SEARCH: what year was Python created?",
            web_search={"enabled": True},
        )
        assert status == 200
        expect_done(events)

        deltas = [e for e in events if e.event == "delta"]
        assert len(deltas) > 0, "Expected delta events in web search response"
        text = "".join(e.data["content"] for e in deltas if isinstance(e.data, dict))
        assert len(text.strip()) > 0, "Assembled text from deltas is empty"


class TestWebSearchCitations:
    """Web search citation event structure.

    Offline: mock guarantees citations are present — assert presence + structure.
    Online: real providers may omit citations — only validate structure if present.
    """

    def test_web_search_produces_citations(self, request, provider_chat):
        """Web search should emit a citations event with at least one item."""
        status, events, _ = stream_message(
            provider_chat["id"],
            "SEARCH: capital of Australia",
            web_search={"enabled": True},
        )
        assert status == 200
        expect_done(events)

        citation_events = [e for e in events if e.event == "citations"]
        if request.config.getoption("mode") == "offline":
            assert len(citation_events) >= 1, (
                f"Expected citations event for web search. "
                f"Event types: {[e.event for e in events]}"
            )
        if not citation_events:
            pytest.skip("Provider did not return citations for this query")

        data = citation_events[0].data
        assert isinstance(data, dict), f"Citations data should be dict, got {type(data)}"
        items = data.get("items", [])
        assert len(items) > 0, f"Citations items should not be empty: {data}"

    def test_citation_has_required_fields(self, request, provider_chat):
        """Each citation should have source, title, and snippet."""
        status, events, _ = stream_message(
            provider_chat["id"],
            "SEARCH: when was the Eiffel Tower built",
            web_search={"enabled": True},
        )
        assert status == 200
        expect_done(events)

        citation_events = [e for e in events if e.event == "citations"]
        if not citation_events:
            if request.config.getoption("mode") == "offline":
                pytest.fail("Mock should always produce citations")
            pytest.skip("Provider did not return citations for this query")

        items = citation_events[0].data["items"]
        for c in items:
            assert "source" in c, f"Citation missing 'source': {c}"
            assert c["source"] == "web", f"Expected source='web', got '{c['source']}'"
            assert "title" in c and len(c["title"]) > 0, f"Citation missing/empty 'title': {c}"
            assert "snippet" in c and len(c["snippet"]) > 0, f"Citation missing/empty 'snippet': {c}"

    def test_web_citation_has_url(self, request, provider_chat):
        """Web citations should include a URL."""
        status, events, _ = stream_message(
            provider_chat["id"],
            "SEARCH: population of Japan",
            web_search={"enabled": True},
        )
        assert status == 200
        expect_done(events)

        citation_events = [e for e in events if e.event == "citations"]
        if not citation_events:
            if request.config.getoption("mode") == "offline":
                pytest.fail("Mock should always produce citations")
            pytest.skip("Provider did not return citations for this query")

        items = citation_events[0].data["items"]
        for c in items:
            assert "url" in c and c["url"] is not None, f"Web citation missing 'url': {c}"
            assert c["url"].startswith("http"), f"URL should start with http: {c['url']}"


class TestWebSearchEventOrdering:
    """SSE event grammar: ping* (delta|tool)* citations? (done|error)"""

    def test_citations_before_done(self, provider_chat):
        """If citations are present, they must appear before the done event."""
        status, events, _ = stream_message(
            provider_chat["id"],
            "SEARCH: capital of France",
            web_search={"enabled": True},
        )
        assert status == 200
        expect_done(events)

        citation_idx = None
        done_idx = None
        for i, e in enumerate(events):
            if e.event == "citations" and citation_idx is None:
                citation_idx = i
            if e.event == "done":
                done_idx = i

        # Citations are optional (provider may not always return them),
        # but if present they must come before done.
        if citation_idx is not None:
            assert citation_idx < done_idx, (
                f"citations at index {citation_idx} should come before done at {done_idx}"
            )

    def test_tool_events_before_done(self, provider_chat):
        """Tool events must appear before the terminal done event."""
        status, events, _ = stream_message(
            provider_chat["id"],
            "SEARCH: who won the latest Nobel Prize in Physics?",
            web_search={"enabled": True},
        )
        assert status == 200
        expect_done(events)

        done_idx = next(i for i, e in enumerate(events) if e.event == "done")
        for i, e in enumerate(events):
            if e.event == "tool":
                assert i < done_idx, f"tool event at {i} should be before done at {done_idx}"


class TestWebSearchDisabledByDefault:
    """When web_search is not requested, no tool events should appear."""

    def test_no_tool_events_without_web_search(self, provider_chat):
        """A normal message (no web_search flag) should not trigger web search."""
        status, events, _ = stream_message(
            provider_chat["id"],
            "What is 2+2? Answer in one word.",
        )
        assert status == 200
        expect_done(events)

        tool_events = [e for e in events if e.event == "tool"]
        assert len(tool_events) == 0, (
            f"Unexpected tool events without web_search: {[t.data for t in tool_events]}"
        )

    def test_no_citations_without_web_search(self, provider_chat):
        """A normal message should not produce citation events."""
        status, events, _ = stream_message(
            provider_chat["id"],
            "Say hello.",
        )
        assert status == 200
        expect_done(events)

        citations = [e for e in events if e.event == "citations"]
        assert len(citations) == 0, "Unexpected citations without web_search"


class TestWebSearchWithStandardModel:
    """Web search on the standard model (gpt-5-mini, web_search: true)."""

    @pytest.mark.openai
    def test_web_search_works_on_standard_model(self, chat_with_model):
        """Web search should also work on gpt-5-mini."""
        chat = chat_with_model(STANDARD_MODEL)
        status, events, _ = stream_message(
            chat["id"],
            "SEARCH: tallest building in the world",
            web_search={"enabled": True},
        )
        assert status == 200
        expect_done(events)

        tool_events = [e for e in events if e.event == "tool"]
        assert len(tool_events) > 0, (
            f"Expected tool events for web search on {STANDARD_MODEL}. "
            f"Event types: {[e.event for e in events]}"
        )


class TestWebSearchTurnStatus:
    """Turn status should reflect web search completion."""

    def test_turn_done_after_web_search(self, provider_chat):
        """Turn state should be 'done' after a successful web search stream."""
        chat_id = provider_chat["id"]
        request_id = str(uuid.uuid4())
        status, events, _ = stream_message(
            chat_id,
            "SEARCH: speed of light",
            web_search={"enabled": True},
            request_id=request_id,
        )
        assert status == 200
        expect_done(events)

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/turns/{request_id}")
        assert resp.status_code == 200
        body = resp.json()
        assert body["state"] == "done"

    def test_messages_persisted_after_web_search(self, provider_chat):
        """Both user and assistant messages should be persisted after web search."""
        chat_id = provider_chat["id"]
        _, events, _ = stream_message(
            chat_id,
            "SEARCH: when was the Eiffel Tower built?",
            web_search={"enabled": True},
        )
        expect_done(events)

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/messages")
        assert resp.status_code == 200
        msgs = resp.json()["items"]
        roles = [m["role"] for m in msgs]
        assert "user" in roles
        assert "assistant" in roles


# ── Online-only tests ────────────────────────────────────────────────────
# Require real provider responses (natural language quality).

@pytest.mark.online_only
class TestWebSearchOnline:
    """Online web search tests — provider-parameterized."""

    def test_web_search_produces_meaningful_answer(self, provider_chat):
        """Real web search should produce a substantive text answer."""
        status, events, _ = stream_message(
            provider_chat["id"],
            "Search the web: who is the current president of France?",
            web_search={"enabled": True},
        )
        assert status == 200
        expect_done(events)

        text = "".join(
            e.data["content"] for e in events
            if e.event == "delta" and isinstance(e.data, dict)
        )
        assert len(text) > 20, f"Answer too short for a real web search: '{text}'"

    def test_web_search_tool_event_has_name(self, provider_chat):
        """Real provider should emit tool events with web_search name."""
        status, events, _ = stream_message(
            provider_chat["id"],
            "What is the current weather in Berlin right now? Search the web.",
            web_search={"enabled": True},
        )
        assert status == 200
        expect_done(events)

        tool_events = [e for e in events if e.event == "tool"]
        ws_tools = [
            t for t in tool_events
            if isinstance(t.data, dict) and t.data.get("name") in ("web_search", "web_search_preview")
        ]
        assert len(ws_tools) > 0, (
            f"No web_search tool events found. Tool events: {[t.data for t in tool_events]}"
        )
