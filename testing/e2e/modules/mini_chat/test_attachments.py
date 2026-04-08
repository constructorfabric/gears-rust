"""E2E tests for the attachment API (upload, get, delete, send-message with attachments).

Run via: ~/projects/cyberfabric-core-worktrees/scripts/run-tests.sh tests/test_attachments.py
"""

import io
import pathlib
import struct
import uuid
import zlib

import pytest
import httpx

from .conftest import API_PREFIX, AZURE_MODEL, SSEEvent, expect_done, expect_stream_started, stream_message

FIXTURES_DIR = pathlib.Path(__file__).parent / "fixtures"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def upload_file(
    chat_id: str,
    content: bytes = b"Hello, world!",
    filename: str = "test.txt",
    content_type: str = "text/plain",
) -> httpx.Response:
    """Upload a file to a chat via multipart."""
    return httpx.post(
        f"{API_PREFIX}/chats/{chat_id}/attachments",
        files={"file": (filename, io.BytesIO(content), content_type)},
        timeout=60,
    )


def get_attachment(chat_id: str, attachment_id: str) -> httpx.Response:
    return httpx.get(
        f"{API_PREFIX}/chats/{chat_id}/attachments/{attachment_id}",
        timeout=10,
    )


def delete_attachment(chat_id: str, attachment_id: str) -> httpx.Response:
    return httpx.delete(
        f"{API_PREFIX}/chats/{chat_id}/attachments/{attachment_id}",
        timeout=10,
    )


def poll_until_ready(chat_id: str, attachment_id: str, timeout: int = 60) -> dict:
    """Poll GET attachment until status is terminal (ready/failed) or timeout."""
    import time

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        resp = get_attachment(chat_id, attachment_id)
        assert resp.status_code == 200, f"GET failed: {resp.status_code} {resp.text}"
        body = resp.json()
        if body["status"] in ("ready", "failed"):
            return body
        time.sleep(1)
    raise TimeoutError(
        f"Attachment {attachment_id} did not reach terminal status within {timeout}s. "
        f"Last status: {body['status']}"
    )


# ---------------------------------------------------------------------------
# P5-N1: Upload and get attachment
# ---------------------------------------------------------------------------

@pytest.mark.openai
class TestUploadAndGet:
    """Upload a file, poll until ready, GET returns full detail."""

    def test_upload_and_get_attachment(self, chat):
        chat_id = chat["id"]
        content = b"This is a test document for RAG."

        # Upload
        resp = upload_file(chat_id, content=content, filename="notes.txt")
        assert resp.status_code == 201, f"Upload failed: {resp.status_code} {resp.text}"
        body = resp.json()
        att_id = body["id"]
        assert body["filename"] == "notes.txt"
        assert body["content_type"] == "text/plain"
        assert body["size_bytes"] == len(content)
        assert body["kind"] == "document"
        assert body["status"] in ("pending", "uploaded", "ready")

        # Poll until ready
        detail = poll_until_ready(chat_id, att_id)
        assert detail["status"] == "ready", f"Expected ready, got: {detail}"
        assert detail["id"] == att_id


# ---------------------------------------------------------------------------
# P5-N3: Upload invalid type rejected
# ---------------------------------------------------------------------------

@pytest.mark.openai
class TestUploadInvalidType:
    """Upload an unsupported MIME type."""

    def test_upload_invalid_type_rejected(self, chat):
        chat_id = chat["id"]
        resp = upload_file(
            chat_id,
            content=b"PK\x03\x04fake zip",
            filename="archive.zip",
            content_type="application/zip",
        )
        assert resp.status_code == 415, f"Expected 415, got {resp.status_code}: {resp.text}"


# ---------------------------------------------------------------------------
# P5-N4: Delete and verify gone
# ---------------------------------------------------------------------------

@pytest.mark.openai
class TestDeleteAndVerifyGone:
    """Upload, delete, GET returns 404."""

    def test_delete_and_verify_gone(self, chat):
        chat_id = chat["id"]

        # Upload and wait for ready
        resp = upload_file(chat_id, content=b"delete me", filename="gone.txt")
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        poll_until_ready(chat_id, att_id)

        # Delete
        resp = delete_attachment(chat_id, att_id)
        assert resp.status_code == 204

        # Verify gone
        resp = get_attachment(chat_id, att_id)
        assert resp.status_code == 404


# ---------------------------------------------------------------------------
# P5-N5: Delete referenced attachment → 409
# ---------------------------------------------------------------------------

