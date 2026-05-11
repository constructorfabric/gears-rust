//! Unit tests for the internal chain state tracker.
use event_broker_sdk::{ProducerId, internal_test_helpers::*};
use uuid::Uuid;

fn pid() -> ProducerId {
    ProducerId(Uuid::new_v4())
}

#[test]
fn peek_returns_zero_for_unknown() {
    let cs = chain_state_new();
    assert_eq!(chain_state_peek(&cs, pid(), "topic", 0), 0);
}

#[test]
fn advance_and_peek() {
    let cs = chain_state_new();
    let p = pid();
    chain_state_advance(&cs, p, "topic", 3, 5);
    assert_eq!(chain_state_peek(&cs, p, "topic", 3), 5);
    chain_state_advance(&cs, p, "topic", 3, 10);
    assert_eq!(chain_state_peek(&cs, p, "topic", 3), 10);
}

#[test]
fn bulk_prime_overwrites() {
    let cs = chain_state_new();
    let p = pid();
    chain_state_advance(&cs, p, "a", 0, 99);
    chain_state_bulk_prime(&cs, [(p, "a".to_owned(), 0, 1), (p, "b".to_owned(), 1, 2)]);
    assert_eq!(chain_state_peek(&cs, p, "a", 0), 1);
    assert_eq!(chain_state_peek(&cs, p, "b", 1), 2);
}

#[test]
fn reset_clears_key() {
    let cs = chain_state_new();
    let p = pid();
    chain_state_advance(&cs, p, "t", 0, 7);
    chain_state_reset(&cs, p, "t", 0);
    assert_eq!(chain_state_peek(&cs, p, "t", 0), 0);
}
