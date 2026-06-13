#[cfg(test)]
use super::helpers::*;

// Chained/monotonic dedup, H1, M1, M2, M3, M7, P3/P4/P5/P6 profiles, Scenario C.
use super::helpers::{broker_with_topic, ctx, wire_event};
use crate::api::EventBroker;
use crate::producer::backend::IngestOutcome;
use uuid::Uuid;

fn chained_event(
    ctx: &modkit_security::SecurityContext,
    topic: &str,
    producer_id: Uuid,
    sequence: i64,
    previous: i64,
) -> crate::internal_test_helpers::WireEvent {
    let mut ev = wire_event(topic, EVT, ctx.subject_tenant_id());
    // Inject meta via JSON so we don't need to name private ProducerMeta.
    let json = serde_json::json!({
        "id": ev.id,
        "type": ev.type_id,
        "topic": ev.topic,
        "tenant_id": ev.tenant_id,
        "source": ev.source,
        "subject": ev.subject,
        "subject_type": ev.subject_type,
        "occurred_at": ev.occurred_at.to_rfc3339(),
        "meta": { "version": 1, "producer_id": producer_id, "sequence": sequence, "previous": previous }
    });
    serde_json::from_value(json).unwrap()
}

fn monotonic_event(
    ctx: &modkit_security::SecurityContext,
    topic: &str,
    producer_id: Uuid,
    sequence: i64,
) -> crate::internal_test_helpers::WireEvent {
    let mut ev = wire_event(topic, EVT, ctx.subject_tenant_id());
    let json = serde_json::json!({
        "id": ev.id,
        "type": ev.type_id,
        "topic": ev.topic,
        "tenant_id": ev.tenant_id,
        "source": ev.source,
        "subject": ev.subject,
        "subject_type": ev.subject_type,
        "occurred_at": ev.occurred_at.to_rfc3339(),
        "meta": { "version": 1, "producer_id": producer_id, "sequence": sequence }
    });
    serde_json::from_value(json).unwrap()
}

// ── Chained dedup ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn chained_accepted_sequence() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = Uuid::new_v4();
    assert_eq!(
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid, 1, -1))
            .await
            .unwrap(),
        IngestOutcome::Accepted
    );
    assert_eq!(
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid, 2, 1))
            .await
            .unwrap(),
        IngestOutcome::Accepted
    );
    assert_eq!(
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid, 3, 2))
            .await
            .unwrap(),
        IngestOutcome::Accepted
    );
}

#[tokio::test]
async fn chained_duplicate_returns_duplicate_not_accepted() {
    // M2: Duplicate must return Duplicate; chain state must NOT advance so the next publish succeeds.
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = Uuid::new_v4();
    broker
        .publish(&c, &chained_event(&c, TOPIC, pid, 1, -1))
        .await
        .unwrap();
    // Retry same event:
    let dup = broker
        .publish(&c, &chained_event(&c, TOPIC, pid, 1, -1))
        .await
        .unwrap();
    assert_eq!(dup, IngestOutcome::Duplicate, "retry must return Duplicate");
    // Next sequence must still succeed (chain not poisoned):
    let next = broker
        .publish(&c, &chained_event(&c, TOPIC, pid, 2, 1))
        .await
        .unwrap();
    assert_eq!(
        next,
        IngestOutcome::Accepted,
        "chain must not be poisoned by Duplicate (M2)"
    );
}

#[tokio::test]
async fn chained_wrong_previous_sequence_violation() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = Uuid::new_v4();
    broker
        .publish(&c, &chained_event(&c, TOPIC, pid, 1, -1))
        .await
        .unwrap();
    let err = broker
        .publish(&c, &chained_event(&c, TOPIC, pid, 2, 99))
        .await
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("SequenceViolation") || format!("{err:?}").contains("sequence"),
        "wrong previous must SequenceViolation: {err:?}"
    );
}

// ── Monotonic dedup ───────────────────────────────────────────────────────────

#[tokio::test]
async fn monotonic_duplicate_returns_duplicate() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = Uuid::new_v4();
    broker
        .publish(&c, &monotonic_event(&c, TOPIC, pid, 1))
        .await
        .unwrap();
    let dup = broker
        .publish(&c, &monotonic_event(&c, TOPIC, pid, 1))
        .await
        .unwrap();
    assert_eq!(dup, IngestOutcome::Duplicate);
}

// ── P4 stateless — no dedup ───────────────────────────────────────────────────

#[tokio::test]
async fn stateless_always_accepted() {
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    for _ in 0..3 {
        assert_eq!(
            broker
                .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
                .await
                .unwrap(),
            IngestOutcome::Accepted
        );
    }
    // All 3 events stored, no dedup.
    assert_eq!(h.stored(TOPIC, 0).await.len(), 3);
}

// ── P5 multi-instance: distinct PIDs never collide (H1 detector) ─────────────

#[tokio::test]
async fn distinct_pids_no_collision() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid1 = Uuid::new_v4();
    let pid2 = Uuid::new_v4();
    // Both start at sequence=1 but different producer IDs — no conflict.
    assert_eq!(
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid1, 1, -1))
            .await
            .unwrap(),
        IngestOutcome::Accepted
    );
    assert_eq!(
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid2, 1, -1))
            .await
            .unwrap(),
        IngestOutcome::Accepted
    );
}

// ── M7: reset_producer_chain clears all three branches ───────────────────────

#[tokio::test]
async fn reset_chain_all_clears_state() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = Uuid::new_v4();
    broker
        .publish(&c, &chained_event(&c, TOPIC, pid, 1, -1))
        .await
        .unwrap();
    // Verify cursor exists.
    assert!(
        !broker
            .producer_cursors(&c, crate::ids::ProducerId(pid))
            .await
            .unwrap()
            .is_empty()
    );
    // Reset all.
    broker
        .reset_producer_chain(&c, crate::ids::ProducerId(pid), None, None)
        .await
        .unwrap();
    assert!(
        broker
            .producer_cursors(&c, crate::ids::ProducerId(pid))
            .await
            .unwrap()
            .is_empty(),
        "after reset_all, cursors must be empty (M7)"
    );
    // After reset, sequence=1 is valid again (fresh start).
    assert_eq!(
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid, 1, -1))
            .await
            .unwrap(),
        IngestOutcome::Accepted
    );
}

// ── Scenario C: DB-restore → PID rotation ─────────────────────────────────────

#[tokio::test]
async fn pid_rotation_starts_fresh_chain() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid1 = Uuid::new_v4();
    broker
        .publish(&c, &chained_event(&c, TOPIC, pid1, 1, -1))
        .await
        .unwrap();
    broker
        .publish(&c, &chained_event(&c, TOPIC, pid1, 2, 1))
        .await
        .unwrap();
    // Producer detects stale state, rotates to fresh PID (Scenario C).
    let pid2 = Uuid::new_v4();
    // Fresh PID, start at sequence=1 — no conflict.
    assert_eq!(
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid2, 1, -1))
            .await
            .unwrap(),
        IngestOutcome::Accepted
    );
    // Old PID still has its own independent state.
    assert_eq!(
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid1, 3, 2))
            .await
            .unwrap(),
        IngestOutcome::Accepted
    );
}
