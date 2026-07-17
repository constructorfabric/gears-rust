//! Mirrors scenarios/errors/. Tests migrated per mock-reference-alignment.
//!
//! The in-process mock has no HTTP layer, so it cannot reproduce status codes,
//! the `application/problem+json` envelope, `Retry-After` headers, or
//! auth (401/403). Each test here asserts the *closest real thing* the mock
//! returns - the `EventBrokerError` variant the broker error model maps onto
//! that status. Scenarios that can only be expressed at the HTTP/auth layer are
//! recorded as divergences in the change notes, not faked.
use super::helpers::*;
#[cfg(test)]
use toolkit_gts::gts_id;

use super::helpers::{broker_with_topic, ctx, wire_event};
use crate::api::{EventBroker, ProducerMode};
use crate::error::EventBrokerError;
use crate::ids::{ConsumerGroupId, SubscriptionId};
use uuid::Uuid;

/// Scenario: errors/1.01-positive-problem-details-envelope.md
///
/// The reference defines the RFC-9457 `problem+json` envelope and triggers it
/// with a representative `404` (GET unknown consumer group). The mock has no
/// HTTP envelope; this asserts the SDK variant that carries the same domain
/// identity the envelope's `context.resource_*` would - `ConsumerGroupNotFound`
/// naming the missing group.
#[tokio::test]
async fn s1_01_positive_problem_details_envelope() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let missing = ConsumerGroupId::from_gts(gts_id!(
        "cf.core.events.consumer_group.v1~example.missing.group.x.v1"
    ));

    let err = broker.get_consumer_group(&c, &missing).await.unwrap_err();
    match err {
        EventBrokerError::ConsumerGroupNotFound {
            ref group_id,
            ref detail,
            ..
        } => {
            assert_eq!(group_id, &missing, "error identifies the missing group");
            assert!(
                detail.contains(&missing.to_string()),
                "detail names the missing resource: {detail}"
            );
        }
        other => panic!("expected ConsumerGroupNotFound, got {other:?}"),
    }
}

/// Scenario: errors/1.04-negative-404-not-found.md
///
/// The reference triggers `404` by opening a stream for an unknown subscription.
/// The mock has no HTTP envelope; this asserts the SDK variant that carries the
/// same domain identity - `SubscriptionNotFound` naming the missing
/// subscription.
#[tokio::test]
async fn s1_04_negative_404_not_found() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let unknown = SubscriptionId(Uuid::parse_str("99999999-8888-7777-6666-555555555555").unwrap());

    let err = match broker.stream(&c, unknown).await {
        Ok(_) => panic!("unknown subscription must be rejected"),
        Err(err) => err,
    };
    match err {
        EventBrokerError::SubscriptionNotFound { id, .. } => {
            assert_eq!(id, unknown, "error identifies the missing subscription");
        }
        other => panic!("expected SubscriptionNotFound, got {other:?}"),
    }
}

/// Scenario: errors/1.05-negative-409-conflict.md
///
/// DIVERGENCE: not faithfully reproducible. The reference `409` (open a stream
/// whose assigned partitions have no committed cursor → `cursor_missing`) is
/// enforced only at the HTTP edge. The mock's `stream()` does NOT require a
/// committed cursor before opening, so the `cursor_missing` precondition cannot
/// be provoked here. The closest representable conflict in the SDK error model
/// is `PositionsNotSet` (unseeded partitions), which the mock does not raise on
/// stream-open. This test documents that the mock streams without a committed
/// cursor - the inverse of the reference's precondition - so the `409` is a
/// divergence.
#[tokio::test]
async fn s1_05_negative_409_conflict() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = broker
        .create_consumer_group(
            &c,
            crate::models::CreateConsumerGroupRequest {
                client_agent: "test-agent/1.0".to_owned(),
                description: None,
            },
        )
        .await
        .unwrap()
        .id;
    let sub = super::helpers::join_group(&c, &broker, &gid, TOPIC).await;

    // No SEEK / no committed cursor → the stream-open precondition fails with
    // PositionsNotSet (the SDK-level analog of the 409 cursor_missing conflict).
    let err = broker
        .stream(&c, sub.subscription_id)
        .await
        .err()
        .expect("unseeded stream-open must be rejected");
    assert!(
        matches!(err, crate::error::EventBrokerError::PositionsNotSet { .. }),
        "expected PositionsNotSet, got {err:?}"
    );
}

