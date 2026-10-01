"""Credential loading for mini-chat E2E tests."""

from __future__ import annotations

import os

import pytest

# Online mode needs at least one provider key. Tests of a provider whose key
# is missing are skipped (see `skip_unless_provider_key`).
_PROVIDER_KEYS = {"openai": "OPENAI_API_KEY", "azure": "AZURE_OPENAI_API_KEY"}


def model_provider(model_id: str) -> str:
    """Provider of a catalog model in config/base.yaml: `azure-*` models are
    on Azure OpenAI, every other model is on OpenAI."""
    return "azure" if model_id.startswith("azure-") else "openai"


def provider_key_missing(provider: str) -> bool:
    return not os.environ.get(_PROVIDER_KEYS[provider])


def skip_unless_provider_key(provider: str) -> None:
    """In online mode, skip the current test when `provider` has no key."""
    if provider_key_missing(provider):
        pytest.skip(f"{_PROVIDER_KEYS[provider]} is not set")


def load_credentials() -> dict[str, str]:
    """Load credentials from environment variables.

    Env vars are sourced by run-e2e.sh from scripts/.env.e2e before pytest starts.
    """
    creds: dict[str, str] = {}
    for key in ("OPENAI_API_KEY", "AZURE_OPENAI_API_KEY", "AZURE_OPENAI_HOST"):
        val = os.environ.get(key)
        if val:
            creds[key] = val

    if not any(k in creds for k in _PROVIDER_KEYS.values()):
        pytest.fail(
            "Online mode requires OPENAI_API_KEY or AZURE_OPENAI_API_KEY.\n"
            "Source them via: source scripts/.env.e2e"
        )

    return creds
