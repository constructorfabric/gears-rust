//! Standalone broker integration tests over HTTP with a separate flight-control Core.
//! Each case supplies its own request bodies and response expectations.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use std::time::Duration;

use serde_json::{Value, json};
use uuid::Uuid;

use common::{EventLog, OWNER_SUBJECT, OWNER_TENANT, SseFrameReader, Streaming, TestServer};

const OCCURRED_AT: &str = "2026-01-01T00:00:00Z";

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

    let group_resp = server
        .post("/event-broker/v1/consumer-groups")
        .send()
        .await
        .expect("create consumer group");
    assert_eq!(group_resp.status(), 201);
    let group = group_resp.json::<Value>().await.expect("group json");
    let group_id = group["id"].as_str().expect("group id").to_owned();
    assert_eq!(
        group,
        json!({
            "id": group_id,
            "kind": "anonymous",
            "tenant": OWNER_TENANT,
            "owner_principal": OWNER_SUBJECT,
            "description": null,
            "created_at": group["created_at"].as_str().expect("created_at"),
        })
    );

    let tenant_id = OWNER_TENANT;
    let sub_resp = server
        .post("/event-broker/v1/subscriptions")
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

    let seek_resp = server
        .post(&format!("/event-broker/v1/subscriptions/{sub_id}:seek"))
        .json(&json!({
            "topology_version": 1, "positions": { topic: [{ "partition": 0, "value": "earliest" }] }
        }))
        .send()
        .await
        .expect("seek");
    assert_eq!(seek_resp.status(), 200);

    let stream_resp = server
        .open_stream(&format!(
            "/event-broker/v1/events:sse?subscription={sub_id}"
        ))
        .await;
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

    let event_id = Uuid::from_u128(0xe001);
    let publish_resp = server
        .publish_retrying_while_unavailable(&json!({
            "id": event_id,
            "type": event_type,
            "subject_tenant": tenant_id,
            "source": "integration-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": OCCURRED_AT,
        }))
        .await;
    assert_eq!(publish_resp.status(), 202);
    assert_eq!(
        publish_resp.text().await.expect("publish body"),
        "",
        "202 Accepted must carry no body"
    );

    let (event_kind, event_data) = reader.next_frame(Duration::from_secs(5)).await;
    assert_eq!(event_kind, "event");
    let sequence_time = event_data["payload"]["sequence_time"]
        .as_str()
        .expect("server assigned sequence_time");
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
                "occurred_at": OCCURRED_AT,
                "trace_parent": null,
                "data": null,
                "partition": 0,
                "sequence": 1,
                "sequence_time": sequence_time,
            },
        })
    );
    drop(reader);
    server.finish().await;
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

    let group_resp = server
        .post("/event-broker/v1/consumer-groups")
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
    assert_eq!(
        group_body_before_restart,
        json!({
            "id": group_id,
            "kind": "anonymous",
            "tenant": OWNER_TENANT,
            "owner_principal": OWNER_SUBJECT,
            "description": null,
            "created_at": group_body_before_restart["created_at"].as_str().expect("created_at"),
        })
    );

    let tenant_id = OWNER_TENANT;
    let sub1_resp = server.post("/event-broker/v1/subscriptions")
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

    server
        .post(&format!("/event-broker/v1/subscriptions/{sub1_id}:seek"))
        .json(&json!({
            "topology_version": 1, "positions": { topic: [{ "partition": 0, "value": "earliest" }] }
        }))
        .send()
        .await
        .expect("seek sub1")
        .error_for_status()
        .expect("seek sub1 must succeed");

    let stream1_resp = server
        .open_stream(&format!(
            "/event-broker/v1/events:sse?subscription={sub1_id}"
        ))
        .await;
    assert_eq!(stream1_resp.status(), 200);
    let mut reader1 = SseFrameReader::new(stream1_resp);
    let (topology1_kind, _) = reader1.next_frame(Duration::from_secs(5)).await;
    assert_eq!(topology1_kind, "topology");

    let event_id = Uuid::from_u128(0xe002);
    let publish_resp = server
        .publish_retrying_while_unavailable(&json!({
            "id": event_id,
            "type": event_type,
            "subject_tenant": tenant_id,
            "source": "integration-test",
            "subject": "s1",
            "subject_type": subject_type,
            "occurred_at": OCCURRED_AT,
        }))
        .await;
    assert_eq!(publish_resp.status(), 202);

    // SEEK leaves a durable cursor for the fresh subscription to reuse.
    let (event1_kind, event1_data) = reader1.next_frame(Duration::from_secs(5)).await;
    assert_eq!(event1_kind, "event");
    assert_eq!(event1_data["payload"]["id"], event_id.to_string());
    assert_eq!(event1_data["payload"]["sequence"], 1);
    drop(reader1);

    server.restart().await;

    let group_resp_after = server
        .get(&format!("/event-broker/v1/consumer-groups/{group_id}"))
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

    // The replacement loses the coordinator's in-memory subscription.
    let sub1_resp_after = server
        .get(&format!("/event-broker/v1/subscriptions/{sub1_id}"))
        .send()
        .await
        .expect("get subscription 1 after restart");
    assert_eq!(
        sub1_resp_after.status(),
        404,
        "a subscription must not survive a restart (subscriptions live in the coordinator memory)"
    );

    // Opening without SEEK proves the first subscription's cursor survives.
    let sub2_resp = server.post("/event-broker/v1/subscriptions")
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

    let stream2_resp = server
        .open_stream(&format!(
            "/event-broker/v1/events:sse?subscription={sub2_id}"
        ))
        .await;
    assert_eq!(
        stream2_resp.status(),
        200,
        "an existing Cursor row must let a fresh subscription stream without seeking"
    );
    let mut reader2 = SseFrameReader::new(stream2_resp);
    let (topology2_kind, _) = reader2.next_frame(Duration::from_secs(5)).await;
    assert_eq!(topology2_kind, "topology");

    // The fresh subscription starts at offset zero, so it replays the retained event.
    let (event2_kind, event2_data) = reader2.next_frame(Duration::from_secs(5)).await;
    assert_eq!(event2_kind, "event");
    assert_eq!(event2_data["payload"]["id"], event_id.to_string());
    assert_eq!(event2_data["payload"]["sequence"], 1);
    drop(reader2);
    server.finish().await;
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

    let producer_resp = server
        .post("/event-broker/v1/producers")
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
        "id": Uuid::from_u128(0xe003),
        "type": event_type,
        "subject_tenant": OWNER_TENANT,
        "source": "integration-test",
        "subject": "s1",
        "subject_type": subject_type,
        "occurred_at": OCCURRED_AT,
        "meta": { "version": 1, "producer": producer_id, "previous": 0, "sequence": 1 },
    });

    let first = server.publish_retrying_while_unavailable(&body).await;
    let first_status = first.status();
    let first_text = first.text().await.expect("first body");
    assert_eq!(first_status, 202, "body: {first_text}");

    // A producer can resubmit the same chained event after losing its response.
    let second = server.publish_retrying_while_unavailable(&body).await;
    assert_eq!(second.status(), 200);
    let second_text = second.text().await.expect("second body");
    assert_eq!(
        first_text, second_text,
        "both submissions must return an identical response body"
    );
    assert_eq!(first_text, "", "202 Accepted must carry no body");

    // The outbox drains asynchronously, so wait for the event's sequence to appear.
    let mut segments = json!(null);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let resp = server
            .get(&format!(
                "/event-broker/v1/topics/segments?topic={topic}&partition=0"
            ))
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
    server.finish().await;
}

