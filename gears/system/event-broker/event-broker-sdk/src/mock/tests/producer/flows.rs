//! Mirrors scenarios/producer/flows/. Tests migrated per mock-reference-alignment.

#[cfg(test)]
use super::super::helpers::*;

use super::super::helpers::{broker_with_topic, ctx, wire_event};
use crate::api::{EventBroker, IngestOutcome, ProducerMode};
use crate::error::EventBrokerError;
use crate::models::{Event, ProducerMeta};
use uuid::Uuid;

// -- Local meta-stamping builders (mirror idempotency.rs idioms) ---------------
// The mock detects producer mode from the `meta` block; we stamp it directly.

fn chained_event(
    ctx: &toolkit_security::SecurityContext,
    topic: &str,
    producer_id: Uuid,
    sequence: i64,
    previous: i64,
) -> Event {
    let mut ev = wire_event(topic, EVT, ctx.subject_tenant_id());
    ev.meta = Some(ProducerMeta {
        version: 1,
        producer_id: Some(producer_id),
        previous: Some(previous),
        sequence: Some(sequence),
        partition_hint: None,
    });
    ev
}

fn monotonic_event(
    ctx: &toolkit_security::SecurityContext,
    topic: &str,
    producer_id: Uuid,
    sequence: i64,
) -> Event {
    let mut ev = wire_event(topic, EVT, ctx.subject_tenant_id());
    ev.meta = Some(ProducerMeta {
        version: 1,
        producer_id: Some(producer_id),
        previous: None,
        sequence: Some(sequence),
        partition_hint: None,
    });
    ev
}

/// Scenario: producer/flows/1.01-positive-register-chained-producer.md
#[tokio::test]
async fn s1_01_register_chained_producer() {
    // Registering a chained-mode producer mints a producer_id bound to the
    // caller's principal (201 Created).
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = broker
        .register_producer(&c, ProducerMode::Chained, "order-service/1.0")
        .await
        .unwrap();
    // A fresh producer has no chain state yet.
    assert!(
        broker
            .get_producer_cursors(&c, pid)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Scenario: producer/flows/1.02-positive-register-monotonic-producer.md
#[tokio::test]
async fn s1_02_register_monotonic_producer() {
    // Registering a monotonic-mode producer mints a producer_id (201 Created).
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = broker
        .register_producer(&c, ProducerMode::Monotonic, "order-service/1.0")
        .await
        .unwrap();
    assert!(
        broker
            .get_producer_cursors(&c, pid)
            .await
            .unwrap()
            .is_empty()
    );

    // Monotonic publishes (producer_id + sequence, no previous) are accepted and
    // dedupe on (producer_id, topic, partition, sequence).
    let mpid = broker
        .register_producer(&c, ProducerMode::Monotonic, "svc/1.0")
        .await
        .unwrap()
        .0;
    assert_eq!(
        broker
            .publish(&c, &monotonic_event(&c, TOPIC, mpid, 1))
            .await
            .unwrap(),
        IngestOutcome::Accepted
    );
    assert_eq!(
        broker
            .publish(&c, &monotonic_event(&c, TOPIC, mpid, 1))
            .await
            .unwrap(),
        IngestOutcome::Duplicate
    );
}

#[tokio::test]
async fn registered_producer_mode_must_match_event_metadata() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = broker
        .register_producer(&c, ProducerMode::Monotonic, "svc/1.0")
        .await
        .unwrap()
        .0;

    let err = broker
        .publish(&c, &chained_event(&c, TOPIC, pid, 1, -1))
        .await
        .unwrap_err();
    assert!(
        matches!(err, EventBrokerError::InvalidEventField { field, .. } if field == "meta"),
        "registered monotonic producer must reject chained metadata: {err:?}"
    );
}

/// Scenario: producer/flows/1.03-positive-chained-mode-sequence.md
#[tokio::test]
async fn s1_03_chained_mode_sequence() {
    // Chained publish whose meta.previous matches the stored last_sequence is
    // admitted (202) and advances last_sequence by one.
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = broker
        .register_producer(&c, ProducerMode::Chained, "svc/1.0")
        .await
        .unwrap()
        .0;
    // Seed the chain to last_sequence = 7 (single partition 0).
    let mut prev = -1;
    for seq in 1..=7 {
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid, seq, prev))
            .await
            .unwrap();
        prev = seq;
    }
    // Next publish previous=7, sequence=8 matches → Accepted.
    assert_eq!(
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid, 8, 7))
            .await
            .unwrap(),
        IngestOutcome::Accepted
    );
    // last_sequence advanced 7 → 8.
    let cursors = broker
        .get_producer_cursors(&c, crate::ids::ProducerId(pid))
        .await
        .unwrap();
    let c0 = cursors
        .iter()
        .find(|cur| cur.topic == TOPIC && cur.partition == 0)
        .expect("cursor for (topic, 0) present");
    assert_eq!(c0.last_sequence, 8);
}

