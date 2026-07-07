"""Conftest for the file-storage S3-backend lifecycle E2E (P2 1.7 Stage 6).

This sub-package is a SIBLING of ``lifecycle/`` (the always-on LocalFs
lifecycle suite), not an extension of it — the local-fs suite must keep
passing with zero external infrastructure, so anything S3-specific lives
here instead. It launches its OWN private server + sidecar pair, exactly
like ``lifecycle/`` does, but wires an ``S3Backend`` (ADR-0005) as the
registry's *default* backend so `POST /files` and `POST /files/{id}/multipart`
mint upload URLs whose `claims.backend_id` names the S3 backend — the only
way to exercise Stage 5's per-request sidecar dispatch against a real S3
backend without a per-request backend selector on the create/multipart-initiate
APIs (there isn't one today; see `gears/file-storage/file-storage/src/domain/
service/create.rs`'s and `multipart_service.rs`'s `self.backends.default_backend()`
calls).

Backend selection finding (P2 1.7 Stage 6 recon)
--------------------------------------------------
`FileStorageConfig.default_backend_id` (added by this stage, `config.rs`) is
`None` by default, which keeps `local-fs` as the registry's default — existing
deployments are unaffected. This suite sets it to the configured S3 backend's
id so newly-created files/multipart sessions route to S3 by default, mirroring
option (ii) from `plan.md`'s Stage 6 write-up (there is no per-request
backend-selector on `POST /files`/`POST /files/{id}/multipart` today, so
option (i) — "mint a request naming the target backend explicitly" — is not
available; `POST /files/{id}/migrate` exists but performs a server-side copy
between backends, which does not exercise the sidecar's `claims.backend_id`
dispatch for the *initial* PUT the way this suite wants to prove).

Prerequisites
-------------
* ``FS_E2E_S3_ENDPOINT`` — base URL of a running S3-compatible HTTP endpoint
  (e.g. ``http://127.0.0.1:19099`` for a local ``s3s-fs`` instance). Required —
  the whole package SKIPS at collection time when this is unset, exactly like
  ``lifecycle/``'s ``_require_e2e_binary`` gate (this is Stage 6's own,
  additional gate; the binaries below are still required once the endpoint is
  configured).
* ``FS_E2E_S3_BUCKET`` (optional, default ``file-storage-e2e``) — bucket name.
  The bucket must already exist at the endpoint (this suite performs no
  `CreateBucket` call — see "Running the S3 e2e locally" below for how to
  pre-create it against `s3s-fs`, which treats a top-level directory under its
  data root as a bucket).
* ``FS_E2E_S3_ACCESS_KEY`` / ``FS_E2E_S3_SECRET_KEY`` (optional, default
  ``test-access-key`` / ``test-secret-key`` — matching this repo's own
  `s3_tests.rs` test-double credentials).
* ``FS_E2E_S3_REGION`` (optional, default ``us-east-1``).
* ``FS_E2E_BINARY`` — same as ``lifecycle/``: a ``cf-gears-example-server``
  binary built with ``--features file-storage``.
* ``FS_SIDECAR_BINARY`` (optional) — same as ``lifecycle/``, falls back to
  ``target/debug/sidecar``.

Running the S3 e2e locally
---------------------------
Option A — ``s3s-fs`` (matches this gear's own Rust dev-dependency test
double, `gears/file-storage/file-storage/src/infra/backend/s3_tests.rs`)::

    cargo install s3s-fs --version 0.14.1 --features binary --root /tmp/s3s-fs-install
    mkdir -p /tmp/s3-e2e-data/file-storage-e2e     # pre-create the bucket dir
    /tmp/s3s-fs-install/bin/s3s-fs --host 127.0.0.1 --port 19099 \\
        --access-key test-access-key --secret-key test-secret-key \\
        /tmp/s3-e2e-data

    cargo build -p cf-gears-example-server --features "$(cat config/e2e-features.txt)"
    cargo build -p cf-gears-file-storage --bin sidecar
    export FS_E2E_BINARY=target/debug/cf-gears-example-server
    export FS_SIDECAR_BINARY=target/debug/sidecar
    export FS_E2E_S3_ENDPOINT=http://127.0.0.1:19099
    export FS_E2E_S3_BUCKET=file-storage-e2e
    export FS_E2E_S3_ACCESS_KEY=test-access-key
    export FS_E2E_S3_SECRET_KEY=test-secret-key
    python -m pytest -vv testing/e2e/gears/file_storage/lifecycle_s3

Option B — MinIO (docker), as an alternative test double::

    docker run -d --name fs-e2e-minio -p 19099:9000 \\
        -e MINIO_ROOT_USER=test-access-key -e MINIO_ROOT_PASSWORD=test-secret-key \\
        minio/minio server /data
    docker run --rm --network host --entrypoint sh minio/mc -c \\
        "mc alias set local http://127.0.0.1:19099 test-access-key test-secret-key && \\
         mc mb local/file-storage-e2e"
    # then export the same FS_E2E_S3_* vars as Option A and run pytest.

Without ``FS_E2E_S3_ENDPOINT`` set, the whole package skips cleanly (verified
via ``pytest --collect-only`` + a run with the var unset — see plan.md).
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import tempfile
from pathlib import Path

import pytest

# Reuse the exact crypto/token-minting/port helpers and the `FileStorageSidecar`
# process wrapper from the local-fs lifecycle conftest — no S3-specific change
# is needed in any of that machinery, only in config/env wiring, so importing
# rather than forking avoids duplicating a second copy to keep in sync.
from gears.file_storage.lifecycle.conftest import (
    FileStorageSidecar,
    _SIGNING_KEY_SEED_B64,
    _derive_sidecar_public_key_b64,
)

# ── Constants ─────────────────────────────────────────────────────────────

# The backend id this suite registers the S3 backend under, and makes the
# registry's default (see module docstring — "Backend selection finding").
S3_BACKEND_ID = "s3-e2e"

# A different port than lifecycle/'s 8096 so both suites can run in the same
# CI pass without colliding if ever invoked together.
_SERVER_PORT = 8098

_REPO_ROOT = Path(__file__).resolve().parents[5]
_LOGS_DIR = _REPO_ROOT / "testing" / "e2e" / "logs"


# ── Env helpers ───────────────────────────────────────────────────────────

def _s3_env() -> dict:
    """Read the S3 test-double connection details from the environment."""
    return {
        "endpoint": os.environ["FS_E2E_S3_ENDPOINT"],
        "bucket": os.environ.get("FS_E2E_S3_BUCKET", "file-storage-e2e"),
        "access_key": os.environ.get("FS_E2E_S3_ACCESS_KEY", "test-access-key"),
        "secret_key": os.environ.get("FS_E2E_S3_SECRET_KEY", "test-secret-key"),
        "region": os.environ.get("FS_E2E_S3_REGION", "us-east-1"),
    }


# ── Config patcher ────────────────────────────────────────────────────────

def _patch_file_storage_s3_config(config_text: str, env) -> str:
    """Inject S3-lifecycle overrides into the patched YAML config.

    Applies the same four substitutions `lifecycle/`'s
    `_patch_file_storage_config` does (storage_root/sidecar_base_url/
    signing_key_seed/bind_addr — `storage_root` is unused by this suite since
    every backend here is S3, but the control plane still parses it, so it is
    still pointed at a private temp dir for hygiene/isolation), THEN appends
    an `s3_backends:` entry plus `default_backend_id` under the
    `file-storage.config` block so the CONTROL PLANE (not just the sidecar)
    knows about the S3 backend and routes new uploads to it by default.

    Uses the same regex-on-YAML-text technique as `lifecycle/conftest.py`
    (and the mini-chat conftest before it) rather than a full YAML
    parse-modify-reserialize — `pyyaml` is not a declared e2e dependency
    (`testing/e2e/requirements.txt` has no `pyyaml` entry) and this suite
    only ever needs to append one well-known block, so introducing a new
    parsing dependency isn't worth it here.
    """
    sidecar_port = None
    storage_root = None
    for sc in env.sidecars:
        if sc.name == "file-storage-sidecar":
            sidecar_port = sc.port
            storage_root = sc._storage_root
            break

    assert sidecar_port is not None, "FileStorageSidecar not found in sidecars"
    assert storage_root is not None, "storage_root not resolved from sidecar"

    sidecar_url = f"http://localhost:{sidecar_port}"
    s3 = _s3_env()

    config_text = re.sub(
        r"(storage_root\s*:\s*).*",
        rf'\1"{storage_root}"',
        config_text,
        count=1,
    )
    config_text = re.sub(
        r"(sidecar_base_url\s*:\s*).*",
        rf'\1"{sidecar_url}"',
        config_text,
        count=1,
    )
    config_text = re.sub(
        r"(signing_key_seed\s*:\s*).*",
        rf'\1"{_SIGNING_KEY_SEED_B64}"',
        config_text,
        count=1,
    )
    config_text = re.sub(
        r"(bind_addr\s*:\s*\"0\.0\.0\.0:)\d+(\")",
        rf"\g<1>{_SERVER_PORT}\g<2>",
        config_text,
        count=1,
    )

    # Append `default_backend_id` + `s3_backends` right after
    # `enable_background_sweep: false` (present in every `file-storage:`
    # block of `config/e2e-local.yaml` today — re-`rg` this literal if the
    # base config's shape ever changes and this substitution stops matching,
    # since `count=1` makes a silent no-match a silent no-op rather than an
    # error).
    s3_backends_block = (
        "\n      default_backend_id: {s3_id}"
        "\n      s3_backends:"
        "\n        - id: {s3_id}"
        "\n          endpoint: {endpoint}"
        "\n          region: {region}"
        "\n          bucket: {bucket}"
        "\n          access_key_id: {access_key}"
        "\n          secret_access_key: {secret_key}"
        "\n          path_style: true"
    ).format(
        s3_id=json.dumps(S3_BACKEND_ID),
        endpoint=json.dumps(s3["endpoint"]),
        region=json.dumps(s3["region"]),
        bucket=json.dumps(s3["bucket"]),
        access_key=json.dumps(s3["access_key"]),
        secret_key=json.dumps(s3["secret_key"]),
    )
    new_config_text, n = re.subn(
        r"(enable_background_sweep\s*:\s*false)",
        lambda m: m.group(1) + s3_backends_block,
        config_text,
        count=1,
    )
    assert n == 1, (
        "expected exactly one 'enable_background_sweep: false' line in the "
        "file-storage config block to append s3_backends after — the base "
        "config/e2e-local.yaml shape may have changed"
    )
    return new_config_text


# ── Session fixtures ──────────────────────────────────────────────────────

@pytest.fixture(scope="session", autouse=True)
def require_file_storage_mounted():
    """No-op override — see `lifecycle/conftest.py`'s identical fixture.

    This suite runs its own private server (via `_lifecycle_s3_test_env`), so
    the parent package's shared-CI-server probe (`localhost:8086`) does not
    apply here.
    """


@pytest.fixture(scope="session", autouse=True)
def _require_e2e_s3_endpoint():
    """Skip the whole `lifecycle_s3` package when S3 e2e prerequisites are unset.

    Gated on TWO things, checked together so a single skip message covers
    both:
    * `FS_E2E_S3_ENDPOINT` — this suite's own dedicated opt-in gate (Stage 6).
      Unset = no S3-compatible endpoint available locally; skip rather than
      fail, per plan.md's Stage 6 design (this suite is optional, not part of
      1.7's required coverage).
    * `FS_E2E_BINARY` — same binary requirement as `lifecycle/`.
    """
    if not os.environ.get("FS_E2E_S3_ENDPOINT"):
        pytest.skip(
            "FS_E2E_S3_ENDPOINT not set — the S3 lifecycle e2e suite is "
            "optional and requires a running S3-compatible endpoint.\n"
            "See this package's conftest.py docstring "
            "('Running the S3 e2e locally') for the exact setup recipe.",
            allow_module_level=True,
        )
    if not os.environ.get("FS_E2E_BINARY"):
        pytest.skip(
            "FS_E2E_BINARY not set — lifecycle_s3 tests need a binary built with\n"
            "  --features file-storage\n"
            "Build:\n"
            "  cargo build -p cf-gears-example-server --features file-storage\n"
            "  cargo build -p cf-gears-file-storage --bin sidecar",
            allow_module_level=True,
        )


@pytest.fixture(scope="session")
def fs_s3_storage_root() -> str:
    """A temp dir passed as `storage_root` (unused by any backend in this
    suite — every backend here is S3 — but the control plane still parses
    the field, and the sidecar's `local-fs` entry, always present per
    `sidecar.rs`'s `BackendRegistry`, needs somewhere to point even though no
    test in this suite exercises it).
    """
    d = tempfile.mkdtemp(prefix="cf-fs-e2e-s3-")
    print(f"[file-storage lifecycle_s3] storage_root={d}")
    return d


@pytest.fixture(scope="session")
def fs_s3_signing_seed() -> str:
    return _SIGNING_KEY_SEED_B64


@pytest.fixture(scope="session")
def _lifecycle_s3_test_env(fs_s3_storage_root, fs_s3_signing_seed):
    """Private, S3-lifecycle-scoped server+sidecar orchestration.

    Mirrors `lifecycle/conftest.py`'s `_lifecycle_test_env` almost exactly;
    the only differences are: a different server port (avoids colliding with
    `lifecycle/`'s private server if ever run in the same session), the S3
    config patch (`_patch_file_storage_s3_config`), and `FS_SIDECAR_S3_BACKENDS`
    added to the sidecar's env so its `BackendRegistry` also carries the S3
    backend for `claims.backend_id` dispatch (Stage 5).
    """
    from lib.orchestrator import GearTestEnv, RunningTestEnv, _log_path, _wait_healthy

    pub_key_b64 = _derive_sidecar_public_key_b64(fs_s3_signing_seed)
    print(f"[file-storage lifecycle_s3] sidecar public key: {pub_key_b64}")

    control_base_url = f"http://localhost:{_SERVER_PORT}"
    s3 = _s3_env()

    sidecar = FileStorageSidecar(
        storage_root=fs_s3_storage_root,
        public_key_b64=pub_key_b64,
        control_base_url=control_base_url,
    )
    # `FileStorageSidecar.start()` builds its env dict internally and has no
    # extension point for extra vars, so set `FS_SIDECAR_S3_BACKENDS` in the
    # process environment before calling `start()` — the sidecar's `env`
    # dict is built as `{**os.environ, ...}`, so anything set here is
    # inherited (mirrors how `FS_SIDECAR_BINARY`/`RUST_LOG` are already
    # threaded through the same way).
    os.environ["FS_SIDECAR_S3_BACKENDS"] = json.dumps(
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

    env = GearTestEnv(
        config_patch=_patch_file_storage_s3_config,
        port=_SERVER_PORT,
        health_path="/healthz",
        health_timeout=60,
        env={"RUST_LOG": os.environ.get("RUST_LOG", "info,file_storage=debug")},
        sidecars=[sidecar],
        log_suffix="file-storage-lifecycle-s3",
    )

    # 1. Start sidecar (env.FS_SIDECAR_S3_BACKENDS already set above).
    sidecar.start()
    sidecar_handles = {sidecar.name: sidecar}

    # 2. Prepare config (injects s3_backends + default_backend_id).
    from lib.orchestrator import _prepare_config
    config_path = _prepare_config(env)

    # 3. Resolve binary from FS_E2E_BINARY (dedicated to the file-storage
    #    e2e suites, same var `lifecycle/` uses).
    binary_str = os.environ.get("FS_E2E_BINARY")
    if not binary_str:
        pytest.fail("FS_E2E_BINARY not set — lifecycle_s3 tests need a binary")
    binary_path = Path(binary_str)
    if not binary_path.exists():
        pytest.fail(f"FS_E2E_BINARY={binary_str!r} does not exist")

    # 4. Start server (private process).
    log = _log_path(env)
    log_fh = open(log, "w")
    proc_env = {**os.environ, **env.env}
    proc = subprocess.Popen(
        [str(binary_path), "--config", str(config_path), "run"],
        cwd=str(_REPO_ROOT),
        stdout=log_fh,
        stderr=subprocess.STDOUT,
        env=proc_env,
    )
    print(f"[lifecycle_s3] server started (pid={proc.pid}, port={_SERVER_PORT}, log={log})")

    # 5. Health check.
    _wait_healthy(env)

    running = RunningTestEnv(
        base_url=f"http://localhost:{_SERVER_PORT}",
        env=env,
        sidecars=sidecar_handles,
    )

    yield running

    # 6. Teardown.
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=3)
    sidecar.stop()


@pytest.fixture(scope="session")
def lifecycle_s3_base_url(_lifecycle_s3_test_env) -> str:
    return _lifecycle_s3_test_env.base_url


@pytest.fixture(scope="session")
def lifecycle_s3_auth_headers() -> dict:
    token = os.environ.get("E2E_AUTH_TOKEN", "e2e-token-tenant-a")
    return {"Authorization": f"Bearer {token}"}


@pytest.fixture
def gts_file_type():
    """A syntactically valid GTS file type accepted at upload time."""
    return os.getenv(
        "E2E_FS_GTS_TYPE",
        "gts.cf.fstorage.file.type.v1~x.e2e.test.v1~",
    )
