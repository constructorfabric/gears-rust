//! Full-process integration tests (design.md D8, task group 10): boots the
//! real, compiled `cf-gears-event-broker-server` binary as a subprocess
//! (`tests/common`) and drives it over real HTTP - the platform's actual
//! `pre_init`/`init`/`post_init`/start-phase lifecycle runs for real here,
//! unlike `EventBrokerHarness` (`src/test_support/harness.rs`), which stubs
//! the type registry and never boots that lifecycle at all. Every request
//! body sent and response body asserted is inlined per test, matching this
//! crate's established convention (`streaming_tests.rs`'s module doc
//! comment) - no shared request/fixture builders hiding what's actually
//! sent or checked.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use std::time::Duration;

use serde_json::{Value, json};
use uuid::Uuid;

use common::{EventLog, SseFrameReader, Streaming, TestServer};

/// The bound on each request a test makes, so a server that stops answering
/// fails the test instead of hanging it.
const REQUEST_BOUND: Duration = Duration::from_secs(10);

#[tokio::test]
async fn publish_then_consume_happy_path() {
    let topic = "gts.cf.core.events.topic.v1~x.eb.t10.topic.v1";
    let event_type = "gts.cf.core.events.event.v1~x.eb.t10.foo.v1~";
    let subject_type = "gts.x.eb.t10.subject.v1~";
    let server = TestServer::start(vec![
        json!({ "id": topic, "partitions": 1, }),
        json!({
            "id": event_type,
            "topic_id": topic,
            "allowed_subject_types": [subject_type],
            "data_schema": {},
        }),
    ])
    .await;
    let client = reqwest::Client::new();

    let group_resp = client
        .post(server.url("/event-broker/v1/consumer-groups"))
        .send()
        .await
        .expect("create consumer group");
    assert_eq!(group_resp.status(), 201);
    let group_id = group_resp
        .json::<serde_json::Value>()
        .await
        .expect("group json")["id"]
        .as_str()
        .expect("group id")
        .to_owned();

    let tenant_id = Uuid::new_v4();
    let sub_resp = client
        .post(server.url("/event-broker/v1/subscriptions"))
        .json(&json!({
            "consumer_group": group_id,
            "client_agent": "integration-test",
            "interests": [{
                "topic": topic,
                "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                "types": [event_type],
            }],
        }))
        .send()
        .await
        .expect("join subscription");
    assert_eq!(sub_resp.status(), 201);
    let sub_id = sub_resp
        .json::<serde_json::Value>()
        .await
        .expect("sub json")["id"]
        .as_str()
        .expect("sub id")
        .to_owned();

    let seek_resp = client
        .post(server.url(&format!("/event-broker/v1/subscriptions/{sub_id}:seek")))
        .json(&json!({
            "topology_version": 1, "positions": { topic: [{ "partition": 0, "value": "earliest" }] }
        }))
        .send()
        .await
        .expect("seek");
    assert_eq!(seek_resp.status(), 200);

    let stream_resp = client
        .get(server.url(&format!(
            "/event-broker/v1/events:sse?subscription={sub_id}"
        )))
        .send()
        .await
        .expect("open sse stream");
    assert_eq!(stream_resp.status(), 200);
    let mut reader = SseFrameReader::new(stream_resp);
    let (topology_kind, topology_data) = reader.next_frame(Duration::from_secs(5)).await;
    assert_eq!(topology_kind, "topology");
    assert_eq!(
        topology_data,
        json!({
            "kind": "topology",
            "topology_version": 1,
            "assigned": [{ "topic": topic, "partition": 0, "offset": 0, "last_examined": 0 }],
        })
    );

    let event_id = Uuid::new_v4();
    let occurred_at = chrono::Utc::now().to_rfc3339();
    let publish_resp = common::publish_retrying_while_unavailable(
        &client,
        &server.url("/event-broker/v1/events"),
        &json!({
            "id": event_id,
            "type": event_type,
            "subject_tenant": tenant_id,
            "source": "integration-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": occurred_at,
        }),
    )
    .await;
    assert_eq!(publish_resp.status(), 202);
    assert_eq!(
        publish_resp.text().await.expect("publish body"),
        "",
        "202 Accepted must carry no body"
    );

    let (event_kind, event_data) = reader.next_frame(Duration::from_secs(5)).await;
    assert_eq!(event_kind, "event");
    // `occurred_at`/`sequence_time` are round-tripped through the server
    // rather than assumed byte-identical to what was sent - chrono's serde
    // `DateTime<Utc>` serialization normalizes to a `Z`-suffixed RFC 3339
    // string, which differs syntactically (same instant) from
    // `to_rfc3339()`'s `+00:00` suffix; `sequence_time` is server-generated
    // and has no caller-supplied value to compare against at all (matching
    // `topics_tests.rs`'s established `created_at` convention).
    let round_tripped_occurred_at = event_data["payload"]["occurred_at"].clone();
    let sequence_time = event_data["payload"]["sequence_time"].clone();
    assert_eq!(
        event_data,
        json!({
            "kind": "event",
            "payload": {
                "id": event_id,
                "type": event_type,
                "subject_tenant": tenant_id,
                "source": "integration-test",
                "subject": "s1",
                "subject_type": subject_type,
                "occurred_at": round_tripped_occurred_at,
                "trace_parent": null,
                "data": null,
                "partition": 0,
                "sequence": 1,
                "sequence_time": sequence_time,
            },
        })
    );
}

