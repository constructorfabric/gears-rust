"""Fixtures for the S3-backend lifecycle E2E (optional).

Runs a private server + sidecar (see ``../private_env.py``) with an S3 backend
as the registry's default backend (``default_backend_id``), so the upload URLs
minted by ``POST /files`` name the S3 backend. There is no per-request backend
selector on ``POST /files``.

Prerequisites, in addition to ``FS_E2E_BINARY`` / ``FS_SIDECAR_BINARY``:
  * ``FS_E2E_S3_ENDPOINT`` -- URL of a running S3-compatible endpoint; the
    package is skipped when unset.
  * ``FS_E2E_S3_BUCKET`` (default ``file-storage-e2e``) -- must already exist.
  * ``FS_E2E_S3_ACCESS_KEY`` / ``FS_E2E_S3_SECRET_KEY`` (default
    ``test-access-key`` / ``test-secret-key``), ``FS_E2E_S3_REGION``
    (default ``us-east-1``).

Local setup with MinIO::

    docker run -d --name fs-e2e-minio -p 19099:9000 \\
        -e MINIO_ROOT_USER=test-access-key -e MINIO_ROOT_PASSWORD=test-secret-key \\
        minio/minio server /data
    docker run --rm --network host --entrypoint sh minio/mc -c \\
        "mc alias set local http://127.0.0.1:19099 test-access-key test-secret-key && \\
         mc mb local/file-storage-e2e"
    export FS_E2E_S3_ENDPOINT=http://127.0.0.1:19099
    pytest testing/e2e/suites/file_storage/lifecycle_s3 -vv
"""

import json
import os
import tempfile

import pytest

from ..private_env import private_server, require_server_binary

S3_BACKEND_ID = "s3-e2e"

# Differs from the shared CI server (8086) and from lifecycle (8096).
_SERVER_PORT = 8098


def _s3_settings() -> dict:
    return {
        "endpoint": os.environ["FS_E2E_S3_ENDPOINT"],
        "bucket": os.environ.get("FS_E2E_S3_BUCKET", "file-storage-e2e"),
        "access_key": os.environ.get("FS_E2E_S3_ACCESS_KEY", "test-access-key"),
        "secret_key": os.environ.get("FS_E2E_S3_SECRET_KEY", "test-secret-key"),
        "region": os.environ.get("FS_E2E_S3_REGION", "us-east-1"),
    }


@pytest.fixture(scope="session", autouse=True)
def _require_e2e_s3_endpoint():
    if not os.environ.get("FS_E2E_S3_ENDPOINT"):
        pytest.skip(
            "FS_E2E_S3_ENDPOINT not set -- the S3 lifecycle suite needs a running "
            "S3-compatible endpoint (see lifecycle_s3/conftest.py docstring).",
            allow_module_level=True,
        )
    require_server_binary("lifecycle_s3")


@pytest.fixture(scope="session")
def fs_base_url():
    """Base URL of the private control-plane server with S3 as the default backend."""
    s3 = _s3_settings()
    # Control plane: `default_backend_id` + one `s3_backends` entry.
    config_yaml = (
        f"\n      default_backend_id: {json.dumps(S3_BACKEND_ID)}"
        "\n      s3_backends:"
        f"\n        - id: {json.dumps(S3_BACKEND_ID)}"
        f"\n          endpoint: {json.dumps(s3['endpoint'])}"
        f"\n          region: {json.dumps(s3['region'])}"
        f"\n          bucket: {json.dumps(s3['bucket'])}"
        f"\n          access_key_id: {json.dumps(s3['access_key'])}"
        f"\n          secret_access_key: {json.dumps(s3['secret_key'])}"
        "\n          path_style: true"
    )
    # Sidecar: the same backend, for per-token `claims.backend_id` dispatch.
    sidecar_s3 = json.dumps(
        [
            {
                "id": S3_BACKEND_ID,
                "endpoint": s3["endpoint"],
                "region": s3["region"],
                "bucket": s3["bucket"],
                "access_key_id": s3["access_key"],
                "secret_access_key": s3["secret_key"],
                "path_style": True,
            }
        ]
    )
    # `storage_root` is still parsed by the control plane and the sidecar keeps a
    # local-fs entry; no test here writes to it.
    with private_server(
        server_port=_SERVER_PORT,
        log_suffix="file-storage-lifecycle-s3",
        storage_root=tempfile.mkdtemp(prefix="cf-fs-e2e-s3-"),
        extra_config_yaml=config_yaml,
        sidecar_vars={"FS_SIDECAR_S3_BACKENDS": sidecar_s3},
    ) as base_url:
        yield base_url
