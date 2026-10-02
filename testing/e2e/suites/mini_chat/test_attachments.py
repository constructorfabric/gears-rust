"""E2E tests for the attachment API (upload, get, delete, send-message with attachments)."""

import base64
import http.client
import io
import json
import pathlib
import struct
import threading
import time
import urllib.parse
import uuid
import zlib

import pytest
import httpx

from .conftest import (
    API_PREFIX,
    BARE_MODEL,
    DEFAULT_MODEL,
    RESOURCE_ATTACHMENT,
    RESOURCE_CHAT,
    STANDARD_MODEL,
    TOKEN_USER_A,
    assert_problem,
    exec_db,
    expect_done,
    expect_stream_started,
    list_messages,
    parse_sse,
    provider_file_id,
    query_db,
    stream_message,
    usage_events,
    uuid_from_db,
    wait_cleanup_terminal,
    wait_for,
    settled_attachment,
)
from .mock_provider.responses import SCENARIOS, Scenario
from .mock_provider.server import FILES_PATH

FIXTURES_DIR = pathlib.Path(__file__).parent / "fixtures"

# `detail` of a canonical service_unavailable Problem (a provider error,
# src/api/rest/error.rs `DomainError::ProviderError`).
DETAIL_SERVICE_UNAVAILABLE = "Service temporarily unavailable"

# ThumbnailConfig default width and height (config.rs), not overridden in base.yaml.
THUMBNAIL_MAX_SIDE = 128

# api-gateway `defaults.body_limit_bytes` (config/base.yaml).
GATEWAY_BODY_LIMIT_BYTES = 64_000_000

# Storage internals that must never appear in attachment responses.
INTERNAL_ATTACHMENT_FIELDS = ("provider_file_id", "storage_backend", "vector_store_id")


# ---------------------------------------------------------------------------
# 10-01, 10-02: Upload and get attachment
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestUploadAndGet:
    """Upload a file; the 201 response and GET return the full detail."""

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
        # Upload waits for indexing (ADR-0007); a real provider can still be
        # indexing at the request deadline, then the row settles later.
        body = settled_attachment(chat_id, body)
        assert body["status"] == "ready", body

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10)
        assert resp.status_code == 200, resp.text
        detail = resp.json()
        assert detail["id"] == att_id
        assert detail["status"] == "ready", detail
        assert (detail["filename"], detail["content_type"], detail["size_bytes"], detail["kind"]) == (
            "notes.txt", "text/plain", len(content), "document",
        )

    def test_provider_storage_fields_not_exposed(self, provider_chat):
        """Neither the upload response nor GET exposes provider storage details."""
        chat_id = provider_chat["id"]
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("notes.txt", io.BytesIO(b"internal fields check"), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201, resp.text
        detail = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{resp.json()['id']}").json()
        for body in (resp.json(), detail):
            for field in INTERNAL_ATTACHMENT_FIELDS:
                assert field not in body, f"{field} exposed: {body}"

    def test_get_nonexistent_attachment_404(self, provider_chat):
        att_id = str(uuid.uuid4())
        resp = httpx.get(f"{API_PREFIX}/chats/{provider_chat['id']}/attachments/{att_id}")
        body = assert_problem(resp, 404, "not_found", resource_type=RESOURCE_ATTACHMENT)
        assert body["context"].get("resource_name") == att_id, body


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
        assert_problem(resp, 400, "invalid_argument", field_reason="UNSUPPORTED_CONTENT_TYPE")


# ---------------------------------------------------------------------------
# 10-03: DELETE Attachment → 204, GET → 404
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestDeleteAndVerifyGone:
    """Upload, delete, GET returns 404."""

    def test_delete_and_verify_gone(self, provider_chat):
        chat_id = provider_chat["id"]

        att_id = _upload_ready(chat_id, "gone.txt", b"delete me", "text/plain")

        resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10)
        assert resp.status_code == 204

        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10)
        assert_problem(resp, 404, "not_found", resource_type=RESOURCE_ATTACHMENT)


# ---------------------------------------------------------------------------
# 10-04: DELETE Referenced Attachment → 409
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestDeleteReferencedAttachment:
    """Upload, attach to a message, then delete → 409."""

    def test_delete_referenced_attachment_409(self, provider_chat):
        chat_id = provider_chat["id"]

        att_id = _upload_ready(chat_id, "ref.txt", b"referenced doc", "text/plain")

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
        body = assert_problem(resp, 409, "already_exists")
        assert body["context"]["resource_name"] == "attachment_locked"

        # The attachment is kept.
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10)
        assert resp.status_code == 200
        assert resp.json()["status"] == "ready"


# ---------------------------------------------------------------------------
# 10-38: Send Message Referencing Two Ready Documents
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestSendMessageWithAttachments:
    """Upload 2 files, send message with attachment_ids, verify stream completes."""

    def test_send_message_with_attachments(self, request, provider_chat, mock_provider):
        """The stream ends in `done`; GET /messages lists both documents on
        the user message as attachment summaries (`attachment_id`, `kind`,
        `filename`, `status`, no `img_thumbnail`). Offline, the provider
        request carries `file_search` on the chat's vector store and no
        `input_image`."""
        chat_id = provider_chat["id"]

        att_ids = [
            _upload_ready(
                chat_id, f"doc{i}.txt", f"Document {i}: The answer is {42 + i}.".encode(), "text/plain",
            )
            for i in range(2)
        ]

        # Send message referencing both attachments
        mock_provider.clear_captured_requests()
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

        user_msg, assistant_msg = list_messages(chat_id)
        assert sorted(user_msg["attachments"], key=lambda a: a["filename"]) == [
            {"attachment_id": att_ids[i], "kind": "document", "filename": f"doc{i}.txt",
             "status": "ready"}
            for i in range(2)
        ], user_msg
        assert assistant_msg["attachments"] == [], assistant_msg

        if request.config.getoption("mode") == "online":
            return
        (req,) = mock_provider.get_captured_requests()
        assert [(t["type"], t["vector_store_ids"]) for t in req["tools"]] == [
            ("file_search", [_vector_store_id(chat_id)]),
        ], req["tools"]
        parts = [p for i in req["input"] if i.get("role") == "user"
                 for p in (i["content"] if isinstance(i["content"], list) else [])]
        assert [p for p in parts if p.get("type") == "input_image"] == [], parts


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
        att_id = _upload_ready(chat_id, "zembla.txt", content, "text/plain")

        # Send message that should trigger file search
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "What is the capital of Zembla? Use the attached document.", "attachment_ids": [att_id]},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Stream failed: {resp.status_code} {resp.text[:500]}"
        events = parse_sse(resp.text)
        expect_done(events)

        # The question names the document, so the model cites it.
        citation_events = [e for e in events if e.event == "citations"]
        assert len(citation_events) == 1, [e.event for e in events]
        file_citations = [c for c in citation_events[0].data["items"] if c["source"] == "file"]
        assert file_citations, citation_events[0].data
        for c in file_citations:
            assert c["attachment_id"] == att_id, c
            assert c["title"] == "zembla.txt", c