/// The event log is on the `SQLite` backend plugin, so the events survive the
/// restart along with the gear's own database.
#[tokio::test]
async fn restart_preserves_events_and_consumer_groups_but_not_subscriptions() {
    let topic = "gts.cf.core.events.topic.v1~x.eb.t10r.topic.v1";
    let event_type = "gts.cf.core.events.event.v1~x.eb.t10r.foo.v1~";
    let subject_type = "gts.x.eb.t10r.subject.v1~";
    let mut server = TestServer::start_with_event_log(
        vec![
            json!({ "id": topic, "partitions": 1, }),
            json!({
                "id": event_type,
                "topic_id": topic,
                "allowed_subject_types": [subject_type],
                "data_schema": {},
            }),
        ],
        EventLog::Sqlite,
    )
    .await;
    let client = reqwest::Client::new();

    let group_resp = client
        .post(server.url("/event-broker/v1/consumer-groups"))
        .send()
        .await
        .expect("create consumer group");
    assert_eq!(group_resp.status(), 201);
    let group_body_before_restart = group_resp
        .json::<serde_json::Value>()
        .await
        .expect("group json");
    let group_id = group_body_before_restart["id"]
        .as_str()
        .expect("group id")
        .to_owned();

    let tenant_id = Uuid::new_v4();
    let sub1_resp = client
        .post(server.url("/event-broker/v1/subscriptions"))
        .json(&json!({
            "consumer_group": group_id,
            "client_agent": "integration-test",
            "interests": [{ "topic": topic, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [event_type] }],
        }))
        .send()
        .await
        .expect("join subscription 1");
    assert_eq!(sub1_resp.status(), 201);
    let sub1_id = sub1_resp
        .json::<serde_json::Value>()
        .await
        .expect("sub1 json")["id"]
        .as_str()
        .expect("sub1 id")
        .to_owned();

    client
        .post(server.url(&format!("/event-broker/v1/subscriptions/{sub1_id}:seek")))
        .json(&json!({
            "topology_version": 1, "positions": { topic: [{ "partition": 0, "value": "earliest" }] }
        }))
        .send()
        .await
        .expect("seek sub1")
        .error_for_status()
        .expect("seek sub1 must succeed");

    let stream1_resp = client
        .get(server.url(&format!(
            "/event-broker/v1/events:sse?subscription={sub1_id}"
        )))
        .send()
        .await
        .expect("open sse stream 1");
    assert_eq!(stream1_resp.status(), 200);
    let mut reader1 = SseFrameReader::new(stream1_resp);
    let (topology1_kind, _) = reader1.next_frame(Duration::from_secs(5)).await;
    assert_eq!(topology1_kind, "topology");

    let event_id = Uuid::new_v4();
    let publish_resp = common::publish_retrying_while_unavailable(
        &client,
        &server.url("/event-broker/v1/events"),
        &json!({
            "id": event_id,
            "type": event_type,
            "subject_tenant": tenant_id,
            "source": "integration-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": chrono::Utc::now().to_rfc3339(),
        }),
    )
    .await;
    assert_eq!(publish_resp.status(), 202);

    // The SEEK above persisted the `Cursor` row for
    // `(consumer_group, topic, partition)` - the fact this test cares about
    // below; reading the event confirms the stream serves from it.
    let (event1_kind, event1_data) = reader1.next_frame(Duration::from_secs(5)).await;
    assert_eq!(event1_kind, "event");
    assert_eq!(event1_data["payload"]["id"], event_id.to_string());
    assert_eq!(event1_data["payload"]["sequence"], 1);
    drop(reader1);

    server.restart().await;

    // `ConsumerGroupRepo` is `SQLite`-backed (`Storage`'s durable
    // namespaces) - the exact same group, unchanged, must still be there.
    let group_resp_after = client
        .get(server.url(&format!("/event-broker/v1/consumer-groups/{group_id}")))
        .send()
        .await
        .expect("get consumer group after restart");
    assert_eq!(group_resp_after.status(), 200);
    assert_eq!(
        group_resp_after
            .json::<serde_json::Value>()
            .await
            .expect("group json after restart"),
        group_body_before_restart,
        "the consumer group must survive a restart unchanged"
    );

    // A subscription is held in the memory of its group's coordinator, so it
    // must NOT survive.
    let sub1_resp_after = client
        .get(server.url(&format!("/event-broker/v1/subscriptions/{sub1_id}")))
        .send()
        .await
        .expect("get subscription 1 after restart");
    assert_eq!(
        sub1_resp_after.status(),
        404,
        "a subscription must not survive a restart (subscriptions live in the coordinator memory)"
    );

    // A brand-new subscription under the SAME consumer group, deliberately
    // NOT seeked: `stream()` only rejects an unseeded partition
    // (`PositionsNotSet`, 409) when `find_cursor` finds nothing. Getting
    // `200` here - not 409 - is the proof the `Cursor` row the SEEK
    // persisted before the restart is still there.
    let sub2_resp = client
        .post(server.url("/event-broker/v1/subscriptions"))
        .json(&json!({
            "consumer_group": group_id,
            "client_agent": "integration-test",
            "interests": [{ "topic": topic, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [event_type] }],
        }))
        .send()
        .await
        .expect("join subscription 2");
    assert_eq!(sub2_resp.status(), 201);
    let sub2_id = sub2_resp
        .json::<serde_json::Value>()
        .await
        .expect("sub2 json")["id"]
        .as_str()
        .expect("sub2 id")
        .to_owned();

    let stream2_resp = client
        .get(server.url(&format!(
            "/event-broker/v1/events:sse?subscription={sub2_id}"
        )))
        .send()
        .await
        .expect("open sse stream 2 without seeking");
    assert_eq!(
        stream2_resp.status(),
        200,
        "an existing Cursor row must let a fresh subscription stream without seeking"
    );
    let mut reader2 = SseFrameReader::new(stream2_resp);
    let (topology2_kind, _) = reader2.next_frame(Duration::from_secs(5)).await;
    assert_eq!(topology2_kind, "topology");

    // The event itself - not just the cursor row - is still in the durable
    // storage backend: a fresh subscription's in-memory replay
    // cursor starts from its own join-time `assigned.offset` (always `0`
    // for a new JOIN, `domain/delivery.rs::join`), so it re-reads from the
    // beginning and must see the same event again.
    let (event2_kind, event2_data) = reader2.next_frame(Duration::from_secs(5)).await;
    assert_eq!(event2_kind, "event");
    assert_eq!(event2_data["payload"]["id"], event_id.to_string());
    assert_eq!(event2_data["payload"]["sequence"], 1);
}

