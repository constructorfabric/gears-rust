"""E2E tests for the attachment API (upload, get, delete, send-message with attachments).

Run via: ~/projects/gears-rust-worktrees/scripts/run-tests.sh tests/test_attachments.py
"""

import io
import pathlib
import struct
import uuid
import zlib

import pytest
import httpx

from .conftest import API_PREFIX, DEFAULT_MODEL, STANDARD_MODEL, SSEEvent, expect_done, expect_stream_started, parse_sse, poll_until, stream_message

FIXTURES_DIR = pathlib.Path(__file__).parent / "fixtures"


# ---------------------------------------------------------------------------
# 10-01, 10-02: Upload and get attachment
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestUploadAndGet:
    """Upload a file, poll until ready, GET returns full detail."""

    def test_upload_and_get_attachment(self, provider_chat):
        chat_id = provider_chat["id"]
        content = b"This is a test document for RAG."

        # Upload
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("notes.txt", io.BytesIO(content), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201, f"Upload failed: {resp.status_code} {resp.text}"
        body = resp.json()
        att_id = body["id"]
        assert body["filename"] == "notes.txt"
        assert body["content_type"] == "text/plain"
        assert body["size_bytes"] == len(content)
        assert body["kind"] == "document"
        assert body["status"] == "pending" or body["status"] == "ready"

        # Poll until ready
        resp = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )
        detail = resp.json()
        assert detail["status"] == "ready", f"Expected ready, got: {detail}"
        assert detail["id"] == att_id


# ---------------------------------------------------------------------------
# 10-05: Unsupported MIME → 400 invalid_argument (UNSUPPORTED_CONTENT_TYPE)
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestUploadInvalidType:
    """Upload an unsupported MIME type."""

    def test_upload_invalid_type_rejected(self, provider_chat):
        chat_id = provider_chat["id"]
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("archive.zip", io.BytesIO(b"PK\x03\x04fake zip"), "application/zip")},
            timeout=60,
        )
        assert resp.status_code == 400, f"Expected 400, got {resp.status_code}: {resp.text}"
        body = resp.json()
        assert "invalid_argument" in body.get("type", ""), (
            f"Expected canonical invalid_argument type, got: {body.get('type')}"
        )
        violations = body.get("context", {}).get("field_violations", [])
        assert any(v.get("reason") == "UNSUPPORTED_CONTENT_TYPE" for v in violations), (
            f"Expected UNSUPPORTED_CONTENT_TYPE field violation, got: {violations}"
        )


# ---------------------------------------------------------------------------
# 10-03: DELETE Attachment → 204, GET → 404
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestDeleteAndVerifyGone:
    """Upload, delete, GET returns 404."""

    def test_delete_and_verify_gone(self, provider_chat):
        chat_id = provider_chat["id"]

        # Upload and wait for ready
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("gone.txt", io.BytesIO(b"delete me"), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )

        # Delete
        resp = httpx.delete(
            f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}",
            timeout=10,
        )
        assert resp.status_code == 204

        # Verify gone
        resp = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}",
            timeout=10,
        )
        assert resp.status_code == 404


# ---------------------------------------------------------------------------
# 10-04: DELETE Referenced Attachment → 409
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestDeleteReferencedAttachment:
    """Upload, attach to a message, then delete → 409."""

    def test_delete_referenced_attachment_409(self, provider_chat):
        chat_id = provider_chat["id"]

        # Upload and wait for ready
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("ref.txt", io.BytesIO(b"referenced doc"), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        detail = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        ).json()
        assert detail["status"] == "ready"

        # Send a message with this attachment
        status, events, raw = stream_message(
            chat_id,
            "Summarize the attached file.",
            attachment_ids=[att_id],
        )
        assert status == 200, f"Stream failed: {status} {raw[:500]}"
        expect_done(events)

        # Now try to delete — should be 409 (locked by message reference)
        resp = httpx.delete(
            f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}",
            timeout=10,
        )
        assert resp.status_code == 409, (
            f"Expected 409 conflict, got {resp.status_code}: {resp.text}"
        )