# ---------------------------------------------------------------------------
# Azure provider: upload, get, send-message with attachments
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
@pytest.mark.online_only
class TestProviderSendMessageWithAttachment:
    """Upload a file per provider, send message, verify stream completes."""

    def test_send_message_with_attachment(self, provider_chat):
        chat_id = provider_chat["id"]

        att_id = _upload_ready(chat_id, "doc.txt", b"The secret code is PROVIDER-42.", "text/plain")

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
@pytest.mark.usefixtures("offline_only")
class TestDualProviderUpload:
    """Upload the same content to an OpenAI chat and an Azure chat.
    Proves DispatchingFileStorage routes each upload to its chat's provider."""

    def test_dual_provider_upload(self, chat_with_model, mock_provider):
        """10-13: each upload is `ready` and reaches the Files API of its own
        provider: POST /v1/files for OpenAI, POST /openai/files for Azure
        (base.yaml `api_path` prefix `/openai`)."""
        content = b"Dual-provider test document content."
        uploads = {}
        for provider, model in (("openai", STANDARD_MODEL), ("azure", DEFAULT_MODEL)):
            chat_id = chat_with_model(model)["id"]
            mock_provider.clear_captured_requests()
            _upload_ready(chat_id, f"dual-{provider}.txt", content, "text/plain")
            uploads[provider] = [p.split("?")[0] for _, p in _file_upload_calls(mock_provider)]

        assert uploads == {"openai": ["/v1/files"], "azure": ["/openai/files"]}, uploads


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
        openai_chat_id = chat_with_model(STANDARD_MODEL)["id"]
        oa_att_id = _upload_ready(openai_chat_id, "rag-oa.txt", content, "text/plain")
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

        # Azure chat (DEFAULT_MODEL = azure-gpt-4.1) — routes through AzureFileStorage + AzureVectorStore
        azure_chat_id = chat_with_model(DEFAULT_MODEL)["id"]
        az_att_id = _upload_ready(azure_chat_id, "rag-az.txt", content, "text/plain")
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
@pytest.mark.usefixtures("offline_only")
class TestImageUploadAndSend:
    """Upload a PNG image, verify it reaches ready, send a message referencing it.

    Offline only: it checks the provider traffic seen by the mock."""

    def test_image_upload_and_send(self, provider_chat, mock_provider):
        """10-15: a 200x100 PNG is `ready` with a WebP thumbnail fitted into
        128x128 keeping the aspect ratio (128x64). The message sends the
        image as `input_image` with its provider file id and no
        `file_search` (no vector store); GET /messages lists the image on
        the user message with the same thumbnail."""
        chat_id = provider_chat["id"]

        png_bytes = make_minimal_png(width=200, height=100, color=(255, 0, 0))

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
        body = settled_attachment(chat_id, body)
        assert body["status"] == "ready", body

        detail = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10).json()
        thumb = detail["img_thumbnail"]
        assert set(thumb) == {"content_type", "width", "height", "data_base64"}, thumb
        assert (thumb["content_type"], thumb["width"], thumb["height"]) == (
            "image/webp", THUMBNAIL_MAX_SIDE, THUMBNAIL_MAX_SIDE // 2,
        ), thumb
        webp = base64.b64decode(thumb["data_base64"], validate=True)
        assert webp[:4] == b"RIFF" and webp[8:12] == b"WEBP", webp[:16]
        # An image is not indexed for file_search: no vector store is created.
        vector_store_calls = [p for p in mock_provider.get_post_paths() if "/vector_stores" in p]
        assert vector_store_calls == []

        # Send a message referencing the image
        mock_provider.clear_captured_requests()
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

        (req,) = mock_provider.get_captured_requests()
        user_items = [i for i in req["input"] if i.get("role") == "user"]
        assert user_items[-1]["content"] == [
            {"type": "input_text", "text": "Describe the attached image. What color is it?"},
            {"type": "input_image", "file_id": provider_file_id(att_id)},
        ], user_items[-1]
        assert "file_search" not in [t.get("type") for t in req.get("tools") or []], req.get("tools")

        user_msg = list_messages(chat_id)[0]
        assert user_msg["attachments"] == [{
            "attachment_id": att_id, "kind": "image", "filename": "red.png",
            "status": "ready", "img_thumbnail": thumb,
        }], user_msg


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
        body = settled_attachment(chat_id, body)
        assert body["status"] == "ready", f"[{provider_label}] Expected ready, got: {body}"
        detail = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10).json()

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
        expect_done(events)

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
        assert detail["img_thumbnail"] is not None, f"[{provider_label}] ready image must have a thumbnail"

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
        doc_resp = _upload(chat_id, "secret-report.txt", doc_content.encode(), "text/plain")
        assert doc_resp.status_code == 201, doc_resp.text
        assert (doc_resp.json()["status"], doc_resp.json()["kind"]) == ("ready", "document")
        doc_id = doc_resp.json()["id"]

        # 2. Upload the cat image
        cat_bytes = (pathlib.Path(__file__).parent / "fixtures" / "cat.jpg").read_bytes()
        img_resp = _upload(chat_id, "animal.jpg", cat_bytes, "image/jpeg")
        assert img_resp.status_code == 201, img_resp.text
        assert (img_resp.json()["status"], img_resp.json()["kind"]) == ("ready", "image")
        img_id = img_resp.json()["id"]

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
        expect_done(events)

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

        # Must mention the code word from the document (file_search).
        assert "flamingo" in response_lower, (
            f"LLM did not find 'FLAMINGO' from the document (file_search). "
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

    @pytest.mark.usefixtures("offline_only")
    def test_oversize_image_rejected(self, provider_chat, mock_provider):
        """Upload an image exceeding uploaded_image_max_size_kb (5 MB) → 400.

        httpx sends a Content-Length, so the handler's pre-check rejects the
        ~6 MB upload from that header (less 64 KiB of multipart framing)
        before the streaming size counter reads the part: the violation is
        on `content_length` (handlers/attachments.rs, step 8c). The streaming
        counter is covered by TestChunkedUpload (no Content-Length).
        """
        chat_id = provider_chat["id"]
        oversize_payload = b"\x89PNG" + b"\x00" * (6 * 1024 * 1024)
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("huge.png", io.BytesIO(oversize_payload), "image/png")},
            timeout=60,
        )
        body = assert_problem(resp, 400, "out_of_range", field_reason="FILE_TOO_LARGE")
        assert [v["field"] for v in body["context"]["field_violations"]] == ["content_length"], body
        assert _file_upload_calls(mock_provider) == []
        # Rejected before the attachment row is inserted.
        assert query_db("SELECT id FROM attachments WHERE chat_id = ?", (chat_id,)) == []

    @pytest.mark.usefixtures("offline_only")
    def test_oversize_document_rejected(self, provider_chat, mock_provider):
        """Upload a document exceeding the per-kind limit (25 MB) → 400.

        As for the image, the Content-Length pre-check rejects the 26 MB
        upload before the streaming size counter reads the part.
        """
        chat_id = provider_chat["id"]
        oversize_payload = b"\x00" * (26 * 1024 * 1024)
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("huge.pdf", io.BytesIO(oversize_payload), "application/pdf")},
            timeout=60,
        )
        body = assert_problem(resp, 400, "out_of_range", field_reason="FILE_TOO_LARGE")
        assert [v["field"] for v in body["context"]["field_violations"]] == ["content_length"], body
        assert _file_upload_calls(mock_provider) == []
        # Rejected before the attachment row is inserted.
        assert query_db("SELECT id FROM attachments WHERE chat_id = ?", (chat_id,)) == []

    @pytest.mark.usefixtures("offline_only")
    @pytest.mark.timeout(30)
    def test_body_over_gateway_limit_413(self, provider_chat, mock_provider):
        """A request whose Content-Length is over the api-gateway
        `body_limit_bytes` (64_000_000, config/base.yaml) is 413 from the
        gateway before the handler runs: no attachment row, nothing sent to
        the provider. Only the headers and the start of the body are sent:
        the gateway answers from the Content-Length."""
        chat_id = provider_chat["id"]
        url = urllib.parse.urlsplit(f"{API_PREFIX}/chats/{chat_id}/attachments")
        conn = http.client.HTTPConnection(url.hostname, url.port, timeout=30)
        try:
            conn.putrequest("POST", url.path)
            conn.putheader("Authorization", f"Bearer {TOKEN_USER_A}")
            conn.putheader("Content-Type", "multipart/form-data; boundary=x")
            conn.putheader("Content-Length", str(GATEWAY_BODY_LIMIT_BYTES + 1))
            conn.endheaders()
            conn.send(b"--x\r\n")
            resp = conn.getresponse()
            status, content_type, body = (
                resp.status, resp.getheader("Content-Type", ""), resp.read(),
            )
        finally:
            conn.close()
        assert status == 413, (status, body)
        # A Problem, but not a canonical one: the gateway's body limit layer
        # answers `type` about:blank and no category, so assert_problem does
        # not apply.
        assert content_type == "application/problem+json", content_type
        problem = json.loads(body)
        assert (problem["type"], problem["title"], problem["status"], problem["detail"]) == (
            "about:blank", "Payload Too Large", 413, "Payload Too Large",
        ), problem
        assert query_db("SELECT id FROM attachments WHERE chat_id = ?", (chat_id,)) == []
        assert _file_upload_calls(mock_provider) == []

    def test_document_within_limit_succeeds(self, provider_chat):
        """Upload a document just under the limit → succeeds."""
        chat_id = provider_chat["id"]
        # 1 MB — well under 25 MB. Real text: a real vector store does not
        # finish indexing a megabyte of a single repeated character.
        line = b"The quick brown fox jumps over the lazy dog near the river bank.\n"
        payload = (line * (1024 * 1024 // len(line) + 1))[: 1024 * 1024]
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/attachments",
            files={"file": ("medium.txt", io.BytesIO(payload), "text/plain")},
            timeout=60,
        )
        assert resp.status_code == 201, (
            f"Expected 201 for within-limit doc, got {resp.status_code}: {resp.text}"
        )
        body = settled_attachment(chat_id, resp.json())
        assert body["status"] == "ready", body


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
        assert settled_attachment(chat_id, resp.json())["status"] == "ready", resp.json()
        att_id = resp.json()["id"]
        detail = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10).json()
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
        att_id = _upload_ready(chat_id, "medium_doc.txt", payload, "text/plain")

        # Send a message referencing the attachment
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Summarize the attached document briefly.", "attachment_ids": [att_id]},
            headers={"Accept": "text/event-stream"},
            timeout=90,
        )
        assert resp.status_code == 200, f"Stream failed: {resp.status_code} {resp.text}"
        events = parse_sse(resp.text)

        expect_stream_started(events)
        done = expect_done(events)
        assert done.data["usage"]["input_tokens"] > 0


