"""Conftest for the file-storage LocalFs lifecycle E2E.

This sub-package launches its OWN private server + sidecar pair so tests can
inspect the ``storage_root`` directory directly.  The seam tests in the parent
package continue to run against the shared CI server (they don't use test_env).

Prerequisites
-------------
* ``FS_E2E_BINARY`` must point at a ``cf-gears-example-server`` binary built
  with ``--features file-storage``.  Without it the whole package is skipped.
  A dedicated var (not the shared ``E2E_BINARY``) so enabling this suite does
  not accidentally un-skip other gears (e.g. mini-chat) gated on ``E2E_BINARY``.
* ``FS_SIDECAR_BINARY`` (optional) may point at a pre-built ``sidecar`` binary.
  If absent the conftest resolves ``target/debug/sidecar`` relative to the repo
  root (same strategy as the orchestrator's server binary fallback).
* The ``cryptography`` Python package is required (listed in requirements.txt)
  to derive the sidecar public key from the fixed seed at runtime and to mint
  signed download tokens directly against the sidecar for the download step.
"""

from __future__ import annotations

import base64
import json
import os
import re
import socket
import subprocess
import tempfile
import time
from pathlib import Path

import httpx
import pytest

# ── Constants ─────────────────────────────────────────────────────────────

# 32-byte all-zeros seed, base64url-encoded (no padding).
# This FIXED seed is written into the patched e2e config so the control plane
# and the sidecar share a stable keypair; it is meaningless outside tests.
_SIGNING_KEY_SEED_B64 = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"

# Repo root (resolved from this file's location:
#   lifecycle/ -> file_storage/ -> gears/ -> e2e/ -> testing/ -> repo root).
_REPO_ROOT = Path(__file__).resolve().parents[5]

# The lifecycle server runs on a different port from the shared CI server (8086)
# to avoid conflicts when both are running simultaneously.
_SERVER_PORT = 8096

_LOGS_DIR = _REPO_ROOT / "testing" / "e2e" / "logs"


# ── Crypto helpers ────────────────────────────────────────────────────────

def _decode_b64url(s: str) -> bytes:
    """Decode a base64url string (with or without padding)."""
    padded = s + "=" * (-len(s) % 4)
    return base64.urlsafe_b64decode(padded)


def _encode_b64url(b: bytes) -> str:
    """Encode bytes to base64url without padding."""
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def _derive_sidecar_public_key_b64(seed_b64: str) -> str:
    """Derive the Ed25519 public key from a base64url-encoded 32-byte seed.

    Requires the ``cryptography`` package (pinned in requirements.txt).
    Returns the public key as a base64url-encoded string (no padding).
    The control plane uses the same key pair: seed → private key → public key.
    The sidecar is configured with the PUBLIC key only (``FS_SIDECAR_PUBLIC_KEY``).
    """
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    seed_bytes = _decode_b64url(seed_b64)
    private_key = Ed25519PrivateKey.from_private_bytes(seed_bytes)
    pub_bytes = private_key.public_key().public_bytes_raw()
    return _encode_b64url(pub_bytes)


def _mint_token(
    seed_b64: str,
    op: str,
    file_id: str,
    version_id: str,
    backend_path: str,
    backend_id: str = "local-fs",
    ttl_secs: int = 300,
) -> str:
    """Mint a signed URL token in the same format the control plane uses.

    Token format (from ``infra/signed_url/mod.rs``):
        base64url(json_payload) + "." + base64url(ed25519_signature)

    ``payload`` is JSON-encoded ``Claims`` with the given fields.
    The signature is Ed25519 over the raw JSON bytes (the control plane uses
    ``ring``; we use ``cryptography`` — both implement Ed25519 PKCS8 / raw).

    The token is validated by the sidecar using ``Verifier::verify()`` which
    calls ``ring::signature::UnparsedPublicKey::verify``.  Both libraries use
    the same Ed25519 algorithm so the tokens are interoperable.
    """
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    seed_bytes = _decode_b64url(seed_b64)
    private_key = Ed25519PrivateKey.from_private_bytes(seed_bytes)

    exp = int(time.time()) + ttl_secs
    claims: dict = {
        "op": op,
        "file_id": file_id,
        "version_id": version_id,
        "backend_id": backend_id,
        "backend_path": backend_path,
        "exp": exp,
    }
    payload_bytes = json.dumps(claims, separators=(",", ":")).encode()
    signature = private_key.sign(payload_bytes)

    return _encode_b64url(payload_bytes) + "." + _encode_b64url(signature)


# ── Port helper ───────────────────────────────────────────────────────────

