//! `handlers/delivery/subscriptions.rs` coverage (`eb-rest-handlers` task
//! 11.5): JOIN validation matrix, authz enforcement, list, get, leave, seek.
//! `:stream`/`:sse` frame + lifecycle coverage lives in `streaming_tests.rs`
//! instead (split out from this file by concern).
//!
//! Every test inlines its own setup (seeding a topic, creating a consumer
//! group, joining a subscription) and asserts the exact response body
//! received - no shared setup helpers, so nothing about what a test sends,
//! or what it checks, is hidden outside the test itself. Dynamic
//! server-minted values (subscription id, consumer-group id, `created_at`)
//! are extracted from the response and reused to build the exact expected
//! body, rather than skipped.
//!
//! One test below still drives `harness.router()` directly with
//! `tower::ServiceExt` rather than `RequestCase::send()` (which eagerly
//! collects the *entire* response body via `axum::body::to_bytes` - fine for
//! ordinary JSON responses, but hangs on `:stream`'s intentionally
//! long-lived body): `stream_seek_and_leave_each_make_their_own_fresh_tenant_check`
//! needs a real `:stream` response to prove its tenant check runs on a live
//! stream open, matching `streaming_tests.rs`'s same raw-router pattern.

use std::sync::Arc;

use authz_resolver_sdk::{EvaluationRequest, PolicyEnforcer};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use serde_json::json;
use tokio_stream::StreamExt;
use toolkit_gts::GtsInstanceId;
use toolkit_security::pep_properties;
use tower::ServiceExt;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::{ConsumerGroup, ConsumerGroupKind};
use crate::domain::repo::{ConsumerGroupRepo, CursorRepo};
use crate::domain::streaming::lease::StreamLeases;
use crate::test_support::{
    DenyingAuthZ, EventBrokerHarness, Json, SpecQuery, SpecQueryHook, StaticTypesRegistry,
    mapping_lines,
};

// -- JOIN validation matrix --

#[tokio::test]
async fn join_happy_path_returns_201_with_full_assignment() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 3 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tenant_id = Uuid::new_v4();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": tenant_id,
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "filter": null,
                "unmatched_types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
            "assigned": [
                { "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partition": 0 },
                { "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partition": 1 },
                { "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partition": 2 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// A named consumer group (a valid consumer-group instance id whose suffix is
/// not a uuid) is recognized but not implemented yet, so JOIN rejects it with
/// 501 Unimplemented rather than the misleading 404 it would otherwise get.
/// Only anonymous groups are supported.
#[tokio::test]
async fn join_named_group_returns_501_unimplemented() {
    let harness = EventBrokerHarness::builder().build().await;

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": "gts.cf.core.events.consumer_group.v1~acme.eb.demo.audit.v1",
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(501);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unimplemented.v1~",
            "title": "Unimplemented",
            "status": 501,
            "detail": "named consumer groups are not implemented",
            "instance": "/event-broker/v1/subscriptions",
            "context": { "resource_type": "gts.cf.core.events.consumer_group.v1~" },
        })
    );
}

/// The tenant-traversal knobs are carried through JOIN and echoed, even though
/// fan-out does not expand them yet. Before they reached the domain model the
/// request accepted them and the conversion dropped them, so a caller asking
/// for unlimited descendants got `201` and no widening at all - a silent
/// mismatch between what was requested and what was stored.
#[tokio::test]
async fn join_carries_the_ownership_filter_through_to_the_echoed_interest() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tenant_id = Uuid::new_v4();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": tenant_id,
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
                "ownership_filter": "tenant_subtree_included",
                "barrier_mode": "ignore",
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": tenant_id,
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
                "ownership_filter": "tenant_subtree_included",
                "barrier_mode": "ignore",
                "filter": null,
                "unmatched_types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
            "assigned": [
                { "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// An interest that names no tenant and no ownership filter reads the caller's
/// mailbox: the JOIN resolves the tenant to the caller's and echoes both.
#[tokio::test]
async fn a_join_without_a_tenant_or_ownership_filter_echoes_the_callers_tenant_and_only_mine() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let caller_tenant = harness.security_context().subject_tenant_id();
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": caller_tenant,
                "ownership_filter": "only_mine",
                "barrier_mode": "respect",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
                "filter": null,
                "unmatched_types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
            "assigned": [
                { "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// `only_mine` reads the caller's own tenant, so naming another one is a
/// mistake the JOIN refuses rather than a wider read.
#[tokio::test]
async fn an_only_mine_interest_on_another_tenant_is_refused_with_invalid_tenant() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::from_u128(0x999),
                "ownership_filter": "only_mine",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "InvalidTenant: interest 0: only_mine reads the caller's own tenant",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "format": "InvalidTenant: interest 0: only_mine reads the caller's own tenant",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

#[tokio::test]
async fn join_rejects_an_unrecognised_barrier_mode() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
                "barrier_mode": "traverse",
            }],
        })))
        .send()
        .await;

    // An unknown enum variant fails at deserialization, before the handler
    // body runs, matching how `ProducerModeDto` rejects an unknown producer
    // mode. Upstream renders it as a canonical `Problem` that names neither
    // the offending field nor its value, so the status and that canonical
    // shape are the whole contract.
    resp.assert_status(422);
    assert_eq!(
        resp.json(),
        serde_json::json!({
            "type": "about:blank",
            "title": "Unprocessable Entity",
            "status": 422,
            "detail": "Unprocessable Entity",
            "instance": "/event-broker/v1/subscriptions",
            "context": {},
        })
    );
}

/// `max_depth` is gone: a body that still sends it, even as `null`, gets a
/// `400` naming the field, not the generic `422` of an unknown field.
#[tokio::test]
async fn a_join_that_sends_max_depth_is_refused_naming_it() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    for max_depth in [json!(0), json!(null)] {
        let resp = harness
            .api_v1()
            .post_subscriptions()
            .with_body(Json(&json!({
                "consumer_group": group,
                "client_agent": "test-agent",
                "interests": [{
                    "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                    "max_depth": max_depth,
                    "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
                }],
            })))
            .send()
            .await;

        resp.assert_status(400);
        assert_eq!(
            resp.json(),
            json!({
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
                "title": "Invalid Argument",
                "status": 400,
                "detail": "InvalidBody: 'max_depth' is not supported; use 'ownership_filter'",
                "instance": "/event-broker/v1/subscriptions",
                "context": {
                    "format": "InvalidBody: 'max_depth' is not supported; use 'ownership_filter'",
                    "resource_type": "gts.cf.core.events.request.v1~",
                },
            }),
            "max_depth {max_depth}"
        );
    }
}

#[tokio::test]
async fn join_unregistered_consumer_group_returns_404() {
    let harness = EventBrokerHarness::builder().build().await;

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": "gts.cf.core.events.consumer_group.v1~x.eb.t1.missing.v1",
            "client_agent": "test-agent",
            "interests": [],
        })))
        .send()
        .await;

    // The id here is a NAMED group (non-uuid suffix), so it now returns 501
    // (named groups not implemented) rather than the former misleading 404. An
    // unregistered ANONYMOUS group still returns 404 - see the anonymous tests.
    resp.assert_status(501);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unimplemented.v1~",
            "title": "Unimplemented",
            "status": 501,
            "detail": "named consumer groups are not implemented",
            "instance": "/event-broker/v1/subscriptions",
            "context": { "resource_type": "gts.cf.core.events.consumer_group.v1~" },
        })
    );
}

#[tokio::test]
async fn join_unregistered_topic_returns_404() {
    let harness = EventBrokerHarness::builder().build().await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.missing.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(404);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": "topic 'gts.cf.core.events.topic.v1~x.eb.missing.topic.v1' is not registered",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "resource_name": "gts.cf.core.events.topic.v1~x.eb.missing.topic.v1",
                "resource_type": "gts.cf.core.events.topic.v1~",
            },
        })
    );
}

/// A topic lookup that cannot be read is not an unregistered topic: the JOIN
/// is answered `503`, retryable, and leaves no subscription behind.
#[tokio::test]
#[tracing_test::traced_test]
async fn a_join_during_a_spec_cache_fault_answers_503() {
    const TENANT: Uuid = Uuid::from_u128(0x7e7a_0503);
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 3 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    harness.drop_metadata_table("event_broker_spec_cache").await;

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": TENANT, "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(503);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
            "title": "Service Unavailable",
            "status": 503,
            "detail": "Service temporarily unavailable",
            "instance": "/event-broker/v1/subscriptions",
            "context": {},
        })
    );
    let logged = std::cell::RefCell::new(Vec::new());
    logs_assert(|lines| {
        *logged.borrow_mut() = mapping_lines(lines);
        Ok(())
    });
    assert_eq!(
        logged.into_inner(),
        vec![
            "WARN answering 503 with the generic body error=storage backend unavailable: \
             database scope resolution failed: database error: Query Error: error returned \
             from database: (code: 1) no such table: event_broker_spec_cache"
                .to_owned()
        ]
    );
    let listed = harness.api_v1().get_subscriptions().send().await;
    listed.assert_status(200);
    assert_eq!(
        listed.json(),
        json!({
            "items": [],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

#[tokio::test]
async fn join_bad_type_pattern_returns_400() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            // Two wildcard segments - violates GTS §10 (at most one
            // wildcard segment may appear in a pattern).
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.*.foo.*"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "BadTypePattern: event type selector must be a valid GTS pattern",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "format": "BadTypePattern: event type selector must be a valid GTS pattern",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

// -- session_timeout parsing (toolkit_utils::iso8601_duration) --

/// The `session_timeout` the stored subscription actually carries - the
/// response does not expose it, so the parse is checked where it takes effect.
fn stored_session_timeout(
    harness: &EventBrokerHarness,
    body: &serde_json::Value,
) -> std::time::Duration {
    let id = Uuid::parse_str(body["id"].as_str().expect("id must be a string"))
        .expect("id must be a uuid");
    harness
        .groups()
        .get(id)
        .expect("the joined subscription must be stored")
        .session_timeout
}

#[tokio::test]
async fn join_without_session_timeout_defaults_to_30_seconds() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    assert_eq!(
        stored_session_timeout(&harness, &resp.json()),
        std::time::Duration::from_secs(30),
        "expected the 30s default session timeout"
    );
}

#[tokio::test]
async fn join_session_timeout_with_hour_designator_is_parsed_correctly() {
    // An hour designator must be honored in full, not silently defaulted to
    // the 30-second fallback.
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "session_timeout": "PT1H",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    assert_eq!(
        stored_session_timeout(&harness, &resp.json()),
        std::time::Duration::from_hours(1),
        "expected the full 1-hour session timeout, not the 30s default"
    );
}

#[tokio::test]
async fn join_malformed_session_timeout_returns_400_not_silently_defaulted() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "session_timeout": "garbage",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "InvalidSessionTimeout: session_timeout must be a valid ISO 8601 duration",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "format": "InvalidSessionTimeout: session_timeout must be a valid ISO 8601 duration",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

#[tokio::test]
async fn join_session_timeout_missing_pt_prefix_returns_400() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "session_timeout": "30S",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json()["detail"],
        "InvalidSessionTimeout: session_timeout must be a valid ISO 8601 duration"
    );
}

