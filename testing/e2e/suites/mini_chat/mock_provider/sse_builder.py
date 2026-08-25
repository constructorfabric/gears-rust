"""Build OpenAI Responses API SSE wire-format bytes from a Scenario."""

from __future__ import annotations

import json
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .responses import Scenario


def _sse_event(event_type: str, data: dict) -> bytes:
    """Format a single SSE event as wire bytes."""
    payload = json.dumps(data, separators=(",", ":"))
    return f"event: {event_type}\ndata: {payload}\n\n".encode()


def _accumulate_text(scenario: "Scenario") -> str:
    """Accumulate text from all delta events in the scenario."""
    parts = []
    for ev in scenario.events:
        if ev.event_type == "response.output_text.delta":
            parts.append(ev.data.get("delta", ""))
    return "".join(parts)


def _count_input_tokens(request_body: dict | None, base_tokens: int) -> int:
    """Estimate input_tokens from the request body.

    Scales with the number of messages in the input array so that
    multi-turn tests see growing input_tokens per turn.
    """
    if request_body is None:
        return base_tokens
    input_field = request_body.get("input", "")
    if isinstance(input_field, list):
        # ~50 tokens per message (system + user/assistant pairs)
        return max(base_tokens, len(input_field) * 50)
    return base_tokens


def _build_completed_data(
    scenario: "Scenario", model: str, response_id: str, text: str,
    request_body: dict | None = None,
) -> dict:
    # OpenAI wraps the response object inside a "response" key
    annotations = scenario.citations or []
    input_tokens = _count_input_tokens(request_body, scenario.usage.input_tokens)
    output_tokens = scenario.usage.output_tokens
    return {
        "response": {
            "id": response_id,
            "object": "response",
            "status": "completed",
            "model": model,
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": text,
                            "annotations": annotations,
                        },
                    ],
                },
            ],
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "total_tokens": input_tokens + output_tokens,
            },
        },
    }


def _build_failed_data(scenario: "Scenario", response_id: str) -> dict:
    return {
        "id": response_id,
        "object": "response",
        "status": "failed",
        "error": scenario.error or {"code": "server_error", "message": "Unknown error"},
    }


def _build_incomplete_data(
    scenario: "Scenario", model: str, response_id: str, text: str,
    request_body: dict | None = None,
) -> dict:
    input_tokens = _count_input_tokens(request_body, scenario.usage.input_tokens)
    output_tokens = scenario.usage.output_tokens
    return {
        "id": response_id,
        "object": "response",
        "status": "incomplete",
        "model": model,
        "incomplete_details": {
            "reason": scenario.incomplete_reason or "max_output_tokens",
        },
        "output": [
            {
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "output_text",
                        "text": text,
                        "annotations": [],
                    },
                ],
            },
        ],
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
        },
    }


def build_sse_stream(
    scenario: "Scenario",
    model: str,
    response_id: str,
    request_body: dict | None = None,
) -> bytes:
    """Build the full SSE byte stream for a scenario."""
    from .responses import should_include_tool_event

    chunks: list[bytes] = []

    for ev in scenario.events:
        if request_body and not should_include_tool_event(ev, request_body):
            continue
        chunks.append(_sse_event(ev.event_type, ev.data))

    text = _accumulate_text(scenario)

    if scenario.terminal == "failed":
        chunks.append(_sse_event("response.failed", _build_failed_data(scenario, response_id)))
    elif scenario.terminal == "incomplete":
        chunks.append(_sse_event(
            "response.incomplete",
            _build_incomplete_data(scenario, model, response_id, text, request_body),
        ))
    else:
        chunks.append(_sse_event(
            "response.completed",
            _build_completed_data(scenario, model, response_id, text, request_body),
        ))

    return b"".join(chunks)
