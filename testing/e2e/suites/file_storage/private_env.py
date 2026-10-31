"""Shared launcher for the private file-storage server + sidecar pairs.

The ``lifecycle/`` (local-fs) and ``lifecycle_s3/`` suites each run their OWN
control-plane server and data-plane sidecar so tests can inspect the backend
directly. Everything that is not backend-specific lives here; each suite's
conftest supplies only its backend wiring (``storage_root``, extra config YAML,
extra sidecar variables).

Prerequisites (common to both suites):
  * ``FS_E2E_BINARY`` -- a ``cf-gears-example-server`` built with
    ``--features file-storage``. A dedicated variable (not the shared
    ``E2E_BINARY``) so that enabling these suites does not un-skip other gears.
  * ``FS_SIDECAR_BINARY`` (optional) -- the ``sidecar`` binary; defaults to
    ``target/debug/sidecar``.
  * The ``cryptography`` package, to derive the sidecar public key from the
    fixed signing seed.
"""

from __future__ import annotations

import base64
import contextlib
import os
import re
import socket
import subprocess
import time
from pathlib import Path

import httpx
import pytest

# Fixed 32-byte all-zeros Ed25519 seed (base64url, no padding), shared by the
# control plane and the sidecar. Meaningless outside tests.
SIGNING_KEY_SEED_B64 = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"

# private_env.py -> file_storage -> suites -> e2e -> testing -> repo root.
REPO_ROOT = Path(__file__).resolve().parents[4]
_LOGS_DIR = REPO_ROOT / "testing" / "e2e" / "logs"

# Must equal the control plane's `finalize_internal_secret` (config/e2e-local.yaml).
_INTERNAL_TOKEN = "dev-file-storage-internal-secret"


def require_server_binary(suite: str) -> None:
    """Skip the whole suite when ``FS_E2E_BINARY`` is not set."""
    if not os.environ.get("FS_E2E_BINARY"):
        pytest.skip(
            f"FS_E2E_BINARY not set -- {suite} tests need a binary built with\n"
            "  --features file-storage\n"
            "Build:\n"
            "  cargo build -p cf-gears-example-server --features file-storage\n"
            "  cargo build -p cf-gears-file-storage --bin sidecar",
            allow_module_level=True,
        )


def _derive_public_key_b64(seed_b64: str) -> str:
    """Ed25519 public key (base64url, no padding) for a base64url 32-byte seed."""
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    seed = base64.urlsafe_b64decode(seed_b64 + "=" * (-len(seed_b64) % 4))
    pub = Ed25519PrivateKey.from_private_bytes(seed).public_key().public_bytes_raw()
    return base64.urlsafe_b64encode(pub).rstrip(b"=").decode()


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _assert_port_available(port: int) -> None:
    """Fail fast if something already listens on ``port``.

    A leaked server from a previous run would answer the health poll and the
    tests would drive the wrong process. A CONNECT probe is used (not ``bind``):
    on BSD/macOS ``bind`` can succeed against a wildcard listener.
    """
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(0.5)
        if s.connect_ex(("127.0.0.1", port)) == 0:
            pytest.fail(
                f"port {port} already has a listener -- a previous file-storage "
                f"server likely leaked.\n"
                f"  lsof -nP -iTCP:{port} -sTCP:LISTEN\n  kill <pid>"
            )


def _stop(proc: subprocess.Popen | None) -> None:
    if proc is None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=3)


def _log_tail(path: Path, n: int = 3000) -> str:
    return path.read_text()[-n:] if path.exists() else ""