#[tokio::test]
async fn join_zero_session_timeout_returns_400() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "session_timeout": "PT0S",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json()["detail"],
        "InvalidSessionTimeout: session_timeout must be a positive ISO 8601 duration"
    );
}

#[tokio::test]
async fn join_fractional_session_timeout_is_stored_exactly() {
    // A value under one second keeps its fraction, as the SDK REST client sends it.
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "session_timeout": "PT0.5S",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    assert_eq!(
        stored_session_timeout(&harness, &resp.json()),
        std::time::Duration::from_millis(500),
        "expected the 500 ms session timeout, not a truncated or default one"
    );
}

#[tokio::test]
async fn join_session_timeout_that_parses_to_zero_returns_400() {
    // An empty fraction, and one whose only non-zero digit is past the ninth, both
    // parse to zero, which is refused like `PT0S`.
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    for session_timeout in ["PT0.S", "PT0.0000000001S"] {
        let resp = harness
            .api_v1()
            .post_subscriptions()
            .with_body(Json(&json!({
                "consumer_group": group,
                "client_agent": "test-agent",
                "session_timeout": session_timeout,
                "interests": [{
                    "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                    "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                    "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
                }],
            })))
            .send()
            .await;

        resp.assert_status(400);
        assert_eq!(
            resp.json()["detail"],
            "InvalidSessionTimeout: session_timeout must be a positive ISO 8601 duration",
            "session_timeout {session_timeout}"
        );
    }
}

#[tokio::test]
async fn join_negative_session_timeout_returns_400() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "session_timeout": "-PT30S",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json()["detail"],
        "InvalidSessionTimeout: session_timeout must be a valid ISO 8601 duration"
    );
}

#[tokio::test]
async fn join_session_timeout_overflow_returns_400_not_a_1_second_timeout() {
    // A value large enough to overflow the minutes-to-seconds conversion must
    // be a loud 400. Wrapping to "0" would clamp to a **1-second** session
    // timeout - a near-instant expiry accepted silently, the opposite of a
    // safe fallback.
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "session_timeout": format!("PT{}M", u64::MAX),
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json()["context"]["resource_type"],
        "gts.cf.core.events.request.v1~"
    );
}

// -- authz enforcement (`gears-rust#4516`, `eb-authz-enforcement`) --

#[tokio::test]
async fn join_rejected_for_missing_topic_consume_permission_returns_403() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            deny_if: |req: &EvaluationRequest| req.resource.properties.contains_key("topic_id"),
        })))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.topic.v1~",
            },
        })
    );
    assert_eq!(
        harness.api_v1().get_subscriptions().send().await.json()["items"],
        json!([]),
        "no subscription must be created for a denied JOIN"
    );
}

/// One interest with an exact and a wildcard pattern costs one consume call,
/// on the topic itself, naming both patterns and the interest's tenant. The
/// only other call is the group check, on the caller's anonymous group in the
/// caller's own tenant.
#[tokio::test]
async fn join_asks_consume_once_on_the_concrete_topic_with_its_patterns() {
    const SUBJECT: Uuid = Uuid::from_u128(0x0c0_0456);
    const CALLER_TENANT: Uuid = Uuid::from_u128(0x0c0_0123);
    const TARGET_TENANT: Uuid = Uuid::from_u128(0x0c0_0234);
    let asked = Arc::new(std::sync::Mutex::new(Vec::<EvaluationRequest>::new()));
    let recorder = Arc::clone(&asked);
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            deny_if: move |req: &EvaluationRequest| {
                recorder.lock().unwrap().push(req.clone());
                false
            },
        })))
        .with_security_context(
            toolkit_security::SecurityContext::builder()
                .subject_id(SUBJECT)
                .subject_tenant_id(CALLER_TENANT)
                .build()
                .unwrap(),
        )
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    asked.lock().unwrap().clear();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": TARGET_TENANT, "ownership_filter": "entire_tenant",
                "types": [
                    "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "gts.cf.core.events.event.v1~x.eb.t1.*",
                ],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let asked: Vec<serde_json::Value> = asked
        .lock()
        .unwrap()
        .iter()
        .map(|req| serde_json::to_value(req).unwrap())
        .collect();
    assert_eq!(
        asked,
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": "gts.cf.core.events.consumer_group.v1~",
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                    "properties": {
                        "topic_id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                        "owner_tenant_id": TARGET_TENANT,
                        "event_type_patterns": [
                            "gts.cf.core.events.event.v1~x.eb.t1.*",
                            "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                        ],
                        "event_types": [],
                        "event_subject_type_patterns": [],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TARGET_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
        ]
    );
}

/// Allows every request. The consume call on a topic (the one resource that
/// declares `event_type`) gets a scope that admits only `admitted`, as a PDP
/// that grants one event type on the topic answers; every other allow carries
/// the tenant constraint tr-authz answers.
struct GrantsOneEventType {
    admitted: &'static str,
}

#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for GrantsOneEventType {
    async fn evaluate(
        &self,
        _ctx: toolkit_security::PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, toolkit_canonical_errors::CanonicalError>
    {
        use authz_resolver_sdk::{Constraint, InPredicate, Predicate};
        let root = request
            .context
            .tenant_context
            .as_ref()
            .and_then(|tenant_context| tenant_context.root_id);
        let declares = |property: &str| {
            request
                .context
                .supported_properties
                .iter()
                .any(|declared| declared == property)
        };
        let mut predicates = Vec::new();
        if let Some(root) = root.filter(|_| declares(pep_properties::OWNER_TENANT_ID)) {
            predicates.push(Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                [root],
            )));
        }
        if declares("event_type") {
            predicates.push(Predicate::In(InPredicate::new(
                "event_type",
                [self.admitted],
            )));
        }
        Ok(authz_resolver_sdk::EvaluationResponse {
            decision: true,
            context: authz_resolver_sdk::EvaluationResponseContext {
                constraints: if predicates.is_empty() {
                    Vec::new()
                } else {
                    vec![Constraint { predicates }]
                },
                ..Default::default()
            },
        })
    }

    async fn evaluate_batch(
        &self,
        ctx: toolkit_security::PlatformSecurityContext,
        req: authz_resolver_sdk::EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, toolkit_canonical_errors::CanonicalError>
    {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
    }
}

#[tokio::test]
async fn join_rejected_for_missing_event_type_consume_permission_returns_403_naming_the_offending_type()
 {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(GrantsOneEventType {
            admitted: "gts.cf.core.events.event.v1~x.eb.t1.bar.v1~",
        })))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "calling principal lacks consume on event type \
                           'gts.cf.core.events.event.v1~x.eb.t1.foo.v1~'",
                "resource_type": "gts.cf.core.events.event_type.v1~",
            },
        })
    );
}

#[tokio::test]
async fn join_rejected_for_unauthorized_interest_tenant_id_returns_403() {
    let interest_tenant = Uuid::new_v4();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            // Denies every request naming the interest's tenant: the consume
            // call on the topic, then the tenant check that classifies its
            // denial. The group gate names the caller's own tenant and passes.
            deny_if: move |req: &EvaluationRequest| {
                req.resource.properties.get(pep_properties::OWNER_TENANT_ID)
                    == Some(&json!(interest_tenant))
            },
        })))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": interest_tenant, "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

#[tokio::test]
async fn join_rejected_for_a_caller_not_authorized_for_the_anonymous_groups_tenant() {
    // Empty `interests`, so the only tenant-scope check JOIN can possibly
    // make is its own group-level one (`eb-tenant-isolation-fix`) against
    // the anonymous group's tenant - distinct from
    // `join_rejected_for_unauthorized_interest_tenant_id_returns_403`,
    // which denies the interest's tenant instead.
    let harness = EventBrokerHarness::builder()
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            deny_if: |req: &EvaluationRequest| {
                req.resource
                    .properties
                    .contains_key(pep_properties::OWNER_TENANT_ID)
            },
        })))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [],
        })))
        .send()
        .await;

    resp.assert_status(403);
}

#[tokio::test]
async fn join_rejected_for_a_named_group_without_consume_permission() {
    let harness = EventBrokerHarness::builder()
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            deny_if: |req: &EvaluationRequest| {
                req.resource.properties.contains_key("consumer_group_id")
            },
        })))
        .build()
        .await;
    let group_id = "gts.cf.core.events.consumer_group.v1~x.eb.t1.namedgroup.v1";
    harness
        .repo()
        .create_consumer_group(ConsumerGroup {
            id: GtsInstanceId::try_new(group_id)
                .expect("test-seeded id must be a valid GTS instance id"),
            kind: ConsumerGroupKind::Named,
            tenant_id: Uuid::new_v4(),
            owner_principal_id: Uuid::new_v4(),
            description: None,
            created_at: Utc::now(),
        })
        .await
        .expect("seeding a named consumer group must not fail");

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group_id,
            "client_agent": "test-agent",
            "interests": [],
        })))
        .send()
        .await;

    // Named groups are not implemented for now, so the named guard short-circuits
    // to 501 before the authz check the seeded denying-policy would otherwise
    // trigger (403). The authz path stays exercised by anonymous-group tests.
    resp.assert_status(501);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unimplemented.v1~",
            "title": "Unimplemented",
            "status": 501,
            "detail": "named consumer groups are not implemented",
            "instance": "/event-broker/v1/subscriptions",
            "context": { "resource_type": "gts.cf.core.events.consumer_group.v1~" },
        })
    );
}

#[tokio::test]
async fn join_a_single_unauthorized_interest_blocks_the_whole_join() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            { "id": "gts.cf.core.events.topic.v1~x.eb.t2.topic.v1", "partitions": 1 },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            // The second interest's topic is denied; the first interest's
            // topic (and both interests' event-type checks) would pass.
            deny_if: |req: &EvaluationRequest| {
                req.resource.properties.get("topic_id")
                    == Some(&json!("gts.cf.core.events.topic.v1~x.eb.t2.topic.v1"))
            },
        })))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [
                {
                    "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                    "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                    "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
                },
                {
                    "topic": "gts.cf.core.events.topic.v1~x.eb.t2.topic.v1",
                    "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                    "types": ["gts.cf.core.events.event.v1~x.eb.t2.foo.v1~"],
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.topic.v1~",
            },
        })
    );
    assert_eq!(
        harness.api_v1().get_subscriptions().send().await.json()["items"],
        json!([]),
        "no subscription must be created for either interest - not even the one that would have passed"
    );
}