/// Scenario: producer/flows/1.04-positive-idempotency-key-dedup.md
#[tokio::test]
async fn s1_04_idempotency_key_dedup() {
    // Re-publishing an already-admitted chained event is recognised as a
    // duplicate: not re-admitted, last_sequence does not advance again, and the
    // chain is not poisoned (next sequence still succeeds).
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = broker
        .register_producer(&c, ProducerMode::Chained, "svc/1.0")
        .await
        .unwrap()
        .0;
    let mut prev = -1;
    for seq in 1..=8 {
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid, seq, prev))
            .await
            .unwrap();
        prev = seq;
    }
    let stored_before = h.stored(TOPIC, 0).await.len();

    // Re-send the same (previous=7, sequence=8) event.
    let dup = broker
        .publish(&c, &chained_event(&c, TOPIC, pid, 8, 7))
        .await
        .unwrap();
    assert_eq!(dup, IngestOutcome::Duplicate, "retry must be Duplicate");

    // No new event appended.
    assert_eq!(h.stored(TOPIC, 0).await.len(), stored_before);
    // last_sequence stays at 8.
    let cursors = broker
        .get_producer_cursors(&c, crate::ids::ProducerId(pid))
        .await
        .unwrap();
    let c0 = cursors.iter().find(|cur| cur.partition == 0).unwrap();
    assert_eq!(c0.last_sequence, 8);

    // Chain not poisoned: next sequence still admits.
    assert_eq!(
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid, 9, 8))
            .await
            .unwrap(),
        IngestOutcome::Accepted
    );
}

/// Scenario: producer/flows/1.05-negative-chained-sequence-violation.md
#[tokio::test]
async fn s1_05_chained_sequence_violation() {
    // A chained publish whose meta.previous does not match the stored
    // last_sequence is rejected (412 SequenceViolation); the error carries the
    // broker's current last_sequence and no event is admitted.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = broker
        .register_producer(&c, ProducerMode::Chained, "svc/1.0")
        .await
        .unwrap()
        .0;
    let mut prev = -1;
    for seq in 1..=7 {
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid, seq, prev))
            .await
            .unwrap();
        prev = seq;
    }
    let stored_before = h.stored(TOPIC, 0).await.len();

    // Stale previous=3 (should be 7). Use a forward sequence (8) so the broker
    // treats it as a chain-link check (not a backward duplicate): seq>last but
    // previous mismatches → SequenceViolation.
    let err = broker
        .publish(&c, &chained_event(&c, TOPIC, pid, 8, 3))
        .await
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("SequenceViolation") || msg.contains("sequence"),
        "stale previous must SequenceViolation: {msg}"
    );
    // Error reports the broker's expected previous (7).
    assert!(
        msg.contains("expected_previous: 7") || msg.contains("expected previous=7"),
        "violation should surface expected_previous=7: {msg}"
    );
    // No event admitted; last_sequence unchanged.
    assert_eq!(h.stored(TOPIC, 0).await.len(), stored_before);
    let cursors = broker
        .get_producer_cursors(&c, crate::ids::ProducerId(pid))
        .await
        .unwrap();
    assert_eq!(
        cursors
            .iter()
            .find(|cur| cur.partition == 0)
            .unwrap()
            .last_sequence,
        7
    );
}

/// Scenario: producer/flows/1.06-positive-cursor-recovery.md
#[tokio::test]
async fn s1_06_cursor_recovery() {
    // After publishing on two partitions, GET cursors reports last_sequence per
    // (topic, partition) so a desynced producer can re-seed meta.previous.
    let (broker, _h) = broker_with_topic(TOPIC, 8).await;
    let c = ctx();
    let pid = broker
        .register_producer(&c, ProducerMode::Chained, "svc/1.0")
        .await
        .unwrap()
        .0;

    // Drive partition 0 to last_sequence=7 using an explicit partition key.
    let key_p0 = partition_key_for(0, 8);
    let mut prev = -1;
    for seq in 1..=7 {
        let mut ev = chained_event(&c, TOPIC, pid, seq, prev);
        ev.partition_key = Some(key_p0.clone());
        broker.publish(&c, &ev).await.unwrap();
        prev = seq;
    }
    // Drive partition 1 to last_sequence=3.
    let key_p1 = partition_key_for(1, 8);
    let mut prev = -1;
    for seq in 1..=3 {
        let mut ev = chained_event(&c, TOPIC, pid, seq, prev);
        ev.partition_key = Some(key_p1.clone());
        broker.publish(&c, &ev).await.unwrap();
        prev = seq;
    }

    let cursors = broker
        .get_producer_cursors(&c, crate::ids::ProducerId(pid))
        .await
        .unwrap();
    let c0 = cursors
        .iter()
        .find(|cur| cur.partition == 0)
        .expect("cursor for p0");
    let c1 = cursors
        .iter()
        .find(|cur| cur.partition == 1)
        .expect("cursor for p1");
    assert_eq!(c0.last_sequence, 7);
    assert_eq!(c1.last_sequence, 3);
}

