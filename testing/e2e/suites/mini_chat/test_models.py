"""Tests for the models endpoint."""

import re

import httpx

from .conftest import (
    API_PREFIX, DEFAULT_MODEL, DISABLED_MODEL, MODULE_DIR, RESOURCE_MODEL, assert_problem,
)


def enabled_catalog_model_ids() -> set[str]:
    """Ids of the `model_catalog` entries of config/base.yaml (the rig's
    static model policy catalog) with `enabled: true`."""
    text = (MODULE_DIR / "config" / "base.yaml").read_text()
    catalog = text[text.index("model_catalog:"):]
    catalog = catalog[:catalog.index("\n  static-mini-chat-audit-plugin:")]
    entries = re.split(r"\n\s+- id: ", catalog)[1:]
    ids = set()
    for entry in entries:
        model_id = entry.split("\n", 1)[0].strip().strip('"')
        flag = re.search(r"\n\s+enabled: (true|false)\n", entry).group(1)
        if flag == "true":
            ids.add(model_id)
    return ids


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
        """11-02: the list holds exactly the enabled catalog models of config/base.yaml."""
        expected = enabled_catalog_model_ids()
        resp = httpx.get(f"{API_PREFIX}/models")
        assert resp.status_code == 200
        assert {m["model_id"] for m in resp.json()["items"]} == expected

    def test_model_has_required_fields(self, server):
        """Every model has the required fields; `multiplier_display` is the
        catalog value of config/base.yaml."""
        resp = httpx.get(f"{API_PREFIX}/models")
        assert resp.status_code == 200
        items = resp.json()["items"]
        for m in items:
            assert "model_id" in m
            assert "display_name" in m
            assert "tier" in m, "model must have tier"
            assert "context_window" in m, "model must have context_window"
        assert {m["model_id"]: m["multiplier_display"] for m in items} == {
            "gpt-5.2": "1x",
            "gpt-5-mini": "1x",
            "gpt-5-nano": "0.5x",
            "azure-gpt-4.1": "3x",
            "gpt-5-bare": "0.5x",
            "gpt-4.1-mini-tiny-ctx": "0.5x",
            "gpt-4.1-mini-tiny-ctx-no-input-limit": "0.5x",
        }


class TestGetModel:
    """GET /v1/models/{model_id}"""

    def test_get_nonexistent_model(self, server):
        resp = httpx.get(f"{API_PREFIX}/models/fake-model-xyz")
        body = assert_problem(resp, 404, "not_found", resource_type=RESOURCE_MODEL)
        assert body["context"]["resource_name"] == "fake-model-xyz", body

    def test_internal_fields_not_exposed(self, server):
        """11-04, 11-06: GET returns the requested model without internal fields."""
        resp = httpx.get(f"{API_PREFIX}/models/{DEFAULT_MODEL}")
        assert resp.status_code == 200
        body = resp.json()
        assert body["model_id"] == DEFAULT_MODEL
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
        assert body["model_id"] == DEFAULT_MODEL
        for field in ("context_window", "tier", "multimodal_capabilities", "description"):
            assert field in body, f"Extended field '{field}' missing from model response"


class TestDisabledModel:
    """A catalog entry with `enabled: false` is invisible to users."""

    def test_disabled_model_not_listed(self, server):
        ids = {m["model_id"] for m in httpx.get(f"{API_PREFIX}/models").json()["items"]}
        assert DISABLED_MODEL not in ids

    def test_get_disabled_model_404(self, server):
        assert_problem(
            httpx.get(f"{API_PREFIX}/models/{DISABLED_MODEL}"), 404, "not_found",
            resource_type=RESOURCE_MODEL,
        )