/// Scenario: errors/1.06-negative-412-sequence-violation.md
///
/// The one producer-protocol error that escapes to callers: a chained-mode
/// publish whose `meta.previous` doesn't match the broker's `last_sequence` is
/// rejected. The mock surfaces this as `EventBrokerError::SequenceViolation`,
/// the SDK mapping of the `412` failed-precondition envelope, carrying the
/// expected previous for resync.
#[tokio::test]
async fn s1_06_negative_412_sequence_violation() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = broker
        .register_producer(&c, ProducerMode::Chained, "svc/1.0")
        .await
        .unwrap()
        .0;

    // Seed a chain at sequence=1 (previous=-1), so broker last_sequence becomes 1.
    broker
        .publish(&c, &chained_event(&c, TOPIC, pid, 1, -1))
        .await
        .unwrap();

    // Publish sequence=2 with a wrong previous (3 instead of 1) → violation.
    let err = broker
        .publish(&c, &chained_event(&c, TOPIC, pid, 2, 3))
        .await
        .unwrap_err();
    match err {
        EventBrokerError::SequenceViolation {
            expected_previous, ..
        } => {
            assert_eq!(
                expected_previous, 1,
                "broker reports its current last_sequence for resync"
            );
        }
        other => panic!("expected SequenceViolation, got {other:?}"),
    }
}

/// Scenario: errors/1.07-negative-429-rate-limited.md
///
/// A tenant exceeding the publish quota is rejected. The mock models the quota
/// via `set_publish_rate_limit`; once the allowance is exhausted, `publish`
/// returns `EventBrokerError::RateLimited` carrying `retry_after_secs` - the SDK
/// mapping of the `429` resource-exhausted envelope.
///
/// DIVERGENCE (partial): the HTTP `Retry-After` header and `problem+json` body
/// are not represented; only `retry_after_secs` on the variant is asserted.
#[tokio::test]
async fn s1_07_negative_429_rate_limited() {
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();

    // Allow exactly one publish, then throttle.
    h.set_publish_rate_limit(Some(1)).await;
    broker
        .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap();

    let err = broker
        .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap_err();
    match err {
        EventBrokerError::RateLimited {
            retry_after_secs, ..
        } => {
            assert!(
                retry_after_secs > 0,
                "throttled publish carries a retry-after hint"
            );
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

/// Scenario: errors/1.08-negative-500-internal.md
///
/// An internal invariant failure returns a generic error with no leaked
/// internals. The mock injects this via `reject_persist`, which surfaces as
/// `EventBrokerError::Internal` - the SDK mapping of the `500` envelope.
///
/// DIVERGENCE (partial): the mock echoes the injected reason in the `Internal`
/// payload (a test affordance); the production HTTP layer scrubs `detail` to a
/// generic message. The scrubbing happens at the edge, not in the SDK variant,
/// so only the variant kind is asserted.
#[tokio::test]
async fn s1_08_negative_500_internal() {
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();

    h.reject_persist(Some("simulated invariant failure")).await;
    let err = broker
        .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, EventBrokerError::Internal(_)),
        "an internal invariant failure surfaces as EventBrokerError::Internal, got {err:?}"
    );
}

// -- Local helper: chained-mode producer event (mirrors idempotency.rs) ---------

fn chained_event(
    ctx: &toolkit_security::SecurityContext,
    topic: &str,
    producer_id: Uuid,
    sequence: i64,
    previous: i64,
) -> crate::models::Event {
    let mut ev = wire_event(topic, EVT, ctx.subject_tenant_id());
    ev.meta = Some(crate::models::ProducerMeta {
        version: 1,
        producer_id: Some(producer_id),
        previous: Some(previous),
        sequence: Some(sequence),
        partition_hint: None,
    });
    ev
}
