"""Tests for authorization enforcement (PEP, PDP, access scoping)."""

import httpx
import pytest
from uuid import uuid4

from .conftest import API_PREFIX, parse_sse, expect_done, expect_stream_started, stream_message, DB_PATH
from .mock_provider.responses import Scenario, MockEvent, Usage


@pytest.mark.multi_provider
class TestAuthorization:
    """Verify authorization enforcement at the API level."""

    def test_pep_enforcement(self, server):
        """PEP returns 404 (not 401/403) for nonexistent resources.

        TODO: Full AuthZ is mocked in e2e. This test verifies the basic
        protection: inaccessible resources return 404, never 401/403.
        """
        fake_id = str(uuid4())

        # Nonexistent chat -> 404
        resp_chat = httpx.get(f"{API_PREFIX}/chats/{fake_id}")
        assert resp_chat.status_code == 404, (
            f"Expected 404 for nonexistent chat, got {resp_chat.status_code}"
        )

        # Nonexistent attachment -> 404
        # Use a real-looking path with two fake UUIDs
        resp_attach = httpx.get(f"{API_PREFIX}/chats/{fake_id}/attachments/{str(uuid4())}")
        assert resp_attach.status_code == 404, (
            f"Expected 404 for nonexistent attachment, got {resp_attach.status_code}"
        )

        # Nonexistent turn -> 404
        resp_turn = httpx.get(f"{API_PREFIX}/chats/{fake_id}/turns/{str(uuid4())}")
        assert resp_turn.status_code == 404, (
            f"Expected 404 for nonexistent turn, got {resp_turn.status_code}"
        )

    def test_pdp_reachable_allows_request(self, server):
        """When PDP is reachable, valid requests succeed.

        TODO: Requires PDP mock that can be toggled offline to test the
        403-on-unreachable path. For now, verify that the happy path works
        (PDP is reachable and allows the request).
        """
        # Create a chat — this proves PDP allowed the operation
        resp = httpx.post(f"{API_PREFIX}/chats", json={})
        assert resp.status_code == 201, (
            f"Expected 201 (PDP reachable + allowed), got {resp.status_code}: {resp.text}"
        )

    def test_nonexistent_resource_returns_404(self, server):
        """PDP denial is masked as 404 at the API level.

        TODO: Requires multi-user setup with PDP denial rules. For now,
        verify the masking pattern: random UUIDs -> 404, never 403.
        """
        for _ in range(3):
            fake_id = str(uuid4())
            resp = httpx.get(f"{API_PREFIX}/chats/{fake_id}")
            assert resp.status_code == 404, (
                f"Expected 404 for nonexistent chat {fake_id}, got {resp.status_code}"
            )
            assert resp.status_code != 403, (
                "PDP denial must be masked as 404, not exposed as 403"
            )

    def test_access_scope_sql(self, server):
        """List endpoints only return the current user's own data.

        TODO: Internal SQL generation is not observable via HTTP. In single-user
        mode this is trivially true. Full verification needs multi-user setup.
        """
        # Create a chat
        create_resp = httpx.post(f"{API_PREFIX}/chats", json={"title": "scope-test"})
        assert create_resp.status_code == 201
        chat_id = create_resp.json()["id"]

        # List chats — our chat should be present
        list_resp = httpx.get(f"{API_PREFIX}/chats")
        assert list_resp.status_code == 200
        items = list_resp.json()["items"]
        found_ids = [c["id"] for c in items]
        assert chat_id in found_ids, (
            f"Created chat {chat_id} not found in list response. "
            f"Got {len(items)} chats: {found_ids[:5]}..."
        )
