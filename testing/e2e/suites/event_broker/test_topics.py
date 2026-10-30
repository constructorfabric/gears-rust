"""Topic introspection scenarios (topics/1.01-1.04).

All tests are read-only.  The pre-provisioned topics injected into the
session-scoped server at startup (``TOPIC_STREAM``, ``TOPIC_LONGPOLL``,
``TOPIC_4P``, ``TOPIC_STRICT``) are visible to the list and segment endpoints.
"""

from __future__ import annotations

from datetime import datetime

import pytest

from .conftest import (
    EVENT_TYPE_STREAM,
    TOPIC_LONGPOLL,
    TOPIC_PREFILLED,
    TOPIC_STREAM,
    TRACE_ID,
)


async def test_list_topics_returns_paged_list(api):
    """scenario: topics/1.01-positive-list-topics.md"""
    async with api() as client:
        resp = await client.get("/topics")
    assert resp.status_code == 200
    body = resp.json()
    # Shape: paged list with items[] and page_info.
    assert "items" in body
    assert "page_info" in body
    page_info = body["page_info"]
    assert "limit" in page_info
    assert "next_cursor" in page_info
    # At least the pre-provisioned topics must appear.
    ids = {item["id"] for item in body["items"]}
    assert TOPIC_STREAM in ids
    assert TOPIC_LONGPOLL in ids
    # Each item has the documented fields.
    happy = next(i for i in body["items"] if i["id"] == TOPIC_STREAM)
    assert happy == {
        "id": TOPIC_STREAM,
        "description": happy["description"],  # value set by _topic_instance helper
        "retention": happy["retention"],       # null or ISO duration from config
    }


@pytest.mark.backend_sensitive
async def test_list_topic_segments_returns_manifest(api):
    """scenario: topics/1.02-positive-list-topic-segments.md"""
    async with api() as client:
        resp = await client.get(
            f"/topics/segments?topic={TOPIC_STREAM}&partition=0"
        )
    assert resp.status_code == 200
    body = resp.json()
    assert body == {
        "topic": TOPIC_STREAM,
        "partition": 0,
        "start_sequence": body["start_sequence"],
        "end_sequence": body["end_sequence"],
        "start_time": body["start_time"],
        "end_time": body["end_time"],
        "segments": body["segments"],
    }
    assert isinstance(body["segments"], list)


async def test_segments_for_unknown_topic_returns_404(api):
    """scenario: topics/1.03-negative-segments-unknown-topic.md"""
    unknown = "gts.cf.core.events.topic.v1~cf.e2e.event_broker.nonexistent.v1"
    async with api() as client:
        resp = await client.get(
            f"/topics/segments?topic={unknown}&partition=0"
        )
    assert resp.status_code == 404
    body = resp.json()
    assert body == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
        "title": "Not Found",
        "status": 404,
        "detail": body["detail"],
        "instance": body["instance"],
        "trace_id": TRACE_ID,
        "context": {
            "resource_type": "gts.cf.core.events.topic.v1~",
            "resource_name": unknown,
        },
    }


async def test_list_event_types_returns_paged_list(api):
    """scenario: topics/1.04-positive-list-event-types.md"""
    async with api() as client:
        resp = await client.get(f"/event-types?topic={TOPIC_STREAM}")
    assert resp.status_code == 200
    body = resp.json()
    assert "items" in body
    assert "page_info" in body
    ids = {item["id"] for item in body["items"]}
    assert EVENT_TYPE_STREAM in ids
    # Each item has the documented fields.
    happy_et = next(i for i in body["items"] if i["id"] == EVENT_TYPE_STREAM)
    assert happy_et == {
        "id": EVENT_TYPE_STREAM,
        "topic": TOPIC_STREAM,
        "description": happy_et["description"],
        "allowed_subject_types": happy_et["allowed_subject_types"],
        "partition_key": happy_et["partition_key"],
        "data_schema": happy_et["data_schema"],
    }


@pytest.mark.backend_sensitive
async def test_segments_report_the_stored_range(api, prefilled_topic):
    """feature 0003 / 0007 §2.6 - `query` reports the stored range.

    TOPIC_PREFILLED holds exactly the seeded events, so the manifest's range is
    theirs: from the first seeded event's sequence and storage time to the
    last's. The `segments` entries are the backend's own shape and stay opaque.
    """
    first, last = prefilled_topic.events[0], prefilled_topic.events[-1]
    async with api() as client:
        resp = await client.get(f"/topics/segments?topic={TOPIC_PREFILLED}&partition=0")
    assert resp.status_code == 200
    body = resp.json()
    assert body == {
        "topic": TOPIC_PREFILLED,
        "partition": 0,
        "start_sequence": first["sequence"],
        "end_sequence": last["sequence"],
        "start_time": body["start_time"],
        "end_time": body["end_time"],
        "segments": body["segments"],
    }
    assert _instant(body["start_time"]) == _instant(first["sequence_time"])
    assert _instant(body["end_time"]) == _instant(last["sequence_time"])


async def test_segments_for_a_partition_past_the_count_returns_400(api):
    """feature 0003 - a partition at or above the topic's count is InvalidPartition."""
    async with api() as client:
        resp = await client.get(f"/topics/segments?topic={TOPIC_PREFILLED}&partition=1")
    assert resp.status_code == 400
    assert resp.json() == {
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "InvalidPartition: partition must be in [0, 1) for this topic",
        "instance": "/event-broker/v1/topics/segments",
        "trace_id": TRACE_ID,
        "context": {
            "format": "InvalidPartition: partition must be in [0, 1) for this topic",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    }


def _instant(text: str) -> datetime:
    return datetime.fromisoformat(text.replace("Z", "+00:00"))
