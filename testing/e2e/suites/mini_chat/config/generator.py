"""Credential loading for mini-chat E2E tests."""

from __future__ import annotations

import os

import pytest

_REQUIRED_ONLINE = ["OPENAI_API_KEY", "AZURE_OPENAI_API_KEY"]


def load_credentials() -> dict[str, str]:
    """Load credentials from environment variables.

    Env vars are sourced by run-e2e.sh from scripts/.env.e2e before pytest starts.
    """
    creds: dict[str, str] = {}
    for key in ("OPENAI_API_KEY", "AZURE_OPENAI_API_KEY", "AZURE_OPENAI_HOST"):
        val = os.environ.get(key)
        if val:
            creds[key] = val

    missing = [k for k in _REQUIRED_ONLINE if k not in creds]
    if missing:
        pytest.fail(
            f"Online mode requires credentials. Missing: {', '.join(missing)}\n"
            f"Source them via: source scripts/.env.e2e"
        )

    return creds
