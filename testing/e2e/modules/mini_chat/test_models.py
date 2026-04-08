"""Tests for the models endpoint."""

import httpx

from .conftest import API_PREFIX, DEFAULT_MODEL, STANDARD_MODEL


class TestListModels:
    """GET /v1/models"""

    def test_list_models(self, server):
        resp = httpx.get(f"{API_PREFIX}/models")
        assert resp.status_code == 200
        body = resp.json()
        assert "items" in body
        assert len(body["items"]) >= 1

    def test_catalog_models_present(self, server):
        """All models from mini-chat.yaml catalog should appear."""
        resp = httpx.get(f"{API_PREFIX}/models")
        model_ids = {m["model_id"] for m in resp.json()["items"]}
        assert DEFAULT_MODEL in model_ids
        assert STANDARD_MODEL in model_ids

    def test_model_has_required_fields(self, server):
        resp = httpx.get(f"{API_PREFIX}/models")
        for m in resp.json()["items"]:
            assert "model_id" in m
            assert "display_name" in m


class TestGetModel:
    """GET /v1/models/{model_id}"""

    def test_get_existing_model(self, server):
        resp = httpx.get(f"{API_PREFIX}/models/{DEFAULT_MODEL}")
        assert resp.status_code == 200
        body = resp.json()
        assert body["model_id"] == DEFAULT_MODEL

    def test_get_nonexistent_model(self, server):
        resp = httpx.get(f"{API_PREFIX}/models/fake-model-xyz")
        assert resp.status_code == 404
