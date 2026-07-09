"""
Cross-platform helpers for Gears build scripts.

Centralises OS-specific logic so that coverage.py, ci.py and similar
tools stay platform-agnostic.
"""
import os
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

IS_WINDOWS: bool = sys.platform == "win32"

# ---------------------------------------------------------------------------
# E2E feature set
# ---------------------------------------------------------------------------

def read_e2e_features(project_root: Path) -> str:
    """Read the E2E feature set from *config/e2e-features.txt*.

    The file must exist (even if empty).  Aborts with a clear error
    message when it is missing.
    """
    p = project_root / "config" / "e2e-features.txt"
    if not p.is_file():
        print(
            f"ERROR: required file not found: {p}\n"
            "Create it with the desired Cargo feature list "
            "(one comma-separated line, may be empty).",
            file=sys.stderr,
        )
        sys.exit(1)
    return p.read_text(encoding="utf-8").strip()


# ---------------------------------------------------------------------------
# E2E server environment overrides
# ---------------------------------------------------------------------------

def e2e_env_overrides() -> dict:
    """Environment overrides required to launch the e2e server on this OS.

    The committed e2e configs (``config/e2e-local.yaml`` etc.) bake in two
    Unix-only values that the gears reject at startup on Windows:

    * grpc-hub listens on a Unix Domain Socket (``uds:///tmp/...``).
    * file-parser sandboxes local reads under ``/tmp``.

    Rather than fork the YAML (and change Linux/macOS behaviour, where these
    values are intentional), we override just those two values via the app's
    figment env layer (``APP__<SECTION>__<KEY>...``):

    * grpc-hub → ephemeral loopback TCP, which works on every platform; gear
      clients resolve the bound address in-process through the directory.
    * file-parser → the OS temp directory, which exists and is writable.

    Returns an empty mapping on non-Windows hosts so Unix runs are untouched.
    """
    if not IS_WINDOWS:
        return {}
    # Forward slashes: accepted by std::path on Windows and avoid any
    # backslash-escaping ambiguity when the value flows through config layers.
    temp_dir = tempfile.gettempdir().replace("\\", "/")
    return {
        "APP__GEARS__GRPC-HUB__CONFIG__LISTEN_ADDR": "127.0.0.1:0",
        "APP__GEARS__FILE-PARSER__CONFIG__ALLOWED_LOCAL_BASE_DIR": temp_dir,
    }


# ---------------------------------------------------------------------------
# Binary helpers
# ---------------------------------------------------------------------------

def binary_name(name: str) -> str:
    """Return *name* with ``.exe`` appended on Windows."""
    return f"{name}.exe" if IS_WINDOWS else name


def find_binary(target_dir: Path, profile: str, name: str) -> Path:
    """Resolve the full path to a Cargo-produced binary.

    Args:
        target_dir: Cargo target directory (e.g. ``project/target``).
        profile: Build profile directory name (``debug`` or ``release``).
        name: Binary name **without** platform extension.
    """
    return target_dir / profile / binary_name(name)


# ---------------------------------------------------------------------------
# Process management
# ---------------------------------------------------------------------------

def popen_new_group(cmd, **kwargs) -> subprocess.Popen:
    """Start a subprocess in its own process group (cross-platform).

    On Unix this sets ``start_new_session=True``; on Windows it uses
    ``CREATE_NEW_PROCESS_GROUP``.  All extra *kwargs* are forwarded to
    :class:`subprocess.Popen`.
    """
    if IS_WINDOWS:
        flags = kwargs.pop("creationflags", 0)
        flags |= 0x00000200  # CREATE_NEW_PROCESS_GROUP
        kwargs["creationflags"] = flags
    else:
        kwargs["start_new_session"] = True
    return subprocess.Popen(cmd, **kwargs)


def stop_process_tree(
    process: subprocess.Popen,
    timeout: int = 15,
) -> None:
    """Gracefully stop *process* and its children, then force-kill on timeout.

    On Unix the whole process group receives ``SIGINT`` first and
    ``SIGKILL`` as a last resort.  On Windows ``terminate()`` /
    ``kill()`` are used instead (no process-group signals).
    """
    # --- graceful shutdown ---------------------------------------------------
    try:
        if IS_WINDOWS:
            process.terminate()
        else:
            os.killpg(os.getpgid(process.pid), signal.SIGINT)
    except Exception:
        try:
            process.terminate()
        except Exception:
            pass

    # --- wait ----------------------------------------------------------------
    try:
        process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        try:
            if IS_WINDOWS:
                process.kill()
            else:
                os.killpg(os.getpgid(process.pid), signal.SIGKILL)
        except Exception:
            process.kill()
        process.wait()


def kill_port_holder(port: int) -> None:
    """Best-effort kill of whatever process holds *port*.

    Works on macOS (``lsof``), Linux (``fuser``) and Windows
    (``netstat`` + ``taskkill``).  Failures are silently ignored.
    """
    try:
        if IS_WINDOWS:
            # netstat -ano | findstr :<port>  → … <pid>
            result = subprocess.run(
                ["netstat", "-ano"],
                capture_output=True, text=True,
            )
            for line in result.stdout.splitlines():
                if f":{port}" in line and "LISTENING" in line:
                    pid = line.strip().split()[-1]
                    subprocess.run(
                        ["taskkill", "/F", "/PID", pid],
                        capture_output=True,
                    )
                    time.sleep(1)
        elif sys.platform == "darwin":
            result = subprocess.run(
                ["lsof", "-ti", f":{port}"],
                capture_output=True, text=True,
            )
            if result.returncode == 0 and result.stdout:
                for pid in result.stdout.strip().split():
                    subprocess.run(
                        ["kill", "-9", pid],
                        capture_output=True,
                    )
                    time.sleep(1)
        else:
            subprocess.run(
                ["fuser", "-k", f"{port}/tcp"],
                capture_output=True,
            )
    except Exception:
        pass