# ---------------------------------------------------------------------------
# Helpers for limit and failure scenarios
# ---------------------------------------------------------------------------

# RagConfig defaults (gears/mini-chat/mini-chat/src/config.rs), not overridden
# in config/base.yaml.
MAX_IMAGES_PER_MESSAGE = 4
MAX_DOCUMENTS_PER_CHAT = 50
MAX_TOTAL_UPLOAD_BYTES_PER_CHAT = 100 * 1_048_576
MAX_IMAGE_BYTES = 5 * 1024 * 1024
MAX_DOCUMENT_BYTES = 25 * 1024 * 1024  # uploaded_file_max_size_kb 25600


def _require_offline(request):
    if request.config.getoption("mode") == "online":
        pytest.skip("uses the mock provider or seeds the DB (offline mode)")


def _upload(chat_id: str, filename: str, payload: bytes, content_type: str) -> httpx.Response:
    return httpx.post(
        f"{API_PREFIX}/chats/{chat_id}/attachments",
        files={"file": (filename, io.BytesIO(payload), content_type)},
        timeout=60,
    )


def _upload_ready(chat_id: str, filename: str, payload: bytes, content_type: str) -> str:
    """Upload a file; the upload is synchronous, so the 201 already reports
    `ready` (ADR-0007). Return the attachment id."""
    resp = _upload(chat_id, filename, payload, content_type)
    assert resp.status_code == 201, f"upload failed: {resp.status_code} {resp.text}"
    body = settled_attachment(chat_id, resp.json())
    assert body["status"] == "ready", body
    return body["id"]


def _clone_attachment(src_id: str, *, size_bytes: int | None = None) -> str:
    """Insert a copy of attachment row `src_id` under a new id and a new
    provider file id (each upload has its own provider file); return the id."""
    cols = [r["name"] for r in query_db("PRAGMA table_info(attachments)")]
    exprs = []
    params: list = []
    new_id = str(uuid.uuid4())
    overrides = {"id": new_id, "provider_file_id": f"file-seed-{uuid.uuid4().hex[:12]}"}
    if size_bytes is not None:
        overrides["size_bytes"] = size_bytes
    for col in cols:
        if col in overrides:
            exprs.append("?")
            params.append(overrides[col])
        else:
            exprs.append(col)
    params.append(src_id)
    inserted = exec_db(
        f"INSERT INTO attachments ({', '.join(cols)}) "
        f"SELECT {', '.join(exprs)} FROM attachments WHERE id = ?",
        tuple(params),
    )
    assert inserted == 1
    return new_id


def _file_upload_calls(mock_provider) -> list[tuple[str, str]]:
    """POST /files requests (provider file uploads) seen by the mock."""
    return [
        (m, p) for m, p in mock_provider.get_request_paths()
        if m == "POST" and p.split("?")[0].endswith("/files") and "/vector_stores/" not in p
    ]


def _chunked_multipart(filename: str, content_type: str, payload: bytes,
                       chunk_size: int = 64 * 1024):
    """A multipart body as a generator (httpx sends it chunked, without
    Content-Length); returns (Content-Type header, body generator)."""
    boundary = uuid.uuid4().hex
    head = (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="file"; filename="{filename}"\r\n'
        f"Content-Type: {content_type}\r\n\r\n"
    ).encode()
    tail = f"\r\n--{boundary}--\r\n".encode()

    def body():
        yield head
        for i in range(0, len(payload), chunk_size):
            yield payload[i:i + chunk_size]
        yield tail

    return f"multipart/form-data; boundary={boundary}", body()


def _upload_chunked(chat_id: str, filename: str, payload: bytes, content_type: str) -> httpx.Response:
    ct, body = _chunked_multipart(filename, content_type, payload)
    resp = httpx.post(
        f"{API_PREFIX}/chats/{chat_id}/attachments",
        content=body, headers={"Content-Type": ct}, timeout=60,
    )
    assert resp.request.headers.get("transfer-encoding") == "chunked"
    assert "content-length" not in resp.request.headers
    return resp


# ---------------------------------------------------------------------------
# 04-11: Too many images in one message
# ---------------------------------------------------------------------------

class TestTooManyImages:
    """More ready images than `max_images_per_message` in one message."""

    def test_too_many_images_rejected(self, request, chat, mock_provider):
        """04-11: max_images_per_message + 1 images → 400 out_of_range
        TOO_MANY_IMAGES; the provider is not called."""
        _require_offline(request)
        chat_id = chat["id"]  # vision-capable default model
        att_ids = [
            _upload_ready(chat_id, f"img{i}.png", make_minimal_png(color=(i * 40, 0, 0)), "image/png")
            for i in range(MAX_IMAGES_PER_MESSAGE + 1)
        ]

        mock_provider.clear_captured_requests()
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Compare these images.", "attachment_ids": att_ids},
            headers={"Accept": "text/event-stream"}, timeout=30,
        )
        assert_problem(resp, 400, "out_of_range", field_reason="TOO_MANY_IMAGES")
        assert mock_provider.get_captured_requests() == [], "provider must not be called"


# ---------------------------------------------------------------------------
# 10-16: provider upload failure → 503, attachment failed with error_code
# ---------------------------------------------------------------------------