#[tokio::test]
async fn join_succeeds_when_its_group_passes_the_consume_check() {
    let interest_tenant_id = Uuid::new_v4();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            // Allows the consume call only on the exact topic this test joins
            // and in the interest's tenant, so the real values are checked,
            // not a hardcoded pass. The group gate names the caller's own
            // tenant and carries no `topic_id`; it is allowed as the only
            // other `consume`. `create_consumer_group`'s `define` check has
            // no resource properties, so it is allowed by action name.
            deny_if: move |req: &EvaluationRequest| {
                let allowed_topic = req.resource.properties.get("topic_id")
                    == Some(&json!("gts.cf.core.events.topic.v1~x.eb.t1.topic.v1"))
                    && req.resource.properties.get(pep_properties::OWNER_TENANT_ID)
                        == Some(&json!(interest_tenant_id));
                let allowed_gate = req.action.name == "consume"
                    && !req.resource.properties.contains_key("topic_id");
                let allowed_define = req.action.name == "define";
                !(allowed_topic || allowed_gate || allowed_define)
            },
        })))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": interest_tenant_id, "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": interest_tenant_id,
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "filter": null,
                "unmatched_types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
            "assigned": [
                { "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

// -- Unmatched types at JOIN --

const UNMATCHED_TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.unmatched.orders.v1";
const REGISTERED_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.unmatched.placed.v1~";
const UNREGISTERED_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.unmatched.refunded.v1~";
const BASE_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.unmatched.base.v1~";
const DERIVED_TYPE: &str =
    "gts.cf.core.events.event.v1~x.eb.unmatched.base.v1~x.eb.unmatched.derived.v1~";
const MATCHING_WILDCARD: &str = "gts.cf.core.events.event.v1~x.eb.unmatched.*";
const NOTHING_WILDCARD: &str = "gts.cf.core.events.event.v1~x.eb.nothing.*";
/// Holds `BASE_TYPE`, which a derived type needs registered to resolve.
const BASE_TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.unmatched.base_home.v1";
const UNMATCHED_TENANT: Uuid = Uuid::from_u128(0x0e5a_0001);

/// `UNMATCHED_TOPIC` with `REGISTERED_TYPE` and `DERIVED_TYPE`, well within
/// the candidate cap. `BASE_TYPE` is registered on `BASE_TOPIC`, not on it.
fn listed_registry() -> StaticTypesRegistry {
    StaticTypesRegistry::of(json!([
        { "id": UNMATCHED_TOPIC, "partitions": 1 },
        { "id": BASE_TOPIC, "partitions": 1 },
        { "id": REGISTERED_TYPE, "topic": UNMATCHED_TOPIC },
        { "id": BASE_TYPE, "topic": BASE_TOPIC },
        { "id": DERIVED_TYPE, "topic": UNMATCHED_TOPIC },
    ]))
}

/// `UNMATCHED_TOPIC` over the candidate cap: 257 filler types, plus
/// `REGISTERED_TYPE` and `DERIVED_TYPE`. `BASE_TYPE` is registered on
/// `BASE_TOPIC`, not on it.
fn over_cap_registry() -> StaticTypesRegistry {
    let mut spec = vec![
        json!({ "id": UNMATCHED_TOPIC, "partitions": 1 }),
        json!({ "id": BASE_TOPIC, "partitions": 1 }),
        json!({ "id": REGISTERED_TYPE, "topic": UNMATCHED_TOPIC }),
        json!({ "id": BASE_TYPE, "topic": BASE_TOPIC }),
        json!({ "id": DERIVED_TYPE, "topic": UNMATCHED_TOPIC }),
    ];
    spec.extend((0..257).map(|n| {
        json!({
            "id": format!("gts.cf.core.events.event.v1~x.eb.unmatched.filler_{n:03}.v1~"),
            "topic": UNMATCHED_TOPIC,
        })
    }));
    StaticTypesRegistry::of(serde_json::Value::Array(spec))
}

/// The second interest names a type the topic has not registered: the JOIN
/// succeeds and reports it in that interest's `unmatched_types`, after the
/// echoed interest fields; the first interest reports none.
#[tokio::test]
async fn join_reports_an_exact_type_that_is_not_registered() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(listed_registry())
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [
                { "topic": UNMATCHED_TOPIC, "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant", "types": [REGISTERED_TYPE] },
                { "topic": UNMATCHED_TOPIC, "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant", "types": [UNREGISTERED_TYPE] },
            ],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [
                {
                    "topic": UNMATCHED_TOPIC,
                    "subject_tenant": UNMATCHED_TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [REGISTERED_TYPE],
                    "filter": null,
                    "unmatched_types": [],
                },
                {
                    "topic": UNMATCHED_TOPIC,
                    "subject_tenant": UNMATCHED_TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [UNREGISTERED_TYPE],
                    "filter": null,
                    "unmatched_types": [UNREGISTERED_TYPE],
                },
            ],
            "assigned": [{ "topic": UNMATCHED_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
    assert_eq!(
        resp.text(),
        format!(
            r#"{{"id":"{id}","consumer_group":"{group}","client_agent":"test-agent","interests":[{{"topic":"{UNMATCHED_TOPIC}","subject_tenant":"{UNMATCHED_TENANT}","ownership_filter":"entire_tenant","barrier_mode":"respect","types":["{REGISTERED_TYPE}"],"filter":null,"unmatched_types":[]}},{{"topic":"{UNMATCHED_TOPIC}","subject_tenant":"{UNMATCHED_TENANT}","ownership_filter":"entire_tenant","barrier_mode":"respect","types":["{UNREGISTERED_TYPE}"],"filter":null,"unmatched_types":["{UNREGISTERED_TYPE}"]}}],"assigned":[{{"topic":"{UNMATCHED_TOPIC}","partition":0}}],"topology_version":1,"created_at":"{created_at}"}}"#
        ),
        "the wire keeps the echoed interest fields in order, `unmatched_types` last"
    );
}

/// A wildcard that matches none of the topic's registered types is reported;
/// one that matches a registered type is not.
#[tokio::test]
async fn join_reports_a_wildcard_that_matches_no_registered_type() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(listed_registry())
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant",
                "types": [NOTHING_WILDCARD, MATCHING_WILDCARD],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [NOTHING_WILDCARD, MATCHING_WILDCARD],
                "filter": null,
                "unmatched_types": [NOTHING_WILDCARD],
            }],
            "assigned": [{ "topic": UNMATCHED_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// A bare type id covers its derived types, so a base type the topic has
/// registered only as a derived type matches it and is not reported. Over
/// the candidate cap the same entry is reported, since there only the exact
/// id is looked up.
#[tokio::test]
async fn join_does_not_report_a_base_type_whose_derived_types_are_registered() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(listed_registry())
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant",
                "types": [BASE_TYPE],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [BASE_TYPE],
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": UNMATCHED_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

#[tokio::test]
async fn join_reports_only_the_unmatched_entries_of_an_interest() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(listed_registry())
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant",
                "types": [REGISTERED_TYPE, UNREGISTERED_TYPE],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [REGISTERED_TYPE, UNREGISTERED_TYPE],
                "filter": null,
                "unmatched_types": [UNREGISTERED_TYPE],
            }],
            "assigned": [{ "topic": UNMATCHED_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// The position in `interests` keys the report, also when interests on
/// different topics interleave and so fall in different consume groups.
#[tokio::test]
async fn join_reports_unmatched_types_by_request_position_across_topics() {
    const OTHER_TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.unmatched.other.v1";
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": UNMATCHED_TOPIC, "partitions": 1 },
            { "id": OTHER_TOPIC, "partitions": 1 },
            { "id": REGISTERED_TYPE, "topic": UNMATCHED_TOPIC },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [
                { "topic": UNMATCHED_TOPIC, "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant", "types": [UNREGISTERED_TYPE] },
                { "topic": OTHER_TOPIC, "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant", "types": [REGISTERED_TYPE] },
                { "topic": UNMATCHED_TOPIC, "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant", "types": [REGISTERED_TYPE] },
            ],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let mut body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    // The coordinator assigns topics in hash order.
    body["assigned"]
        .as_array_mut()
        .expect("assigned must be an array")
        .sort_by_key(|a| a["topic"].as_str().unwrap_or_default().to_owned());
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [
                {
                    "topic": UNMATCHED_TOPIC,
                    "subject_tenant": UNMATCHED_TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [UNREGISTERED_TYPE],
                    "filter": null,
                    "unmatched_types": [UNREGISTERED_TYPE],
                },
                {
                    "topic": OTHER_TOPIC,
                    "subject_tenant": UNMATCHED_TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [REGISTERED_TYPE],
                    "filter": null,
                    "unmatched_types": [REGISTERED_TYPE],
                },
                {
                    "topic": UNMATCHED_TOPIC,
                    "subject_tenant": UNMATCHED_TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [REGISTERED_TYPE],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": UNMATCHED_TOPIC, "partition": 0 },
                { "topic": OTHER_TOPIC, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

#[tokio::test]
async fn an_over_cap_join_reports_an_unregistered_exact_type() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(over_cap_registry())
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant",
                "types": [UNREGISTERED_TYPE],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [UNREGISTERED_TYPE],
                "filter": null,
                "unmatched_types": [UNREGISTERED_TYPE],
            }],
            "assigned": [{ "topic": UNMATCHED_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

#[tokio::test]
async fn an_over_cap_join_does_not_report_a_registered_exact_type() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(over_cap_registry())
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant",
                "types": [REGISTERED_TYPE],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [REGISTERED_TYPE],
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": UNMATCHED_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// Over the candidate cap the broker cannot tell what a wildcard matches, so
/// it never reports one, even one that matches no registered type.
#[tokio::test]
async fn an_over_cap_join_never_reports_a_wildcard() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(over_cap_registry())
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant",
                "types": [NOTHING_WILDCARD],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [NOTHING_WILDCARD],
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": UNMATCHED_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// Over the candidate cap an exact entry is looked up by its id alone, so a
/// base type registered only as a derived type is reported, where a topic
/// within the cap matches it against the derived type.
#[tokio::test]
async fn an_over_cap_join_reports_a_base_type_registered_only_as_derived_types() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(over_cap_registry())
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant",
                "types": [BASE_TYPE, DERIVED_TYPE],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [BASE_TYPE, DERIVED_TYPE],
                "filter": null,
                "unmatched_types": [BASE_TYPE],
            }],
            "assigned": [{ "topic": UNMATCHED_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// Over the candidate cap JOIN asks the spec cache which exact types are
/// registered; when that lookup cannot be read the JOIN answers `503`,
/// retryable, and leaves no subscription behind.
#[tokio::test]
#[tracing_test::traced_test]
async fn an_over_cap_join_whose_registered_type_lookup_fails_answers_503() {
    let hook = SpecQueryHook::new();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(over_cap_registry())
        .with_spec_query_hook(hook.clone())
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    hook.arm(|query| match query {
        SpecQuery::RegisteredEventTypes => Err(DomainError::StorageUnavailable {
            reason: "the registered-type lookup failed".to_owned(),
            source: None,
        }),
        SpecQuery::EventType | SpecQuery::EventTypeCandidates => Ok(()),
    });

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant",
                "types": [UNREGISTERED_TYPE],
            }],
        })))
        .send()
        .await;

    resp.assert_status(503);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
            "title": "Service Unavailable",
            "status": 503,
            "detail": "Service temporarily unavailable",
            "instance": "/event-broker/v1/subscriptions",
            "context": {},
        })
    );
    let logged = std::cell::RefCell::new(Vec::new());
    logs_assert(|lines| {
        *logged.borrow_mut() = mapping_lines(lines);
        Ok(())
    });
    assert_eq!(
        logged.into_inner(),
        vec![
            "WARN answering 503 with the generic body error=storage backend unavailable: \
             the registered-type lookup failed"
                .to_owned()
        ]
    );
    assert_eq!(hook.calls(SpecQuery::RegisteredEventTypes), 1);
    let listed = harness.api_v1().get_subscriptions().send().await;
    listed.assert_status(200);
    assert_eq!(
        listed.json(),
        json!({
            "items": [],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

/// `unmatched_types` belongs to the JOIN response only: a later read and the
/// listing echo the subscription without it.
#[tokio::test]
async fn get_and_list_do_not_carry_unmatched_types() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(listed_registry())
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let joined = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [
                { "topic": UNMATCHED_TOPIC, "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant", "types": [REGISTERED_TYPE] },
                { "topic": UNMATCHED_TOPIC, "subject_tenant": UNMATCHED_TENANT, "ownership_filter": "entire_tenant", "types": [UNREGISTERED_TYPE] },
            ],
        })))
        .send()
        .await;
    joined.assert_status(201);
    let id = joined.json()["id"].as_str().unwrap().to_owned();
    let created_at = joined.json()["created_at"].as_str().unwrap().to_owned();
    let subscription = json!({
        "id": id,
        "consumer_group": group,
        "client_agent": "test-agent",
        "interests": [
            {
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [REGISTERED_TYPE],
                "filter": null,
            },
            {
                "topic": UNMATCHED_TOPIC,
                "subject_tenant": UNMATCHED_TENANT,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [UNREGISTERED_TYPE],
                "filter": null,
            },
        ],
        "assigned": [{ "topic": UNMATCHED_TOPIC, "partition": 0 }],
        "topology_version": 1,
        "created_at": created_at,
    });

    let read = harness.api_v1().get_subscription(&id).send().await;
    read.assert_status(200);
    assert_eq!(read.json(), subscription);
    let listed = harness.api_v1().get_subscriptions().send().await;
    listed.assert_status(200);
    assert_eq!(
        listed.json(),
        json!({
            "items": [subscription],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

#[tokio::test]
async fn stream_seek_and_leave_each_make_their_own_fresh_tenant_check() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_for_double = Arc::clone(&calls);
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            // Group create makes 1 PEP call and `join()` 2 here: its own
            // group-level tenant-scope check against the anonymous group it
            // joins, plus one `consume` on the topic for its one group of
            // interests. `stream`/`seek`/`leave` each make one more of their
            // own (`find_authorized_subscription`) - never zero, and never a
            // reuse of JOIN's checks: each is its own fresh tenant-scope
            // check. Never denies, so every operation still succeeds; only
            // the call count matters.
            deny_if: move |_: &EvaluationRequest| {
                calls_for_double.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                false
            },
        })))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let join_resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;
    join_resp.assert_status(201);
    let id = join_resp.json()["id"].as_str().unwrap().to_owned();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);

    harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { ("gts.cf.core.events.topic.v1~x.eb.t1.topic.v1"): [{ "partition": 0, "value": "earliest" }] }
        })))
        .send()
        .await
        .assert_status(200);
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        4,
        "seek must make exactly one additional tenant check"
    );

    let request = Request::builder()
        .method("GET")
        .uri(format!("/event-broker/v1/events:stream?subscription={id}"))
        .body(Body::empty())
        .expect("request must build");
    let response = harness
        .router()
        .clone()
        .oneshot(request)
        .await
        .expect("the service itself must not error");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        5,
        "stream must make exactly one additional tenant check"
    );
    let mut stream = response.into_body().into_data_stream();
    tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("must receive the first frame within 2s")
        .expect("stream must not end before the first frame")
        .expect("first chunk must not be an error");
    drop(stream);

    let delete_resp = harness.api_v1().delete_subscription(&id).send().await;
    delete_resp.assert_status(204);

    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        6,
        "leave must make exactly one additional tenant check"
    );
}