class FileStorageSidecar:
    """The file-storage data-plane sidecar as a subprocess.

    Satisfies ``lib.orchestrator.SidecarProtocol`` (``name``, ``port``,
    ``start()``, ``stop()``). ``extra_vars`` carries backend-specific process
    variables such as ``FS_SIDECAR_S3_BACKENDS``.
    """

    name = "file-storage-sidecar"

    def __init__(
        self,
        storage_root: str,
        public_key_b64: str,
        control_base_url: str,
        extra_vars: dict[str, str] | None = None,
    ) -> None:
        self.storage_root = storage_root
        self._public_key_b64 = public_key_b64
        self._control_base_url = control_base_url
        self._extra_vars = extra_vars or {}
        self.port = _free_port()
        self._proc: subprocess.Popen | None = None
        self._log_fh = None

    @property
    def _log_path(self) -> Path:
        return _LOGS_DIR / f"file-storage-sidecar-{self.port}.log"

    @staticmethod
    def _binary() -> Path:
        explicit = os.environ.get("FS_SIDECAR_BINARY")
        if explicit:
            p = Path(explicit)
            if not p.exists():
                pytest.fail(f"FS_SIDECAR_BINARY={explicit!r} does not exist")
            return p
        candidate = REPO_ROOT / "target" / "debug" / "sidecar"
        if candidate.exists():
            return candidate
        pytest.fail(
            "file-storage sidecar binary not found at target/debug/sidecar.\n"
            "Build it: cargo build -p cf-gears-file-storage --bin sidecar\n"
            "Or set FS_SIDECAR_BINARY=<path>."
        )

    def start(self) -> None:
        binary = self._binary()
        _LOGS_DIR.mkdir(parents=True, exist_ok=True)
        self._log_fh = open(self._log_path, "w")
        proc_vars = {
            **os.environ,
            "FS_SIDECAR_ADDR": f"0.0.0.0:{self.port}",
            "FS_SIDECAR_PUBLIC_KEY": self._public_key_b64,
            "FS_SIDECAR_BACKEND_ROOT": self.storage_root,
            "FS_SIDECAR_CONTROL_URL": self._control_base_url,
            "FS_SIDECAR_INTERNAL_TOKEN": _INTERNAL_TOKEN,
            "RUST_LOG": os.environ.get("RUST_LOG", "info"),
            **self._extra_vars,
        }
        self._proc = subprocess.Popen(
            [str(binary)],
            env=proc_vars,
            cwd=str(REPO_ROOT),
            stdout=self._log_fh,
            stderr=subprocess.STDOUT,
        )
        self._wait_up()

    def _wait_up(self, timeout: float = 30.0) -> None:
        """Poll the upload path until the sidecar answers with any HTTP status.

        The sidecar has no health endpoint; an unsigned PUT is rejected, which
        is enough to prove it accepts connections.
        """
        dummy = "00000000-0000-0000-0000-000000000000"
        url = f"http://127.0.0.1:{self.port}/api/file-storage-data/v1/upload/{dummy}/{dummy}"
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self._proc.poll() is not None:
                pytest.fail(
                    f"file-storage sidecar exited early (rc={self._proc.returncode})\n"
                    f"Log tail:\n{_log_tail(self._log_path, 2000)}"
                )
            try:
                if httpx.put(url, content=b"", timeout=1).status_code in (200, 400, 401, 403, 404, 422):
                    return
            except httpx.TransportError:
                pass
            time.sleep(0.3)
        pytest.fail(f"file-storage sidecar not ready within {timeout}s (port={self.port})")

    def stop(self) -> None:
        _stop(self._proc)
        self._proc = None
        if self._log_fh is not None:
            self._log_fh.close()
            self._log_fh = None