class TestUploadProviderFailure:
    """The provider Files API fails during the upload."""

    def test_upload_failure_marks_attachment_failed(self, request, chat, mock_provider):
        """10-16: provider 500 on POST /files → 503 service_unavailable with
        Retry-After 10 (every provider error, api/rest/error.rs; the upload
        concurrency limit answers 5, 10-40); the inserted row is visible with
        status failed and an error_code."""
        _require_offline(request)
        chat_id = chat["id"]
        mock_provider.set_fault("POST", FILES_PATH, 500)

        resp = _upload(chat_id, "fail.txt", b"provider will fail", "text/plain")
        assert_problem(resp, 503, "service_unavailable", detail=DETAIL_SERVICE_UNAVAILABLE)
        assert resp.headers.get("Retry-After") == "10", resp.headers

        # The Problem carries no attachment id; the row is found in the DB.
        rows = query_db("SELECT id FROM attachments WHERE chat_id = ?", (chat_id,))
        assert len(rows) == 1, rows
        att_id = uuid_from_db(rows[0]["id"])
        detail = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10)
        assert detail.status_code == 200, detail.text
        body = detail.json()
        assert (body["status"], body["error_code"]) == ("failed", "upload_failed"), body


# ---------------------------------------------------------------------------
# 10-17, 10-19: chunked uploads (no Content-Length) — streaming size counter
# ---------------------------------------------------------------------------

class TestChunkedUpload:
    """Uploads without Content-Length are measured while streaming."""

    def test_chunked_oversize_image_rejected(self, request, chat, mock_provider):
        """10-17: a chunked image over uploaded_image_max_size_kb (5 MB) passes
        the Content-Length pre-check and is stopped by the streaming counter:
        400 out_of_range FILE_TOO_LARGE; nothing reaches the provider, and
        the attachment row is `failed` with `file_too_large`."""
        _require_offline(request)
        payload = b"\x89PNG" + b"\x00" * (MAX_IMAGE_BYTES - 4 + 1)
        resp = _upload_chunked(chat["id"], "big.png", payload, "image/png")
        assert_problem(resp, 400, "out_of_range", field_reason="FILE_TOO_LARGE")
        assert _file_upload_calls(mock_provider) == []
        # The row was inserted before the body was read; the counter fails it.
        rows = query_db(
            "SELECT status, error_code FROM attachments WHERE chat_id = ?", (chat["id"],),
        )
        assert rows == [{"status": "failed", "error_code": "file_too_large"}], rows

    def test_chunked_upload_within_limit_ready(self, request, chat):
        """10-19: a chunked document within the limit → 201, ready, exact size_bytes."""
        _require_offline(request)
        payload = b"chunked upload line\n" * 10_000  # 200 KB
        resp = _upload_chunked(chat["id"], "chunked.txt", payload, "text/plain")
        assert resp.status_code == 201, f"{resp.status_code} {resp.text}"
        body = resp.json()
        assert body["status"] == "ready", body
        assert body["size_bytes"] == len(payload)


# ---------------------------------------------------------------------------
# 10-26, 10-27: per-chat document count and storage limits
# ---------------------------------------------------------------------------

class TestPerChatLimits:
    """Per-chat limits, reached by seeding attachment rows in the DB."""

    def test_document_limit_exceeded(self, request, chat, mock_provider):
        """10-26: a chat with max_documents_per_chat (50) documents rejects the
        next document: 429 resource_exhausted, violation document_limit."""
        _require_offline(request)
        chat_id = chat["id"]
        src = _upload_ready(chat_id, "seed.txt", b"seed document", "text/plain")
        for _ in range(MAX_DOCUMENTS_PER_CHAT - 1):
            _clone_attachment(src)

        mock_provider.clear_captured_requests()
        resp = _upload(chat_id, "one-more.txt", b"over the limit", "text/plain")
        assert_problem(resp, 429, "resource_exhausted", violation_subject="document_limit")
        assert _file_upload_calls(mock_provider) == []

    def test_storage_limit_exceeded(self, request, chat, mock_provider):
        """10-27: the chat's documents total 1 KiB less than
        max_total_upload_mb_per_chat (100 MB), each within the 25 MB per-file
        limit. A 100-byte upload still fits; a 2 KiB upload would cross the
        limit: 429 resource_exhausted, violation storage_limit, nothing sent
        to the provider."""
        _require_offline(request)
        chat_id = chat["id"]
        seed = b"seed document"
        src = _upload_ready(chat_id, "seed.txt", seed, "text/plain")
        headroom = 1024
        sizes = [MAX_DOCUMENT_BYTES] * 3 + [MAX_DOCUMENT_BYTES - len(seed) - headroom]
        for size in sizes:
            _clone_attachment(src, size_bytes=size)
        assert len(seed) + sum(sizes) == MAX_TOTAL_UPLOAD_BYTES_PER_CHAT - headroom

        _upload_ready(chat_id, "fits.txt", b"f" * 100, "text/plain")

        mock_provider.clear_captured_requests()
        resp = _upload(chat_id, "one-more.txt", b"o" * 2048, "text/plain")
        assert_problem(resp, 429, "resource_exhausted", violation_subject="storage_limit")
        assert _file_upload_calls(mock_provider) == []


# ---------------------------------------------------------------------------
# Upload concurrency limit (RagConfig `max_concurrent_uploads`, default 10)
# ---------------------------------------------------------------------------

MAX_CONCURRENT_UPLOADS = 10  # RagConfig default (config.rs), not overridden in base.yaml
UPLOAD_RETRY_AFTER_SECS = 5  # handlers/attachments.rs, upload_attachment


class TestUploadConcurrencyLimit:
    """The upload handler takes a permit before it reads the file body."""

    @pytest.mark.timeout(60)
    def test_upload_over_concurrency_limit_503(self, request, chat):
        """10 uploads hold every permit (each has sent its multipart headers
        and waits before the rest of the body; its attachment row exists).
        The next upload is 503 service_unavailable with Retry-After 5 and
        stores nothing. Each held upload then completes `ready`."""
        _require_offline(request)
        chat_id = chat["id"]
        releases = [threading.Event() for _ in range(MAX_CONCURRENT_UPLOADS)]
        results: dict[int, httpx.Response] = {}

        def held_upload(i: int) -> None:
            content_type, body = _chunked_multipart(f"held{i}.txt", "text/plain", b"held upload")
            head = next(body)

            def gated():
                yield head
                releases[i].wait(30)
                yield from body

            results[i] = httpx.post(
                f"{API_PREFIX}/chats/{chat_id}/attachments",
                content=gated(), headers={"Content-Type": content_type}, timeout=45,
            )

        def rows() -> int:
            return query_db(
                "SELECT COUNT(*) AS n FROM attachments WHERE chat_id = ?", (chat_id,),
            )[0]["n"]

        threads = [threading.Thread(target=held_upload, args=(i,)) for i in range(len(releases))]
        try:
            for t in threads:
                t.start()
            deadline = time.monotonic() + 20
            while rows() < MAX_CONCURRENT_UPLOADS and time.monotonic() < deadline:
                time.sleep(0.1)
            assert rows() == MAX_CONCURRENT_UPLOADS

            resp = _upload(chat_id, "one-more.txt", b"no permit left", "text/plain")
            assert_problem(resp, 503, "service_unavailable")
            assert resp.headers["Retry-After"] == str(UPLOAD_RETRY_AFTER_SECS)
            assert rows() == MAX_CONCURRENT_UPLOADS
        finally:
            # One at a time: the gateway throttles a burst of provider uploads.
            for i, t in enumerate(threads):
                releases[i].set()
                t.join(timeout=30)

        assert sorted(results) == list(range(MAX_CONCURRENT_UPLOADS))
        for i, r in sorted(results.items()):
            assert r.status_code == 201, (i, r.status_code, r.text)
            assert r.json()["status"] == "ready", r.json()


# ---------------------------------------------------------------------------
# 05-07, 10-34, 01-06: citations and images in the provider exchange
# ---------------------------------------------------------------------------