/// Joins a subscription (with one real topic interest, so it has an
/// assigned partition a `seek` target can address) using a
/// `PolicyEnforcer` that allows exactly the 2 PEP calls setup makes - the
/// anonymous group's own `consumer_group:define` check
/// (`create_consumer_group`) and `join`'s group-level tenant-scope check
/// against that group (empty `interests`, so no consume checks) - and
/// denies every call after. Simulates a caller who was authorized to create
/// the group and subscription but is not authorized for its tenant on a
/// later call, the same technique
/// `stream_seek_and_leave_each_make_their_own_fresh_tenant_check` uses.
/// Returns the harness, the created subscription's id, and its consumer
/// group.
async fn harness_with_subscription_then_deny_further_tenant_checks()
-> (EventBrokerHarness, String, String) {
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            { "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~", "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1" },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            deny_if: move |_: &EvaluationRequest| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 2
            },
        })))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let join_resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            // Empty on purpose: an interest here would make its own
            // tenant-scope check during JOIN, which `deny_if` (denying any
            // tenant-scope check) would also catch, breaking JOIN itself.
            // `seek`'s target topic/partition doesn't need to be one of the
            // subscription's own interests (see `DeliveryServiceImpl::seek`),
            // so an empty interests list plus the seeded topic above is
            // enough for every test using this helper.
            "interests": [],
        })))
        .send()
        .await;
    join_resp.assert_status(201);
    let id = join_resp.json()["id"].as_str().unwrap().to_owned();
    (harness, id, group)
}

#[tokio::test]
async fn get_subscription_rejects_a_caller_from_a_different_tenant() {
    let (harness, id, _group) = harness_with_subscription_then_deny_further_tenant_checks().await;

    let resp = harness.api_v1().get_subscription(&id).send().await;

    resp.assert_status(403);
}

#[tokio::test]
async fn leave_rejects_a_caller_from_a_different_tenant() {
    let (harness, id, _group) = harness_with_subscription_then_deny_further_tenant_checks().await;

    let resp = harness.api_v1().delete_subscription(&id).send().await;

    resp.assert_status(403);
    assert!(
        harness
            .groups()
            .get(Uuid::parse_str(&id).unwrap())
            .is_some(),
        "a denied leave must not delete the subscription"
    );
}

#[tokio::test]
async fn seek_rejects_a_caller_from_a_different_tenant() {
    let (harness, id, group) = harness_with_subscription_then_deny_further_tenant_checks().await;
    let topic = toolkit_gts::GtsInstanceId::try_new("gts.cf.core.events.topic.v1~x.eb.t1.topic.v1")
        .unwrap();

    let resp = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": {
                "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1": [{
                    "partition": 0,
                    "value": "earliest",
                }],
            },
        })))
        .send()
        .await;

    resp.assert_status(403);
    assert!(
        harness
            .repo()
            .find_cursor(&GtsInstanceId::try_new(&group).unwrap(), &topic, 0)
            .await
            .expect("repo lookup must not fail")
            .is_none(),
        "a denied seek must not write a cursor"
    );
}

#[tokio::test]
async fn stream_rejects_a_caller_from_a_different_tenant() {
    let (harness, id, _group) = harness_with_subscription_then_deny_further_tenant_checks().await;

    let resp = harness.api_v1().get_events_stream(&id).send().await;

    resp.assert_status(403);
    assert!(
        !harness.leases().is_held(Uuid::parse_str(&id).unwrap()),
        "a denied stream open must not take the subscription's stream lease - \
         the lease is acquired after the authz check, so a 403 never reaches it"
    );
}

/// A harness whose REST caller is a subject of `tenant`, and a subscription
/// another subject of the same tenant joined in process, one interest on the
/// `x.eb.t1` topic. Returns the harness, the subscription id and its group.
async fn harness_with_a_subscription_joined_by_another_principal(
    tenant: Uuid,
) -> (EventBrokerHarness, Uuid, GtsInstanceId) {
    let topic = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": topic, "partitions": 1 },
        ])))
        .with_security_context(
            toolkit_security::SecurityContext::builder()
                .subject_id(Uuid::from_u128(0x3513))
                .subject_tenant_id(tenant)
                .build()
                .unwrap(),
        )
        .build()
        .await;
    let joiner = toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x3511))
        .subject_tenant_id(tenant)
        .build()
        .unwrap();
    let group = harness
        .delivery()
        .create_consumer_group(
            &joiner,
            crate::domain::model::ConsumerGroupCreateInput {
                client_agent: Some("joiner".to_owned()),
                description: None,
            },
        )
        .await
        .expect("the group is created")
        .id;
    let id = harness
        .delivery()
        .join(
            &joiner,
            crate::domain::delivery::JoinRequest {
                consumer_group: group.clone(),
                client_agent: "joiner".to_owned(),
                interests: vec![crate::domain::model::Interest {
                    topic: GtsInstanceId::try_new(topic).unwrap(),
                    subject_tenant: tenant,
                    ownership: crate::domain::model::OwnershipFilter::EntireTenant,
                    barrier_mode: crate::domain::model::BarrierMode::Respect,
                    types: vec![
                        gts::GtsIdPattern::try_new("gts.cf.core.events.event.v1~x.eb.t1.foo.v1~")
                            .unwrap(),
                    ],
                    subject_types: Vec::new(),
                    filter: None,
                }],
                session_timeout: std::time::Duration::from_secs(30),
            },
        )
        .await
        .expect("the JOIN is allowed")
        .subscription
        .id;
    (harness, id, group)
}

#[tokio::test]
async fn leave_by_another_principal_of_the_tenant_returns_403_subscription_not_owned() {
    let (harness, id, _group) =
        harness_with_a_subscription_joined_by_another_principal(Uuid::from_u128(0x3512)).await;

    let resp = harness
        .api_v1()
        .delete_subscription(&id.to_string())
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": format!("/event-broker/v1/subscriptions/{id}"),
            "context": {
                "reason": format!("subscription '{id}' was joined by another principal"),
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );
    harness
        .api_v1()
        .get_subscription(&id.to_string())
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn seek_by_another_principal_of_the_tenant_returns_403_subscription_not_owned() {
    let (harness, id, group) =
        harness_with_a_subscription_joined_by_another_principal(Uuid::from_u128(0x3512)).await;
    let topic = GtsInstanceId::try_new("gts.cf.core.events.topic.v1~x.eb.t1.topic.v1").unwrap();

    let resp = harness
        .api_v1()
        .post_subscription_seek(&id.to_string())
        .with_body(Json(&json!({
            "topology_version": 1,
            "positions": {
                "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1": [{
                    "partition": 0,
                    "value": "earliest",
                }],
            },
        })))
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "reason": format!("subscription '{id}' was joined by another principal"),
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );
    let cursor = harness
        .repo()
        .find_cursor(&group, &topic, 0)
        .await
        .expect("repo lookup must not fail");
    assert!(cursor.is_none(), "the refused seek wrote {cursor:?}");
}