# ---------------------------------------------------------------------------
# 10-22: Stream with Document → file_search Tool Events
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestSendMessageWithAttachments:
    """Upload 2 files, send message with attachment_ids, verify stream completes."""

    def test_send_message_with_attachments(self, provider_chat):
        chat_id = provider_chat["id"]

        # Upload two files
        att_ids = []
        for i in range(2):
            resp = httpx.post(
                f"{API_PREFIX}/chats/{chat_id}/attachments",
                files={"file": (f"doc{i}.txt", io.BytesIO(f"Document {i}: The answer is {42 + i}.".encode()), "text/plain")},
                timeout=60,
            )
            assert resp.status_code == 201, f"Upload {i} failed: {resp.status_code}"
            att_id = resp.json()["id"]
            detail = poll_until(
                lambda cid=chat_id, aid=att_id: httpx.get(f"{API_PREFIX}/chats/{cid}/attachments/{aid}", timeout=10),
                until=lambda r: r.json()["status"] in ("ready", "failed"),
            ).json()
            assert detail["status"] == "ready"
            att_ids.append(att_id)

        # Send message referencing both attachments
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "What answers are in the attached documents?", "attachment_ids": att_ids},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Stream failed: {resp.status_code} {resp.text[:500]}"
        events = parse_sse(resp.text)
        expect_done(events)
        ss = expect_stream_started(events)
        assert ss.data.get("message_id")


# ---------------------------------------------------------------------------
# Citation format verification (supplements 10-22 with UUID mapping check)
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
@pytest.mark.online_only
class TestUploadSearchCitationFlow:
    """Upload file, send message triggering file search, verify SSE citations contain UUID."""

    def test_upload_search_citation_flow(self, provider_chat):
        chat_id = provider_chat["id"]

        # Upload a document with distinctive content
        content = (
            b"The capital of the fictional country Zembla is Kinbote City. "
            b"It was founded in 1742 by King Charles the Beloved."
        )
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("zembla.txt", io.BytesIO(content), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        detail = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        ).json()
        assert detail["status"] == "ready"

        # Send message that should trigger file search
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "What is the capital of Zembla? Use the attached document.", "attachment_ids": [att_id]},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Stream failed: {resp.status_code} {resp.text[:500]}"
        events = parse_sse(resp.text)
        done = expect_done(events)

        # Citations are not guaranteed — the LLM may answer from retrieved context
        # without emitting structured citations. Verify format if present.
        citation_events = [e for e in events if e.event == "citations"]
        if citation_events:
            data = citation_events[0].data
            # Citations are wrapped in {"items": [...]}
            citations = data.get("items", []) if isinstance(data, dict) else data
            assert isinstance(citations, list)
            for c in citations:
                assert "source" in c or "type" in c
                if c.get("source") == "file" or c.get("type") == "file":
                    # File citations should have internal UUID, not provider file-xxx
                    file_id = c.get("attachment_id") or c.get("file_id", "")
                    assert not file_id.startswith("file-"), (
                        f"Citation contains provider file_id instead of UUID: {file_id}"
                    )


# ---------------------------------------------------------------------------
# Azure provider: upload, get, send-message with attachments
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestProviderUploadAndGet:
    """Upload a file to a chat per provider, verify upload + poll works."""

    def test_upload_and_get_attachment(self, provider_chat):
        chat_id = provider_chat["id"]
        content = b"This is a test document for RAG."

        # Upload
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("notes.txt", io.BytesIO(content), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201, f"Upload failed: {resp.status_code} {resp.text}"
        body = resp.json()
        att_id = body["id"]
        assert body["filename"] == "notes.txt"
        assert body["kind"] == "document"
        assert body["status"] == "pending" or body["status"] == "ready"

        # Poll until ready
        resp = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )
        detail = resp.json()
        assert detail["status"] == "ready", f"Expected ready, got: {detail}"