#[tokio::test]
async fn producer_chain_sequence_resubmission_is_deduped_not_double_persisted() {
    let topic = "gts.cf.core.events.topic.v1~x.eb.t10p.topic.v1";
    let event_type = "gts.cf.core.events.event.v1~x.eb.t10p.foo.v1~";
    let subject_type = "gts.x.eb.t10p.subject.v1~";
    let server = TestServer::start(vec![
        json!({ "id": topic, "partitions": 1, }),
        json!({
            "id": event_type,
            "topic_id": topic,
            "allowed_subject_types": [subject_type],
            "data_schema": {},
        }),
    ])
    .await;
    let client = reqwest::Client::new();

    let producer_resp = client
        .post(server.url("/event-broker/v1/producers"))
        .json(&json!({ "mode": "chained", "client_agent": "integration-test" }))
        .send()
        .await
        .expect("register producer");
    assert_eq!(producer_resp.status(), 201);
    let producer_id = producer_resp
        .json::<serde_json::Value>()
        .await
        .expect("producer json")["id"]
        .as_str()
        .expect("producer id")
        .to_owned();

    let body = json!({
        "id": Uuid::new_v4(),
        "type": event_type,
        "subject_tenant": Uuid::new_v4(),
        "source": "integration-test",
        "subject": "s1",
        "subject_type": subject_type,
        "occurred_at": chrono::Utc::now().to_rfc3339(),
        "meta": { "version": 1, "producer": producer_id, "previous": 0, "sequence": 1 },
    });

    // Retried while unavailable, the way a real producer publishes: a `503`
    // is ambiguous, not fatal. A retry after one still lands on `202` because
    // nothing was enqueued - the guard that raises it runs before the write.
    let first = common::publish_retrying_while_unavailable(
        &client,
        &server.url("/event-broker/v1/events"),
        &body,
    )
    .await;
    // Body in the message: a failure shows the whole Problem. A 503 carries
    // the generic detail whatever its source (the ingest pipeline not yet
    // wired, a scope error, a DB failure); the server log names which.
    let first_status = first.status();
    let first_text = first.text().await.expect("first body");
    assert_eq!(first_status, 202, "body: {first_text}");

    // The exact same chained event, resubmitted (a real producer retrying
    // after e.g. a dropped response) - must be accepted (ignored), not
    // rejected as a chain-sequence violation, and must not be persisted a
    // second time.
    let second = common::publish_retrying_while_unavailable(
        &client,
        &server.url("/event-broker/v1/events"),
        &body,
    )
    .await;
    // A deduped resubmission is 200 OK (no new event persisted), distinct from
    // the 202 Accepted a fresh publish returns (ingest events.rs).
    assert_eq!(second.status(), 200);
    let second_text = second.text().await.expect("second body");
    assert_eq!(
        first_text, second_text,
        "both submissions must return an identical response body"
    );
    assert_eq!(first_text, "", "202 Accepted must carry no body");

    // Publish is asynchronous (ingest outbox drains in the background,
    // design.md D5) - poll briefly for `end_sequence` to settle, matching
    // `topics_tests.rs::get_topic_segments_happy_path_reflects_published_events`'s
    // established convention, rather than asserting on the very next
    // request.
    // Deadline rather than a fixed iteration count, matching this file's other
    // waits (5s frame timeouts, 15s `wait_for_healthy`): a loaded CI runner can
    // take seconds to drain the outbox, and that is not a failure.
    let mut segments = json!(null);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let resp = client
            .get(server.url(&format!(
                "/event-broker/v1/topics/segments?topic={topic}&partition=0"
            )))
            .send()
            .await
            .expect("get topic segments");
        assert_eq!(resp.status(), 200);
        segments = resp.json().await.expect("segments json");
        if segments["end_sequence"] == json!(1) || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        segments["end_sequence"],
        json!(1),
        "exactly one event must have drained from the outbox: {segments}"
    );
    assert_eq!(
        segments["segments"],
        json!([{ "start_sequence": 1, "end_sequence": 1, "event_count": 1 }]),
        "the duplicate submission must not have been persisted a second time: {segments}"
    );
}