def _free_port() -> int:
    """Return an OS-assigned free TCP port."""
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# ── Sidecar class (SidecarProtocol) ──────────────────────────────────────

class FileStorageSidecar:
    """Launches the file-storage data-plane sidecar as a subprocess.

    Implements ``SidecarProtocol`` expected by ``lib.orchestrator.test_env``:
    ``name``, ``port``, ``start()``, ``stop()``.

    Configuration (matches ``sidecar.rs`` env vars):
      * ``FS_SIDECAR_ADDR``         bind address (0.0.0.0:<port>)
      * ``FS_SIDECAR_PUBLIC_KEY``   base64url Ed25519 public key (no padding)
      * ``FS_SIDECAR_BACKEND_ROOT`` local-fs root dir
      * ``FS_SIDECAR_CONTROL_URL``  control-plane base URL for finalize callback
    """

    name = "file-storage-sidecar"

    def __init__(
        self,
        storage_root: str,
        public_key_b64: str,
        control_base_url: str = "",
    ) -> None:
        self._storage_root = storage_root
        self._public_key_b64 = public_key_b64
        self._control_base_url = control_base_url
        self._port: int = _free_port()
        self._proc: subprocess.Popen | None = None

    @property
    def port(self) -> int:
        return self._port

    def _resolve_binary(self) -> Path:
        """Resolve the sidecar binary path.

        Priority:
        1. FS_SIDECAR_BINARY env var (explicit path — CI or developer override)
        2. target/debug/sidecar in the repo root (cargo build debug output)
        """
        explicit = os.environ.get("FS_SIDECAR_BINARY")
        if explicit:
            p = Path(explicit)
            if not p.exists():
                pytest.fail(f"FS_SIDECAR_BINARY={explicit!r} does not exist")
            return p
        candidate = _REPO_ROOT / "target" / "debug" / "sidecar"
        if candidate.exists():
            return candidate
        pytest.fail(
            "file-storage sidecar binary not found at target/debug/sidecar.\n"
            "Build it: cargo build -p cf-gears-file-storage --bin sidecar\n"
            "Or set FS_SIDECAR_BINARY=<path>."
        )

    def start(self) -> None:
        binary = self._resolve_binary()
        _LOGS_DIR.mkdir(parents=True, exist_ok=True)
        log_path = _LOGS_DIR / f"file-storage-sidecar-{self._port}.log"
        log_fh = open(log_path, "w")

        env = {
            **os.environ,
            "FS_SIDECAR_ADDR": f"0.0.0.0:{self._port}",
            "FS_SIDECAR_PUBLIC_KEY": self._public_key_b64,
            "FS_SIDECAR_BACKEND_ROOT": self._storage_root,
            # Finalize callback: the sidecar POSTs here after every successful PUT.
            "FS_SIDECAR_CONTROL_URL": self._control_base_url,
            # Quiet logging for the sidecar unless overridden.
            "RUST_LOG": os.environ.get("RUST_LOG", "info"),
        }

        self._proc = subprocess.Popen(
            [str(binary)],
            env=env,
            cwd=str(_REPO_ROOT),
            stdout=log_fh,
            stderr=subprocess.STDOUT,
        )
        print(
            f"[file-storage sidecar] started "
            f"(pid={self._proc.pid}, port={self._port}, log={log_path})"
        )
        self._wait_up()

    def _wait_up(self, timeout: int = 30) -> None:
        """Poll the sidecar upload endpoint until it answers (any HTTP status).

        The sidecar has no /healthz, so we probe the data-plane upload path with
        a dummy UUID pair.  Any HTTP response (even 401 / 403) proves the server
        is accepting connections.
        """
        dummy = "00000000-0000-0000-0000-000000000000"
        url = (
            f"http://127.0.0.1:{self._port}"
            f"/api/file-storage-data/v1/upload/{dummy}/{dummy}"
        )
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self._proc and self._proc.poll() is not None:
                log_path = _LOGS_DIR / f"file-storage-sidecar-{self._port}.log"
                tail = log_path.read_text()[-2000:] if log_path.exists() else ""
                pytest.fail(
                    f"file-storage sidecar exited early (rc={self._proc.returncode})\n"
                    f"Log tail:\n{tail}"
                )
            try:
                # Send an empty PUT; the sidecar will reject it (401 missing token)
                # but that confirms the server is up.
                r = httpx.put(url, content=b"", timeout=1)
                if r.status_code in (200, 400, 401, 403, 404, 422):
                    print(f"[file-storage sidecar] ready at port {self._port}")
                    return
            except httpx.TransportError:
                pass
            time.sleep(0.3)
        pytest.fail(
            f"file-storage sidecar did not become ready within {timeout}s "
            f"(port={self._port})"
        )

    def stop(self) -> None:
        if self._proc is not None:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._proc.kill()
                self._proc.wait(timeout=3)
            self._proc = None


