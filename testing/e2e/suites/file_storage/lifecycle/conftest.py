"""Fixtures for the local-fs lifecycle E2E.

Runs a private server + sidecar (see ``../private_env.py``) whose backend root
is a temp directory the tests read directly.

Run (needs ``FS_E2E_BINARY``, see ``private_env.py``):
    pytest testing/e2e/suites/file_storage/lifecycle/ -vv
"""

import tempfile

import pytest

from ..private_env import private_server, require_server_binary

# Differs from the shared CI server (8086) and from lifecycle_s3 (8098).
_SERVER_PORT = 8096


@pytest.fixture(scope="session", autouse=True)
def _require_e2e_binary():
    require_server_binary("lifecycle")


@pytest.fixture(scope="session")
def fs_storage_root() -> str:
    """Backend root shared by the server config, the sidecar and the tests."""
    return tempfile.mkdtemp(prefix="cf-fs-e2e-")


@pytest.fixture(scope="session")
def fs_base_url(fs_storage_root):
    """Base URL of the private control-plane server."""
    with private_server(
        server_port=_SERVER_PORT,
        log_suffix="file-storage-lifecycle",
        storage_root=fs_storage_root,
    ) as base_url:
        yield base_url
