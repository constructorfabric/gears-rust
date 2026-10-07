"""Tests for error mapping: provider errors -> client-facing SSE error events.

A provider failure after the stream opened is an SSE `error` event with
`{code, message}` (ADR-0004); the codes are listed in DESIGN §3.3
"Streaming error codes".
"""

import httpx
import pytest

from .conftest import (
    API_PREFIX, expect_stream_started, list_messages, parse_sse, poll_turn, usage_events,
)
from .mock_provider.responses import MockEvent, Scenario, Usage

# Provider identifiers the sanitizer must scrub (shapes of real IDs; the
# storage-ID pattern needs at least 12 characters after the prefix).
PROVIDER_IDS = (
    "resp_0a1b2c3d4e5f6a7b8c9d",
    "file-AbCdEf0123456789XyZ",
    "vs_0123456789abcdefABCD",
    "assistant-0123456789abcdEF",
)
LEAKY_MESSAGE = "Upstream failure for " + ", ".join(PROVIDER_IDS)



def _stream_error(chat_id: str) -> tuple[dict, str]:
    """Send a message and return (error event data, request_id)."""
    resp = httpx.post(
        f"{API_PREFIX}/chats/{chat_id}/messages:stream",
        json={"content": "trigger error"},
        headers={"Accept": "text/event-stream"},
        timeout=90,
    )
    assert resp.status_code == 200, f"expected an SSE stream, got {resp.status_code}: {resp.text}"
    events = parse_sse(resp.text)
    assert [e.event for e in events][-1] == "error", [e.event for e in events]
    rid = expect_stream_started(events).data["request_id"]
    return events[-1].data, rid