# ── Config patcher ────────────────────────────────────────────────────────

def _patch_file_storage_config(config_text: str, env) -> str:
    """Inject lifecycle-specific overrides into the patched YAML config.

    Finds the file-storage gear block and replaces:
      * ``storage_root``     → the session temp dir
      * ``sidecar_base_url`` → the launched sidecar's URL
      * ``signing_key_seed`` → the fixed test seed
      * ``bind_addr``        → the lifecycle server port (avoids CI collision)

    Uses the same regex-on-YAML-text technique as the mini-chat conftest so we
    don't introduce a new dependency.
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

    # Replace storage_root (any current value).
    config_text = re.sub(
        r"(storage_root\s*:\s*).*",
        rf'\1"{storage_root}"',
        config_text,
        count=1,
    )
    # Replace sidecar_base_url (any current value).
    config_text = re.sub(
        r"(sidecar_base_url\s*:\s*).*",
        rf'\1"{sidecar_url}"',
        config_text,
        count=1,
    )
    # Replace signing_key_seed (any current value).
    config_text = re.sub(
        r"(signing_key_seed\s*:\s*).*",
        rf'\1"{_SIGNING_KEY_SEED_B64}"',
        config_text,
        count=1,
    )
    # Override the server bind port so we don't collide with the shared CI server.
    config_text = re.sub(
        r"(bind_addr\s*:\s*\"0\.0\.0\.0:)\d+(\")",
        rf"\g<1>{_SERVER_PORT}\g<2>",
        config_text,
        count=1,
    )
    return config_text


# ── Session fixtures ──────────────────────────────────────────────────────

@pytest.fixture(scope="session", autouse=True)
def require_file_storage_mounted():
    """Override the parent package's shared-server probe.

    The parent ``file_storage/conftest.py`` declares this fixture as
    ``autouse=True`` to skip seam tests when the CI shared server isn't
    running.  The lifecycle sub-package runs its OWN private server (via
    ``_lifecycle_test_env``), so the probe against ``localhost:8086``
    is irrelevant here.  Overriding with a no-op prevents the inherited
    fixture from skipping the entire lifecycle suite.
    """


@pytest.fixture(scope="session", autouse=True)
def _require_e2e_binary():
    """Skip the whole lifecycle package when FS_E2E_BINARY is not set.

    The lifecycle tests need a server binary built with --features file-storage
    AND the sidecar binary.  We gate on a DEDICATED env var (``FS_E2E_BINARY``)
    rather than the shared ``E2E_BINARY`` so that enabling this suite does not
    accidentally un-skip other gears (e.g. mini-chat) that gate on ``E2E_BINARY``
    and expect their own scoped invocation (``make e2e-mini-chat``).
    """
    if not os.environ.get("FS_E2E_BINARY"):
        pytest.skip(
            "FS_E2E_BINARY not set — lifecycle tests need a binary built with\n"
            "  --features file-storage\n"
            "Build:\n"
            "  cargo build -p cf-gears-example-server --features file-storage\n"
            "  cargo build -p cf-gears-file-storage --bin sidecar\n"
            "Run:\n"
            "  FS_E2E_BINARY=target/debug/cf-gears-example-server \\\n"
            "  pytest testing/e2e/gears/file_storage/lifecycle/ -vv",
            allow_module_level=True,
        )


@pytest.fixture(scope="session")
def fs_storage_root() -> str:
    """A temp directory used as the LocalFsBackend root for this test session.

    The path is shared with:
    * the server's file-storage.config.storage_root (via config_patch)
    * the sidecar's FS_SIDECAR_BACKEND_ROOT
    * the lifecycle test, which reads files from it to verify on-disk content.
    """
    d = tempfile.mkdtemp(prefix="cf-fs-e2e-")
    print(f"[file-storage lifecycle] storage_root={d}")
    return d


@pytest.fixture(scope="session")
def fs_signing_seed() -> str:
    """The fixed Ed25519 seed used by both the control plane and the sidecar."""
    return _SIGNING_KEY_SEED_B64


@pytest.fixture(scope="session")
def _lifecycle_test_env(fs_storage_root, fs_signing_seed):
    """Private, lifecycle-scoped server+sidecar orchestration.

    This fixture is intentionally named with a leading underscore and does NOT
    override the root-level ``test_env`` / ``gear_test_env`` fixtures.  Keeping
    it separate prevents the lifecycle suite from clobbering the session-global
    ``test_env`` that other gears (e.g. mini-chat) rely on for their own
    ``gear_test_env`` override.

    Duplicates the orchestration logic from ``lib.orchestrator.test_env`` so
    the lifecycle suite is fully self-contained:

    1. Start the sidecar (so its port is known).
    2. Patch the config (inject sidecar URL, storage root, signing seed, port).
    3. Resolve the server binary (FS_E2E_BINARY env var — dedicated to this
       suite so it does not un-skip other gears gated on E2E_BINARY).
    4. Start the server.
    5. Wait for health.
    6. Yield a RunningTestEnv-compatible namespace.
    7. Teardown: stop server and sidecar.
    """
    from lib.orchestrator import GearTestEnv, RunningTestEnv, _log_path, _wait_healthy

    pub_key_b64 = _derive_sidecar_public_key_b64(fs_signing_seed)
    print(f"[file-storage lifecycle] sidecar public key: {pub_key_b64}")

    control_base_url = f"http://localhost:{_SERVER_PORT}"

    sidecar = FileStorageSidecar(
        storage_root=fs_storage_root,
        public_key_b64=pub_key_b64,
        control_base_url=control_base_url,
    )

    env = GearTestEnv(
        config_patch=_patch_file_storage_config,
        port=_SERVER_PORT,
        health_path="/healthz",
        health_timeout=60,
        env={"RUST_LOG": os.environ.get("RUST_LOG", "info,file_storage=debug")},
        sidecars=[sidecar],
        log_suffix="file-storage-lifecycle",
    )

    # 1. Start sidecar
    sidecar.start()
    sidecar_handles = {sidecar.name: sidecar}

    # 2. Prepare config
    from lib.orchestrator import _prepare_config
    config_path = _prepare_config(env)

    # 3. Resolve binary from FS_E2E_BINARY env var (dedicated to this suite).
    binary_str = os.environ.get("FS_E2E_BINARY")
    if not binary_str:
        pytest.fail(
            "FS_E2E_BINARY not set — lifecycle tests need a binary built with\n"
            "  --features file-storage"
        )
    binary_path = Path(binary_str)
    if not binary_path.exists():
        pytest.fail(f"FS_E2E_BINARY={binary_str!r} does not exist")

    # 4. Start server (private process — does NOT write to the global _server_proc)
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
    print(f"[lifecycle] server started (pid={proc.pid}, port={_SERVER_PORT}, log={log})")

    # 5. Health check
    _wait_healthy(env)

    running = RunningTestEnv(
        base_url=f"http://localhost:{_SERVER_PORT}",
        env=env,
        sidecars=sidecar_handles,
    )

    yield running

    # 6. Teardown
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=3)
    sidecar.stop()


@pytest.fixture(scope="session")
def lifecycle_base_url(_lifecycle_test_env) -> str:
    """Base URL of the private lifecycle server."""
    return _lifecycle_test_env.base_url


@pytest.fixture(scope="session")
def lifecycle_sidecar(_lifecycle_test_env) -> FileStorageSidecar:
    """The running sidecar instance."""
    return _lifecycle_test_env.sidecars["file-storage-sidecar"]


@pytest.fixture(scope="session")
def lifecycle_auth_headers() -> dict:
    """Auth headers for the lifecycle server (matches static-authn-plugin token)."""
    token = os.environ.get("E2E_AUTH_TOKEN", "e2e-token-tenant-a")
    return {"Authorization": f"Bearer {token}"}


@pytest.fixture(scope="session")
def fs_token_minter(fs_signing_seed):
    """Factory that mints signed sidecar tokens from the fixed test seed.

    Returns a callable ``mint(op, file_id, version_id, backend_path, **kwargs)``
    that produces a token in the same format the control plane issues.

    This is used to directly call the sidecar's download endpoint without going
    through the control-plane ``GET /download-url``, which requires the version
    to be in ``available`` status (set by the ``finalize_upload`` s2s callback
    that the thin P1 sidecar intentionally omits).
    """
    def mint(
        op: str,
        file_id: str,
        version_id: str,
        backend_path: str,
        backend_id: str = "local-fs",
        ttl_secs: int = 300,
    ) -> str:
        return _mint_token(
            seed_b64=fs_signing_seed,
            op=op,
            file_id=file_id,
            version_id=version_id,
            backend_path=backend_path,
            backend_id=backend_id,
            ttl_secs=ttl_secs,
        )
    return mint