#[tokio::test]
async fn consumer_group_listing_paginates_against_the_real_sqlite_backend() {
    let server = TestServer::start(vec![]).await;
    let client = reqwest::Client::new();

    let mut created = Vec::new();
    for _ in 0..3 {
        let resp = client
            .post(server.url("/event-broker/v1/consumer-groups"))
            .send()
            .await
            .expect("create consumer group");
        assert_eq!(resp.status(), 201);
        created.push(resp.json::<serde_json::Value>().await.expect("group json"));
    }
    let expected_by_id: std::collections::BTreeMap<String, serde_json::Value> = created
        .into_iter()
        .map(|g| (g["id"].as_str().expect("id").to_owned(), g))
        .collect();
    assert_eq!(expected_by_id.len(), 3);

    let page1 = client
        .get(server.url("/event-broker/v1/consumer-groups?limit=2"))
        .send()
        .await
        .expect("page 1");
    assert_eq!(page1.status(), 200);
    let body1 = page1.json::<serde_json::Value>().await.expect("page1 json");
    let items1 = body1["items"].as_array().expect("items1").clone();
    assert_eq!(
        items1.len(),
        2,
        "page 1 must carry exactly `limit` items: {body1}"
    );
    assert_eq!(body1["page_info"]["limit"], 2);
    assert!(body1["page_info"]["prev_cursor"].is_null());
    let next_cursor = body1["page_info"]["next_cursor"]
        .as_str()
        .expect("a third group remains, so page 1 must carry a next_cursor")
        .to_owned();

    let page2 = client
        .get(server.url(&format!(
            "/event-broker/v1/consumer-groups?limit=2&cursor={next_cursor}"
        )))
        .send()
        .await
        .expect("page 2");
    assert_eq!(page2.status(), 200);
    let body2 = page2.json::<serde_json::Value>().await.expect("page2 json");
    let items2 = body2["items"].as_array().expect("items2").clone();
    assert_eq!(
        items2.len(),
        1,
        "page 2 must carry the one remaining item: {body2}"
    );
    assert_eq!(body2["page_info"]["limit"], 2);
    assert!(body2["page_info"]["next_cursor"].is_null());
    let prev_cursor = body2["page_info"]["prev_cursor"]
        .as_str()
        .expect("page 1 exists, so page 2 must carry a prev_cursor")
        .to_owned();

    let mut seen = std::collections::BTreeSet::new();
    for item in items1.iter().chain(items2.iter()) {
        let id = item["id"].as_str().expect("item id");
        assert_eq!(
            item, &expected_by_id[id],
            "item for '{id}' did not match its exact create-time body"
        );
        assert!(
            seen.insert(id.to_owned()),
            "'{id}' appeared on more than one page"
        );
    }
    assert_eq!(seen, expected_by_id.keys().cloned().collect());

    let back = client
        .get(server.url(&format!(
            "/event-broker/v1/consumer-groups?limit=2&cursor={prev_cursor}"
        )))
        .send()
        .await
        .expect("walk prev_cursor");
    assert_eq!(back.status(), 200);
    let body_back = back.json::<serde_json::Value>().await.expect("back json");
    assert_eq!(
        body_back["items"], body1["items"],
        "walking prev_cursor from page 2 must return exactly page 1"
    );
    assert!(body_back["page_info"]["prev_cursor"].is_null());
}