/// Scenario: producer/flows/1.07-positive-chain-reset.md
#[tokio::test]
async fn s1_07_chain_reset() {
    // Operator reset clears the chain for a producer; the next publish starts a
    // fresh chain from sequence 1.
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = broker
        .register_producer(&c, ProducerMode::Chained, "svc/1.0")
        .await
        .unwrap()
        .0;
    let mut prev = -1;
    for seq in 1..=5 {
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid, seq, prev))
            .await
            .unwrap();
        prev = seq;
    }
    assert!(
        !broker
            .get_producer_cursors(&c, crate::ids::ProducerId(pid))
            .await
            .unwrap()
            .is_empty()
    );

    // Reset all state for the producer (omitting topic/partition resets all).
    broker
        .reset_producer_chain(
            &c,
            crate::ids::ProducerId(pid),
            crate::models::ResetScope::AllTopics,
        )
        .await
        .unwrap();
    assert!(
        broker
            .get_producer_cursors(&c, crate::ids::ProducerId(pid))
            .await
            .unwrap()
            .is_empty(),
        "after reset, cursors must be empty"
    );

    // Fresh chain from sequence 1 is accepted again.
    assert_eq!(
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid, 1, -1))
            .await
            .unwrap(),
        IngestOutcome::Accepted
    );
}

/// Scenario: producer/flows/1.08-negative-unknown-producer.md
#[tokio::test]
async fn s1_08_unknown_producer() {
    // A chained publish carrying an unregistered Producer-Id is rejected: the id
    // must come from POST /v1/producers first.
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let unknown = Uuid::parse_str("deadbeef-0000-0000-0000-000000000000").unwrap();
    let err = broker
        .publish(&c, &chained_event(&c, TOPIC, unknown, 1, -1))
        .await
        .expect_err("publishing with an unregistered producer_id must be rejected");
    assert!(
        matches!(err, crate::error::EventBrokerError::UnknownProducer { .. }),
        "expected UnknownProducer for an unknown producer, got {err:?}"
    );
}

/// Scenario: producer/flows/1.09-flow-chained-producer-desync-recovery.md
#[tokio::test]
async fn s1_09_chained_producer_desync_recovery() {
    // Three-exchange flow:
    //   1. Stale-sequence publish → SequenceViolation (412).
    //   2. Read broker cursor → authoritative last_sequence=7.
    //   3. Republish with corrected previous=7 → Accepted, chain advances to 8.
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = broker
        .register_producer(&c, ProducerMode::Chained, "svc/1.0")
        .await
        .unwrap()
        .0;
    // Broker has accepted through sequence 7 on partition 0.
    let mut prev = -1;
    for seq in 1..=7 {
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid, seq, prev))
            .await
            .unwrap();
        prev = seq;
    }

    // Exchange 1: producer thinks last_sequence=3 and resumes its chain, sending
    // the next forward sequence with previous=3 → broker expected previous=7, so
    // SequenceViolation (412).
    let err = broker
        .publish(&c, &chained_event(&c, TOPIC, pid, 8, 3))
        .await
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("SequenceViolation") || format!("{err:?}").contains("sequence"),
        "stale publish must SequenceViolation: {err:?}"
    );

    // Exchange 2: read cursor → authoritative last_sequence = 7.
    let cursors = broker
        .get_producer_cursors(&c, crate::ids::ProducerId(pid))
        .await
        .unwrap();
    let c0 = cursors.iter().find(|cur| cur.partition == 0).unwrap();
    assert_eq!(c0.last_sequence, 7);

    // Exchange 3: republish with corrected previous=7, sequence=8 → Accepted.
    assert_eq!(
        broker
            .publish(&c, &chained_event(&c, TOPIC, pid, 8, c0.last_sequence))
            .await
            .unwrap(),
        IngestOutcome::Accepted
    );
    let cursors = broker
        .get_producer_cursors(&c, crate::ids::ProducerId(pid))
        .await
        .unwrap();
    assert_eq!(
        cursors
            .iter()
            .find(|cur| cur.partition == 0)
            .unwrap()
            .last_sequence,
        8
    );
}

// -- Test-local helper ---------------------------------------------------------
// Find a partition_key string that the mock routes to `target`
// partition under `parts` partitions, so chained-flow tests can pin events to a
// chosen partition.
fn partition_key_for(target: u32, parts: u32) -> String {
    use crate::mock::partitioning::partition_for;
    for i in 0..100_000u32 {
        let key = format!("flowkey-{i}");
        if partition_for(&key, parts) == target {
            return key;
        }
    }
    panic!("no partition key found for target {target} / {parts} partitions");
}
