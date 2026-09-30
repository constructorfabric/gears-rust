"""Pytest fixtures for the settings-service E2E smoke suite.

The suite runs against the standard e2e server (``config/e2e-local.yaml``).
Tokens map to static identities (static-authn-plugin) inside the static
tenant tree (static-tr-plugin); ``e2e-token-tenant-a`` is the root tenant,
which is what an administrator browsing the platform's settings is.

The gear boots with no categories and no declarations: nothing contributes
settings in the e2e fleet yet, and categories have no REST surface of their
own. The smoke therefore covers what an empty deployment can show — the gear
is up on its routes, the read surfaces answer, malformed input is refused
before the store, an unknown setting is absent — and leaves the write gates,
which need a declaration to reach, to the crate's surface tests.
"""
from __future__ import annotations

import os
from urllib.parse import quote

import pytest

SETTINGS_SERVICE = "/settings-service/v1"

# A well-formed setting key nobody declared: the base type this gear owns,
# then a derived half naming a vendor, the `settings` package, a category and
# a leaf. Parses everywhere; resolves nowhere.
UNDECLARED_KEY = "gts.cf.core.settings.setting_type.v1~acme.settings.network.never_declared.v1~"


@pytest.fixture
def base_url():
    """API Gateway base URL."""
    return os.getenv("E2E_BASE_URL", "http://localhost:8086")


@pytest.fixture
def tenant_a_headers():
    """Headers for the e2e-root tenant (root of the whole tree)."""
    token = os.getenv("E2E_AUTH_TOKEN", "e2e-token-tenant-a")
    return {"Authorization": f"Bearer {token}"}


@pytest.fixture
def settings_url(base_url):
    """The gear's versioned prefix."""
    return f"{base_url}{SETTINGS_SERVICE}"


def encoded(key: str) -> str:
    """A setting key as a path segment: the `~` and `.` it carries, escaped."""
    return quote(key, safe="")