#[tokio::test]
async fn consumer_group_listing_paginates_against_the_real_sqlite_backend() {
    let server = TestServer::start(vec![]).await;

    let mut created = Vec::new();
    for _ in 0..3 {
        let resp = server
            .post("/event-broker/v1/consumer-groups")
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

    let page1 = server
        .get("/event-broker/v1/consumer-groups?limit=2")
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

    let page2 = server
        .get(&format!(
            "/event-broker/v1/consumer-groups?limit=2&cursor={next_cursor}"
        ))
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

    let back = server
        .get(&format!(
            "/event-broker/v1/consumer-groups?limit=2&cursor={prev_cursor}"
        ))
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
    server.finish().await;
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

    let group_resp = server
        .post("/event-broker/v1/consumer-groups")
        .send()
        .await
        .expect("create consumer group");
    assert_eq!(group_resp.status(), 201);
    let group = group_resp.json::<Value>().await.expect("group json");
    let group_id = group["id"].as_str().expect("group id").to_owned();
    assert_eq!(
        group,
        json!({
            "id": group_id,
            "kind": "anonymous",
            "tenant": OWNER_TENANT,
            "owner_principal": OWNER_SUBJECT,
            "description": null,
            "created_at": group["created_at"].as_str().expect("created_at"),
        })
    );

    let tenant_id = OWNER_TENANT;
    let sub_resp = server
        .post("/event-broker/v1/subscriptions")
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

    let seek_resp = server
        .post(&format!("/event-broker/v1/subscriptions/{sub_id}:seek"))
        .json(&json!({
            "topology_version": 1, "positions": { topic: [{ "partition": 0, "value": "earliest" }] }
        }))
        .send()
        .await
        .expect("seek");
    assert_eq!(seek_resp.status(), 200);

    let stream_resp = server
        .open_stream(&format!(
            "/event-broker/v1/events:sse?subscription={sub_id}"
        ))
        .await;
    assert_eq!(stream_resp.status(), 200);
    let opened = tokio::time::Instant::now();
    let mut reader = SseFrameReader::new(stream_resp);

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

    let event_id = Uuid::from_u128(0xe004);
    let publish = async {
        tokio::time::sleep_until(opened + PUBLISH_AFTER).await;
        server
            .publish_retrying_while_unavailable(&json!({
                "id": event_id,
                "type": event_type,
                "subject_tenant": tenant_id,
                "source": "integration-test",
                "subject": "s1",
                "subject_type": subject_type,
                "occurred_at": OCCURRED_AT,
            }))
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
    let delivered = non_heartbeat
        .iter()
        .find(|(kind, _)| kind == "event")
        .map_or(Value::Null, |(_, data)| data.clone());
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
                        "occurred_at": OCCURRED_AT,
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
    drop(reader);
    server.finish().await;
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
    assert_eq!(refused.status.code(), Some(1));
}
