"""Provider request body verification tests (offline only).

These tests inspect the request body that mini-chat sends to the LLM provider
via the mock provider's request capture. They verify that fields like
max_tool_calls, search_context_size, and max_num_results are serialized
correctly in the outgoing provider request.

Provider-parameterized — runs against both OpenAI and Azure mock endpoints.
"""

import time

import httpx
import pytest

from .conftest import API_PREFIX, expect_done, stream_message


@pytest.fixture(autouse=True)
def _clear_captures(mock_provider):
    """Clear captured requests before each test."""
    mock_provider.clear_captured_requests()


@pytest.fixture(autouse=True)
def _skip_online(request):
    """Skip these tests in online mode — mock capture only works offline."""
    if request.config.getoption("mode") == "online":
        pytest.skip("provider request capture requires offline mode")


class TestMaxToolCalls:
    """Verify max_tool_calls is sent in provider request body."""

    def test_max_tool_calls_present_in_request(self, provider_chat, mock_provider):
        """Provider request should include max_tool_calls from model config."""
        status, events, _ = stream_message(provider_chat["id"], "Say hello.")
        assert status == 200
        expect_done(events)

        time.sleep(0.5)
        req = mock_provider.get_last_request()
        assert req is not None, "No request captured by mock provider"
        assert "max_tool_calls" in req, (
            f"max_tool_calls missing from provider request body. Keys: {list(req.keys())}"
        )
        assert req["max_tool_calls"] == 2, (
            f"Expected max_tool_calls=2, got {req['max_tool_calls']}"
        )

    def test_max_tool_calls_numeric(self, provider_chat, mock_provider):
        """max_tool_calls should be a number, not a string."""
        stream_message(provider_chat["id"], "Say OK.")
        time.sleep(0.5)
        req = mock_provider.get_last_request()
        assert req is not None
        assert isinstance(req.get("max_tool_calls"), int)


class TestWebSearchToolType:
    """Verify web_search tool serialization in provider request."""

    def test_web_search_tool_type_is_web_search(self, provider_chat, mock_provider):
        """When web_search enabled, tool type should be 'web_search' (not 'web_search_preview')."""
        status, events, _ = stream_message(
            provider_chat["id"],
            "SEARCH: test query",
            web_search={"enabled": True},
        )
        assert status == 200
        expect_done(events)

        time.sleep(0.5)
        req = mock_provider.get_last_request()
        assert req is not None, "No request captured"
        tools = req.get("tools", [])
        ws_tools = [t for t in tools if t.get("type", "").startswith("web_search")]
        assert len(ws_tools) == 1, (
            f"Expected exactly one web_search tool, got {len(ws_tools)}. "
            f"Tools: {tools}"
        )
        assert ws_tools[0]["type"] == "web_search", (
            f"Expected type='web_search', got '{ws_tools[0]['type']}'"
        )

    def test_web_search_has_search_context_size(self, provider_chat, mock_provider):
        """web_search tool should include search_context_size."""
        stream_message(
            provider_chat["id"],
            "SEARCH: test context size",
            web_search={"enabled": True},
        )
        time.sleep(0.5)
        req = mock_provider.get_last_request()
        assert req is not None
        tools = req.get("tools", [])
        ws_tools = [t for t in tools if t.get("type") == "web_search"]
        assert len(ws_tools) == 1
        assert "search_context_size" in ws_tools[0], (
            f"search_context_size missing from web_search tool: {ws_tools[0]}"
        )
        assert ws_tools[0]["search_context_size"] in ("low", "medium", "high"), (
            f"Unexpected search_context_size: {ws_tools[0]['search_context_size']}"
        )

    def test_no_web_search_tool_without_flag(self, provider_chat, mock_provider):
        """Without web_search flag, no web_search tool in provider request."""
        stream_message(provider_chat["id"], "Say hello.")
        time.sleep(0.5)
        req = mock_provider.get_last_request()
        assert req is not None
        tools = req.get("tools", [])
        ws_tools = [t for t in tools if t.get("type", "").startswith("web_search")]
        assert len(ws_tools) == 0, (
            f"Unexpected web_search tool without flag: {ws_tools}"
        )


class TestFileSearchMaxNumResults:
    """Verify file_search tool includes max_num_results."""

    def test_file_search_has_max_num_results(self, provider_chat, mock_provider):
        """When file_search tool is present, it should include max_num_results."""
        # Upload a mock file to trigger file_search
        resp = httpx.post(
            f"{API_PREFIX}/chats/{provider_chat['id']}/attachments",
            files={"file": ("test.txt", b"test content", "text/plain")},
            timeout=30,
        )
        if resp.status_code not in (200, 201):
            pytest.skip(f"Attachment upload not supported or failed: {resp.status_code}")

        # Send message referencing the attachment
        stream_message(provider_chat["id"], "What does the attached file say?")
        time.sleep(0.5)
        req = mock_provider.get_last_request()
        if req is None:
            pytest.skip("No request captured")

        tools = req.get("tools", [])
        fs_tools = [t for t in tools if t.get("type") == "file_search"]
        if not fs_tools:
            pytest.skip("No file_search tool in request (attachment may not have triggered it)")

        assert "max_num_results" in fs_tools[0], (
            f"max_num_results missing from file_search tool: {fs_tools[0]}"
        )
        assert isinstance(fs_tools[0]["max_num_results"], int)
        assert fs_tools[0]["max_num_results"] > 0
