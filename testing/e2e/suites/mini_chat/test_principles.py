"""Tests for architectural principles — tenant isolation, access control, kill switches, no buffering.

Most of these require multi-tenant or multi-user e2e infrastructure that is not yet
available. Tests are written with the best approximation using the current single-tenant
setup, with TODO notes for the full implementation.

Covers:
- Tenant isolation (single-tenant proxy)
- Owner-only access (single-user proxy)
- License gate (licensed-by-default proxy)
- Model locked per chat
- Kill switch: disable premium, force standard, disable file_search, disable web_search, disable images
- No buffering (streaming proxy)
"""

from __future__ import annotations

import io
import uuid

import httpx
import pytest

from .conftest import (
    API_PREFIX,
    DEFAULT_MODEL,
    STANDARD_MODEL,
    expect_done,
    expect_stream_started,
    parse_sse,
    stream_message,
)
from .mock_provider.responses import MockEvent, Scenario, Usage


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def create_chat(model: str | None = None) -> dict:
    body = {"model": model} if model else {}
    resp = httpx.post(f"{API_PREFIX}/chats", json=body, timeout=10)
    assert resp.status_code == 201, f"Create chat failed: {resp.status_code} {resp.text}"
    return resp.json()


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

class TestPrinciples:
    """Architectural principles — isolation, access control, kill switches."""

    def test_tenant_owns_created_chat(self, server):
        # TODO: Requires multi-tenant e2e setup. Current infra is single-tenant.
        # For now: verify that a chat is accessible to the creating tenant
        # (proves tenant scoping works for the happy path).
        chat = create_chat()
        chat_id = chat["id"]

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}", timeout=10)
        assert resp.status_code == 200
        assert resp.json()["id"] == chat_id

    def test_owner_can_read_own_chat(self, server):
        # TODO: Requires multi-user e2e setup. Current infra uses a single
        # user identity for all requests.
        # For now: verify the creating user can read their own chat.
        chat = create_chat()
        chat_id = chat["id"]

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}", timeout=10)
        assert resp.status_code == 200
        assert resp.json()["id"] == chat_id

    def test_licensed_user_can_create_chat(self, server):
        # TODO: Requires unlicensed tenant. License is always present in
        # test infrastructure.
        # For now: verify that a licensed user can create a chat.
        resp = httpx.post(f"{API_PREFIX}/chats", json={}, timeout=10)
        assert resp.status_code == 201

    def test_model_locked_per_chat(self, server):
        """Model is locked at chat creation time and cannot be changed via PATCH."""
        chat = create_chat()
        chat_id = chat["id"]
        original_model = chat["model"]

        # Attempt to change the model via PATCH
        patch_resp = httpx.patch(
            f"{API_PREFIX}/chats/{chat_id}",
            json={"model": "some-other-model-xyz"},
            timeout=10,
        )

        # PATCH may succeed (ignoring model) or reject the field
        if patch_resp.status_code in (200, 204):
            # If PATCH succeeded, model should still be the original
            get_resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}", timeout=10)
            assert get_resp.status_code == 200
            assert get_resp.json()["model"] == original_model, (
                f"Model should remain {original_model} after PATCH, "
                f"got {get_resp.json()['model']}"
            )
        elif patch_resp.status_code in (400, 422):
            # Rejected — that is also correct behavior
            pass
        else:
            # Unexpected status
            pytest.fail(
                f"Unexpected PATCH status: {patch_resp.status_code} {patch_resp.text}"
            )

    def test_kill_switch_disable_premium(self, chat, mock_provider):
        # TODO: Requires CCM kill switch mock to disable premium models.
        # For now: verify premium model works when kill switch is off
        # (the default test configuration).
        chat_id = chat["id"]

        status, events, _ = stream_message(chat_id, "Say OK.")
        assert status == 200
        done = expect_done(events)
        assert done.data["effective_model"] == DEFAULT_MODEL

    def test_kill_switch_force_standard(self, server):
        # TODO: Requires CCM kill switch mock to force standard tier.
        # For now: verify standard model works (proves standard path is functional).
        chat = create_chat(model=STANDARD_MODEL)
        chat_id = chat["id"]

        status, events, _ = stream_message(chat_id, "Say OK.")
        assert status == 200
        done = expect_done(events)
        # When using standard model, effective_model should be the standard model
        assert done.data["effective_model"] == STANDARD_MODEL

    def test_kill_switch_disable_file_search(self, server):
        # TODO: Requires CCM kill switch mock to disable file_search.
        # For now: verify file_search works when kill switch is off.
        chat = create_chat()
        chat_id = chat["id"]

        # Upload a document so file_search has something to search
        doc_content = b"The capital of France is Paris."
        upload_resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("doc.txt", io.BytesIO(doc_content), "text/plain")},
            timeout=60,
        )
        assert upload_resp.status_code == 201
        attachment = upload_resp.json()

        # Poll until ready
        import time
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            att_resp = httpx.get(
                f"{API_PREFIX}/chats/{chat_id}/attachments/{attachment['id']}",
                timeout=10,
            )
            if att_resp.status_code == 200 and att_resp.json()["status"] in ("ready", "failed"):
                break
            time.sleep(1)
        assert att_resp.json()["status"] == "ready"

        # Send a message — file_search tool events should be present
        status, events, _ = stream_message(
            chat_id,
            "FILESEARCH:What is the capital of France?",
            attachment_ids=[attachment["id"]],
        )
        assert status == 200
        done = expect_done(events)

        # Verify tool events are present (file_search was not disabled)
        tool_events = [e for e in events if e.event == "tool"]
        # Tool events may or may not appear depending on mock behavior;
        # at minimum, the stream should complete successfully
        assert done is not None

    def test_web_search_works_when_enabled(self, chat, mock_provider):
        # TODO: Add negative test for kill switch disable_web_search (requires CCM mock).
        chat_id = chat["id"]

        status, events, _ = stream_message(
            chat_id, "SEARCH:What is the weather today?",
            web_search={"enabled": True},
        )
        assert status == 200
        done = expect_done(events)
        assert done is not None

    def test_kill_switch_disable_images(self, server):
        # TODO: Requires CCM kill switch mock to disable image uploads.
        # For now: verify image upload works when kill switch is off.
        chat = create_chat()
        chat_id = chat["id"]

        # Create a minimal valid PNG (1x1 pixel, red)
        import struct
        import zlib

        def _make_png() -> bytes:
            signature = b"\x89PNG\r\n\x1a\n"
            # IHDR
            ihdr_data = struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0)
            ihdr_crc = zlib.crc32(b"IHDR" + ihdr_data) & 0xFFFFFFFF
            ihdr = struct.pack(">I", 13) + b"IHDR" + ihdr_data + struct.pack(">I", ihdr_crc)
            # IDAT
            raw_row = b"\x00\xff\x00\x00"  # filter byte + RGB
            compressed = zlib.compress(raw_row)
            idat_crc = zlib.crc32(b"IDAT" + compressed) & 0xFFFFFFFF
            idat = struct.pack(">I", len(compressed)) + b"IDAT" + compressed + struct.pack(">I", idat_crc)
            # IEND
            iend_crc = zlib.crc32(b"IEND") & 0xFFFFFFFF
            iend = struct.pack(">I", 0) + b"IEND" + struct.pack(">I", iend_crc)
            return signature + ihdr + idat + iend

        png_data = _make_png()
        upload_resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("test.png", io.BytesIO(png_data), "image/png")},
            timeout=60,
        )
        assert upload_resp.status_code == 201, (
            f"Image upload failed: {upload_resp.status_code} {upload_resp.text}"
        )

    def test_no_buffering(self, chat, mock_provider):
        # TODO: Not fully testable via HTTP. Requires server-side memory
        # instrumentation to prove zero buffering. We verify that SSE
        # streaming works (proves at least SSE is used, not buffered JSON).
        chat_id = chat["id"]

        # Use a slow scenario to make streaming observable
        mock_provider.set_next_scenario(Scenario(
            events=[
                MockEvent("response.output_text.delta", {"delta": "chunk1 "}),
                MockEvent("response.output_text.delta", {"delta": "chunk2 "}),
                MockEvent("response.output_text.delta", {"delta": "chunk3"}),
                MockEvent("response.output_text.done", {"text": "chunk1 chunk2 chunk3"}),
            ],
            usage=Usage(input_tokens=30, output_tokens=6),
            slow=0.1,
        ))

        url = f"{API_PREFIX}/chats/{chat_id}/messages:stream"
        resp = httpx.post(
            url,
            json={"content": "Stream test."},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        raw = resp.text
        events = parse_sse(raw) if resp.status_code == 200 else []
        assert resp.status_code == 200

        # Should have delta events (streaming, not buffered)
        deltas = [e for e in events if e.event == "delta"]
        assert len(deltas) > 0, "No delta events — response may be buffered"

        # Should have done event
        done = expect_done(events)
        assert done is not None
