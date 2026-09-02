//! Tests for the election subscription table.
//!
//! The property that matters most is a negative one — nothing here is a lease —
//! and it is asserted at the service level, in `leader_tests`, where a renewal is
//! shown to succeed after the subscription is dropped. What is asserted here is
//! the table's own behaviour: identity, ownership, and the fan-out `S5` uses.

use cluster_sdk::leader::{LeaderStatus, LeaderWatchEvent};

use super::{ElectionSubscriptions, SubscriptionId};

#[test]
fn ids_are_unguessable_and_distinct_per_participant() {
    // Two participants in *one* election hold two subscriptions, so the id
    // cannot be the election name. It is also the whole authority `await_change`
    // presents - there is no token on that call - so it has to be unguessable.
    let table = ElectionSubscriptions::new();
    let first = table.open("event-broker", "ledger", "orders");
    let second = table.open("event-broker", "ledger", "orders");
    assert_ne!(first, second);
    assert_eq!(table.len(), 2);
}

#[test]
fn attaching_replaces_the_reader_and_keeps_the_id() {
    // A client's `election_id` survives a reconnect, so a broken stream needs no
    // fresh `join`; and the older reader loses, which is how section 6.6's
    // one-reader-per-election rule lands on a streaming projection.
    let table = ElectionSubscriptions::new();
    let id = table.open("event-broker", "ledger", "orders");

    let mut first = table.attach(&id, "event-broker").expect("the first reader");
    let mut second = table
        .attach(&id, "event-broker")
        .expect("a reconnect attaches to the same id");

    table.broadcast_terminal(&LeaderWatchEvent::Status(LeaderStatus::Lost));

    assert!(
        first.try_recv().is_err(),
        "the replaced reader receives nothing further"
    );
    assert!(
        matches!(
            second.try_recv(),
            Ok(LeaderWatchEvent::Status(LeaderStatus::Lost))
        ),
        "the newer reader wins"
    );
}

#[test]
fn an_unknown_id_and_a_foreign_id_are_the_same_answer() {
    // A distinguishable "exists, but not yours" would make the table enumerable,
    // which is the same reason a foreign lease token and an absent one give one
    // answer (DESIGN section 5.8.1).
    let table = ElectionSubscriptions::new();
    let id = table.open("event-broker", "ledger", "orders");

    assert!(table.attach(&id, "api-gateway").is_none());
    assert!(
        table
            .attach(&SubscriptionId::mint(), "event-broker")
            .is_none()
    );
}

#[test]
fn a_registered_subscription_with_no_reader_is_skipped_not_buffered() {
    // The window between `join` and `await_change`. Buffering into a channel that
    // may never be read is the abandoned-subscription leak A6 and S2 exist to
    // bound, so the state is represented rather than approximated.
    let table = ElectionSubscriptions::new();
    let id = table.open("event-broker", "ledger", "orders");

    table.broadcast_terminal(&LeaderWatchEvent::Status(LeaderStatus::Lost));

    let mut events = table.attach(&id, "event-broker").expect("attaches");
    assert!(
        events.try_recv().is_err(),
        "an event broadcast before the stream opened was not buffered for it"
    );
}

#[test]
fn broadcast_delivers_the_shutdown_sequence_in_order() {
    // Item S5's payload: remote leaders receive `Status(Lost)` and then
    // `Closed(Shutdown)`, in that order (DESIGN section 4.8). The ordering is the
    // channel's; what is asserted here is that the fan-out preserves it.
    let table = ElectionSubscriptions::new();
    let id = table.open("event-broker", "ledger", "orders");
    let mut events = table.attach(&id, "event-broker").expect("attaches");

    table.broadcast_terminal(&LeaderWatchEvent::Status(LeaderStatus::Lost));
    table.broadcast_terminal(&LeaderWatchEvent::Closed(
        cluster_sdk::ClusterError::Shutdown,
    ));

    assert!(matches!(
        events.try_recv(),
        Ok(LeaderWatchEvent::Status(LeaderStatus::Lost))
    ));
    assert!(matches!(
        events.try_recv(),
        Ok(LeaderWatchEvent::Closed(
            cluster_sdk::ClusterError::Shutdown
        ))
    ));
}

