"""Build OpenAI Responses API SSE wire-format bytes from a Scenario."""

from __future__ import annotations

import json
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .responses import Scenario


class _EventWriter:
    """Formats SSE events as the Responses API sends them: every event's
    data carries its `type` and a `sequence_number` counted from 0 within
    the stream. `event_lines=False` leaves out the `event:` lines (the
    event name is then only in `data.type`)."""

    def __init__(self, event_lines: bool = True):
        self._event_lines = event_lines
        self._sequence = 0

    def __call__(self, event_type: str, data: dict) -> bytes:
        payload = json.dumps(
            {"type": event_type, **data, "sequence_number": self._sequence},
            separators=(",", ":"),
        )
        self._sequence += 1
        if self._event_lines:
            return f"event: {event_type}\ndata: {payload}\n\n".encode()
        return f"data: {payload}\n\n".encode()


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


def _usage(scenario: "Scenario", request_body: dict | None) -> dict:
    """`response.usage`, with the token details the real API always sends."""
    usage = scenario.usage
    input_tokens = (
        _count_input_tokens(request_body, usage.input_tokens)
        if usage.scale_input else usage.input_tokens
    )
    return {
        "input_tokens": input_tokens,
        "input_tokens_details": {"cached_tokens": usage.cached_tokens},
        "output_tokens": usage.output_tokens,
        "output_tokens_details": {"reasoning_tokens": usage.reasoning_tokens},
        "total_tokens": input_tokens + usage.output_tokens,
    }


def sent_usage(scenario: "Scenario", request_body: dict | None) -> dict | None:
    """The `response.usage` the stream of `scenario` carries in its terminal
    event, or None when it carries none."""
    if scenario.http_error_status is not None or scenario.terminal == "none":
        return None
    if any(ev.event_type == "error" for ev in scenario.events):
        return None
    if scenario.terminal == "failed" and not scenario.failed_with_usage:
        return None
    if scenario.terminal == "completed" and scenario.usage.omit:
        return None
    return _usage(scenario, request_body)


def _completed_output(scenario: "Scenario", text: str, request_body: dict | None) -> list[dict]:
    from .responses import has_tool

    if scenario.output is not None:
        # A web_search_call item exists only when the request offered the tool.
        return [
            item for item in scenario.output
            if item.get("type") != "web_search_call" or has_tool(request_body or {}, "web_search")
        ]
    return [
        *scenario.output_items,
        {
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": text,
                    "annotations": scenario.citations or [],
                },
            ],
        },
    ]


def _build_completed_data(
    scenario: "Scenario", model: str, response_id: str, text: str,
    request_body: dict | None = None,
) -> dict:
    # OpenAI wraps the response object inside a "response" key
    response = {
        "id": response_id,
        "object": "response",
        "status": "completed",
        "model": model,
        "output": _completed_output(scenario, text, request_body),
    }
    if not scenario.usage.omit:
        response["usage"] = _usage(scenario, request_body)
    return {"type": "response.completed", "response": response}


def _build_failed_data(
    scenario: "Scenario", model: str, response_id: str, request_body: dict | None = None,
) -> dict:
    # OpenAI puts the error inside the response object: response.error.
    return {
        "type": "response.failed",
        "response": {
            "id": response_id,
            "object": "response",
            "status": "failed",
            "model": model,
            "error": scenario.error or {"code": "server_error", "message": "Unknown error"},
            "incomplete_details": None,
            "output": [],
            "usage": _usage(scenario, request_body) if scenario.failed_with_usage else None,
        },
    }


def _build_incomplete_data(
    scenario: "Scenario", model: str, response_id: str, text: str,
    request_body: dict | None = None,
) -> dict:
    return {
        "type": "response.incomplete",
        "response": {
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
            "usage": _usage(scenario, request_body),
        },
    }


def _event_data(ev, request_body: dict | None) -> dict:
    """Event payload as sent: a code_interpreter_call item carries its
    `outputs` only when the request asks for them (`include`)."""
    item = ev.data.get("item")
    if (
        ev.event_type == "response.output_item.done"
        and isinstance(item, dict)
        and item.get("type") == "code_interpreter_call"
        and "code_interpreter_call.outputs" not in ((request_body or {}).get("include") or [])
    ):
        return {**ev.data, "item": {**item, "outputs": None}}
    return ev.data


def build_sse_chunks(
    scenario: "Scenario",
    model: str,
    response_id: str,
    request_body: dict | None = None,
) -> list[tuple[float, bytes]]:
    """The SSE events of a scenario, one (delay before it, bytes) per event."""
    from .responses import should_include_tool_event

    chunks: list[tuple[float, bytes]] = []
    _sse_event = _EventWriter(event_lines=not scenario.omit_event_lines)

    for ev in scenario.events:
        if request_body and not should_include_tool_event(ev, request_body):
            continue
        chunks.append((ev.delay, _sse_event(ev.event_type, _event_data(ev, request_body))))
        if ev.event_type == "error":
            # A flat `error` event ends a real stream: no terminal event follows.
            return chunks

    text = _accumulate_text(scenario)

    if scenario.terminal == "none":
        return chunks
    if scenario.terminal == "failed":
        chunks.append((0, _sse_event(
            "response.failed", _build_failed_data(scenario, model, response_id, request_body),
        )))
    elif scenario.terminal == "incomplete":
        chunks.append((0, _sse_event(
            "response.incomplete",
            _build_incomplete_data(scenario, model, response_id, text, request_body),
        )))
    else:
        chunks.append((0, _sse_event(
            "response.completed",
            _build_completed_data(scenario, model, response_id, text, request_body),
        )))

    return chunks


def build_sse_stream(
    scenario: "Scenario",
    model: str,
    response_id: str,
    request_body: dict | None = None,
) -> bytes:
    """Build the full SSE byte stream for a scenario."""
    return b"".join(c for _, c in build_sse_chunks(scenario, model, response_id, request_body))