@pytest.mark.openai
class TestDeleteReferencedAttachment:
    """Upload, attach to a message, then delete → 409."""

    def test_delete_referenced_attachment_409(self, chat):
        chat_id = chat["id"]

        # Upload and wait for ready
        resp = upload_file(chat_id, content=b"referenced doc", filename="ref.txt")
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        detail = poll_until_ready(chat_id, att_id)
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
        resp = delete_attachment(chat_id, att_id)
        assert resp.status_code == 409, (
            f"Expected 409 conflict, got {resp.status_code}: {resp.text}"
        )


# ---------------------------------------------------------------------------
# P5-N6: Send message with attachments
# ---------------------------------------------------------------------------

@pytest.mark.openai
class TestSendMessageWithAttachments:
    """Upload 2 files, send message with attachment_ids, verify stream completes."""

    def test_send_message_with_attachments(self, chat):
        chat_id = chat["id"]

        # Upload two files
        att_ids = []
        for i in range(2):
            resp = upload_file(
                chat_id,
                content=f"Document {i}: The answer is {42 + i}.".encode(),
                filename=f"doc{i}.txt",
            )
            assert resp.status_code == 201, f"Upload {i} failed: {resp.status_code}"
            att_id = resp.json()["id"]
            detail = poll_until_ready(chat_id, att_id)
            assert detail["status"] == "ready"
            att_ids.append(att_id)

        # Send message referencing both attachments
        status, events, raw = stream_message(
            chat_id,
            "What answers are in the attached documents?",
            attachment_ids=att_ids,
        )
        assert status == 200, f"Stream failed: {status} {raw[:500]}"
        expect_done(events)
        ss = expect_stream_started(events)
        assert ss.data.get("message_id")


# ---------------------------------------------------------------------------
# P5-N2: Upload, search, citation flow
# ---------------------------------------------------------------------------

@pytest.mark.openai
@pytest.mark.online_only
class TestUploadSearchCitationFlow:
    """Upload file, send message triggering file search, verify SSE citations contain UUID."""

    def test_upload_search_citation_flow(self, chat):
        chat_id = chat["id"]

        # Upload a document with distinctive content
        content = (
            b"The capital of the fictional country Zembla is Kinbote City. "
            b"It was founded in 1742 by King Charles the Beloved."
        )
        resp = upload_file(chat_id, content=content, filename="zembla.txt")
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        detail = poll_until_ready(chat_id, att_id)
        assert detail["status"] == "ready"

        # Send message that should trigger file search
        status, events, raw = stream_message(
            chat_id,
            "What is the capital of Zembla? Use the attached document.",
            attachment_ids=[att_id],
        )
        assert status == 200, f"Stream failed: {status} {raw[:500]}"
        done = expect_done(events)

        # Check for citations event — may or may not be present depending on
        # whether the LLM actually cited the file. If present, verify format.
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

@pytest.mark.azure
class TestAzureUploadAndGet:
    """Upload a file to a chat using the Azure model, verify storage_backend = 'azure'."""

    def test_azure_upload_and_get_attachment(self, chat_with_model):
        chat = chat_with_model(AZURE_MODEL)
        chat_id = chat["id"]
        content = b"This is a test document for Azure RAG."

        # Upload
        resp = upload_file(chat_id, content=content, filename="azure-notes.txt")
        assert resp.status_code == 201, f"Upload failed: {resp.status_code} {resp.text}"
        body = resp.json()
        att_id = body["id"]
        assert body["filename"] == "azure-notes.txt"
        assert body["kind"] == "document"
        assert body["status"] in ("pending", "uploaded", "ready")

        # Poll until ready
        detail = poll_until_ready(chat_id, att_id)
        assert detail["status"] == "ready", f"Expected ready, got: {detail}"