/// Fills a subscriber's consumer-visible buffer past capacity, so it is
/// genuinely full. Returns how many events were attempted.
fn flood(table: &ElectionSubscriptions, id: &SubscriptionId) -> usize {
    // Twice the buffer, so it is full whatever the exact size.
    let pushed = super::SUBSCRIPTION_BUFFER * 2;
    table.fill_buffer_for_test(id, pushed);
    pushed
}

#[test]
fn broadcast_drops_the_shutdown_two_step_under_backpressure() {
    // `SUB-1`. Back-pressure is worst exactly during a drain, which is exactly
    // when the two-step is sent, so a fan-out that only ever `try_send`s drops
    // both terminal events precisely when they matter. Profile 1 answers this by
    // reserving two `OwnedPermit`s at channel construction
    // (`cluster-sdk/src/leader/watch.rs`, `TERMINAL_HEADROOM`); this is the
    // Profile 3 mirror of that mechanism.
    let table = ElectionSubscriptions::new();
    let id = table.open("event-broker", "ledger", "orders");
    let mut events = table.attach(&id, "event-broker").expect("attaches");

    // Nobody drains: the subscriber is wedged, which is the whole scenario.
    let pushed = flood(&table, &id);

    table.broadcast_terminal(&LeaderWatchEvent::Status(LeaderStatus::Lost));
    table.broadcast_terminal(&LeaderWatchEvent::Closed(
        cluster_sdk::ClusterError::Shutdown,
    ));

    let mut delivered = Vec::new();
    while let Ok(event) = events.try_recv() {
        delivered.push(event);
    }

    let terminals: Vec<&LeaderWatchEvent> = delivered
        .iter()
        .filter(|event| {
            matches!(
                event,
                LeaderWatchEvent::Status(LeaderStatus::Lost) | LeaderWatchEvent::Closed(_)
            )
        })
        .collect();

    assert_eq!(
        terminals.len(),
        2,
        "ADR-003: the leader must observe Status(Lost) then Closed(Shutdown). Delivered {} of \
         {pushed} flood events plus {} terminal events",
        delivered.len() - terminals.len(),
        terminals.len()
    );
    assert!(
        matches!(terminals[0], LeaderWatchEvent::Status(LeaderStatus::Lost)),
        "Status(Lost) must arrive first, got: {:?}",
        terminals[0]
    );
    assert!(
        matches!(terminals[1], LeaderWatchEvent::Closed(_)),
        "Closed(Shutdown) must arrive second, got: {:?}",
        terminals[1]
    );
    assert!(
        matches!(
            delivered.last(),
            Some(LeaderWatchEvent::Closed(
                cluster_sdk::ClusterError::Shutdown
            ))
        ),
        "and it must be the last thing on the stream"
    );
}

#[test]
fn closing_removes_the_subscription() {
    let table = ElectionSubscriptions::new();
    let id = table.open("event-broker", "ledger", "orders");
    assert!(!table.is_empty());

    table.close(&id);

    assert!(table.is_empty());
    assert!(
        table.attach(&id, "event-broker").is_none(),
        "a closed subscription cannot be reattached; recovery is a fresh join"
    );
}

#[tokio::test(start_paused = true)]
async fn a_swept_subscription_is_gone_and_a_kept_one_is_untouched() {
    // What the `retain(|id| ..)` seam used to assert, now against the sweep that
    // replaced it. The policy itself is exercised in `sweep_tests`; what matters
    // here is that a pass leaves the table in the two states a caller can
    // observe - reattachable, or absent.
    let table = ElectionSubscriptions::new();
    let kept = table.open("event-broker", "ledger", "orders");
    let swept = table.open("api-gateway", "routes", "orders");
    let _reader = table.attach(&kept, "event-broker").expect("attaches");

    tokio::time::advance(std::time::Duration::from_mins(1)).await;
    let report = table.sweep(std::time::Duration::from_secs(15));

    assert_eq!(report.reaped_total(), 1);
    assert_eq!(table.len(), 1);
    assert!(table.attach(&kept, "event-broker").is_some());
    assert!(
        table.attach(&swept, "api-gateway").is_none(),
        "a swept subscription cannot be reattached; recovery is a fresh join"
    );
}