def _file_search_scenario(citations: list[dict]) -> Scenario:
    """The mock `FILESEARCH:*` scenario with the given file citations."""
    base = SCENARIOS["FILESEARCH:*"]
    return Scenario(events=list(base.events), usage=base.usage, citations=citations)


class TestFileCitationMapping:
    """File citations reach the client with the attachment id, not the provider file id."""

    def test_file_citation_maps_to_attachment_id(self, request, chat, mock_provider):
        """05-07: a `file_citation` for the provider file of an attachment is sent
        as `{source: file, attachment_id: <attachment UUID>, title: <filename>}`;
        the OpenAI `file_citation` has no text or range, so `snippet` is empty
        and there is no `span`. A citation of an unknown provider file is
        dropped; no provider file id appears in the stream."""
        _require_offline(request)
        chat_id = chat["id"]
        att_id = _upload_ready(chat_id, "zembla.txt", b"Zembla notes.", "text/plain")
        file_id = provider_file_id(att_id)
        mock_provider.set_next_scenario(_file_search_scenario([
            {"type": "file_citation", "file_id": file_id,
             "filename": "provider-name.txt", "index": 13},
            {"type": "file_citation", "file_id": "file-unknown-0123456789",
             "filename": "other.txt", "index": 5},
        ]))

        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "FILESEARCH: what is in the notes?"},
            headers={"Accept": "text/event-stream"}, timeout=30,
        )
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        expect_done(events)
        citations = [e.data for e in events if e.event == "citations"]
        assert citations == [{"items": [{
            "source": "file",
            "title": "zembla.txt",
            "attachment_id": att_id,
            "snippet": "",
        }]}]
        assert file_id not in resp.text

    @pytest.mark.timeout(40)
    def test_citation_of_deleted_attachment_dropped(self, request, chat, mock_provider):
        """10-29: file_search may still return a deleted document until its
        provider file is removed (ADR-0007), but a citation of it is omitted;
        the citation of a kept document is sent."""
        _require_offline(request)
        chat_id = chat["id"]
        deleted = _upload_ready(chat_id, "old.txt", b"Old notes.", "text/plain")
        kept = _upload_ready(chat_id, "new.txt", b"New notes.", "text/plain")
        deleted_file, kept_file = provider_file_id(deleted), provider_file_id(kept)
        resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}/attachments/{deleted}", timeout=10)
        assert resp.status_code == 204

        mock_provider.set_next_scenario(_file_search_scenario([
            {"type": "file_citation", "file_id": deleted_file, "filename": "old.txt", "index": 5},
            {"type": "file_citation", "file_id": kept_file, "filename": "new.txt", "index": 13},
        ]))
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "FILESEARCH: what is in the notes?"},
            headers={"Accept": "text/event-stream"}, timeout=30,
        )
        assert resp.status_code == 200, resp.text
        events = parse_sse(resp.text)
        expect_done(events)
        citations = [e.data for e in events if e.event == "citations"]
        assert [[c["attachment_id"] for c in d["items"]] for d in citations] == [[kept]]
        assert deleted_file not in resp.text
        # Let the provider cleanup of the deleted attachment finish inside this test.
        assert wait_cleanup_terminal([deleted]) == {deleted: "done"}


class TestImageInProviderRequest:
    """Image attachments in the provider request, and on a model without vision."""

    def test_image_sent_as_input_image(self, request, chat, mock_provider):
        """10-34 (offline part): the user message of the provider request carries
        the text followed by `input_image` with the image's provider file id."""
        _require_offline(request)
        chat_id = chat["id"]  # vision-capable default model
        att_id = _upload_ready(chat_id, "red.png", make_minimal_png(color=(255, 0, 0)), "image/png")
        file_id = provider_file_id(att_id)

        mock_provider.clear_captured_requests()
        status, events, raw = stream_message(
            chat_id, "What color is the image?", attachment_ids=[att_id],
        )
        assert status == 200, raw
        expect_done(events)

        captured = mock_provider.get_captured_requests()
        assert len(captured) == 1, captured
        user_items = [i for i in captured[0]["input"] if i.get("role") == "user"]
        assert user_items[-1]["content"] == [
            {"type": "input_text", "text": "What color is the image?"},
            {"type": "input_image", "file_id": file_id},
        ]

    def test_image_on_model_without_vision_400(self, request, chat_with_model, mock_provider):
        """01-06: an image attachment in a chat whose model has no VISION_INPUT
        (gpt-5-bare) is 400 invalid_argument VISION_NOT_SUPPORTED; no turn is
        created and the provider is not called."""
        _require_offline(request)
        chat_id = chat_with_model(BARE_MODEL)["id"]
        att_id = _upload_ready(chat_id, "red.png", make_minimal_png(), "image/png")

        mock_provider.clear_captured_requests()
        resp = httpx.post(
            f"{API_PREFIX}/chats/{chat_id}/messages:stream",
            json={"content": "Describe the image.", "attachment_ids": [att_id]},
            headers={"Accept": "text/event-stream"}, timeout=30,
        )
        assert_problem(resp, 400, "invalid_argument", field_reason="VISION_NOT_SUPPORTED")
        assert mock_provider.get_captured_requests() == []
        assert query_db("SELECT id FROM chat_turns WHERE chat_id = ?", (chat_id,)) == []


# ---------------------------------------------------------------------------
# DELETE of an unknown attachment
# ---------------------------------------------------------------------------

class TestUploadUnknownChat:
    """POST /attachments on a chat that does not exist."""

    def test_upload_to_unknown_chat_404(self, server):
        resp = _upload(str(uuid.uuid4()), "a.txt", b"no chat", "text/plain")
        assert_problem(resp, 404, "not_found", resource_type=RESOURCE_CHAT)


@pytest.mark.usefixtures("offline_only")
class TestUploadChatModelLeftCatalog:
    """The chat's model is no longer in the catalog: the upload has no provider
    to store the file with and is rejected like a send
    (test_streaming.py TestChatModelLeftCatalog)."""

    def test_upload_to_chat_with_model_missing_from_catalog_400(self, chat, mock_provider):
        chat_id = chat["id"]
        assert exec_db(
            "UPDATE chats SET model = ? WHERE id = ?", ("gpt-removed-from-catalog", chat_id),
        ) == 1
        mock_provider.clear_captured_requests()
        resp = _upload(chat_id, "a.txt", b"no model", "text/plain")
        body = assert_problem(
            resp, 400, "invalid_argument",
            field_reason="INVALID_MODEL", resource_type=RESOURCE_CHAT,
        )
        assert body["context"]["field_violations"][0]["field"] == "model", body
        assert query_db("SELECT id FROM attachments WHERE chat_id = ?", (chat_id,)) == []
        assert mock_provider.get_post_paths() == []


class TestDeleteMissingAttachment:
    """DELETE /attachments/{id} for an attachment that does not exist."""

    def test_delete_unknown_attachment_404(self, chat):
        att_id = str(uuid.uuid4())
        resp = httpx.delete(f"{API_PREFIX}/chats/{chat['id']}/attachments/{att_id}")
        body = assert_problem(resp, 404, "not_found", resource_type=RESOURCE_ATTACHMENT)
        assert body["context"].get("resource_name") == att_id, body

    def test_second_delete_attachment_is_idempotent(self, chat):
        """A repeated DELETE of an attachment returns 204 (idempotent)."""
        att_id = _upload_ready(chat["id"], "twice.txt", b"delete me twice", "text/plain")
        url = f"{API_PREFIX}/chats/{chat['id']}/attachments/{att_id}"
        assert httpx.delete(url).status_code == 204
        assert httpx.delete(url).status_code == 204
        assert httpx.get(url).status_code == 404


