"""Shared create/upload/download steps for the file-storage lifecycle suites.

Plain helpers (not fixtures, not test dependencies): every test calls them to
build its own data.
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass

import httpx

REQUEST_TIMEOUT = 30.0
API_BASE = "/api/file-storage/v1"


@dataclass
class UploadedFile:
    file_id: str
    version_id: str
    upload: httpx.Response  # the sidecar's PUT response


def create_file(client: httpx.Client, gts_file_type: str, **extra) -> dict:
    """``POST /files`` for a fresh file; returns the create ticket."""
    r = client.post(
        f"{API_BASE}/files",
        json={
            "owner_kind": "user",
            "owner_id": str(uuid.uuid4()),
            "name": f"e2e-{uuid.uuid4()}.bin",
            "gts_file_type": gts_file_type,
            "mime_type": "application/octet-stream",
            **extra,
        },
    )
    assert r.status_code == 201, f"POST /files failed: {r.status_code}\n{r.text}"
    return r.json()


def upload_file(client: httpx.Client, gts_file_type: str, payload: bytes) -> UploadedFile:
    """Create a file and PUT ``payload`` to the sidecar's signed upload URL."""
    ticket = create_file(client, gts_file_type)
    upload = httpx.put(ticket["upload_url"], content=payload, timeout=REQUEST_TIMEOUT)
    assert upload.status_code == 200, f"sidecar PUT failed: {upload.status_code}\n{upload.text}"
    return UploadedFile(ticket["file_id"], ticket["version_id"], upload)


def download_url(client: httpx.Client, file_id: str) -> str:
    """``GET /files/{id}/download-url``; returns the signed sidecar URL."""
    r = client.get(f"{API_BASE}/files/{file_id}/download-url")
    assert r.status_code == 200, f"GET download-url failed: {r.status_code}\n{r.text}"
    return r.json()["download_url"]


def download_bytes(url: str) -> bytes:
    """GET a signed sidecar download URL."""
    dl = httpx.get(url, timeout=REQUEST_TIMEOUT)
    assert dl.status_code == 200, f"sidecar GET failed: {dl.status_code}\n{dl.text}"
    return dl.content