def _make_config_patch(server_port: int, extra_yaml: str | None):
    """Build the ``GearTestEnv.config_patch`` callback.

    Regex-on-YAML-text (``pyyaml`` is not an e2e dependency). Overrides
    ``storage_root``, ``sidecar_base_url``, ``signing_key_seed`` and
    ``bind_addr``; when ``extra_yaml`` is given it is appended right after the
    ``finalize_internal_secret`` line of the file-storage block.
    """

    def patch(config_text: str, gear_env) -> str:
        sidecar = next(sc for sc in gear_env.sidecars if sc.name == FileStorageSidecar.name)
        root = Path(sidecar.storage_root).as_posix()
        url = f"http://localhost:{sidecar.port}"
        config_text = re.sub(
            r"(storage_root\s*:\s*).*",
            lambda m: f'{m.group(1)}"{root}"',
            config_text,
            count=1,
        )
        config_text = re.sub(r"(sidecar_base_url\s*:\s*).*", rf'\1"{url}"', config_text, count=1)
        config_text = re.sub(
            r"(signing_key_seed\s*:\s*).*", rf'\1"{SIGNING_KEY_SEED_B64}"', config_text, count=1
        )
        config_text = re.sub(
            r"(bind_addr\s*:\s*\"0\.0\.0\.0:)\d+(\")",
            rf"\g<1>{server_port}\g<2>",
            config_text,
            count=1,
        )
        if extra_yaml:
            config_text, n = re.subn(
                r"(finalize_internal_secret\s*:\s*\"[^\"]*\")",
                lambda m: m.group(1) + extra_yaml,
                config_text,
                count=1,
            )
            assert n == 1, (
                "no quoted 'finalize_internal_secret' line in the file-storage config "
                "block -- config/e2e-local.yaml shape changed"
            )
        return config_text

    return patch


@contextlib.contextmanager
def private_server(
    *,
    server_port: int,
    log_suffix: str,
    storage_root: str,
    extra_config_yaml: str | None = None,
    sidecar_vars: dict[str, str] | None = None,
):
    """Start a private sidecar + control-plane server; yield the server base URL.

    Order: sidecar first (its port is injected into the server config), then
    the server, then a readiness poll that also notices an early server exit.
    Both processes are stopped on exit.
    """
    from lib.orchestrator import GearTestEnv, _log_path, _prepare_config

    binary = os.environ.get("FS_E2E_BINARY")
    if not binary or not Path(binary).exists():
        pytest.fail(f"FS_E2E_BINARY={binary!r} is not set or does not exist")

    _assert_port_available(server_port)
    base_url = f"http://localhost:{server_port}"
    sidecar = FileStorageSidecar(
        storage_root=storage_root,
        public_key_b64=_derive_public_key_b64(SIGNING_KEY_SEED_B64),
        control_base_url=base_url,
        extra_vars=sidecar_vars,
    )
    gear_env = GearTestEnv(
        config_patch=_make_config_patch(server_port, extra_config_yaml),
        port=server_port,
        health_path="/healthz",
        health_timeout=60,
        env={"RUST_LOG": os.environ.get("RUST_LOG", "info,file_storage=debug")},
        sidecars=[sidecar],
        log_suffix=log_suffix,
    )

    proc = None
    log_fh = None
    try:
        sidecar.start()
        config_path = _prepare_config(gear_env)
        log = _log_path(gear_env)
        log_fh = open(log, "w")
        proc = subprocess.Popen(
            [binary, "--config", str(config_path), "run"],
            cwd=str(REPO_ROOT),
            stdout=log_fh,
            stderr=subprocess.STDOUT,
            env={**os.environ, **gear_env.env},
        )
        deadline = time.monotonic() + gear_env.health_timeout
        while True:
            if proc.poll() is not None:
                pytest.fail(
                    f"{log_suffix} server exited (rc={proc.returncode}, port={server_port}).\n"
                    f"Log tail:\n{_log_tail(log)}"
                )
            try:
                if httpx.get(f"{base_url}{gear_env.health_path}", timeout=3).status_code == 200:
                    break
            except httpx.TransportError:
                pass
            if time.monotonic() >= deadline:
                pytest.fail(
                    f"{log_suffix} server not healthy within {gear_env.health_timeout}s.\n"
                    f"Log tail:\n{_log_tail(log)}"
                )
            time.sleep(0.5)
        yield base_url
    finally:
        _stop(proc)
        if log_fh is not None:
            log_fh.close()
        sidecar.stop()