def _raw_upload(chat_id: str, body: bytes, content_type: str | None) -> httpx.Response:
    """POST /attachments with a hand-built body (httpx `files=` always adds a
    part Content-Type)."""
    headers = {} if content_type is None else {"Content-Type": content_type}
    return httpx.post(
        f"{API_PREFIX}/chats/{chat_id}/attachments",
        content=body, headers=headers, timeout=30,
    )


def _multipart(*parts: tuple[str, str | None, str | None, bytes]) -> bytes:
    """A multipart/form-data body with boundary `e2e-boundary` from
    (field name, filename, part Content-Type, data) tuples."""
    out = b""
    for name, filename, ctype, data in parts:
        disposition = f'form-data; name="{name}"'
        if filename is not None:
            disposition += f'; filename="{filename}"'
        out += b"--e2e-boundary\r\n"
        out += f"Content-Disposition: {disposition}\r\n".encode()
        if ctype is not None:
            out += f"Content-Type: {ctype}\r\n".encode()
        out += b"\r\n" + data + b"\r\n"
    return out + b"--e2e-boundary--\r\n"


MULTIPART_CT = "multipart/form-data; boundary=e2e-boundary"


@pytest.mark.usefixtures("offline_only")
class TestUploadMultipartErrors:
    """Malformed upload requests (handlers/attachments.rs, `upload_attachment`):
    400 invalid_argument of the attachment resource with the field violation
    reason, nothing stored, nothing sent to the provider. Offline only: checks
    the mock's traffic."""

    @pytest.mark.parametrize(("body", "content_type", "reason"), [
        pytest.param(
            _multipart(("file", "a.txt", "text/plain", b"x")), "multipart/form-data",
            "BOUNDARY_REQUIRED", id="no_boundary",
        ),
        pytest.param(b"not a multipart body", MULTIPART_CT, "MULTIPART_ERROR", id="no_parts"),
        pytest.param(
            _multipart(("other", None, None, b"value")), MULTIPART_CT, "MISSING_FILE",
            id="no_file_field",
        ),
        pytest.param(
            _multipart(("file", "a.txt", None, b"no part type")), MULTIPART_CT,
            "MISSING_CONTENT_TYPE", id="file_without_content_type",
        ),
    ])
    def test_malformed_upload_400(self, chat, mock_provider, body, content_type, reason):
        chat_id = chat["id"]
        mock_provider.clear_captured_requests()
        resp = _raw_upload(chat_id, body, content_type)
        assert_problem(
            resp, 400, "invalid_argument", field_reason=reason,
            resource_type=RESOURCE_ATTACHMENT,
        )
        assert query_db("SELECT id FROM attachments WHERE chat_id = ?", (chat_id,)) == []
        assert mock_provider.get_post_paths() == []


@pytest.mark.usefixtures("offline_only")
class TestFileSearchToolEvents:
    """Provider-native file_search calls are sent as tool events and counted."""

    def test_file_search_tool_events_and_counter(self, chat):
        """With a ready document the mock `FILESEARCH:*` answer runs one
        file_search: `file_search` tool events `start` (empty `details`) then
        `done` (`details.files_searched`), the turn
        counts one completed file search and its usage event reports
        `file_search_calls` 1."""
        chat_id = chat["id"]
        _upload_ready(chat_id, "notes.txt", b"Searchable notes.", "text/plain")
        rid = str(uuid.uuid4())
        status, events, raw = stream_message(
            chat_id, "FILESEARCH: what do the notes say?", request_id=rid,
        )
        assert status == 200, raw
        expect_done(events)

        tools = [
            (e.data["name"], e.data["phase"], e.data["details"]) for e in events if e.event == "tool"
        ]
        # `files_searched` counts the results of
        # `response.file_search_call.completed`; OpenAI sends none there
        # (the mock does the same), so it is 0.
        assert tools == [
            ("file_search", "start", {}), ("file_search", "done", {"files_searched": 0}),
        ], tools
        rows = query_db(
            "SELECT file_search_completed_count FROM chat_turns WHERE request_id = ?", (rid,),
        )
        assert rows == [{"file_search_completed_count": 1}], rows
        usage = usage_events(rid)
        assert [u["file_search_calls"] for u in usage] == [1], usage


def _vector_store_id(chat_id: str) -> str:
    rows = query_db("SELECT vector_store_id FROM chat_vector_stores WHERE chat_id = ?", (chat_id,))
    assert len(rows) == 1 and rows[0]["vector_store_id"], rows
    return rows[0]["vector_store_id"]


@pytest.mark.usefixtures("offline_only")
class TestUploadVectorStoreProviderMismatch:
    """The chat's vector store belongs to another storage backend (DB seed)."""

    @pytest.mark.timeout(30)
    def test_upload_after_switch_to_other_provider_409(self, chat_with_model, mock_provider):
        """A document uploaded in an Azure chat creates the chat's vector
        store with the `azure` backend. With `chats.model` switched to the
        OpenAI model gpt-5.2 in the DB, the next document is stored at
        OpenAI, then the vector store step finds the azure store: 409
        already_exists (chat resource, `resource_name` provider_mismatch)
        with the fixed detail. The attachment is `failed` with
        `vector_store_failed`, the store row is kept, no vector store is
        written, and the file just stored at OpenAI is deleted."""
        chat_id = chat_with_model(DEFAULT_MODEL)["id"]
        _upload_ready(chat_id, "first.txt", b"stored with azure", "text/plain")
        store = query_db(
            "SELECT provider, vector_store_id FROM chat_vector_stores WHERE chat_id = ?", (chat_id,),
        )
        assert [r["provider"] for r in store] == ["azure"], store
        assert exec_db("UPDATE chats SET model = ? WHERE id = ?", (STANDARD_MODEL, chat_id)) == 1

        mock_provider.clear_captured_requests()
        resp = _upload(chat_id, "second.txt", b"stored with openai", "text/plain")
        body = assert_problem(resp, 409, "already_exists", resource_type=RESOURCE_CHAT)
        assert body["detail"] == "chat vector store belongs to another provider", body
        assert body["context"]["resource_name"] == "provider_mismatch", body

        rows = query_db(
            "SELECT status, error_code, provider_file_id FROM attachments "
            "WHERE chat_id = ? AND filename = ?",
            (chat_id, "second.txt"),
        )
        assert [(r["status"], r["error_code"]) for r in rows] == [
            ("failed", "vector_store_failed"),
        ], rows
        assert query_db(
            "SELECT provider, vector_store_id FROM chat_vector_stores WHERE chat_id = ?", (chat_id,),
        ) == store
        assert mock_provider.get_post_paths() == ["/v1/files"]
        file_id = rows[0]["provider_file_id"]
        wait_for(
            lambda: ("DELETE", f"/v1/files/{file_id}") in mock_provider.get_request_paths(),
            "the delete of the file stored at OpenAI",
        )


def _failed_upload_row(chat_id: str) -> dict:
    """The only attachment row of the chat, after a failed upload (the
    Problem carries no attachment id)."""
    rows = query_db(
        "SELECT id, status, error_code, provider_file_id FROM attachments WHERE chat_id = ?",
        (chat_id,),
    )
    assert len(rows) == 1, rows
    return rows[0]


