"""Tests for the models endpoint."""

import pytest
import httpx

from .conftest import API_PREFIX, DEFAULT_MODEL, STANDARD_MODEL


@pytest.mark.multi_provider
class TestListModels:
    """GET /v1/models"""

    def test_list_models(self, server):
        resp = httpx.get(f"{API_PREFIX}/models")
        assert resp.status_code == 200
        body = resp.json()
        assert "items" in body
        assert len(body["items"]) >= 1
        assert DEFAULT_MODEL in [m["model_id"] for m in body["items"]]

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
            assert "tier" in m, "model must have tier"
            assert "context_window" in m, "model must have context_window"


@pytest.mark.multi_provider
class TestGetModel:
    """GET /v1/models/{model_id}"""

    def test_get_existing_model(self, server):
        resp = httpx.get(f"{API_PREFIX}/models/{DEFAULT_MODEL}")
        assert resp.status_code == 200
        body = resp.json()
        assert body["model_id"] == DEFAULT_MODEL
        assert "provider_id" not in body, "provider_id must not be exposed"
        assert "provider_model_id" not in body, "provider_model_id must not be exposed"

    def test_get_nonexistent_model(self, server):
        resp = httpx.get(f"{API_PREFIX}/models/fake-model-xyz")
        assert resp.status_code == 404

    def test_internal_fields_not_exposed(self, server):
        """11-06: Internal fields must not be in model response."""
        resp = httpx.get(f"{API_PREFIX}/models/{DEFAULT_MODEL}")
        assert resp.status_code == 200
        body = resp.json()
        for field in (
            "provider_id",
            "provider_model_id",
            "input_tokens_credit_multiplier_micro",
            "output_tokens_credit_multiplier_micro",
        ):
            assert field not in body, f"Internal field '{field}' exposed in model response"

    def test_extended_response_fields(self, server):
        """11-08: Model response should include extended fields."""
        resp = httpx.get(f"{API_PREFIX}/models/{DEFAULT_MODEL}")
        assert resp.status_code == 200
        body = resp.json()
        for field in ("context_window", "tier", "multimodal_capabilities", "description"):
            assert field in body, f"Extended field '{field}' missing from model response"


@pytest.mark.multi_provider
class TestModelFieldPresence:
    """Verify only enabled models are exposed and all have required fields."""

    def test_all_listed_models_have_required_fields(self, server):
        resp = httpx.get(f"{API_PREFIX}/models")
        assert resp.status_code == 200
        items = resp.json()["items"]
        assert len(items) >= 1
        for model in items:
            assert "model_id" in model, f"model missing model_id: {model}"
            assert "display_name" in model, f"model missing display_name: {model}"
            assert "tier" in model, f"model missing tier: {model}"
            assert "context_window" in model, f"model missing context_window: {model}"
