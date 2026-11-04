use std::time::Duration;

use event_broker_sdk::backend::Partition;
use futures_util::FutureExt as _;
use tokio::time::Instant;
use uuid::Uuid;

use crate::maintenance::prioritizer::{Due, Key, Lease, Policy, Prioritizer, Taken};

/// How a test settles a lease.
type Settle = fn(Lease<u8>);

fn contended(lease: Lease<u8>) {
    lease.contended("held");
}

fn failed(lease: Lease<u8>) {
    lease.failed("refused");
}

fn key(partition: u32) -> Key {
    (Uuid::from_u128(1), Partition::new(partition))
}

fn queue() -> Prioritizer<u8> {
    Prioritizer::new("test", Policy::default())
}

fn lease(queue: &Prioritizer<u8>) -> Lease<u8> {
    match queue.take() {
        Taken::Lease(lease) => lease,
        Taken::Until(_) => panic!("expected a lease, got Until"),
        Taken::Empty => panic!("expected a lease, got Empty"),
    }
}

/// `(key, attempt)` of the next lease, which is then settled `Done`.
fn next(queue: &Prioritizer<u8>) -> (Key, u32) {
    let lease = lease(queue);
    let shown = (lease.key(), lease.attempt());
    lease.done();
    shown
}

/// How long `take` says to wait.
fn until(queue: &Prioritizer<u8>) -> Duration {
    match queue.take() {
        Taken::Until(at) => at.saturating_duration_since(Instant::now()),
        Taken::Lease(_) => panic!("expected Until, got a lease"),
        Taken::Empty => panic!("expected Until, got Empty"),
    }
}

fn is_empty(queue: &Prioritizer<u8>) -> bool {
    matches!(queue.take(), Taken::Empty)
}

/// Postpones `key` once more with `settle` and answers `(attempt, wait)`:
/// the attempt the lease carried and how long the key then waits. Leaves the
/// clock where the key is due again.
async fn postpone(queue: &Prioritizer<u8>, settle: Settle) -> (u32, Duration) {
    let lease = lease(queue);
    let attempt = lease.attempt();
    settle(lease);
    let wait = until(queue);
    tokio::time::advance(wait).await;
    (attempt, wait)
}

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

#[tokio::test(start_paused = true)]
async fn a_poke_during_a_lease_keeps_the_key_pending() {
    let queue = queue();
    queue.offer(key(0), 1);
    let lease = lease(&queue);
    // After the job's commit, before it settles.
    queue.offer(key(0), 1);
    lease.done();
    assert_eq!(queue.peek(), Due::Now);
    assert_eq!(next(&queue), (key(0), 0));
    assert!(is_empty(&queue));
}

#[tokio::test(start_paused = true)]
async fn a_poke_before_take_is_part_of_the_lease() {
    let queue = queue();
    queue.offer(key(0), 1);
    queue.offer(key(0), 1);
    assert_eq!(next(&queue), (key(0), 0));
    assert_eq!(queue.peek(), Due::Never);
    assert!(is_empty(&queue));
}

#[tokio::test(start_paused = true)]
async fn a_priority_raised_while_leased_is_used_for_the_next_lease() {
    let queue = queue();
    queue.offer(key(0), 1);
    let lease = lease(&queue);
    queue.offer(key(1), 3);
    queue.offer(key(0), 5);
    queue.offer(key(0), 2);
    lease.done();
    // Back at 5, ahead of the key at 3 offered before it.
    assert_eq!([next(&queue), next(&queue)], [(key(0), 0), (key(1), 0)]);
}