@pytest.mark.usefixtures("offline_only")
class TestUploadVectorStoreIndexing:
    """A document is `ready` only once the vector store has indexed it.

    The chat uses gpt-5.2 (OpenAI paths). Adding a file answers
    `in_progress` by default; the upload then reads the file's status."""

    @pytest.mark.timeout(30)
    def test_upload_ready_after_indexing_completes(self, chat_with_model, mock_provider):
        """Two `in_progress` answers, then `completed`: 201 `ready` after two
        status reads of the vector store file. The upload form's purpose is
        `assistants`."""
        chat_id = chat_with_model(STANDARD_MODEL)["id"]
        mock_provider.set_indexing(["in_progress", "in_progress", "completed"])

        att_id = _upload_ready(chat_id, "indexed.txt", b"indexed document", "text/plain")

        vs_id = _vector_store_id(chat_id)
        file_id = provider_file_id(att_id)
        reads = [
            (m, p) for m, p in mock_provider.get_request_paths()
            if m == "GET" and p == f"/v1/vector_stores/{vs_id}/files/{file_id}"
        ]
        assert len(reads) == 2, mock_provider.get_request_paths()
        assert [f["purpose"] for f in mock_provider.get_uploaded_files()] == ["assistants"]

    @pytest.mark.timeout(30)
    @pytest.mark.parametrize("statuses", [
        pytest.param(["in_progress", "failed"], id="failed_after_poll"),
        pytest.param(["failed"], id="failed_on_add"),
        pytest.param(["in_progress", "cancelled"], id="cancelled"),
    ])
    def test_indexing_failure_marks_attachment_failed(
        self, chat_with_model, mock_provider, statuses,
    ):
        """The vector store reports `failed` (or `cancelled`) with a
        `last_error`: 503 service_unavailable with Retry-After, the
        attachment is `failed` with `indexing_failed`, and the file just
        stored at the provider is deleted."""
        chat_id = chat_with_model(STANDARD_MODEL)["id"]
        mock_provider.set_indexing(
            statuses, last_error={"code": "unsupported_file", "message": "file type not supported"},
        )

        resp = _upload(chat_id, "broken.txt", b"cannot be indexed", "text/plain")
        assert_problem(resp, 503, "service_unavailable", detail=DETAIL_SERVICE_UNAVAILABLE)
        assert resp.headers.get("Retry-After") == "10", resp.headers
        # The provider error code stays internal.
        assert "indexing_failed" not in resp.text, resp.text

        row = _failed_upload_row(chat_id)
        detail = httpx.get(
            f"{API_PREFIX}/chats/{chat_id}/attachments/{uuid_from_db(row['id'])}", timeout=10,
        ).json()
        assert (detail["status"], detail["error_code"]) == ("failed", "indexing_failed"), detail
        file_id = row["provider_file_id"]
        wait_for(
            lambda: ("DELETE", f"/v1/files/{file_id}") in mock_provider.get_request_paths(),
            "the delete of the stored file",
        )

    @pytest.mark.timeout(30)
    def test_vector_store_create_failure_503(self, chat_with_model, mock_provider):
        """The provider fails to create the chat's vector store: 503
        service_unavailable, the attachment is `failed` with
        `vector_store_failed`, no store is recorded, and the stored file is
        deleted."""
        chat_id = chat_with_model(STANDARD_MODEL)["id"]
        mock_provider.set_fault("POST", "/v1/vector_stores", 500)

        resp = _upload(chat_id, "no-store.txt", b"no vector store", "text/plain")
        assert_problem(resp, 503, "service_unavailable", detail=DETAIL_SERVICE_UNAVAILABLE)
        assert resp.headers.get("Retry-After") == "10", resp.headers
        assert "vector_store_failed" not in resp.text, resp.text

        row = _failed_upload_row(chat_id)
        assert (row["status"], row["error_code"]) == ("failed", "vector_store_failed"), row
        assert query_db(
            "SELECT vector_store_id FROM chat_vector_stores WHERE chat_id = ?", (chat_id,),
        ) == []
        file_id = row["provider_file_id"]
        wait_for(
            lambda: ("DELETE", f"/v1/files/{file_id}") in mock_provider.get_request_paths(),
            "the delete of the stored file",
        )


# Upload reaper (config/base.yaml `upload_reaper`): a `pending` / `uploaded`
# row not updated for this long is failed with `upload_abandoned`; the scan
# runs every 2 s.
UPLOAD_REAPER_STALE_AFTER_SECS = 60
# Waits of the reaper test, summed into its pytest timeout.
_REAPER_CLIENT_READ_TIMEOUT = 3
_REAPER_POLL_CHECK_SECS = 6
_REAPER_REAP_TIMEOUT = UPLOAD_REAPER_STALE_AFTER_SECS + 30
_REAPER_DELETE_TIMEOUT = 10
_REAPER_CLEANUP_TIMEOUT = 20
_REAPER_TIMEOUT = (
    _REAPER_CLIENT_READ_TIMEOUT + _REAPER_POLL_CHECK_SECS + _REAPER_REAP_TIMEOUT
    + _REAPER_DELETE_TIMEOUT + _REAPER_CLEANUP_TIMEOUT + 40  # seeding, uploads
)


@pytest.mark.usefixtures("offline_only")
class TestUploadReaper:
    """An upload whose request is dropped before it records the outcome
    leaves the attachment `uploaded`; the upload reaper fails it and the
    attachment cleanup deletes the provider file. Slow: the reaper waits
    `stale_after_secs` (60 s, the minimum) after the row's last update."""

    @pytest.mark.timeout(_REAPER_TIMEOUT)
    def test_abandoned_upload_failed_and_provider_file_deleted(
        self, chat_with_model, mock_provider,
    ):
        """The chat holds max_documents_per_chat - 1 documents. The client
        gives up on the next upload while the vector store is still indexing
        it (indexing held): the server stops polling and the row stays
        `uploaded` and counts against the limit (one more upload is 429).
        About 60 s later GET reports `failed` with `upload_abandoned`, the
        provider file is deleted (and so gone from the chat's vector store),
        the cleanup ends in `done`, and the failed row no longer counts: a
        new document upload is `ready`."""
        chat_id = chat_with_model(STANDARD_MODEL)["id"]
        seed_id = _upload_ready(chat_id, "seed.txt", b"seed document", "text/plain")
        for _ in range(MAX_DOCUMENTS_PER_CHAT - 2):
            _clone_attachment(seed_id)

        mock_provider.hold_indexing()
        started = time.monotonic()
        with pytest.raises(httpx.ReadTimeout):
            httpx.post(
                f"{API_PREFIX}/chats/{chat_id}/attachments",
                files={"file": ("abandoned.txt", io.BytesIO(b"never indexed"), "text/plain")},
                timeout=httpx.Timeout(10, read=_REAPER_CLIENT_READ_TIMEOUT),
            )

        rows = query_db(
            "SELECT id, status, provider_file_id FROM attachments "
            "WHERE chat_id = ? AND filename = 'abandoned.txt'", (chat_id,),
        )
        assert len(rows) == 1 and rows[0]["status"] == "uploaded", rows
        att_id = uuid_from_db(rows[0]["id"])
        file_id = rows[0]["provider_file_id"]
        vs_id = _vector_store_id(chat_id)
        status_read = ("GET", f"/v1/vector_stores/{vs_id}/files/{file_id}")
        assert file_id in (mock_provider.vector_store_file_ids(vs_id) or [])

        # The `uploaded` row counts against the document limit.
        resp = _upload(chat_id, "over-limit.txt", b"over the limit", "text/plain")
        assert_problem(resp, 429, "resource_exhausted", violation_subject="document_limit")

        # The dropped request stops polling the indexing status (the poll
        # interval is at most 2 s).
        time.sleep(1)
        reads = mock_provider.get_request_paths().count(status_read)
        assert reads > 0, "the upload never polled the indexing status"
        time.sleep(_REAPER_POLL_CHECK_SECS - 1)
        assert mock_provider.get_request_paths().count(status_read) == reads, (
            "the upload kept polling after the client disconnected"
        )

        def detail() -> dict | None:
            resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10)
            assert resp.status_code == 200, resp.text
            body = resp.json()
            return body if body["status"] != "uploaded" else None

        body = wait_for(
            detail, "the reaper to fail the abandoned upload",
            timeout=_REAPER_REAP_TIMEOUT, interval=1,
        )
        assert (body["status"], body["error_code"]) == ("failed", "upload_abandoned"), body
        # Not before the row was stale.
        assert time.monotonic() - started >= UPLOAD_REAPER_STALE_AFTER_SECS

        wait_for(
            lambda: ("DELETE", f"/v1/files/{file_id}") in mock_provider.get_request_paths(),
            "the delete of the provider file", timeout=_REAPER_DELETE_TIMEOUT,
        )
        assert wait_cleanup_terminal([att_id], timeout=_REAPER_CLEANUP_TIMEOUT) == {
            att_id: "done",
        }
        assert file_id not in (mock_provider.vector_store_file_ids(vs_id) or []), (
            mock_provider.vector_store_file_ids(vs_id)
        )

        # The failed row no longer counts: the chat has room for a document.
        mock_provider.hold_indexing(False)
        _upload_ready(chat_id, "after-reap.txt", b"room again", "text/plain")


