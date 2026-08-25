"""Tests for chat CRUD operations."""

import uuid

import pytest
import httpx

from .conftest import API_PREFIX, DEFAULT_MODEL, STANDARD_MODEL


@pytest.mark.multi_provider
class TestCreateChat:
    """POST /v1/chats"""

    def test_create_chat_default_model(self, server):
        resp = httpx.post(f"{API_PREFIX}/chats", json={})
        assert resp.status_code == 201
        body = resp.json()
        assert "id" in body
        assert body["model"] == DEFAULT_MODEL
        assert body["message_count"] == 0
        assert "created_at" in body
        assert "updated_at" in body

    def test_create_chat_with_model(self, server):
        resp = httpx.post(f"{API_PREFIX}/chats", json={"model": STANDARD_MODEL})
        assert resp.status_code == 201
        assert resp.json()["model"] == STANDARD_MODEL

    def test_create_chat_with_title(self, server):
        resp = httpx.post(f"{API_PREFIX}/chats", json={"title": "My Test Chat"})
        assert resp.status_code == 201
        assert resp.json()["title"] == "My Test Chat"

    def test_create_chat_invalid_model(self, server):
        resp = httpx.post(f"{API_PREFIX}/chats", json={"model": "nonexistent-model"})
        assert resp.status_code in (400, 404)
        body = resp.json()
        assert "type" in body and "status" in body and "detail" in body


@pytest.mark.multi_provider
class TestGetChat:
    """GET /v1/chats/{id}"""

    def test_get_chat(self, provider_chat):
        chat_id = provider_chat["id"]
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 200
        body = resp.json()
        assert body["id"] == chat_id
        assert body["model"] == provider_chat["model"]
        assert "created_at" in body
        assert "updated_at" in body

    def test_get_chat_not_found(self, server):
        fake_id = str(uuid.uuid4())
        resp = httpx.get(f"{API_PREFIX}/chats/{fake_id}")
        assert resp.status_code == 404
        body = resp.json()
        assert "type" in body and "status" in body and "detail" in body


@pytest.mark.multi_provider
class TestListChats:
    """GET /v1/chats"""

    def test_list_chats(self, provider_chat):
        resp = httpx.get(f"{API_PREFIX}/chats")
        assert resp.status_code == 200
        body = resp.json()
        assert "items" in body
        assert "page_info" in body
        assert len(body["items"]) >= 1
        assert provider_chat["id"] in [c["id"] for c in body["items"]]

    def test_list_chats_pagination(self, server):
        # Create a few chats
        for _ in range(3):
            r = httpx.post(f"{API_PREFIX}/chats", json={})
            assert r.status_code == 201
        resp = httpx.get(f"{API_PREFIX}/chats", params={"limit": 2})
        assert resp.status_code == 200
        body = resp.json()
        assert "page_info" in body
        assert len(body["items"]) <= 2


@pytest.mark.multi_provider
class TestUpdateChat:
    """PATCH /v1/chats/{id}"""

    def test_update_title(self, provider_chat):
        chat_id = provider_chat["id"]
        resp = httpx.patch(
            f"{API_PREFIX}/chats/{chat_id}",
            json={"title": "Updated Title"},
        )
        assert resp.status_code == 200
        assert resp.json()["title"] == "Updated Title"
        assert "updated_at" in resp.json()

    def test_update_not_found(self, server):
        fake_id = str(uuid.uuid4())
        resp = httpx.patch(
            f"{API_PREFIX}/chats/{fake_id}",
            json={"title": "Nope"},
        )
        assert resp.status_code == 404

    def test_update_whitespace_title_rejected(self, provider_chat):
        """02-12: Whitespace-only title should be rejected."""
        chat_id = provider_chat["id"]
        resp = httpx.patch(
            f"{API_PREFIX}/chats/{chat_id}",
            json={"title": "   "},
        )
        assert resp.status_code in (400, 422), f"Expected 400/422 for whitespace title, got {resp.status_code}"

    def test_update_title_max_length(self, provider_chat):
        """02-13: Title exceeding max length should be rejected."""
        chat_id = provider_chat["id"]
        resp = httpx.patch(
            f"{API_PREFIX}/chats/{chat_id}",
            json={"title": "A" * 1001},
        )
        assert resp.status_code in (400, 422), f"Expected 400/422 for long title, got {resp.status_code}"


@pytest.mark.multi_provider
class TestDeleteChat:
    """DELETE /v1/chats/{id}"""

    def test_delete_chat(self, provider_chat):
        chat_id = provider_chat["id"]
        resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 204

        # Verify gone
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 404

    def test_delete_not_found(self, server):
        fake_id = str(uuid.uuid4())
        resp = httpx.delete(f"{API_PREFIX}/chats/{fake_id}")
        assert resp.status_code == 404
        body = resp.json()
        assert "type" in body and "status" in body and "detail" in body