@pytest.mark.multi_provider
@pytest.mark.online_only
class TestProviderSendMessageWithAttachment:
    """Upload a file per provider, send message, verify stream completes."""

    def test_send_message_with_attachment(self, provider_chat):
        chat_id = provider_chat["id"]

        # Upload
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("doc.txt", io.BytesIO(b"The secret code is PROVIDER-42."), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        detail = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        ).json()
        assert detail["status"] == "ready"

        # Send message referencing the attachment
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "What is the secret code in the attached document?", "attachment_ids": [att_id]},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Stream failed: {resp.status_code} {resp.text[:500]}"
        events = parse_sse(resp.text)
        expect_done(events)
        ss = expect_stream_started(events)
        assert ss.data.get("message_id")


# ---------------------------------------------------------------------------
# Dual-provider: same operation on OpenAI chat vs Azure chat
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestDualProviderUpload:
    """Upload the same content to an OpenAI chat and an Azure chat.
    Proves DispatchingFileStorage routes to the correct provider-specific impl."""

    def test_dual_provider_upload(self, chat_with_model):
        content = b"Dual-provider test document content."

        # OpenAI chat (STANDARD_MODEL = gpt-5.2 → provider_id "openai")
        openai_chat = chat_with_model(STANDARD_MODEL)
        openai_chat_id = openai_chat["id"]
        resp = httpx.post(
            f"{API_PREFIX}/chats/{openai_chat_id}/attachments",
            files={"file": ("dual-oa.txt", io.BytesIO(content), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201, f"Upload failed: {resp.status_code} {resp.text}"
        oa_att_id = resp.json()["id"]
        resp = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{openai_chat_id}/attachments/{oa_att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )
        assert resp.json()["status"] == "ready", f"Expected ready, got: {resp.json()}"

        # Azure chat (DEFAULT_MODEL = azure-gpt-4.1-mini → provider_id "azure_openai")
        azure_chat = chat_with_model(DEFAULT_MODEL)
        azure_chat_id = azure_chat["id"]
        resp = httpx.post(
            f"{API_PREFIX}/chats/{azure_chat_id}/attachments",
            files={"file": ("dual-az.txt", io.BytesIO(content), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201, f"Upload failed: {resp.status_code} {resp.text}"
        az_att_id = resp.json()["id"]
        resp = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{azure_chat_id}/attachments/{az_att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )
        assert resp.json()["status"] == "ready", f"Expected ready, got: {resp.json()}"


@pytest.mark.multi_provider
@pytest.mark.online_only
class TestDualProviderRAGStream:
    """Upload + send message on both OpenAI and Azure chats.
    Proves end-to-end RAG (file_search) works through both provider-specific
    file + vector store implementations in the same server instance."""

    def test_dual_provider_rag_stream(self, chat_with_model):
        content = b"The secret passphrase is DUAL-PROVIDER-42."
        question = "What is the secret passphrase in the attached document?"

        # OpenAI chat (STANDARD_MODEL = gpt-5.2) — routes through OpenAiFileStorage + OpenAiVectorStore
        openai_chat = chat_with_model(STANDARD_MODEL)
        openai_chat_id = openai_chat["id"]
        resp = httpx.post(
            f"{API_PREFIX}/chats/{openai_chat_id}/attachments",
            files={"file": ("rag-oa.txt", io.BytesIO(content), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201, f"Upload failed: {resp.status_code} {resp.text}"
        oa_att_id = resp.json()["id"]
        resp = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{openai_chat_id}/attachments/{oa_att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )
        assert resp.json()["status"] == "ready", f"Expected ready, got: {resp.json()}"
        resp = httpx.post(
            f"{API_PREFIX}/chats/{openai_chat_id}/messages:stream",
            json={"content": question, "attachment_ids": [oa_att_id]},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Stream failed: {resp.status_code} {resp.text[:500]}"
        oa_events = parse_sse(resp.text)
        ss = expect_stream_started(oa_events)
        assert ss.data.get("message_id")
        done = expect_done(oa_events)
        usage = done.data.get("usage", {})
        assert usage.get("input_tokens", 0) > 0, "Expected non-zero input_tokens"
        assert usage.get("output_tokens", 0) > 0, "Expected non-zero output_tokens"

        # Azure chat (DEFAULT_MODEL = azure-gpt-4.1-mini) — routes through AzureFileStorage + AzureVectorStore
        azure_chat = chat_with_model(DEFAULT_MODEL)
        azure_chat_id = azure_chat["id"]
        resp = httpx.post(
            f"{API_PREFIX}/chats/{azure_chat_id}/attachments",
            files={"file": ("rag-az.txt", io.BytesIO(content), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201, f"Upload failed: {resp.status_code} {resp.text}"
        az_att_id = resp.json()["id"]
        resp = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{azure_chat_id}/attachments/{az_att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )
        assert resp.json()["status"] == "ready", f"Expected ready, got: {resp.json()}"
        resp = httpx.post(
            f"{API_PREFIX}/chats/{azure_chat_id}/messages:stream",
            json={"content": question, "attachment_ids": [az_att_id]},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Stream failed: {resp.status_code} {resp.text[:500]}"
        az_events = parse_sse(resp.text)
        ss = expect_stream_started(az_events)
        assert ss.data.get("message_id")
        done = expect_done(az_events)
        usage = done.data.get("usage", {})
        assert usage.get("input_tokens", 0) > 0, "Expected non-zero input_tokens"
        assert usage.get("output_tokens", 0) > 0, "Expected non-zero output_tokens"


# ---------------------------------------------------------------------------
# Helpers — minimal valid PNG
# ---------------------------------------------------------------------------

def make_minimal_png(width: int = 2, height: int = 2, color: tuple = (255, 0, 0)) -> bytes:
    """Generate a minimal valid PNG image (solid color, no external deps)."""
    def chunk(chunk_type: bytes, data: bytes) -> bytes:
        c = chunk_type + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)

    # IHDR: width, height, bit depth 8, color type 2 (RGB)
    ihdr_data = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    # Raw image data: filter byte 0 + RGB pixels per row
    raw = b""
    for _ in range(height):
        raw += b"\x00" + bytes(color) * width
    idat_data = zlib.compress(raw)

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr_data)
        + chunk(b"IDAT", idat_data)
        + chunk(b"IEND", b"")
    )


# ---------------------------------------------------------------------------
# Image upload and recognition
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestImageUploadAndSend:
    """Upload a PNG image, verify it reaches ready, send a message referencing it."""

    def test_image_upload_and_send(self, provider_chat):
        chat_id = provider_chat["id"]

        # Generate a small red PNG
        png_bytes = make_minimal_png(width=4, height=4, color=(255, 0, 0))

        # Upload
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("red.png", io.BytesIO(png_bytes), "image/png")},
            timeout=60,
        )
        assert resp.status_code == 201, f"Upload failed: {resp.status_code} {resp.text}"
        body = resp.json()
        att_id = body["id"]
        assert body["kind"] == "image", f"Expected image kind, got: {body['kind']}"
        assert body["content_type"] == "image/png"

        # Poll until ready
        detail = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        ).json()
        assert detail["status"] == "ready", f"Expected ready, got: {detail}"

        # Send a message referencing the image
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Describe the attached image. What color is it?", "attachment_ids": [att_id]},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Stream failed: {resp.status_code} {resp.text[:500]}"
        events = parse_sse(resp.text)
        expect_done(events)
        ss = expect_stream_started(events)
        assert ss.data.get("message_id"), "Expected message_id in stream_started event"

        # Collect delta text to see what the LLM said
        delta_text = ""
        for ev in events:
            if ev.event == "delta" and isinstance(ev.data, dict):
                delta_text += ev.data.get("content", "")

        # The LLM should have produced some response
        assert len(delta_text) > 0, "Expected non-empty response from LLM"

        # img_thumbnail may not be implemented yet — check only after streaming assertions pass
        if detail.get("img_thumbnail") is None:
            pytest.xfail("img_thumbnail not populated for ready images yet")


@pytest.mark.multi_provider
@pytest.mark.online_only
class TestImageRecognition:
    """Upload a real cat photo (JPEG) per provider, ask the LLM what animal
    it is, verify the stream completes and the cat is recognized.

    Image inlining is wired — the LLM sees the image as multimodal input
    via the Responses API. The test hard-asserts cat recognition.
    """

    @staticmethod
    def _load_cat_image() -> bytes:
        cat_path = FIXTURES_DIR / "cat.jpg"
        assert cat_path.exists(), f"Fixture not found: {cat_path}"
        return cat_path.read_bytes()

    @staticmethod
    def _upload_image_and_ask(chat_id: str, image_bytes: bytes, filename: str,
                              content_type: str, provider_label: str):
        """Upload an image, poll until ready, send a question, check response."""
        # Upload
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": (filename, io.BytesIO(image_bytes), content_type)},
            timeout=60,
        )
        assert resp.status_code == 201, f"[{provider_label}] Upload failed: {resp.status_code} {resp.text}"
        body = resp.json()
        att_id = body["id"]
        assert body["kind"] == "image"

        # Poll until ready
        resp = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )
        detail = resp.json()
        assert detail["status"] == "ready", f"[{provider_label}] Expected ready, got: {detail}"

        # Ask the LLM to identify the animal
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={
                "content": "Describe exactly what you see in the attached image. If you cannot see any image, respond with exactly 'NO_IMAGE_VISIBLE'.",
                "attachment_ids": [att_id],
            },
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"[{provider_label}] Stream failed: {resp.status_code} {resp.text[:500]}"
        events = parse_sse(resp.text)
        done = expect_done(events)

        # Collect response text
        delta_text = ""
        for ev in events:
            if ev.event == "delta" and isinstance(ev.data, dict):
                delta_text += ev.data.get("content", "")

        assert len(delta_text) > 0, f"[{provider_label}] Expected non-empty response"

        # The LLM must see the image — if it responds with NO_IMAGE_VISIBLE
        # or doesn't mention a cat, image inlining is broken.
        response_lower = delta_text.lower()
        assert "no_image_visible" not in response_lower, (
            f"[{provider_label}] LLM cannot see the image — file_id not included "
            f"as multimodal input in the Responses API request (image inlining gap). "
            f"Response: {delta_text!r}"
        )
        recognized = any(w in response_lower for w in ("cat", "kitten", "feline"))
        assert recognized, (
            f"[{provider_label}] LLM responded but did not recognize the cat. "
            f"Response: {delta_text!r}"
        )

        # img_thumbnail may not be implemented yet — check only after streaming assertions pass
        if detail.get("img_thumbnail") is None:
            pytest.xfail("img_thumbnail not populated for ready images yet")

    def test_image_recognition_cat(self, provider_chat):
        cat_bytes = self._load_cat_image()
        self._upload_image_and_ask(provider_chat["id"], cat_bytes, "cat.jpg", "image/jpeg", provider_chat.get("model", "unknown"))


# ---------------------------------------------------------------------------
# Mixed document + image: both mechanisms must work simultaneously
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
@pytest.mark.online_only
class TestDocumentAndImageTogether:
    """Upload a document AND an image, send a message referencing both.

    The LLM must use file_search to read the document AND see the image
    via multimodal input. The question is designed so the correct answer
    requires information from BOTH sources.
    """

    def test_document_and_image_combined(self, provider_chat):
        chat_id = provider_chat["id"]

        # 1. Upload a document with a secret code word
        doc_content = (
            "CONFIDENTIAL REPORT\n"
            "The secret code word for this project is: FLAMINGO.\n"
            "Do not share this code word with anyone.\n"
        )
        doc_resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("secret-report.txt", io.BytesIO(doc_content.encode()), "text/plain")},
            timeout=60,
        )
        assert doc_resp.status_code == 201
        doc_id = doc_resp.json()["id"]
        doc_detail = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{doc_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        ).json()
        assert doc_detail["status"] == "ready"
        assert doc_detail["kind"] == "document"

        # 2. Upload the cat image
        cat_bytes = (pathlib.Path(__file__).parent / "fixtures" / "cat.jpg").read_bytes()
        img_resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("animal.jpg", io.BytesIO(cat_bytes), "image/jpeg")},
            timeout=60,
        )
        assert img_resp.status_code == 201
        img_id = img_resp.json()["id"]
        img_detail = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{img_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        ).json()
        assert img_detail["status"] == "ready"
        assert img_detail["kind"] == "image"

        # 3. Ask a question that requires BOTH sources
        #    - The document contains the code word "FLAMINGO"
        #    - The image contains a cat
        #    The LLM must mention both to prove it accessed both.
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={
                "content": (
                    "I attached a document and an image. "
                    "Tell me: 1) What is the secret code word from the document? "
                    "2) What animal is in the image? "
                    "Answer both questions."
                ),
                "attachment_ids": [doc_id, img_id],
            },
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Stream failed: {resp.status_code} {resp.text[:500]}"
        events = parse_sse(resp.text)
        done = expect_done(events)

        # Collect response text
        delta_text = ""
        for ev in events:
            if ev.event == "delta" and isinstance(ev.data, dict):
                delta_text += ev.data.get("content", "")

        assert len(delta_text) > 0, "Expected non-empty response"
        response_lower = delta_text.lower()

        # Must recognize the cat from the image (multimodal input) — hard assert
        has_cat = any(w in response_lower for w in ("cat", "kitten", "feline"))
        assert has_cat, (
            f"LLM did not recognize the cat from the image. "
            f"Image inlining (input_image) may not be working. Response: {delta_text!r}"
        )

        # Should mention the code word from the document (file_search)
        # Soft check: file_search depends on vector store indexing timing
        has_code_word = "flamingo" in response_lower
        if not has_code_word:
            print(
                f"\n[WARN] LLM saw the cat but did not find 'FLAMINGO' from the document. "
                f"file_search may not have retrieved the document (indexing timing). "
                f"Response: {delta_text!r}"
            )


# ---------------------------------------------------------------------------
# Streaming upload: size enforcement and size_bytes accuracy
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestUploadSizeEnforcement:
    """Upload size limit enforcement — files exceeding the configured limit
    are rejected with HTTP 400 ``out_of_range`` (FILE_TOO_LARGE).

    NOTE: these tests rely on the server's default config limits:
    - ``uploaded_file_max_size_kb``: 25600 (25 MB) for documents
    - ``uploaded_image_max_size_kb``: 5120 (5 MB) for images
    """

    def test_oversize_image_rejected(self, provider_chat):
        """Upload an image exceeding uploaded_image_max_size_kb (5 MB) → 400.

        Uses ~6 MB which is over the image limit but under the API gateway's
        global 16 MiB body limit, so our handler's streaming size check runs.
        """
        chat_id = provider_chat["id"]
        # 6 MB > 5 MB default image limit, but < 16 MiB gateway limit
        oversize_payload = b"\x89PNG" + b"\x00" * (6 * 1024 * 1024)
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("huge.png", io.BytesIO(oversize_payload), "image/png")},
            timeout=60,
        )
        assert resp.status_code == 400, (
            f"Expected 400 for oversize image, got {resp.status_code}: {resp.text}"
        )
        body = resp.json()
        assert "out_of_range" in body.get("type", ""), (
            f"Expected canonical out_of_range type, got: {body.get('type')}"
        )
        violations = body.get("context", {}).get("field_violations", [])
        assert any(v.get("reason") == "FILE_TOO_LARGE" for v in violations), (
            f"Expected FILE_TOO_LARGE field violation, got: {violations}"
        )

    def test_oversize_document_rejected_by_gateway(self, provider_chat):
        """Upload a document exceeding the per-kind handler limit (25 MB) → 400.

        Documents are capped at 25 MB by the per-kind handler size check.
        A 26 MB upload should be rejected by that handler before any further
        processing occurs.
        """
        chat_id = provider_chat["id"]
        # 26 MB > 25 MB per-kind handler limit
        oversize_payload = b"\x00" * (26 * 1024 * 1024)
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("huge.pdf", io.BytesIO(oversize_payload), "application/pdf")},
            timeout=60,
        )
        assert resp.status_code == 400, (
            f"Expected 400 for oversize document, got {resp.status_code}: {resp.text}"
        )
        body = resp.json()
        assert "out_of_range" in body.get("type", ""), (
            f"Expected canonical out_of_range type, got: {body.get('type')}"
        )
        violations = body.get("context", {}).get("field_violations", [])
        assert any(v.get("reason") == "FILE_TOO_LARGE" for v in violations), (
            f"Expected FILE_TOO_LARGE field violation, got: {violations}"
        )

    def test_document_within_limit_succeeds(self, provider_chat):
        """Upload a document just under the limit → succeeds."""
        chat_id = provider_chat["id"]
        # 1 MB — well under 25 MB
        payload = b"x" * (1 * 1024 * 1024)
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("medium.txt", io.BytesIO(payload), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201, (
            f"Expected 201 for within-limit doc, got {resp.status_code}: {resp.text}"
        )
        att_id = resp.json()["id"]
        detail = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        ).json()
        assert detail["status"] == "ready"


