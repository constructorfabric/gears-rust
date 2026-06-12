#!/usr/bin/env python3
"""Publish all workspace crates with automatic retry on crates.io rate-limit (429)."""

import os
import re
import subprocess
import shutil
import sys
import time
from datetime import datetime, timezone
from pathlib import Path


_CARGO_BIN: str = ""
_GIT_BIN: str = ""


def _require_bin(name: str) -> str:
    path = shutil.which(name)
    if path is None:
        print(f"Error: '{name}' not found. Ensure it is installed and on PATH.", file=sys.stderr)
        sys.exit(1)
    return path


def check_prerequisites() -> None:
    global _CARGO_BIN, _GIT_BIN
    _CARGO_BIN = _require_bin("cargo")
    _GIT_BIN = _require_bin("git")

    token = os.environ.get("CARGO_REGISTRY_TOKEN")
    if not token:
        print("Error: CARGO_REGISTRY_TOKEN environment variable is not set.", file=sys.stderr)
        sys.exit(1)

    result = subprocess.run(
        [_CARGO_BIN, "workspaces", "--version"],
        capture_output=True,
    )
    if result.returncode != 0:
        print("Error: cargo-workspaces is not installed.\nRun cargo install cargo-workspaces", file=sys.stderr)
        sys.exit(1)


def repo_root() -> Path:
    try:
        result = subprocess.run(
            [_GIT_BIN, "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            check=True,
        )
    except subprocess.CalledProcessError as e:
        print(f"Error: 'git rev-parse --show-toplevel' failed (exit {e.returncode}).", file=sys.stderr)
        sys.exit(1)
    return Path(result.stdout.strip())


def parse_retry_time(text: str) -> datetime | None:
    # e.g. "Please try again after Wed, 10 Jun 2026 11:15:18 GMT"
    m = re.search(r"Please try again after\s+([A-Za-z0-9, :]+ GMT)", text)
    if not m:
        return None
    ts = m.group(1).strip()
    try:
        # Replace GMT with +0000 for reliable parsing across Python versions
        ts_normalized = ts.replace(" GMT", " +0000")
        dt = datetime.strptime(ts_normalized, "%a, %d %b %Y %H:%M:%S %z")
        return dt
    except ValueError:
        return None


def run_publish() -> tuple[int, str]:
    print("\n=== Starting cargo workspaces publish --from-git ===\n", flush=True)

    proc = subprocess.Popen(
        [_CARGO_BIN, "workspaces", "publish", "--from-git"],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        cwd=repo_root(),
    )

    output_lines: list[str] = []

    assert proc.stdout is not None
    for line in proc.stdout:
        output_lines.append(line)
        stripped = line.rstrip()

        # Current crate being published
        m = re.search(r"Publishing\s+(\S+)\s+v([\d\.\-a-zA-Z+]+)", stripped)
        if m:
            print(f"\n[PUBLISHING] {m.group(1)} {m.group(2)}", flush=True)
            continue

        # Uploading
        m = re.search(r"Uploading\s+(\S+)\s+v([\d\.\-a-zA-Z+]+)", stripped)
        if m:
            print(f"[UPLOADED]   {m.group(1)} {m.group(2)}", flush=True)
            continue

        # Already published / skipped
        if re.search(r"already published", stripped, re.IGNORECASE):
            crate_match = re.search(r"(\S+)\s+v([\d\.\-a-zA-Z+]+)", stripped)
            if crate_match:
                print(f"[SKIPPED]    {crate_match.group(1)} {crate_match.group(2)} (already published)", flush=True)
            else:
                print(f"[SKIPPED]    {stripped}", flush=True)
            continue

        # Pass through everything else quietly
        print(f"  {stripped}", flush=True)

    proc.wait()
    return proc.returncode, "".join(output_lines)


def main() -> int:
    check_prerequisites()
    MAX_RETRIES = 5
    retry_count = 0
    while True:
        returncode, full_output = run_publish()

        if returncode == 0:
            print("\n=== All crates published successfully ===", flush=True)
            return 0

        if "429 Too Many Requests" in full_output or "Too Many Requests" in full_output:
            retry_dt = parse_retry_time(full_output)
            if retry_dt:
                retry_count += 1
                if retry_count > MAX_RETRIES:
                    print(
                        f"\n[RATE LIMIT] Exceeded max retries ({MAX_RETRIES}). "
                        f"Giving up on crates.io rate-limit backoff.",
                        file=sys.stderr,
                        flush=True,
                    )
                    return 1
                sleep_sec = (retry_dt - datetime.now(timezone.utc)).total_seconds()
                sleep_sec = int(sleep_sec) + 1
                if sleep_sec > 0:
                    wait_until = retry_dt.strftime("%a, %d %b %Y %H:%M:%S GMT")
                    print(
                        f"\n[RATE LIMITED] crates.io asked us to wait until {wait_until}"
                        f" (sleeping {sleep_sec}s, retry {retry_count}/{MAX_RETRIES})\n",
                        flush=True,
                    )
                    time.sleep(sleep_sec)
                    continue

        # Unknown / hard failure – print everything and exit
        print(full_output, file=sys.stderr)
        return returncode


if __name__ == "__main__":
    sys.exit(main())