@pytest.mark.usefixtures("offline_only")
class TestUploadFilename:
    """The stored filename (handlers/attachments.rs, domain/mime_validation.rs)."""

    def test_part_without_filename_is_named_upload(self, chat):
        """A `file` part without `filename=` is stored as "upload"."""
        chat_id = chat["id"]
        resp = _raw_upload(chat_id, _multipart(("file", None, "text/plain", b"unnamed")), MULTIPART_CT)
        assert resp.status_code == 201, resp.text
        att = resp.json()
        assert (att["filename"], att["content_type"], att["status"]) == (
            "upload", "text/plain", "ready",
        ), att
        get = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att['id']}", timeout=10)
        assert get.json()["filename"] == "upload", get.json()

    def test_long_filename_truncated_keeping_extension(self, chat):
        """A filename over 255 characters is cut to 255, keeping the
        extension: 300 + ".txt" → 251 characters of the stem + ".txt"."""
        chat_id = chat["id"]
        att_id = _upload_ready(chat_id, "a" * 300 + ".txt", b"long name", "text/plain")
        get = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att_id}", timeout=10)
        assert get.json()["filename"] == "a" * 251 + ".txt", get.json()

    def test_octet_stream_with_unknown_extension_400(self, chat, mock_provider):
        """`application/octet-stream` is resolved from the extension; for an
        unknown one (`.qqq`) it stays octet-stream, which is not a supported
        type: 400 invalid_argument UNSUPPORTED_CONTENT_TYPE on `content_type`,
        nothing stored, provider not called."""
        chat_id = chat["id"]
        mock_provider.clear_captured_requests()
        resp = _upload(chat_id, "data.qqq", b"\x00\x01binary", "application/octet-stream")
        body = assert_problem(
            resp, 400, "invalid_argument", field_reason="UNSUPPORTED_CONTENT_TYPE",
            resource_type=RESOURCE_ATTACHMENT,
        )
        assert [v["field"] for v in body["context"]["field_violations"]] == ["content_type"], body
        assert query_db("SELECT id FROM attachments WHERE chat_id = ?", (chat_id,)) == []
        assert mock_provider.get_post_paths() == []


class TestCsvUpload:
    """`rag.allow_csv_upload` (on by default, not overridden in base.yaml):
    `text/csv` is remapped to `text/plain` (domain/mime_validation.rs
    `remap_csv_to_plain`) and indexed as a document."""

    def test_csv_stored_as_text_plain_document(self, chat):
        chat_id = chat["id"]
        payload = b"name,value\nalpha,1\nbeta,2\n"
        resp = _upload(chat_id, "data.csv", payload, "text/csv")
        assert resp.status_code == 201, resp.text
        att = resp.json()
        assert (att["filename"], att["content_type"], att["kind"], att["status"], att["size_bytes"]) == (
            "data.csv", "text/plain", "document", "ready", len(payload),
        ), att
        get = httpx.get(f"{API_PREFIX}/chats/{chat_id}/attachments/{att['id']}", timeout=10)
        assert get.status_code == 200, get.text
        assert (get.json()["content_type"], get.json()["kind"]) == ("text/plain", "document"), get.json()


@pytest.mark.multi_provider
@pytest.mark.usefixtures("offline_only")
class TestAttachmentsInProviderRequest:
    """Offline counterparts of the online attachment tests: the provider
    request, not the model's answer."""

    @pytest.mark.timeout(30)
    def test_document_and_image_in_one_request(self, provider_chat, mock_provider):
        """10-24 (offline part): a message with a ready document and an image
        sends one request with the text and `input_image` (the image's
        provider file id) in the user message and the `file_search` tool on
        the chat's vector store."""
        chat_id = provider_chat["id"]
        doc_id = _upload_ready(chat_id, "report.txt", b"The code word is FLAMINGO.", "text/plain")
        img_id = _upload_ready(chat_id, "animal.png", make_minimal_png(), "image/png")

        mock_provider.clear_captured_requests()
        status, events, raw = stream_message(
            chat_id, "What is in the image and the report?", attachment_ids=[doc_id, img_id],
        )
        assert status == 200, raw
        expect_done(events)

        (req,) = mock_provider.get_captured_requests()
        user_items = [i for i in req["input"] if i.get("role") == "user"]
        assert user_items[-1]["content"] == [
            {"type": "input_text", "text": "What is in the image and the report?"},
            {"type": "input_image", "file_id": provider_file_id(img_id)},
        ]
        assert [(t["type"], t["vector_store_ids"]) for t in req["tools"]] == [
            ("file_search", [_vector_store_id(chat_id)]),
        ], req["tools"]
        assert req["metadata"]["feature"] == "file_search", req["metadata"]

    @pytest.mark.timeout(30)
    def test_medium_document_upload_and_stream(self, provider_chat, mock_provider):
        """10-35 (offline part): a ~500 KB document goes through the
        streaming upload: 201 `ready` with its exact size, its provider file
        is added to the chat's vector store, and a message referencing it
        sends `file_search` on that store and ends in `done`."""
        chat_id = provider_chat["id"]
        payload = b"The quick brown fox. " * 25_000
        resp = _upload(chat_id, "medium_doc.txt", payload, "text/plain")
        assert resp.status_code == 201, resp.text
        att = resp.json()
        assert (att["status"], att["size_bytes"]) == ("ready", len(payload)), att
        vs_id = _vector_store_id(chat_id)
        assert mock_provider.vector_store_file_ids(vs_id) == [provider_file_id(att["id"])]

        mock_provider.clear_captured_requests()
        status, events, raw = stream_message(
            chat_id, "Summarize the attached document briefly.", attachment_ids=[att["id"]],
        )
        assert status == 200, raw
        expect_done(events)
        (req,) = mock_provider.get_captured_requests()
        assert [(t["type"], t["vector_store_ids"]) for t in req["tools"]] == [
            ("file_search", [vs_id]),
        ], req["tools"]



class TestFileSearchModelSupport:
    """10-48: a chat on a model with `tool_support.file_search: false`
    (gpt-5-nano) never sends the file_search tool, even with a ready document."""

    @pytest.mark.usefixtures("offline_only")
    def test_no_file_search_tool_on_model_without_support(self, chat_with_model, mock_provider):
        chat_id = chat_with_model("gpt-5-nano")["id"]
        _upload_ready(chat_id, "notes.txt", b"Quarterly revenue grew.", "text/plain")

        mock_provider.clear_captured_requests()
        status, events, raw = stream_message(chat_id, "What grew?")
        assert status == 200, raw
        expect_done(events)
        streamed = mock_provider.get_captured_requests()
        assert len(streamed) == 1, streamed
        tools = [t.get("type") for t in streamed[0].get("tools", [])]
        assert "file_search" not in tools, streamed[0].get("tools")