class TestErrorMapping:
    """Provider-level errors map to stable streaming error codes."""

    @pytest.fixture(autouse=True)
    def _skip_online(self, request):
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (offline mode)")

    @pytest.mark.timeout(30)
    def test_post_stream_sse_error_event(self, chat, mock_provider):
        """A `response.failed` mid-stream (error in `response.error`, as OpenAI
        sends it) is an SSE error `provider_error` with the provider message;
        the turn fails."""
        mock_provider.set_next_scenario(Scenario(
            terminal="failed",
            error={"code": "server_error", "message": "Mock fail"},
            events=[MockEvent("response.output_text.delta", {"delta": "Partial"})],
        ))
        data, rid = _stream_error(chat["id"])
        assert data["code"] == "provider_error"
        assert data["message"] == "Mock fail", data
        assert poll_turn(chat["id"], rid)["state"] == "error"

    @pytest.mark.timeout(30)
    def test_error_event_keeps_provider_message(self, chat, mock_provider):
        """A flat SSE `error` event (`{"type":"error","code","message"}`) is an
        SSE error `provider_error` with the provider message; the turn fails."""
        mock_provider.set_next_scenario(Scenario(
            events=[
                MockEvent("response.output_text.delta", {"delta": "Partial"}),
                MockEvent("error", {
                    "type": "error", "code": "server_error",
                    "message": "Mock error event", "param": None,
                }),
            ],
        ))
        data, rid = _stream_error(chat["id"])
        assert data["code"] == "provider_error"
        assert data["message"] == "Mock error event", data
        assert poll_turn(chat["id"], rid)["state"] == "error"

    @pytest.mark.timeout(30)
    def test_function_call_without_knowledge_search_is_unexpected_tool_use(
        self, chat, mock_provider,
    ):
        """The provider answers with a `function_call` output item while no
        function tool was offered (knowledge search is not configured on the
        rig): SSE error `unexpected_tool_use`, the turn fails with that code."""
        mock_provider.set_next_scenario(Scenario(
            events=[MockEvent("response.output_text.delta", {"delta": "Let me look"})],
            output_items=[{
                "type": "function_call",
                "id": "fc_mock_1",
                "call_id": "call_mock_1",
                "name": "search_knowledge",
                "arguments": "{\"query\": \"anything\"}",
                "status": "completed",
            }],
        ))
        data, rid = _stream_error(chat["id"])
        assert data["code"] == "unexpected_tool_use", data
        turn = poll_turn(chat["id"], rid)
        assert (turn["state"], turn["error_code"]) == ("error", "unexpected_tool_use"), turn

    def test_provider_504_is_provider_error(self, chat, mock_provider):
        """An HTTP 504 returned by the provider is a non-429 provider error:
        `provider_error` with the sanitized `error.message` of the provider's
        JSON body.

        `provider_timeout` is for the gateway's own timeout
        (test_provider_timeout_error_code).
        """
        mock_provider.set_next_scenario(Scenario(
            http_error_status=504,
            http_error_body={"error": {"message": "Gateway Timeout", "type": "timeout"}},
        ))
        data, _ = _stream_error(chat["id"])
        assert data == {"code": "provider_error", "message": "Gateway Timeout"}, data

    @pytest.mark.timeout(30)
    def test_provider_timeout_error_code(self, chat, mock_provider):
        """17-03: the provider sends no response headers within OAGW
        `proxy_timeout_secs` (8 s in base.yaml): SSE error `provider_timeout`;
        the turn fails."""
        mock_provider.set_next_scenario(Scenario(header_delay=10))
        data, rid = _stream_error(chat["id"])
        assert data["code"] == "provider_timeout", data
        turn = poll_turn(chat["id"], rid)
        assert turn["state"] == "error"

    def test_provider_unavailable_error_code(self, chat, mock_provider):
        """An HTTP 503 from the provider is `provider_error` with the
        sanitized `error.message` of the provider's JSON body."""
        mock_provider.set_next_scenario(Scenario(
            http_error_status=503,
            http_error_body={"error": {"message": "Service Unavailable", "type": "server_error"}},
        ))
        data, _ = _stream_error(chat["id"])
        assert data == {"code": "provider_error", "message": "Service Unavailable"}, data

    def test_rate_limited_error_code(self, chat, mock_provider):
        """An HTTP 429 from the provider is `rate_limited`. The message is the
        gear's own text, not the provider's body; without a `Retry-After`
        header (the mock sends none) it names no delay."""
        mock_provider.set_next_scenario(Scenario(
            http_error_status=429,
            http_error_body={"error": {"message": "Rate limited", "type": "rate_limit_error"}},
        ))
        data, _ = _stream_error(chat["id"])
        assert data["code"] == "rate_limited", data

    @pytest.mark.timeout(30)
    def test_rate_limited_with_retry_after(self, chat, mock_provider):
        """A provider 429 with `Retry-After: 7` (seconds): OAGW passes the
        status and the header through (DESIGN, "Provider rate limit"), and
        the delay reaches the client only in the SSE message; the error has
        no other field and the turn fails with `rate_limited`."""
        mock_provider.set_next_scenario(Scenario(
            http_error_status=429,
            http_error_body={"error": {"message": "Rate limited", "type": "rate_limit_error"}},
            http_error_headers={"Retry-After": "7"},
        ))
        data, rid = _stream_error(chat["id"])
        assert data["code"] == "rate_limited", data
        assert "7" in data["message"], data
        turn = poll_turn(chat["id"], rid)
        assert (turn["state"], turn["error_code"]) == ("error", "rate_limited"), turn

    @pytest.mark.timeout(30)
    def test_stream_without_terminal_event_is_provider_error(self, chat, mock_provider):
        """The provider stream ends (the connection closes) after a delta and
        without response.completed/failed/incomplete: an invalid response,
        so SSE `error` `provider_error` (DESIGN §3.3: "an invalid response
        ... or the provider stream failed"; `stream_interrupted` is only for
        a provider task that ends without a terminal event). The turn fails
        with that code, keeps no answer, and is settled on the estimate
        (billing outcome `failed`)."""
        mock_provider.set_next_scenario(Scenario(
            events=[MockEvent("response.output_text.delta", {"delta": "Partial"})],
            terminal="none",
        ))
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat['id']}/messages:stream",
            json={"content": "trigger error"},
            headers={"Accept": "text/event-stream"}, timeout=30,
        )
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        assert [e.event for e in events] == ["stream_started", "delta", "error"], events
        assert events[-1].data["code"] == "provider_error", events[-1].data
        rid = expect_stream_started(events).data["request_id"]
        turn = poll_turn(chat["id"], rid)
        assert (turn["state"], turn["error_code"], turn.get("assistant_message_id")) == (
            "error", "provider_error", None,
        ), turn
        assert list_messages(chat["id"])[-1]["role"] == "user"
        (event,) = usage_events(rid)
        assert (event["billing_outcome"], event["settlement_method"]) == (
            "failed", "estimated",
        ), event

    @pytest.mark.timeout(30)
    def test_function_call_with_invalid_json_arguments_is_provider_error(self, chat, mock_provider):
        """A `function_call` output item whose `arguments` is not JSON: the
        provider adapter rejects the response before it looks at the tool
        name (openai_responses.rs), so it is `provider_error` with the parse
        error, not `unexpected_tool_use`; the turn fails with that code."""
        mock_provider.set_next_scenario(Scenario(
            events=[MockEvent("response.output_text.delta", {"delta": "Let me look"})],
            output_items=[{
                "type": "function_call",
                "id": "fc_mock_1",
                "call_id": "call_mock_1",
                "name": "search_knowledge",
                "arguments": "{not json",
                "status": "completed",
            }],
        ))
        data, rid = _stream_error(chat["id"])
        assert data["code"] == "provider_error", data
        turn = poll_turn(chat["id"], rid)
        assert (turn["state"], turn["error_code"]) == ("error", "provider_error"), turn

    @pytest.mark.timeout(30)
    def test_completed_without_usage_is_provider_error(self, chat, mock_provider):
        """A `response.completed` without `usage` (the real API always sends
        it; openai_responses.rs `ResponseObject.usage` is required) is an
        invalid response: SSE error `provider_error` with the parse error,
        no `done`, no zero usage reported. The turn fails and is settled on
        the estimate."""
        mock_provider.set_next_scenario(Scenario(
            events=[MockEvent("response.output_text.delta", {"delta": "No usage"})],
            usage=Usage(omit=True),
        ))
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat['id']}/messages:stream",
            json={"content": "trigger error"},
            headers={"Accept": "text/event-stream"}, timeout=30,
        )
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        assert [e.event for e in events] == ["stream_started", "delta", "error"], events
        data = events[-1].data
        assert data["code"] == "provider_error", data
        rid = expect_stream_started(events).data["request_id"]
        turn = poll_turn(chat["id"], rid)
        assert (turn["state"], turn["error_code"]) == ("error", "provider_error"), turn
        (event,) = usage_events(rid)
        assert (event["billing_outcome"], event["settlement_method"]) == (
            "failed", "estimated",
        ), event

    @pytest.mark.parametrize("source", ["response_failed", "http_500"])
    def test_error_message_no_provider_ids(self, chat, mock_provider, source):
        """Provider response, file, vector store and assistant IDs never reach
        the client; each is replaced by `[provider_id]` (infra/llm/mod.rs
        `sanitize_provider_message`) and the rest of the message is kept."""
        if source == "response_failed":
            scenario = Scenario(
                terminal="failed",
                error={"code": "server_error", "message": LEAKY_MESSAGE},
                events=[MockEvent("response.output_text.delta", {"delta": "x"})],
            )
        else:
            scenario = Scenario(
                http_error_status=500,
                http_error_body={"error": {"message": LEAKY_MESSAGE, "type": "server_error"}},
            )
        mock_provider.set_next_scenario(scenario)
        data, _ = _stream_error(chat["id"])
        assert data["code"] == "provider_error"
        for provider_id in PROVIDER_IDS:
            assert provider_id not in data["message"], (
                f"provider id {provider_id} leaked: {data['message']!r}"
            )
        assert data["message"] == "Upstream failure for " + ", ".join(
            ["[provider_id]"] * len(PROVIDER_IDS),
        ), data