#[tokio::test]
async fn list_subscriptions_excludes_a_different_tenant() {
    // `AllowAllAuthZ` (the harness default) can't distinguish tenants at
    // all, so this needs a `PolicyEnforcer` that actually denies one
    // specific foreign tenant by value while allowing everything else
    // (including the harness's own tenant-scope checks).
    let foreign_tenant_id = Uuid::new_v4();
    let harness = EventBrokerHarness::builder()
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            deny_if: move |req: &EvaluationRequest| {
                req.resource.properties.get(pep_properties::OWNER_TENANT_ID)
                    == Some(&json!(foreign_tenant_id.to_string()))
            },
        })))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let own = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(
            &json!({ "consumer_group": group, "client_agent": "own", "interests": [] }),
        ))
        .send()
        .await;
    own.assert_status(201);
    let own_id = own.json()["id"].as_str().unwrap().to_owned();

    // A subscription belonging to a different tenant, joined directly into
    // the coordinator (there's no REST path to create one under a tenant the
    // harness's own `ctx` doesn't hold - that's exactly the property this
    // test is checking).
    let foreign = crate::domain::model::Subscription {
        id: Uuid::new_v4(),
        tenant_id: foreign_tenant_id,
        joiner: crate::domain::model::Joiner {
            subject: Uuid::new_v4(),
            tenant: foreign_tenant_id,
            subject_type: None,
        },
        consumer_group: GtsInstanceId::try_new(&group).unwrap(),
        client_agent: "foreign".to_owned(),
        interests: vec![],
        topics: vec![],
        assigned: vec![],
        topology_version: 1,
        session_timeout: std::time::Duration::from_secs(30),
        created_at: Utc::now(),
    };
    harness.groups().join(foreign, &[]);

    let resp = harness.api_v1().get_subscriptions().send().await;

    resp.assert_status(200);
    let ids: Vec<String> = resp.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, vec![own_id]);
}

// -- list / get / leave --

#[tokio::test]
async fn list_subscriptions_happy_path_and_filter_by_consumer_group() {
    let harness = EventBrokerHarness::builder().build().await;
    let group_a = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let group_b = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let joined_a = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(
            &json!({ "consumer_group": group_a, "client_agent": "a", "interests": [] }),
        ))
        .send()
        .await;
    joined_a.assert_status(201);
    let joined_b = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(
            &json!({ "consumer_group": group_b, "client_agent": "b", "interests": [] }),
        ))
        .send()
        .await;
    joined_b.assert_status(201);

    let all = harness.api_v1().get_subscriptions().send().await;
    all.assert_status(200);
    let mut all_body = all.json();
    all_body["items"]
        .as_array_mut()
        .unwrap()
        .sort_by_key(|s| s["id"].as_str().unwrap().to_owned());
    let mut expected_items = vec![joined_a.json(), joined_b.json()];
    expected_items.sort_by_key(|s| s["id"].as_str().unwrap().to_owned());
    assert_eq!(
        all_body,
        json!({
            "items": expected_items,
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );

    let filtered = harness
        .api_v1()
        .get_subscriptions()
        .with_query("$filter", format!("consumer_group%20eq%20'{group_a}'"))
        .send()
        .await;
    filtered.assert_status(200);
    assert_eq!(
        filtered.json(),
        json!({
            "items": [joined_a.json()],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

#[tokio::test]
async fn get_subscription_happy_path_and_404() {
    let harness = EventBrokerHarness::builder().build().await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let joined = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(
            &json!({ "consumer_group": group, "client_agent": "a", "interests": [] }),
        ))
        .send()
        .await;
    let id = joined.json()["id"].as_str().unwrap().to_owned();

    let found = harness.api_v1().get_subscription(&id).send().await;
    found.assert_status(200);
    assert_eq!(found.json(), joined.json());

    let missing_id = Uuid::new_v4();
    let not_found = harness
        .api_v1()
        .get_subscription(&missing_id.to_string())
        .send()
        .await;
    not_found.assert_status(404);
    assert_eq!(
        not_found.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": format!("subscription '{missing_id}' does not exist"),
            "instance": format!("/event-broker/v1/subscriptions/{missing_id}"),
            "context": {
                "resource_name": missing_id.to_string(),
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );
}

#[tokio::test]
async fn leave_subscription_happy_path_and_404() {
    let harness = EventBrokerHarness::builder().build().await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let joined = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(
            &json!({ "consumer_group": group, "client_agent": "a", "interests": [] }),
        ))
        .send()
        .await;
    let id = joined.json()["id"].as_str().unwrap().to_owned();

    let delete_resp = harness.api_v1().delete_subscription(&id).send().await;
    delete_resp.assert_status(204);
    assert_eq!(delete_resp.text(), "", "204 No Content must carry no body");

    let get_after_delete = harness.api_v1().get_subscription(&id).send().await;
    get_after_delete.assert_status(404);
    assert_eq!(
        get_after_delete.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": format!("subscription '{id}' does not exist"),
            "instance": format!("/event-broker/v1/subscriptions/{id}"),
            "context": {
                "resource_name": id,
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );

    let missing_id = Uuid::new_v4();
    let delete_missing = harness
        .api_v1()
        .delete_subscription(&missing_id.to_string())
        .send()
        .await;
    delete_missing.assert_status(404);
    assert_eq!(
        delete_missing.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": format!("subscription '{missing_id}' does not exist"),
            "instance": format!("/event-broker/v1/subscriptions/{missing_id}"),
            "context": {
                "resource_name": missing_id.to_string(),
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );
}

// -- seek --

/// An exact offset inside the partition's admissible range is stored and
/// echoed back.
///
/// Six events are published first: the range a partition admits ends at the
/// highest sequence it has assigned, so `5` is only a position a cursor may
/// hold once there is something up there. This test asserted `200` against an
/// empty partition before the broker validated the range at all.
#[tokio::test]
async fn seek_exact_offset_returns_200_with_resolved_value() {
    let topic = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
    let tenant_id = Uuid::new_v4();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": topic, "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": topic,
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": topic,
                "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    publish_and_settle(&harness, &id, topic, tenant_id, 6).await;

    let resp = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { (topic): [{ "partition": 0, "value": 5 }] }
        })))
        .send()
        .await;

    resp.assert_status(200);
    assert_eq!(
        resp.json(),
        json!({ "positions": { (topic): [{ "partition": 0, "value": 5 }] } })
    );
}

#[tokio::test]
async fn seek_earliest_sentinel_resolves_to_zero_on_empty_partition() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { ("gts.cf.core.events.topic.v1~x.eb.t1.topic.v1"): [{ "partition": 0, "value": "earliest" }] }
        })))
        .send()
        .await;

    resp.assert_status(200);
    assert_eq!(
        resp.json(),
        json!({ "positions": { ("gts.cf.core.events.topic.v1~x.eb.t1.topic.v1"): [{ "partition": 0, "value": 0 }] } })
    );
}

/// A registered topic its backend does not hold yet reads as a partition
/// nothing was written to (feature 0007 §4.3): a consumer may join and seek
/// before any producer publishes, and before provisioning completes. An exact
/// position above 0 is out of the range `[0, 0]` such a partition admits.
#[tokio::test]
async fn seek_before_the_topic_is_provisioned_resolves_as_an_empty_partition() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .without_provisioning()
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let earliest = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { ("gts.cf.core.events.topic.v1~x.eb.t1.topic.v1"): [{ "partition": 0, "value": "earliest" }] }
        })))
        .send()
        .await;
    earliest.assert_status(200);
    assert_eq!(
        earliest.json(),
        json!({ "positions": { ("gts.cf.core.events.topic.v1~x.eb.t1.topic.v1"): [{ "partition": 0, "value": 0 }] } })
    );

    let above = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { ("gts.cf.core.events.topic.v1~x.eb.t1.topic.v1"): [{ "partition": 0, "value": 1 }] }
        })))
        .send()
        .await;
    above.assert_status(400);
    assert_eq!(
        above.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "resource_type": "gts.cf.core.events.request.v1~",
                "field_violations": [{
                    "field": "positions",
                    "description": "topic gts.cf.core.events.topic.v1~x.eb.t1.topic.v1 partition 0: \
                                    the seek position is above the valid range [0, 0]",
                    "reason": "above_high_water_mark",
                }],
            },
        })
    );
}

#[tokio::test]
async fn seek_invalid_sentinel_returns_400() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { ("gts.cf.core.events.topic.v1~x.eb.t1.topic.v1"): [{ "partition": 0, "value": "not-a-sentinel" }] }
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "InvalidSeekValue: 'not-a-sentinel' is not a valid SEEK value - expected \
                       an integer, \"earliest\", \"latest\", or \"at:<ISO-8601>\"",
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "format": "InvalidSeekValue: 'not-a-sentinel' is not a valid SEEK value - \
                           expected an integer, \"earliest\", \"latest\", or \"at:<ISO-8601>\"",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

#[tokio::test]
async fn seek_unassigned_partition_returns_409() {
    let topic = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": topic, "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": topic,
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // The subscription is assigned partition 0 (the topic's only partition).
    // Partition 1 is not in its assignment, so the seek is rejected before any
    // backend work - nothing is seeded.
    let resp = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { (topic): [{ "partition": 1, "value": "earliest" }] }
        })))
        .send()
        .await;

    resp.assert_status(409);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 409,
            "detail": "Operation precondition not met",
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "violations": [{
                    "type": "partition_not_assigned",
                    "subject": format!("{topic}:1"),
                    "description": format!(
                        "partition is not assigned to subscription {id} (topology_version=1)"
                    ),
                }],
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );
}

#[tokio::test]
async fn seek_stale_topology_version_returns_412() {
    let topic = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": topic, "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": topic,
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Partition 0 IS assigned, but the caller echoes a topology_version that no
    // longer matches the subscription's current one (as a concurrent rebalance
    // would leave it). The fence rejects the whole seek before the assignment
    // check, so this is told apart from a genuine partition_not_assigned. The
    // body is minimal: it carries no topology_version and no assignment - the
    // caller re-reads the subscription for the fresh state.
    let resp = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 999,
            "positions": { (topic): [{ "partition": 0, "value": "earliest" }] }
        })))
        .send()
        .await;

    resp.assert_status(412);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 412,
            "detail": "Operation precondition not met",
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "violations": [{
                    "type": "topology_version_mismatch",
                    "subject": id,
                    "description": "subscription topology changed; re-read the subscription and re-seek",
                }],
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );
}