/// A consume scope window of 5s, and a stream held for 12s: it stays open and
/// delivers only because its refresh worker renews the scope before each
/// window ends.
///
/// Margins, with a 5s TTL, a 1s evaluation deadline, a 1s heartbeat and one
/// interest group:
/// - The open's window ends about 5s after the JOIN. The JOIN stores its
///   consume answer in the decision memo for one TTL. The open gives its one
///   group (`k = 0`, so no stagger) a window of one TTL from the open, capped
///   by the memo entry it reuses (`ScopeExpiry::of_group`, `Remembered::cap`
///   in `domain/delivery.rs`). The open follows the JOIN by milliseconds.
/// - A `respect` group's refresh lead is one evaluation deadline plus 2s, so
///   3s (`StreamingConfig::refresh_lead`). A renewal starts 3s before its
///   group expires, cannot reuse the memo entry it replaces, and opens a new
///   5s window from its start, so renewals come about every 2s, each with 3s
///   to finish against a PDP that answers in milliseconds.
/// - Without a renewal the session ends the stream at the scope's expiry,
///   about 5s after the open, with a `terminal` control frame.
/// - The publish at 7s is 2s past that edge, and the 12s mark 7s past it. An
///   open stream owes a heartbeat for each idle second, so the 2s after the
///   mark always hold one.
#[tokio::test]
async fn a_stream_outlives_its_consume_scope_window_through_renewals() {
    const PUBLISH_AFTER: Duration = Duration::from_secs(7);
    const HELD_FOR: Duration = Duration::from_secs(12);
    const HEARTBEAT_WITHIN: Duration = Duration::from_secs(2);

    let topic = "gts.cf.core.events.topic.v1~x.eb.renew.topic.v1";
    let event_type = "gts.cf.core.events.event.v1~x.eb.renew.foo.v1~";
    let subject_type = "gts.x.eb.renew.subject.v1~";
    let server = TestServer::start_with_streaming(
        vec![
            json!({ "id": topic, "partitions": 1, }),
            json!({
                "id": event_type,
                "topic_id": topic,
                "allowed_subject_types": [subject_type],
                "data_schema": {},
            }),
        ],
        Streaming {
            heartbeat_secs: 1,
            authz_scope_ttl_secs: 5,
            authz_eval_deadline_secs: 1,
        },
    )
    .await;
    // The whole request, body included, is bounded; the stream is opened with
    // a client of its own, since its body never ends.
    let client = reqwest::Client::builder()
        .timeout(REQUEST_BOUND)
        .build()
        .expect("build client");
    let stream_client = reqwest::Client::new();

    let group_resp = client
        .post(server.url("/event-broker/v1/consumer-groups"))
        .send()
        .await
        .expect("create consumer group");
    assert_eq!(group_resp.status(), 201);
    let group_id = group_resp.json::<Value>().await.expect("group json")["id"]
        .as_str()
        .expect("group id")
        .to_owned();

    let tenant_id = Uuid::new_v4();
    let sub_resp = client
        .post(server.url("/event-broker/v1/subscriptions"))
        .json(&json!({
            "consumer_group": group_id,
            "client_agent": "integration-test",
            "interests": [{
                "topic": topic,
                "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                "types": [event_type],
            }],
        }))
        .send()
        .await
        .expect("join subscription");
    assert_eq!(sub_resp.status(), 201);
    let sub_id = sub_resp.json::<Value>().await.expect("sub json")["id"]
        .as_str()
        .expect("sub id")
        .to_owned();

    let seek_resp = client
        .post(server.url(&format!("/event-broker/v1/subscriptions/{sub_id}:seek")))
        .json(&json!({
            "topology_version": 1, "positions": { topic: [{ "partition": 0, "value": "earliest" }] }
        }))
        .send()
        .await
        .expect("seek");
    assert_eq!(seek_resp.status(), 200);

    let stream_resp = tokio::time::timeout(
        REQUEST_BOUND,
        stream_client
            .get(server.url(&format!(
                "/event-broker/v1/events:sse?subscription={sub_id}"
            )))
            .send(),
    )
    .await
    .expect("stream opens within the bound")
    .expect("open sse stream");
    assert_eq!(stream_resp.status(), 200);
    let opened = tokio::time::Instant::now();
    let mut reader = SseFrameReader::new(stream_resp);

    // Each frame and how long after the open it arrived, up to the first
    // heartbeat past `HELD_FOR`, the end of the stream, or the read deadline.
    let mut frames: Vec<(Duration, String, Value)> = Vec::new();
    let read_deadline = opened + HELD_FOR + HEARTBEAT_WITHIN;
    let read = async {
        // One deadline for the whole read: a heartbeat every second would
        // keep a bound per frame from ever firing.
        let _read_until_deadline = tokio::time::timeout_at(read_deadline, async {
            while let Some((kind, data)) = reader
                .try_next_frame(
                    read_deadline.saturating_duration_since(tokio::time::Instant::now()),
                )
                .await
            {
                let at = opened.elapsed();
                let held = kind == "heartbeat" && at >= HELD_FOR;
                frames.push((at, kind, data));
                if held {
                    break;
                }
            }
        })
        .await;
    };

    let event_id = Uuid::new_v4();
    let publish = async {
        tokio::time::sleep_until(opened + PUBLISH_AFTER).await;
        common::publish_retrying_while_unavailable(
            &client,
            &server.url("/event-broker/v1/events"),
            &json!({
                "id": event_id,
                "type": event_type,
                "subject_tenant": tenant_id,
                "source": "integration-test",
                "subject": "s1",
                "subject_type": subject_type,
                "occurred_at": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await
    };
    let ((), publish_resp) = tokio::join!(read, publish);
    assert_eq!(publish_resp.status(), 202);
    assert_eq!(
        publish_resp.text().await.expect("publish body"),
        "",
        "202 Accepted must carry no body"
    );

    let non_heartbeat: Vec<(String, Value)> = frames
        .iter()
        .filter(|(_, kind, _)| kind != "heartbeat")
        .map(|(_, kind, data)| (kind.clone(), data.clone()))
        .collect();
    // `occurred_at` is normalized by the server and `sequence_time` is set by
    // it, so both are taken from the frame (`Null` when no event arrived).
    let delivered = non_heartbeat
        .iter()
        .find(|(kind, _)| kind == "event")
        .map_or(Value::Null, |(_, data)| data.clone());
    let round_tripped_occurred_at = delivered["payload"]["occurred_at"].clone();
    let sequence_time = delivered["payload"]["sequence_time"].clone();
    assert_eq!(
        non_heartbeat,
        vec![
            (
                "topology".to_owned(),
                json!({
                    "kind": "topology",
                    "topology_version": 1,
                    "assigned": [{
                        "topic": topic,
                        "partition": 0,
                        "offset": 0,
                        "last_examined": 0,
                    }],
                })
            ),
            (
                "event".to_owned(),
                json!({
                    "kind": "event",
                    "payload": {
                        "id": event_id,
                        "type": event_type,
                        "subject_tenant": tenant_id,
                        "source": "integration-test",
                        "subject": "s1",
                        "subject_type": subject_type,
                        "occurred_at": round_tripped_occurred_at,
                        "trace_parent": null,
                        "data": null,
                        "partition": 0,
                        "sequence": 1,
                        "sequence_time": sequence_time,
                    },
                })
            ),
        ],
        "frames after the open: {frames:#?}"
    );
    assert!(
        frames
            .iter()
            .any(|(at, kind, _)| kind == "heartbeat" && *at >= HELD_FOR),
        "the stream must still be open {HELD_FOR:?} after the open: {frames:#?}"
    );
    assert!(
        !frames
            .iter()
            .any(|(_, kind, data)| kind == "control" && data["code"] == "terminal"),
        "the stream must not end: {frames:#?}"
    );
    let expired: Vec<String> = server
        .logs()
        .into_iter()
        .filter(|line| line.contains("consume scope expired"))
        .collect();
    assert_eq!(expired, Vec::<String>::new());
}

/// The startup check refuses a consume scope TTL that does not outlast the
/// longest refresh lead: 4s is not greater than 2 x 1s + 2s, so the process
/// exits at init with the check's message instead of serving.
#[tokio::test]
async fn a_server_whose_scope_ttl_is_too_short_refuses_to_start() {
    let refused = TestServer::start_refused(
        vec![],
        Streaming {
            heartbeat_secs: 5,
            authz_scope_ttl_secs: 4,
            authz_eval_deadline_secs: 1,
        },
    )
    .await;

    let message = concat!(
        "streaming: authz_scope_ttl_secs is 4 and authz_eval_deadline_secs is 1; ",
        "in standalone and cluster_delivery modes authz_scope_ttl_secs must be greater than ",
        "2 x authz_eval_deadline_secs + 2, so a stream's consume scope is refreshed ahead of ",
        "its expiry",
    );
    assert!(
        refused.logs.iter().any(|line| line.contains(message)),
        "the server's output must carry the check's message: {:#?}",
        refused.logs
    );
    assert!(
        refused.status.is_some_and(|status| !status.success()),
        "the server must exit with a failure status: {:?}",
        refused.status
    );
}