#[tokio::test(start_paused = true)]
async fn the_highest_priority_goes_first_and_ties_in_offer_order() {
    let queue = queue();
    queue.offer(key(1), 1);
    queue.offer(key(2), 3);
    queue.offer(key(3), 3);
    queue.offer(key(4), 2);
    queue.offer(key(5), 0);
    // A pending key whose priority is raised moves up.
    queue.offer(key(5), 2);
    let order: Vec<_> = (0..5).map(|_| next(&queue)).collect();
    assert_eq!(
        order,
        [
            (key(2), 0),
            (key(3), 0),
            (key(4), 0),
            (key(5), 0),
            (key(1), 0),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn done_busy_and_gone_drop_a_clean_key_and_keep_a_dirty_one() {
    let settles: [(&str, Settle); 3] = [
        ("done", Lease::done),
        ("busy", Lease::busy),
        ("gone", Lease::gone),
    ];
    for (name, settle) in settles {
        let queue = queue();
        queue.offer(key(0), 1);
        settle(lease(&queue));
        assert_eq!(queue.peek(), Due::Never, "{name} clean");

        queue.offer(key(0), 1);
        let lease = lease(&queue);
        queue.offer(key(0), 1);
        settle(lease);
        assert_eq!(queue.peek(), Due::Now, "{name} dirty");
    }
}

#[tokio::test(start_paused = true)]
async fn again_deferred_and_dropped_come_back_at_once_uncounted() {
    let settles: [(&str, Settle); 3] = [
        ("again", Lease::again),
        ("deferred", Lease::deferred),
        ("dropped", drop),
    ];
    for (name, settle) in settles {
        let queue = queue();
        queue.offer(key(0), 4);
        settle(lease(&queue));
        assert_eq!(queue.peek(), Due::Now, "{name}");
        assert_eq!(next(&queue), (key(0), 0), "{name}");
    }
}

#[tokio::test(start_paused = true)]
async fn failed_postponements_double_up_to_the_cap() {
    let queue = queue();
    queue.offer(key(0), 1);
    let mut seen = Vec::new();
    for _ in 0..11 {
        seen.push(postpone(&queue, failed).await);
    }
    assert_eq!(
        seen,
        [
            (0, ms(100)),
            (1, ms(200)),
            (2, ms(400)),
            (3, ms(800)),
            (4, ms(1600)),
            (5, ms(3200)),
            (6, ms(6400)),
            (7, ms(12_800)),
            (8, ms(25_600)),
            (9, ms(30_000)),
            (10, ms(30_000)),
        ]
    );
    assert_eq!(next(&queue), (key(0), 11));
}

#[tokio::test(start_paused = true)]
async fn contended_postponements_wait_a_fixed_time() {
    let queue = queue();
    queue.offer(key(0), 1);
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(postpone(&queue, contended).await);
    }
    assert_eq!(seen, [(0, ms(250)), (1, ms(250)), (2, ms(250))]);
}

#[tokio::test(start_paused = true)]
async fn a_poke_does_not_shorten_a_postponement() {
    let queue = queue();
    queue.offer(key(0), 1);
    lease(&queue).contended("held");
    queue.offer(key(0), 1);
    assert_eq!(until(&queue), ms(250));
}

#[tokio::test(start_paused = true)]
async fn done_and_again_reset_postponements() {
    let settles: [(&str, Settle); 2] = [("done", Lease::done), ("again", Lease::again)];
    for (name, settle) in settles {
        let queue = queue();
        queue.offer(key(0), 1);
        postpone(&queue, failed).await;
        postpone(&queue, failed).await;
        let lease = lease(&queue);
        assert_eq!(lease.attempt(), 2, "{name}");
        queue.offer(key(0), 1);
        settle(lease);
        assert_eq!(next(&queue), (key(0), 0), "{name}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_key_is_stuck_after_stuck_after_postponements_until_done() {
    let queue = Prioritizer::new(
        "test",
        Policy {
            warn_after: 2,
            stuck_after: 3,
            ..Policy::default()
        },
    );
    queue.offer(key(0), 1);
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.push((postpone(&queue, contended).await, queue.stuck()));
    }
    assert_eq!(
        seen,
        [
            ((0, ms(250)), 0),
            ((1, ms(250)), 0),
            ((2, ms(30_000)), 1),
            ((3, ms(30_000)), 1),
        ]
    );
    lease(&queue).done();
    assert_eq!(queue.stuck(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_key_is_stuck_once_postponed_for_stuck_age() {
    let queue = Prioritizer::new(
        "test",
        Policy {
            failed_first: ms(400),
            failed_max: ms(10_000),
            stuck_after: 100,
            stuck_age: ms(1000),
            ..Policy::default()
        },
    );
    queue.offer(key(0), 1);
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push((postpone(&queue, failed).await, queue.stuck()));
    }
    // Postponed at 0, 400 ms and 1200 ms: the third is past the age.
    assert_eq!(
        seen,
        [((0, ms(400)), 0), ((1, ms(800)), 0), ((2, ms(10_000)), 1),]
    );
}

#[tokio::test(start_paused = true)]
async fn a_stuck_key_dropped_as_gone_is_no_longer_counted() {
    let queue = Prioritizer::new(
        "test",
        Policy {
            stuck_after: 1,
            ..Policy::default()
        },
    );
    queue.offer(key(0), 1);
    postpone(&queue, failed).await;
    assert_eq!(queue.stuck(), 1);
    lease(&queue).gone();
    assert_eq!((queue.stuck(), queue.peek()), (0, Due::Never));
}

#[tokio::test(start_paused = true)]
async fn peek_and_take_tell_when_the_next_key_is_due() {
    let queue = queue();
    assert_eq!(queue.peek(), Due::Never);
    assert!(is_empty(&queue));

    queue.offer(key(0), 1);
    assert_eq!(queue.peek(), Due::Now);

    let start = Instant::now();
    lease(&queue).contended("held");
    assert_eq!(queue.peek(), Due::At(start + ms(250)));
    assert!(matches!(queue.take(), Taken::Until(at) if at == start + ms(250)));

    // A leased key is not due.
    queue.offer(key(1), 1);
    let other = lease(&queue);
    assert_eq!(queue.peek(), Due::At(start + ms(250)));

    tokio::time::advance(ms(250)).await;
    assert_eq!(queue.peek(), Due::Now);
    assert_eq!(next(&queue), (key(0), 1));
    other.done();
    assert_eq!(queue.peek(), Due::Never);
}

#[tokio::test(start_paused = true)]
async fn a_key_becoming_pending_wakes_a_worker() {
    let queue = queue();
    let wakeup = queue.wakeup();
    let woken = || wakeup.notified().now_or_never().is_some();

    queue.offer(key(0), 1);
    assert!(woken(), "a new key");
    queue.offer(key(0), 1);
    assert!(!woken(), "an offer to a pending key");

    let leased = lease(&queue);
    queue.offer(key(0), 1);
    assert!(!woken(), "an offer to a leased key");
    leased.done();
    assert!(woken(), "a dirty key back to pending");

    queue.offer(key(1), 1);
    let _ = woken();
    let first = lease(&queue);
    assert!(
        woken(),
        "a take that leaves keys ready passes the wakeup on"
    );
    let second = lease(&queue);
    assert!(!woken(), "a take that leaves nothing");
    first.done();
    second.done();
}
