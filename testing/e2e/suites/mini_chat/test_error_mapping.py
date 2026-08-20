"""Tests for error mapping: provider errors -> client-facing SSE/JSON errors."""

import httpx
import pytest
from uuid import uuid4

from .conftest import API_PREFIX, parse_sse, expect_done, DB_PATH
from .mock_provider.responses import Scenario, MockEvent, Usage


def _create_chat() -> str:
    """Create a chat and return its id."""
    resp = httpx.post(f"{API_PREFIX}/chats", json={})
    assert resp.status_code == 201
    return resp.json()["id"]


class TestErrorMapping:
    """Verify that provider-level errors are correctly mapped to client-facing errors."""

    @pytest.fixture(autouse=True)
    def _skip_online(self, request):
        if request.config.getoption("mode") == "online":
            pytest.skip("requires mock provider (offline mode)")

    def test_post_stream_sse_error_event(self, server, mock_provider):
        """Mid-stream provider failure should surface as an SSE error event."""
        chat_id = _create_chat()

        mock_provider.set_next_scenario(Scenario(
            terminal="failed",
            error={"code": "server_error", "message": "Mock fail"},
            events=[MockEvent("response.output_text.delta", {"delta": "Partial"})],
        ))

        _resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "trigger error"},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        status, raw = _resp.status_code, _resp.text
        events = parse_sse(raw) if status == 200 else []
        assert status == 200, f"Expected SSE stream (200), got {status}: {raw}"

        error_events = [e for e in events if e.event == "error"]
        assert len(error_events) >= 1, (
            f"Expected at least one 'error' SSE event, got events: {[e.event for e in events]}"
        )
        error_data = error_events[0].data
        assert isinstance(error_data, dict), f"Error event data should be dict, got: {error_data}"
        assert "code" in error_data, f"Error event missing 'code' field: {error_data}"

    @pytest.mark.xfail(reason="BUG: 504 mapped to provider_error instead of provider_timeout")
    def test_provider_timeout_error_code(self, server, mock_provider):
        """504 from provider should map to a timeout-related error."""
        chat_id = _create_chat()

        mock_provider.set_next_scenario(Scenario(
            http_error_status=504,
            http_error_body={"error": {"message": "Gateway Timeout", "type": "timeout"}},
        ))

        _resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "trigger timeout"},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        status, raw = _resp.status_code, _resp.text
        events = parse_sse(raw) if status == 200 else []

        # Could be a pre-stream JSON error or an SSE error event
        if status != 200:
            # Pre-stream JSON error
            body = httpx.Response(status_code=status, text=raw).json() if raw.strip().startswith("{") else {}
            error_code = body.get("code", body.get("error", {}).get("code", ""))
            assert "timeout" in error_code.lower() or "provider" in error_code.lower() or status in (502, 504), (
                f"Expected timeout-related error, got status={status}, body={raw[:500]}"
            )
        else:
            # SSE error event
            error_events = [e for e in events if e.event == "error"]
            assert len(error_events) >= 1, (
                f"Expected error event for 504, got: {[e.event for e in events]}"
            )
            code = error_events[0].data.get("code", "") if isinstance(error_events[0].data, dict) else ""
            assert code == "provider_timeout", (
                f"Expected provider_timeout error code, got: {code}"
            )

    def test_provider_unavailable_error_code(self, server, mock_provider):
        """503 from provider should map to a provider_error or similar."""
        chat_id = _create_chat()

        mock_provider.set_next_scenario(Scenario(
            http_error_status=503,
            http_error_body={"error": {"message": "Service Unavailable", "type": "server_error"}},
        ))

        _resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "trigger unavailable"},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        status, raw = _resp.status_code, _resp.text
        events = parse_sse(raw) if status == 200 else []

        if status != 200:
            # Accept any 5xx pass-through or mapped error
            assert status == 503, f"Expected 503 for unavailable provider, got {status}"
        else:
            error_events = [e for e in events if e.event == "error"]
            assert len(error_events) >= 1, (
                f"Expected error event for 503, got: {[e.event for e in events]}"
            )
            code = error_events[0].data.get("code", "") if isinstance(error_events[0].data, dict) else ""
            assert code == "provider_error", (
                f"Expected provider_error error code, got: {code}"
            )

    @pytest.mark.xfail(reason="BUG: 429 mapped to provider_error instead of rate_limited")
    def test_rate_limited_error_code(self, server, mock_provider):
        """429 from provider should map to a rate_limited error or similar."""
        chat_id = _create_chat()

        mock_provider.set_next_scenario(Scenario(
            http_error_status=429,
            http_error_body={"error": {"message": "Rate limited", "type": "rate_limit_error"}},
        ))

        _resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "trigger rate limit"},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        status, raw = _resp.status_code, _resp.text
        events = parse_sse(raw) if status == 200 else []

        if status != 200:
            assert status in (429, 400, 503), f"Expected rate-limit mapped status, got {status}: {raw[:300]}"
        else:
            error_events = [e for e in events if e.event == "error"]
            assert len(error_events) >= 1, (
                f"Expected error event for 429, got: {[e.event for e in events]}"
            )
            code = error_events[0].data.get("code", "") if isinstance(error_events[0].data, dict) else ""
            assert "rate" in code.lower() or "limit" in code.lower() or "throttl" in code.lower(), (
                f"Expected rate-limit error code, got: {code}"
            )

    @pytest.mark.xfail(reason="BUG: provider batch ID leaks in error message")
    def test_error_message_no_provider_ids(self, server, mock_provider):
        """Provider-internal IDs (resp_*, batch_*) must not leak to the client."""
        chat_id = _create_chat()

        mock_provider.set_next_scenario(Scenario(
            terminal="failed",
            error={
                "code": "server_error",
                "message": "Error processing resp_abc123xyz in batch_456",
            },
            events=[MockEvent("response.output_text.delta", {"delta": "x"})],
        ))

        _resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "trigger id leak"},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        status, raw = _resp.status_code, _resp.text
        events = parse_sse(raw) if status == 200 else []

        # Find any error in the response (SSE event or JSON body)
        error_message = ""
        if status == 200:
            for e in events:
                if e.event == "error" and isinstance(e.data, dict):
                    error_message = e.data.get("message", "")
                    break
        else:
            import json
            try:
                body = json.loads(raw)
                error_message = body.get("message", body.get("error", {}).get("message", ""))
            except (json.JSONDecodeError, ValueError):
                error_message = raw

        assert "resp_abc123" not in error_message, (
            f"Provider response ID leaked to client: {error_message}"
        )
        assert "batch_456" not in error_message, (
            f"Provider batch ID leaked to client: {error_message}"
        )

    def test_404_masking_authz_denial(self, server):
        """Accessing a nonexistent chat returns 404 (not 403) — verifies masking pattern."""
        # TODO: Full multi-user authz test requires multi-user e2e setup.
        # For now, verify the basic masking: nonexistent resources -> 404, never 403.
        fake_chat_id = str(uuid4())

        resp = httpx.get(f"{API_PREFIX}/chats/{fake_chat_id}")
        assert resp.status_code == 404, (
            f"Expected 404 for nonexistent chat, got {resp.status_code}: {resp.text}"
        )
        # Must not be 403
        assert resp.status_code != 403, "Must not expose 403 for nonexistent resources"