#[tokio::test]
async fn seek_unknown_subscription_returns_404() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let missing_id = Uuid::new_v4();

    let resp = harness
        .api_v1()
        .post_subscription_seek(&missing_id.to_string())
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { ("gts.cf.core.events.topic.v1~x.eb.t1.topic.v1"): [{ "partition": 0, "value": 0 }] }
        })))
        .send()
        .await;

    resp.assert_status(404);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": format!("subscription '{missing_id}' does not exist"),
            "instance": format!("/event-broker/v1/subscriptions/{missing_id}:seek"),
            "context": {
                "resource_name": missing_id.to_string(),
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );
}

#[tokio::test]
async fn seek_unregistered_topic_returns_404() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { ("gts.cf.core.events.topic.v1~x.eb.missing.topic.v1"): [{ "partition": 0, "value": 0 }] }
        })))
        .send()
        .await;

    resp.assert_status(404);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": "topic 'gts.cf.core.events.topic.v1~x.eb.missing.topic.v1' is not registered",
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "resource_name": "gts.cf.core.events.topic.v1~x.eb.missing.topic.v1",
                "resource_type": "gts.cf.core.events.topic.v1~",
            },
        })
    );
}

/// Waits until the partition's high-water mark has reached `expected`, by
/// asking the broker to resolve `"latest"`.
///
/// Publishing is accepted with `202` and the sequence is assigned afterwards by
/// the ingest outbox, so a test that needs a known ceiling has to wait for one
/// rather than assume it. Nothing about a request or an assertion is hidden
/// here: this only decides *when* the test may proceed.
async fn await_hwm(harness: &EventBrokerHarness, subscription: &str, topic: &str, expected: i64) {
    for _ in 0..300 {
        let resp = harness
            .api_v1()
            .post_subscription_seek(subscription)
            .with_body(Json(&json!({
                "topology_version": 1, "positions": { (topic): [{ "partition": 0, "value": "latest" }] }
            })))
            .send()
            .await;
        if resp.json()
            == json!({ "positions": { (topic): [{ "partition": 0, "value": expected }] } })
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the high-water mark never reached {expected}");
}

/// Publishes `count` events onto partition 0 of the single-partition test
/// topic and returns once every one of them has a sequence.
async fn publish_and_settle(
    harness: &EventBrokerHarness,
    subscription: &str,
    topic: &str,
    tenant_id: Uuid,
    count: i64,
) {
    for n in 0..count {
        harness
            .api_v1()
            .post_events()
            .with_body(Json(&json!({
                "id": Uuid::new_v4(),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant_id,
                "source": "test-seek-range",
                "subject": format!("s{n}"),
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": chrono::Utc::now().to_rfc3339(),
            })))
            .send()
            .await
            .assert_status(202);
    }
    await_hwm(harness, subscription, topic, count).await;
}

/// The range's own edges: the floor and the ceiling are both positions a cursor
/// may hold, and one step outside either is not.
///
/// A populated partition that retention has not touched has a floor of `0` -
/// the position below its oldest surviving event - and a ceiling of the highest
/// sequence assigned.
#[tokio::test]
async fn seek_accepts_the_range_edges_and_rejects_one_step_outside_either() {
    let topic = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
    let tenant_id = Uuid::new_v4();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": topic, "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": topic,
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": topic,
                "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    publish_and_settle(&harness, &id, topic, tenant_id, 3).await;

    let floor = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { (topic): [{ "partition": 0, "value": 0 }] }
        })))
        .send()
        .await;
    floor.assert_status(200);
    assert_eq!(
        floor.json(),
        json!({ "positions": { (topic): [{ "partition": 0, "value": 0 }] } })
    );

    let ceiling = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { (topic): [{ "partition": 0, "value": 3 }] }
        })))
        .send()
        .await;
    ceiling.assert_status(200);
    assert_eq!(
        ceiling.json(),
        json!({ "positions": { (topic): [{ "partition": 0, "value": 3 }] } })
    );

    let below = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { (topic): [{ "partition": 0, "value": -1 }] }
        })))
        .send()
        .await;
    below.assert_status(400);
    assert_eq!(
        below.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "field_violations": [{
                    "field": "positions",
                    "description": format!("topic {topic} partition 0: the seek position is below the valid range [0, 3]"),
                    "reason": "below_retention_floor",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );

    let above = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { (topic): [{ "partition": 0, "value": 4 }] }
        })))
        .send()
        .await;
    above.assert_status(400);
    assert_eq!(
        above.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "field_violations": [{
                    "field": "positions",
                    "description": format!("topic {topic} partition 0: the seek position is above the valid range [0, 3]"),
                    "reason": "above_high_water_mark",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

/// A request naming several partitions applies all of them or none.
///
/// Asserted by reading the cursors afterwards rather than inferred from the
/// status code: a `400` with one cursor written would look identical from
/// outside.
#[tokio::test]
async fn seek_writes_no_cursor_at_all_when_one_partition_is_inadmissible() {
    let topic = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": topic, "partitions": 2 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": topic,
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Nothing has been published, so both partitions admit only `0`. The first
    // entry is admissible and the second is not.
    let resp = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": {
                (topic): [
                    { "partition": 0, "value": 0 },
                    { "partition": 1, "value": 5 },
                ]
            }
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "field_violations": [{
                    "field": "positions",
                    "description": format!("topic {topic} partition 1: the seek position is above the valid range [0, 0]"),
                    "reason": "above_high_water_mark",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );

    let topic_id = GtsInstanceId::try_new(topic).unwrap();
    let group_id = GtsInstanceId::try_new(&group).unwrap();
    for partition in [0, 1] {
        assert!(
            harness
                .repo()
                .find_cursor(&group_id, &topic_id, partition)
                .await
                .expect("repo lookup must not fail")
                .is_none(),
            "partition {partition} must have no cursor: the rejected request \
             applies nothing",
        );
    }
}

/// Every offending entry is reported, not just the first, so a caller
/// correcting several positions needs one round trip rather than one per
/// partition.
#[tokio::test]
async fn seek_reports_one_violation_per_offending_partition() {
    let topic = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": topic, "partitions": 3 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": topic,
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": {
                (topic): [
                    { "partition": 0, "value": 7 },
                    { "partition": 1, "value": 0 },
                    { "partition": 2, "value": -3 },
                ]
            }
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "field_violations": [
                    {
                        "field": "positions",
                        "description": format!("topic {topic} partition 0: the seek position is above the valid range [0, 0]"),
                        "reason": "above_high_water_mark",
                    },
                    {
                        "field": "positions",
                        "description": format!("topic {topic} partition 2: the seek position is below the valid range [0, 0]"),
                        "reason": "below_retention_floor",
                    },
                ],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

/// A JSON object may repeat a key, and a plain map keeps only its last value,
/// which would drop the earlier entries for that topic unchecked. The whole
/// seek is refused instead, and no cursor is written.
///
/// The body is a raw string because `json!` cannot express a repeated key.
#[tokio::test]
async fn seek_rejects_a_positions_object_that_names_a_topic_twice() {
    let topic = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": topic, "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": topic,
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_header(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        )
        .with_body(
            r#"{"topology_version":1,"positions":{"gts.cf.core.events.topic.v1~x.eb.t1.topic.v1":[{"partition":0,"value":0}],"gts.cf.core.events.topic.v1~x.eb.t1.topic.v1":[{"partition":0,"value":"earliest"}]}}"#,
        )
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "InvalidBody: `positions` names the same topic more than once",
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "format": "InvalidBody: `positions` names the same topic more than once",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );

    let topic_id = GtsInstanceId::try_new(topic).unwrap();
    let group_id = GtsInstanceId::try_new(&group).unwrap();
    assert!(
        harness
            .repo()
            .find_cursor(&group_id, &topic_id, 0)
            .await
            .expect("repo lookup must not fail")
            .is_none(),
        "the rejected request must write no cursor",
    );
}

/// Distinct topic keys in one body each keep their entries, and the response
/// lists them in sorted order whatever order the body names them in.
#[tokio::test]
async fn seek_accepts_distinct_topic_keys_in_one_request() {
    let t1 = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
    let t2 = "gts.cf.core.events.topic.v1~x.eb.t2.topic.v1";
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": t1, "partitions": 1 },
            { "id": t2, "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tenant_id = Uuid::new_v4();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [
                {
                    "topic": t1,
                    "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                    "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
                },
                {
                    "topic": t2,
                    "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                    "types": ["gts.cf.core.events.event.v1~x.eb.t2.foo.v1~"],
                },
            ],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // The body names `t2` before `t1`.
    let resp = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_header(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        )
        .with_body(
            r#"{"topology_version":1,"positions":{"gts.cf.core.events.topic.v1~x.eb.t2.topic.v1":[{"partition":0,"value":"latest"}],"gts.cf.core.events.topic.v1~x.eb.t1.topic.v1":[{"partition":0,"value":0}]}}"#,
        )
        .send()
        .await;

    resp.assert_status(200);
    assert_eq!(
        resp.text(),
        format!(
            r#"{{"positions":{{"{t1}":[{{"partition":0,"value":0}}],"{t2}":[{{"partition":0,"value":0}}]}}}}"#
        )
    );
}

/// A cursor names a position, not an event, and the position it names may be
/// one retention has erased.
///
/// Four events are published and the partition is truncated through sequence
/// 2. The floor is then 2 - the position below the oldest surviving event,
/// which is exactly the event that was removed - so a cursor may sit there,
/// and a stream opened on it delivers the events already stored above it.
#[tokio::test]
async fn a_cursor_may_sit_on_an_erased_position_and_delivery_resumes_above_it() {
    let topic = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
    let tenant_id = Uuid::new_v4();
    let memory = memory_event_broker_plugin::MemoryBackend::new();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": topic, "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": topic,
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .with_backend(std::sync::Arc::new(memory.clone()))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": topic,
                "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    publish_and_settle(&harness, &id, topic, tenant_id, 2).await;
    for n in 2..4 {
        harness
            .api_v1()
            .post_events()
            .with_body(Json(&json!({
                "id": Uuid::new_v4(),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant_id,
                "source": "test-erased-cursor",
                "subject": format!("s{n}"),
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": chrono::Utc::now().to_rfc3339(),
            })))
            .send()
            .await
            .assert_status(202);
    }
    await_hwm(&harness, &id, topic, 4).await;

    // Retention takes the two events of the first burst.
    memory
        .truncate(
            &event_broker_sdk::backend::TopicPartition::new(
                gts::GtsInstanceId::try_new(topic).unwrap(),
                event_broker_sdk::backend::Partition::new(0),
            ),
            event_broker_sdk::Sequence::assigned(2),
        )
        .expect("truncate must succeed");

    // 1 is now below the floor, and 2 - the erased event's own position - is
    // the floor itself.
    let below = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { (topic): [{ "partition": 0, "value": 1 }] }
        })))
        .send()
        .await;
    below.assert_status(400);
    assert_eq!(
        below.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "field_violations": [{
                    "field": "positions",
                    "description": format!("topic {topic} partition 0: the seek position is below the valid range [2, 4]"),
                    "reason": "below_retention_floor",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );

    let onto_erased = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { (topic): [{ "partition": 0, "value": 2 }] }
        })))
        .send()
        .await;
    onto_erased.assert_status(200);
    assert_eq!(
        onto_erased.json(),
        json!({ "positions": { (topic): [{ "partition": 0, "value": 2 }] } })
    );

    // And a stream opened on that cursor resumes at the next event still
    // stored, which is 3 - not at 2, and not at the start.
    let request = Request::builder()
        .method("GET")
        .uri(format!("/event-broker/v1/events:stream?subscription={id}"))
        .body(Body::empty())
        .expect("request must build");
    let response = harness
        .router()
        .clone()
        .oneshot(request)
        .await
        .expect("the service itself must not error");
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();

    let mut sequences: Vec<serde_json::Value> = Vec::new();
    for _ in 0..12 {
        let Ok(Some(Ok(chunk))) =
            tokio::time::timeout(std::time::Duration::from_secs(3), stream.next()).await
        else {
            break;
        };
        for part in String::from_utf8_lossy(&chunk).split("\r\n\r\n").skip(1) {
            let Ok(frame) =
                serde_json::from_str::<serde_json::Value>(part.trim_end_matches("\r\n"))
            else {
                continue;
            };
            if frame["kind"] == "event" {
                sequences.push(frame["payload"]["sequence"].clone());
            }
        }
        if sequences.len() == 2 {
            break;
        }
    }
    assert_eq!(
        sequences,
        vec![json!(3), json!(4)],
        "delivery resumes at the first stored event above the cursor"
    );
}

