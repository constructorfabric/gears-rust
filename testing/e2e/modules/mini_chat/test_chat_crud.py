"""Tests for chat CRUD operations."""

import uuid

import pytest
import httpx

from .conftest import API_PREFIX, DEFAULT_MODEL, STANDARD_MODEL


class TestCreateChat:
    """POST /v1/chats"""

    def test_create_chat_default_model(self, server):
        resp = httpx.post(f"{API_PREFIX}/chats", json={})
        assert resp.status_code == 201
        body = resp.json()
        assert "id" in body
        assert body["model"] == DEFAULT_MODEL
        assert body["message_count"] == 0

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


class TestGetChat:
    """GET /v1/chats/{id}"""

    def test_get_chat(self, chat):
        chat_id = chat["id"]
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 200
        body = resp.json()
        assert body["id"] == chat_id
        assert body["model"] == chat["model"]

    def test_get_chat_not_found(self, server):
        fake_id = str(uuid.uuid4())
        resp = httpx.get(f"{API_PREFIX}/chats/{fake_id}")
        assert resp.status_code == 404


class TestListChats:
    """GET /v1/chats"""

    def test_list_chats(self, chat):
        resp = httpx.get(f"{API_PREFIX}/chats")
        assert resp.status_code == 200
        body = resp.json()
        assert "items" in body
        assert len(body["items"]) >= 1

    def test_list_chats_pagination(self, server):
        # Create a few chats
        for _ in range(3):
            httpx.post(f"{API_PREFIX}/chats", json={})
        resp = httpx.get(f"{API_PREFIX}/chats", params={"limit": 2})
        assert resp.status_code == 200
        body = resp.json()
        assert len(body["items"]) <= 2


class TestUpdateChat:
    """PATCH /v1/chats/{id}"""

    def test_update_title(self, chat):
        chat_id = chat["id"]
        resp = httpx.patch(
            f"{API_PREFIX}/chats/{chat_id}",
            json={"title": "Updated Title"},
        )
        assert resp.status_code == 200
        assert resp.json()["title"] == "Updated Title"

    def test_update_not_found(self, server):
        fake_id = str(uuid.uuid4())
        resp = httpx.patch(
            f"{API_PREFIX}/chats/{fake_id}",
            json={"title": "Nope"},
        )
        assert resp.status_code == 404


class TestDeleteChat:
    """DELETE /v1/chats/{id}"""

    def test_delete_chat(self, chat):
        chat_id = chat["id"]
        resp = httpx.delete(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 204

        # Verify gone
        resp = httpx.get(f"{API_PREFIX}/chats/{chat_id}")
        assert resp.status_code == 404

    def test_delete_not_found(self, server):
        fake_id = str(uuid.uuid4())
        resp = httpx.delete(f"{API_PREFIX}/chats/{fake_id}")
        assert resp.status_code == 404