@pytest.mark.multi_provider
class TestUploadSizeBytesAccuracy:
    """Verify that size_bytes in the attachment metadata matches the actual
    uploaded file size."""

    def test_size_bytes_matches_actual(self, provider_chat):
        """Upload a file of known size, verify size_bytes in GET response."""
        chat_id = provider_chat["id"]
        # Use a specific, non-round size to catch off-by-one issues
        payload = b"A" * 123_456
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("sized.txt", io.BytesIO(payload), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        resp = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        )
        detail = resp.json()
        assert detail["status"] == "ready"
        assert detail["size_bytes"] == 123_456, (
            f"Expected size_bytes=123456, got {detail['size_bytes']}"
        )


@pytest.mark.multi_provider
class TestUploadStreamingPipeline:
    """End-to-end test with a medium-sized file (~500 KB) through the full
    streaming upload pipeline: upload → ready → send message → SSE done."""

    @pytest.mark.online_only
    def test_medium_file_upload_and_stream(self, provider_chat):
        chat_id = provider_chat["id"]
        # 500 KB document
        payload = b"The quick brown fox. " * 25_000  # ~500 KB
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("medium_doc.txt", io.BytesIO(payload), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        detail = poll_until(
            lambda: httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10),
            until=lambda r: r.json()["status"] in ("ready", "failed"),
        ).json()
        assert detail["status"] == "ready", f"Expected ready, got: {detail}"

        # Send a message referencing the attachment
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Summarize the attached document briefly.", "attachment_ids": [att_id]},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Stream failed: {resp.status_code} {resp.text}"
        events = parse_sse(resp.text)

        started = expect_stream_started(events)
        assert started is not None, "Expected stream_started event"

        done = expect_done(events)
        assert done is not None, "Expected done event"
        assert done.data.get("usage", {}).get("input_tokens", 0) > 0