/// A partition retention has emptied admits only its ceiling.
///
/// There is no lower position that could be honoured, because no event remains
/// for delivery to resume from - so `earliest` and `latest` answer alike and
/// anything below is rejected. The ceiling is the highest sequence *assigned*,
/// which erasure never lowers.
#[tokio::test]
async fn an_emptied_partition_admits_only_its_ceiling() {
    let topic = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
    let tenant_id = Uuid::new_v4();
    let memory = memory_event_broker_plugin::MemoryBackend::new();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": topic, "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": topic,
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .with_backend(std::sync::Arc::new(memory.clone()))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": topic,
                "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    publish_and_settle(&harness, &id, topic, tenant_id, 3).await;

    // Retention empties the partition.
    memory
        .truncate(
            &event_broker_sdk::backend::TopicPartition::new(
                gts::GtsInstanceId::try_new(topic).unwrap(),
                event_broker_sdk::backend::Partition::new(0),
            ),
            event_broker_sdk::Sequence::assigned(3),
        )
        .expect("truncate must succeed");

    let ceiling = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { (topic): [{ "partition": 0, "value": 3 }] }
        })))
        .send()
        .await;
    ceiling.assert_status(200);
    assert_eq!(
        ceiling.json(),
        json!({ "positions": { (topic): [{ "partition": 0, "value": 3 }] } })
    );

    // `earliest` answers the same, because there is no earlier position left.
    let earliest = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { (topic): [{ "partition": 0, "value": "earliest" }] }
        })))
        .send()
        .await;
    earliest.assert_status(200);
    assert_eq!(
        earliest.json(),
        json!({ "positions": { (topic): [{ "partition": 0, "value": 3 }] } })
    );

    let below = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1, "positions": { (topic): [{ "partition": 0, "value": 2 }] }
        })))
        .send()
        .await;
    below.assert_status(400);
    assert_eq!(
        below.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "field_violations": [{
                    "field": "positions",
                    "description": format!("topic {topic} partition 0: the seek position is below the valid range [3, 3]"),
                    "reason": "below_retention_floor",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

#[tokio::test]
async fn second_join_on_same_group_reduces_first_subscription_and_increments_version() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 4 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tenant_id = Uuid::new_v4();

    let resp_a = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;
    resp_a.assert_status(201);
    let id_a = resp_a.json()["id"].as_str().unwrap().to_owned();
    assert_eq!(resp_a.json()["topology_version"], 1);
    assert_eq!(resp_a.json()["assigned"].as_array().unwrap().len(), 4);

    let resp_b = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "b",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;
    resp_b.assert_status(201);
    assert_eq!(resp_b.json()["topology_version"], 2);
    assert_eq!(resp_b.json()["assigned"].as_array().unwrap().len(), 2);

    // First subscription's DB record must reflect the reduced assignment.
    let get_a = harness.api_v1().get_subscription(&id_a).send().await;
    get_a.assert_status(200);
    assert_eq!(get_a.json()["topology_version"], 2);
    assert_eq!(get_a.json()["assigned"].as_array().unwrap().len(), 2);
}

// -- client_agent encoding, length and grammar ----------------------------

#[tokio::test]
async fn join_rejects_a_non_ascii_client_agent() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "agent\u{1}/1.0",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "field_violations": [{
                    "field": "client_agent",
                    "description": "must contain only printable ASCII (0x20-0x7E)",
                    "reason": "ascii_only",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

#[tokio::test]
async fn join_rejects_an_over_length_client_agent_before_parsing_session_timeout() {
    // `client_agent` is checked ahead of `session_timeout`, so a request that
    // breaks both reports the field violation rather than the duration parse.
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a".repeat(257),
            "session_timeout": "garbage",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "field_violations": [{
                    "field": "client_agent",
                    "description": "must be 1-256 bytes, got 257",
                    "reason": "field_too_long",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

#[tokio::test]
async fn join_rejects_a_client_agent_that_breaks_the_user_agent_grammar() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "agent/",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::new_v4(), "ownership_filter": "entire_tenant",
                "types": ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "field_violations": [{
                    "field": "client_agent",
                    "description": "must follow the RFC 9110 User-Agent grammar: \
                                    product *( SP ( product / comment ) )",
                    "reason": "user_agent_grammar",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let listed = harness.api_v1().get_subscriptions().send().await;
    listed.assert_status(200);
    assert_eq!(
        listed.json(),
        json!({
            "items": [],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        }),
        "a refused JOIN creates no subscription"
    );
}

// -- JOIN guards and limits (#5494) --

const GUARD_TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
const GUARD_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~";

type RecordedPdp = Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

/// A PDP that allows every request and records each one, so a test reads the
/// requests a JOIN asked; an empty list means no PDP call was made.
fn recording_pdp() -> (PolicyEnforcer, RecordedPdp) {
    let recorded = RecordedPdp::default();
    let sink = Arc::clone(&recorded);
    let enforcer = PolicyEnforcer::new(Arc::new(DenyingAuthZ {
        deny_if: move |req: &EvaluationRequest| {
            sink.lock()
                .unwrap()
                .push(serde_json::to_value(req).unwrap());
            false
        },
    }));
    (enforcer, recorded)
}

/// `n` distinct event-type selectors.
fn type_selectors(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("gts.cf.core.events.event.v1~x.eb.t1.limit{i}.v1~"))
        .collect()
}

#[tokio::test]
async fn a_join_by_a_caller_with_no_identity_is_refused_before_any_pdp_call_over_rest() {
    let (enforcer, pdp) = recording_pdp();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
        ])))
        .with_policy_enforcer(enforcer)
        .with_security_context(
            toolkit_security::SecurityContext::builder()
                .subject_id(Uuid::nil())
                .subject_tenant_id(Uuid::nil())
                .build()
                .unwrap(),
        )
        .build()
        .await;
    // No such group exists: the guard answers before the group lookup, so the
    // caller learns nothing about which groups exist.
    let group = event_broker_sdk::ConsumerGroupId(Uuid::from_u128(0x1803)).to_gts();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": GUARD_TOPIC,
                "subject_tenant": Uuid::from_u128(0x1804), "ownership_filter": "entire_tenant",
                "types": [GUARD_TYPE],
            }],
        })))
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "the caller has no identity: its subject or tenant is the nil UUID",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(*pdp.lock().unwrap(), Vec::<serde_json::Value>::new());
    // Read from the coordinator: a listing as this caller is refused too.
    assert_eq!(
        harness
            .groups()
            .list()
            .iter()
            .map(|subscription| subscription.id)
            .collect::<Vec<_>>(),
        Vec::<Uuid>::new()
    );
}

#[tokio::test]
async fn a_list_of_subscriptions_by_a_caller_with_no_identity_is_refused_before_any_pdp_call_over_rest()
 {
    let (enforcer, pdp) = recording_pdp();
    let harness = EventBrokerHarness::builder()
        .with_policy_enforcer(enforcer)
        .with_security_context(
            toolkit_security::SecurityContext::builder()
                .subject_id(Uuid::nil())
                .subject_tenant_id(Uuid::nil())
                .build()
                .unwrap(),
        )
        .build()
        .await;

    let resp = harness.api_v1().get_subscriptions().send().await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "the caller has no identity: its subject or tenant is the nil UUID",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(*pdp.lock().unwrap(), Vec::<serde_json::Value>::new());
}