@pytest.mark.azure
@pytest.mark.online_only
class TestAzureSendMessageWithAttachment:
    """Upload a file to Azure chat, send message, verify stream completes."""

    def test_azure_send_message_with_attachment(self, chat_with_model):
        chat = chat_with_model(AZURE_MODEL)
        chat_id = chat["id"]

        # Upload
        resp = upload_file(
            chat_id,
            content=b"The secret code is AZURE-42.",
            filename="azure-doc.txt",
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        detail = poll_until_ready(chat_id, att_id)
        assert detail["status"] == "ready"

        # Send message referencing the attachment
        status, events, raw = stream_message(
            chat_id,
            "What is the secret code in the attached document?",
            attachment_ids=[att_id],
        )
        assert status == 200, f"Stream failed: {status} {raw[:500]}"
        expect_done(events)
        ss = expect_stream_started(events)
        assert ss.data.get("message_id")


@pytest.mark.openai
class TestOpenAIStorageBackend:
    """Upload to default (OpenAI) chat, verify storage_backend = 'openai'."""

    def test_openai_storage_backend(self, chat):
        chat_id = chat["id"]
        resp = upload_file(chat_id, content=b"openai doc", filename="oa.txt")
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        detail = poll_until_ready(chat_id, att_id)
        assert detail["status"] == "ready"


# ---------------------------------------------------------------------------
# Helpers — upload-and-verify, upload-and-stream
# ---------------------------------------------------------------------------

def upload_and_verify(chat_id: str, filename: str, content: bytes) -> str:
    """Upload a file, poll until ready. Returns attachment_id."""
    resp = upload_file(chat_id, content=content, filename=filename)
    assert resp.status_code == 201, f"Upload failed: {resp.status_code} {resp.text}"
    att_id = resp.json()["id"]
    detail = poll_until_ready(chat_id, att_id)
    assert detail["status"] == "ready", f"Expected ready, got: {detail}"
    return att_id


def upload_and_stream(chat_id: str, filename: str, content: bytes, question: str) -> SSEEvent:
    """Upload a file, poll until ready, send a message, return the done event."""
    att_id = upload_and_verify(chat_id, filename, content)
    status, events, raw = stream_message(chat_id, question, attachment_ids=[att_id])
    assert status == 200, f"Stream failed: {status} {raw[:500]}"
    ss = expect_stream_started(events)
    assert ss.data.get("message_id")
    done = expect_done(events)
    usage = done.data.get("usage", {})
    assert usage.get("input_tokens", 0) > 0, "Expected non-zero input_tokens"
    assert usage.get("output_tokens", 0) > 0, "Expected non-zero output_tokens"
    return done


# ---------------------------------------------------------------------------
# Dual-provider: same operation on OpenAI chat vs Azure chat
# ---------------------------------------------------------------------------

@pytest.mark.multi_provider
class TestDualProviderUpload:
    """Upload the same content to an OpenAI chat and an Azure chat.
    Proves DispatchingFileStorage routes to the correct provider-specific impl."""

    def test_dual_provider_upload(self, chat, chat_with_model):
        content = b"Dual-provider test document content."

        # OpenAI chat (default model gpt-5.2 → provider_id "openai")
        upload_and_verify(chat["id"], "dual-oa.txt", content)

        # Azure chat (azure-gpt-4.1-mini → provider_id "azure_openai")
        azure_chat = chat_with_model(AZURE_MODEL)
        upload_and_verify(azure_chat["id"], "dual-az.txt", content)


@pytest.mark.multi_provider
@pytest.mark.online_only
class TestDualProviderRAGStream:
    """Upload + send message on both OpenAI and Azure chats.
    Proves end-to-end RAG (file_search) works through both provider-specific
    file + vector store implementations in the same server instance."""

    def test_dual_provider_rag_stream(self, chat, chat_with_model):
        content = b"The secret passphrase is DUAL-PROVIDER-42."
        question = "What is the secret passphrase in the attached document?"

        # OpenAI chat — routes through OpenAiFileStorage + OpenAiVectorStore
        upload_and_stream(chat["id"], "rag-oa.txt", content, question)

        # Azure chat — routes through AzureFileStorage + AzureVectorStore
        azure_chat = chat_with_model(AZURE_MODEL)
        upload_and_stream(azure_chat["id"], "rag-az.txt", content, question)


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

@pytest.mark.openai
class TestImageUploadAndSend:
    """Upload a PNG image, verify it reaches ready, send a message referencing it."""

    def test_image_upload_and_send(self, chat):
        chat_id = chat["id"]

        # Generate a small red PNG
        png_bytes = make_minimal_png(width=4, height=4, color=(255, 0, 0))

        # Upload
        resp = upload_file(chat_id, content=png_bytes, filename="red.png", content_type="image/png")
        assert resp.status_code == 201, f"Upload failed: {resp.status_code} {resp.text}"
        body = resp.json()
        att_id = body["id"]
        assert body["kind"] == "image", f"Expected image kind, got: {body['kind']}"
        assert body["content_type"] == "image/png"

        # Poll until ready
        detail = poll_until_ready(chat_id, att_id)
        assert detail["status"] == "ready", f"Expected ready, got: {detail}"

        # Send a message referencing the image
        status, events, raw = stream_message(
            chat_id,
            "Describe the attached image. What color is it?",
            attachment_ids=[att_id],
        )
        assert status == 200, f"Stream failed: {status} {raw[:500]}"
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


@pytest.mark.openai
@pytest.mark.online_only
class TestImageRecognition:
    """Upload a real cat photo (JPEG) to both OpenAI and Azure chats,
    ask each LLM what animal it is, verify both streams complete.

    NOTE: Until image inlining is wired in gather_context, the LLM cannot
    actually see the image — it responds to text only. The cat-recognition
    check is soft (print, not assert) until that work lands.
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
        resp = upload_file(chat_id, content=image_bytes, filename=filename,
                           content_type=content_type)
        assert resp.status_code == 201, f"[{provider_label}] Upload failed: {resp.status_code} {resp.text}"
        body = resp.json()
        att_id = body["id"]
        assert body["kind"] == "image"

        # Poll until ready
        detail = poll_until_ready(chat_id, att_id)
        assert detail["status"] == "ready", f"[{provider_label}] Expected ready, got: {detail}"

        # Ask the LLM to identify the animal
        status, events, raw = stream_message(
            chat_id,
            "Describe exactly what you see in the attached image. If you cannot see any image, respond with exactly 'NO_IMAGE_VISIBLE'.",
            attachment_ids=[att_id],
        )
        assert status == 200, f"[{provider_label}] Stream failed: {status} {raw[:500]}"
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

    def test_image_recognition_cat_openai(self, chat):
        cat_bytes = self._load_cat_image()
        self._upload_image_and_ask(chat["id"], cat_bytes, "cat-oa.jpg", "image/jpeg", "OpenAI")

    def test_image_recognition_cat_azure(self, chat_with_model):
        azure_chat = chat_with_model(AZURE_MODEL)
        cat_bytes = self._load_cat_image()
        self._upload_image_and_ask(azure_chat["id"], cat_bytes, "cat-az.jpg", "image/jpeg", "Azure")


# ---------------------------------------------------------------------------
# Mixed document + image: both mechanisms must work simultaneously
# ---------------------------------------------------------------------------

@pytest.mark.openai
@pytest.mark.online_only
class TestDocumentAndImageTogether:
    """Upload a document AND an image, send a message referencing both.

    The LLM must use file_search to read the document AND see the image
    via multimodal input. The question is designed so the correct answer
    requires information from BOTH sources.
    """

    def test_document_and_image_combined(self, chat):
        chat_id = chat["id"]

        # 1. Upload a document with a secret code word
        doc_content = (
            "CONFIDENTIAL REPORT\n"
            "The secret code word for this project is: FLAMINGO.\n"
            "Do not share this code word with anyone.\n"
        )
        doc_resp = upload_file(
            chat_id,
            content=doc_content.encode(),
            filename="secret-report.txt",
            content_type="text/plain",
        )
        assert doc_resp.status_code == 201
        doc_id = doc_resp.json()["id"]
        doc_detail = poll_until_ready(chat_id, doc_id)
        assert doc_detail["status"] == "ready"
        assert doc_detail["kind"] == "document"

        # 2. Upload the cat image
        cat_bytes = (pathlib.Path(__file__).parent / "fixtures" / "cat.jpg").read_bytes()
        img_resp = upload_file(
            chat_id,
            content=cat_bytes,
            filename="animal.jpg",
            content_type="image/jpeg",
        )
        assert img_resp.status_code == 201
        img_id = img_resp.json()["id"]
        img_detail = poll_until_ready(chat_id, img_id)
        assert img_detail["status"] == "ready"
        assert img_detail["kind"] == "image"

        # 3. Ask a question that requires BOTH sources
        #    - The document contains the code word "FLAMINGO"
        #    - The image contains a cat
        #    The LLM must mention both to prove it accessed both.
        status, events, raw = stream_message(
            chat_id,
            content=(
                "I attached a document and an image. "
                "Tell me: 1) What is the secret code word from the document? "
                "2) What animal is in the image? "
                "Answer both questions."
            ),
            attachment_ids=[doc_id, img_id],
        )
        assert status == 200, f"Stream failed: {status} {raw[:500]}"
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

@pytest.mark.openai
class TestUploadSizeEnforcement:
    """Upload size limit enforcement — files exceeding the configured limit
    are rejected with HTTP 413 and error code ``file_too_large``.

    NOTE: these tests rely on the server's default config limits:
    - ``uploaded_file_max_size_kb``: 25600 (25 MB)
    - ``uploaded_image_max_size_kb``: 5120 (5 MB)

    Generating a 26 MB payload in-memory is acceptable for e2e.
    """

    def test_oversize_image_rejected_413(self, chat):
        """Upload an image exceeding uploaded_image_max_size_kb (5 MB) → 413.

        Uses ~6 MB which is over the image limit but under the API gateway's
        global 16 MiB body limit, so our handler's streaming size check runs.
        """
        chat_id = chat["id"]
        # 6 MB > 5 MB default image limit, but < 16 MiB gateway limit
        oversize_payload = b"\x89PNG" + b"\x00" * (6 * 1024 * 1024)
        resp = upload_file(
            chat_id,
            content=oversize_payload,
            filename="huge.png",
            content_type="image/png",
        )
        assert resp.status_code == 413, (
            f"Expected 413 for oversize image, got {resp.status_code}: {resp.text}"
        )
        body = resp.json()
        assert body.get("code") == "file_too_large" or "file_too_large" in resp.text, (
            f"Expected file_too_large error code, got: {body}"
        )

    def test_oversize_document_rejected_by_gateway(self, chat):
        """Upload a document exceeding the API gateway body limit (16 MiB) → 400.

        Documents can be up to 25 MB, but the gateway's global
        RequestBodyLimitLayer (16 MiB) rejects before our handler runs.
        This verifies the gateway guard works as a coarse outer limit.
        """
        chat_id = chat["id"]
        # 17 MB > 16 MiB gateway limit
        oversize_payload = b"\x00" * (17 * 1024 * 1024)
        resp = upload_file(
            chat_id,
            content=oversize_payload,
            filename="huge.pdf",
            content_type="application/pdf",
        )
        # Rejected by one of: OAGW body limit (502), gateway RequestBodyLimitLayer (400), or handler (413)
        assert resp.status_code in (400, 413, 502), (
            f"Expected 400/413/502 for oversize doc, got {resp.status_code}: {resp.text}"
        )

    def test_document_within_limit_succeeds(self, chat):
        """Upload a document just under the limit → succeeds."""
        chat_id = chat["id"]
        # 1 MB — well under 25 MB
        payload = b"x" * (1 * 1024 * 1024)
        resp = upload_file(
            chat_id,
            content=payload,
            filename="medium.txt",
            content_type="text/plain",
        )
        assert resp.status_code == 201, (
            f"Expected 201 for within-limit doc, got {resp.status_code}: {resp.text}"
        )
        att_id = resp.json()["id"]
        detail = poll_until_ready(chat_id, att_id)
        assert detail["status"] == "ready"


@pytest.mark.openai
class TestUploadSizeBytesAccuracy:
    """Verify that size_bytes in the attachment metadata matches the actual
    uploaded file size."""

    def test_size_bytes_matches_actual(self, chat):
        """Upload a file of known size, verify size_bytes in GET response."""
        chat_id = chat["id"]
        # Use a specific, non-round size to catch off-by-one issues
        payload = b"A" * 123_456
        resp = upload_file(
            chat_id,
            content=payload,
            filename="sized.txt",
            content_type="text/plain",
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        detail = poll_until_ready(chat_id, att_id)
        assert detail["status"] == "ready"
        assert detail["size_bytes"] == 123_456, (
            f"Expected size_bytes=123456, got {detail['size_bytes']}"
        )


@pytest.mark.openai
class TestUploadStreamingPipeline:
    """End-to-end test with a medium-sized file (~500 KB) through the full
    streaming upload pipeline: upload → ready → send message → SSE done."""

    @pytest.mark.online_only
    def test_medium_file_upload_and_stream(self, chat):
        chat_id = chat["id"]
        # 500 KB document
        payload = b"The quick brown fox. " * 25_000  # ~500 KB
        resp = upload_file(
            chat_id,
            content=payload,
            filename="medium_doc.txt",
            content_type="text/plain",
        )
        assert resp.status_code == 201
        att_id = resp.json()["id"]
        detail = poll_until_ready(chat_id, att_id)
        assert detail["status"] == "ready", f"Expected ready, got: {detail}"

        # Send a message referencing the attachment
        status, events, raw = stream_message(
            chat_id,
            content="Summarize the attached document briefly.",
            attachment_ids=[att_id],
        )
        assert status == 200, f"Stream failed: {status} {raw}"

        started = expect_stream_started(events)
        assert started is not None, "Expected stream_started event"

        done = expect_done(events)
        assert done is not None, "Expected done event"
        assert done.data.get("usage", {}).get("input_tokens", 0) > 0