#[tokio::test]
async fn a_join_with_64_interests_is_accepted() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tenant_id = Uuid::from_u128(0x1805);
    let interest = json!({ "topic": GUARD_TOPIC, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [GUARD_TYPE] });

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": vec![interest; 64],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    let echoed = json!({
        "topic": GUARD_TOPIC,
        "subject_tenant": tenant_id,
        "types": [GUARD_TYPE],
        "ownership_filter": "entire_tenant",
        "barrier_mode": "respect",
        "filter": null,
        "unmatched_types": [GUARD_TYPE],
    });
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": vec![echoed; 64],
            "assigned": [{ "topic": GUARD_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

#[tokio::test]
async fn a_join_with_65_interests_is_refused_with_too_many_interests() {
    let (enforcer, pdp) = recording_pdp();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
        ])))
        .with_policy_enforcer(enforcer)
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    pdp.lock().unwrap().clear();
    let interest = json!({
        "topic": GUARD_TOPIC,
        "subject_tenant": Uuid::from_u128(0x1806), "ownership_filter": "entire_tenant",
        "types": [GUARD_TYPE],
    });

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": vec![interest; 65],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "TooManyInterests: interests count 65 exceeds the maximum of 64",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "format": "TooManyInterests: interests count 65 exceeds the maximum of 64",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(*pdp.lock().unwrap(), Vec::<serde_json::Value>::new());
    assert_eq!(
        harness.api_v1().get_subscriptions().send().await.json(),
        json!({
            "items": [],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

#[tokio::test]
async fn an_interest_with_32_types_is_accepted() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tenant_id = Uuid::from_u128(0x1807);

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [
                { "topic": GUARD_TOPIC, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [GUARD_TYPE] },
                { "topic": GUARD_TOPIC, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": type_selectors(32) },
            ],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [
                {
                    "topic": GUARD_TOPIC,
                    "subject_tenant": tenant_id,
                    "types": [GUARD_TYPE],
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "filter": null,
                    "unmatched_types": [GUARD_TYPE],
                },
                {
                    "topic": GUARD_TOPIC,
                    "subject_tenant": tenant_id,
                    "types": type_selectors(32),
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "filter": null,
                    "unmatched_types": type_selectors(32),
                },
            ],
            "assigned": [{ "topic": GUARD_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

#[tokio::test]
async fn an_interest_with_33_types_is_refused_with_too_many_types() {
    let (enforcer, pdp) = recording_pdp();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
        ])))
        .with_policy_enforcer(enforcer)
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    pdp.lock().unwrap().clear();
    let tenant_id = Uuid::from_u128(0x1808);

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [
                { "topic": GUARD_TOPIC, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [GUARD_TYPE] },
                { "topic": GUARD_TOPIC, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": type_selectors(33) },
            ],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "TooManyTypes: interest 1: types count 33 exceeds the maximum of 32",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "format": "TooManyTypes: interest 1: types count 33 exceeds the maximum of 32",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(*pdp.lock().unwrap(), Vec::<serde_json::Value>::new());
    assert_eq!(
        harness.api_v1().get_subscriptions().send().await.json(),
        json!({
            "items": [],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

#[tokio::test]
async fn a_join_with_a_nil_interest_tenant_is_refused_with_invalid_tenant() {
    let (enforcer, pdp) = recording_pdp();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
        ])))
        .with_policy_enforcer(enforcer)
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    pdp.lock().unwrap().clear();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": GUARD_TOPIC,
                "subject_tenant": Uuid::nil(), "ownership_filter": "entire_tenant",
                "types": [GUARD_TYPE],
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "InvalidTenant: interest 0: the interest's tenant must not be the nil UUID",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "format": "InvalidTenant: interest 0: the interest's tenant must not be the nil UUID",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(*pdp.lock().unwrap(), Vec::<serde_json::Value>::new());
    assert_eq!(
        harness.api_v1().get_subscriptions().send().await.json(),
        json!({
            "items": [],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

// -- The subject-type selector --

const SUBJECT_SELECTOR: &str = "gts.x.eb.t1.customer.v1~";

fn subject_selectors(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("gts.x.eb.t1.subject{i}.v1~"))
        .collect()
}

#[tokio::test]
async fn join_echoes_a_subject_type_selector() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tenant_id = Uuid::from_u128(0x1910);

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": GUARD_TOPIC,
                "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                "types": [GUARD_TYPE],
                "subject_types": [SUBJECT_SELECTOR],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": GUARD_TOPIC,
                "subject_tenant": tenant_id,
                "types": [GUARD_TYPE],
                "subject_types": [SUBJECT_SELECTOR],
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "filter": null,
                "unmatched_types": [GUARD_TYPE],
            }],
            "assigned": [{ "topic": GUARD_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
    let expected = json!({
        "id": id,
        "consumer_group": group,
        "client_agent": "test-agent",
        "interests": [{
            "topic": GUARD_TOPIC,
            "subject_tenant": tenant_id,
            "types": [GUARD_TYPE],
            "subject_types": [SUBJECT_SELECTOR],
            "ownership_filter": "entire_tenant",
            "barrier_mode": "respect",
            "filter": null,
        }],
        "assigned": [{ "topic": GUARD_TOPIC, "partition": 0 }],
        "topology_version": 1,
        "created_at": created_at,
    });
    let read = harness.api_v1().get_subscription(&id).send().await;
    read.assert_status(200);
    assert_eq!(read.json(), expected);
}

/// The topic's one type allows only `ORDER_SUBJECT`, and the selector names
/// another subject type: no event the interest selects can match it. The
/// JOIN still succeeds, and `unmatched_types` reports `types` entries only,
/// never a subject-type selector.
#[tokio::test]
async fn join_with_a_subject_type_no_registered_type_allows_reports_nothing() {
    const ORDER_SUBJECT: &str = "gts.x.eb.t1.order.v1~";
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
            {
                "id": GUARD_TYPE,
                "topic": GUARD_TOPIC,
                "allowed_subject_types": [ORDER_SUBJECT],
            },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tenant_id = Uuid::from_u128(0x1912);

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": GUARD_TOPIC,
                "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                "types": [GUARD_TYPE],
                "subject_types": [SUBJECT_SELECTOR],
            }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": GUARD_TOPIC,
                "subject_tenant": tenant_id,
                "types": [GUARD_TYPE],
                "subject_types": [SUBJECT_SELECTOR],
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": GUARD_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

#[tokio::test]
async fn join_without_a_selector_echoes_no_subject_types_key() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
        ])))
        .build()
        .await;
    let tenant_id = Uuid::from_u128(0x1911);

    for interest in [
        json!({ "topic": GUARD_TOPIC, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [GUARD_TYPE] }),
        json!({
            "topic": GUARD_TOPIC,
            "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
            "types": [GUARD_TYPE],
            "subject_types": [],
        }),
    ] {
        let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let resp = harness
            .api_v1()
            .post_subscriptions()
            .with_body(Json(&json!({
                "consumer_group": group,
                "client_agent": "test-agent",
                "interests": [interest],
            })))
            .send()
            .await;

        resp.assert_status(201);
        let body = resp.json();
        let id = body["id"].as_str().expect("id must be a string").to_owned();
        let created_at = body["created_at"]
            .as_str()
            .expect("created_at must be a string")
            .to_owned();
        assert_eq!(
            body,
            json!({
                "id": id,
                "consumer_group": group,
                "client_agent": "test-agent",
                "interests": [{
                    "topic": GUARD_TOPIC,
                    "subject_tenant": tenant_id,
                    "types": [GUARD_TYPE],
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "filter": null,
                    "unmatched_types": [GUARD_TYPE],
                }],
                "assigned": [{ "topic": GUARD_TOPIC, "partition": 0 }],
                "topology_version": 1,
                "created_at": created_at,
            })
        );
    }
}

#[tokio::test]
async fn join_with_32_subject_types_is_accepted() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tenant_id = Uuid::from_u128(0x1912);

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [
                { "topic": GUARD_TOPIC, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [GUARD_TYPE] },
                {
                    "topic": GUARD_TOPIC,
                    "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                    "types": [GUARD_TYPE],
                    "subject_types": subject_selectors(32),
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(201);
    let body = resp.json();
    let id = body["id"].as_str().expect("id must be a string").to_owned();
    let created_at = body["created_at"]
        .as_str()
        .expect("created_at must be a string")
        .to_owned();
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [
                {
                    "topic": GUARD_TOPIC,
                    "subject_tenant": tenant_id,
                    "types": [GUARD_TYPE],
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "filter": null,
                    "unmatched_types": [GUARD_TYPE],
                },
                {
                    "topic": GUARD_TOPIC,
                    "subject_tenant": tenant_id,
                    "types": [GUARD_TYPE],
                    "subject_types": subject_selectors(32),
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "filter": null,
                    "unmatched_types": [GUARD_TYPE],
                },
            ],
            "assigned": [{ "topic": GUARD_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

#[tokio::test]
async fn join_with_33_subject_types_is_refused_with_too_many_subject_types() {
    let (enforcer, pdp) = recording_pdp();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
        ])))
        .with_policy_enforcer(enforcer)
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    pdp.lock().unwrap().clear();
    let tenant_id = Uuid::from_u128(0x1913);

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [
                { "topic": GUARD_TOPIC, "subject_tenant": tenant_id, "ownership_filter": "entire_tenant", "types": [GUARD_TYPE] },
                {
                    "topic": GUARD_TOPIC,
                    "subject_tenant": tenant_id, "ownership_filter": "entire_tenant",
                    "types": [GUARD_TYPE],
                    "subject_types": subject_selectors(33),
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "TooManySubjectTypes: interest 1: subject types count 33 exceeds the maximum of 32",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "format": "TooManySubjectTypes: interest 1: subject types count 33 exceeds the maximum of 32",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(*pdp.lock().unwrap(), Vec::<serde_json::Value>::new());
    assert_eq!(
        harness.api_v1().get_subscriptions().send().await.json(),
        json!({
            "items": [],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

#[tokio::test]
async fn join_with_an_invalid_subject_type_pattern_is_refused() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            // Two wildcard segments: a GTS pattern carries at most one.
            "interests": [{
                "topic": GUARD_TOPIC,
                "subject_tenant": Uuid::from_u128(0x1914), "ownership_filter": "entire_tenant",
                "types": [GUARD_TYPE],
                "subject_types": ["gts.*.foo.*"],
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "BadTypePattern: subject type selector must be a valid GTS pattern",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "format": "BadTypePattern: subject type selector must be a valid GTS pattern",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(
        harness.api_v1().get_subscriptions().send().await.json(),
        json!({
            "items": [],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

#[tokio::test]
async fn an_interests_type_count_is_checked_before_its_subject_type_count() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": GUARD_TOPIC,
                "subject_tenant": Uuid::from_u128(0x1915), "ownership_filter": "entire_tenant",
                "types": type_selectors(33),
                "subject_types": subject_selectors(33),
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "TooManyTypes: interest 0: types count 33 exceeds the maximum of 32",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "format": "TooManyTypes: interest 0: types count 33 exceeds the maximum of 32",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(
        harness.api_v1().get_subscriptions().send().await.json(),
        json!({
            "items": [],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

#[tokio::test]
async fn an_interests_subject_type_count_is_checked_before_its_tenant() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": GUARD_TOPIC, "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [{
                "topic": GUARD_TOPIC,
                "subject_tenant": Uuid::nil(), "ownership_filter": "entire_tenant",
                "types": [GUARD_TYPE],
                "subject_types": subject_selectors(33),
            }],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "TooManySubjectTypes: interest 0: subject types count 33 exceeds the maximum of 32",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "format": "TooManySubjectTypes: interest 0: subject types count 33 exceeds the maximum of 32",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(
        harness.api_v1().get_subscriptions().send().await.json(),
        json!({
            "items": [],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

#[tokio::test]
async fn join_by_another_principal_of_the_tenant_returns_403_consumer_group_not_owned() {
    let tenant = Uuid::from_u128(0x3612);
    let harness = EventBrokerHarness::builder()
        .with_security_context(
            toolkit_security::SecurityContext::builder()
                .subject_id(Uuid::from_u128(0x3613))
                .subject_tenant_id(tenant)
                .build()
                .unwrap(),
        )
        .build()
        .await;
    let creator = toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x3611))
        .subject_tenant_id(tenant)
        .build()
        .unwrap();
    let group = harness
        .delivery()
        .create_consumer_group(
            &creator,
            crate::domain::model::ConsumerGroupCreateInput {
                client_agent: Some("creator".to_owned()),
                description: None,
            },
        )
        .await
        .expect("the group is created")
        .id
        .to_string();

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "test-agent",
            "interests": [],
        })))
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": format!("consumer group '{group}' was created by another principal"),
                "resource_type": "gts.cf.core.events.consumer_group.v1~",
            },
        })
    );
    assert!(
        harness.groups().list().is_empty(),
        "the refused JOIN created no subscription"
    );
}
