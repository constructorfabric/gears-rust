//! The batch run rules of feature 0001 §5.2: how a request splits into runs
//! (R3's unit), each run's own shape (R3), and each run against its chain's
//! committed head (R4 chained, R5 monotonic). Every case asserts the whole
//! value the domain hands to infra.

use toolkit_gts::GtsInstanceId;
use uuid::Uuid;

use super::{ChainHead, ChainRun, ProducerChainCheck, PublishEnqueue, RunDecision, RunEvent};
use crate::domain::ingest::ProducerMode;

const P: Uuid = Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_0a01);
const Q: Uuid = Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_0a02);

fn topic() -> GtsInstanceId {
    GtsInstanceId::try_new("gts.cf.core.events.topic.v1~x.eb.runs.topic.v1").expect("valid id")
}

fn chained(producer_id: Uuid, previous: i64, sequence: i64) -> PublishEnqueue {
    PublishEnqueue::builder(0, b"payload".to_vec(), "application/json")
        .chain(ProducerChainCheck {
            producer_id,
            topic: topic(),
            previous,
            sequence,
            mode: ProducerMode::Chained,
        })
        .build()
}

fn stateless() -> PublishEnqueue {
    PublishEnqueue::builder(0, b"payload".to_vec(), "application/json").build()
}

/// A chained run of `(previous, sequence)` pairs, indexed from 0.
fn chained_run(links: &[(i64, i64)]) -> ChainRun {
    ChainRun {
        producer_id: P,
        topic: topic(),
        mode: ProducerMode::Chained,
        events: links
            .iter()
            .enumerate()
            .map(|(index, &(previous, sequence))| RunEvent {
                index,
                previous,
                sequence,
            })
            .collect(),
    }
}

/// A monotonic run of sequences, indexed from 0; `previous` is collapsed to 0
/// as the ingest path does.
fn monotonic_run(sequences: &[i64]) -> ChainRun {
    ChainRun {
        producer_id: P,
        topic: topic(),
        mode: ProducerMode::Monotonic,
        events: sequences
            .iter()
            .enumerate()
            .map(|(index, &sequence)| RunEvent {
                index,
                previous: 0,
                sequence,
            })
            .collect(),
    }
}

// -- runs --

#[test]
fn a_batch_splits_into_one_run_per_producer_in_request_order_keeping_batch_indexes() {
    let requests = vec![
        chained(Q, 4, 5),
        stateless(),
        chained(P, 0, 1),
        chained(Q, 5, 6),
        chained(P, 1, 2),
    ];

    assert_eq!(
        ChainRun::split(&requests),
        vec![
            ChainRun {
                producer_id: Q,
                topic: topic(),
                mode: ProducerMode::Chained,
                events: vec![
                    RunEvent {
                        index: 0,
                        previous: 4,
                        sequence: 5
                    },
                    RunEvent {
                        index: 3,
                        previous: 5,
                        sequence: 6
                    },
                ],
            },
            ChainRun {
                producer_id: P,
                topic: topic(),
                mode: ProducerMode::Chained,
                events: vec![
                    RunEvent {
                        index: 2,
                        previous: 0,
                        sequence: 1
                    },
                    RunEvent {
                        index: 4,
                        previous: 1,
                        sequence: 2
                    },
                ],
            },
        ]
    );
}

#[test]
fn a_batch_of_only_stateless_events_has_no_runs() {
    assert_eq!(ChainRun::split(&[stateless(), stateless()]), vec![]);
}

// -- R3: in-run shape --

#[test]
fn r3_a_linked_chained_run_is_well_formed() {
    assert_eq!(
        chained_run(&[(9, 10), (10, 11), (11, 12)]).first_malformed(),
        None
    );
}

#[test]
fn r3_a_chained_link_that_skips_the_prior_sequence_names_that_event() {
    assert_eq!(chained_run(&[(0, 1), (2, 3)]).first_malformed(), Some(1));
}

#[test]
fn r3_a_chained_event_whose_sequence_does_not_pass_its_previous_names_that_event() {
    assert_eq!(chained_run(&[(3, 3)]).first_malformed(), Some(0));
    assert_eq!(chained_run(&[(0, 1), (1, 1)]).first_malformed(), Some(1));
}

#[test]
fn r3_the_first_offending_event_is_named_when_several_offend() {
    assert_eq!(
        chained_run(&[(0, 1), (1, 2), (5, 6), (9, 9)]).first_malformed(),
        Some(2)
    );
}

#[test]
fn r3_a_strictly_increasing_monotonic_run_is_well_formed_with_gaps() {
    assert_eq!(monotonic_run(&[4, 5, 9, 12]).first_malformed(), None);
}

#[test]
fn r3_a_monotonic_run_that_repeats_or_falls_back_names_that_event() {
    assert_eq!(monotonic_run(&[4, 5, 5]).first_malformed(), Some(2));
    assert_eq!(monotonic_run(&[4, 7, 6]).first_malformed(), Some(2));
}

#[test]
fn r3_the_index_named_is_the_batch_position_not_the_run_position() {
    let requests = vec![chained(Q, 0, 1), chained(P, 0, 1), chained(P, 2, 3)];
    let malformed: Vec<Option<usize>> = ChainRun::split(&requests)
        .iter()
        .map(ChainRun::first_malformed)
        .collect();
    assert_eq!(malformed, vec![None, Some(2)]);
}

// -- R4: chained run vs the committed head --

#[test]
fn r4_an_absent_head_lets_the_first_event_take_its_numbers() {
    assert_eq!(
        chained_run(&[(9, 10), (10, 11)]).decide(ChainHead::Absent),
        RunDecision::Advance {
            duplicates: 0,
            head: 11
        }
    );
}

#[test]
fn r4_a_run_whose_first_event_links_to_the_head_advances_to_its_last_sequence() {
    assert_eq!(
        chained_run(&[(9, 10), (10, 11), (11, 12)]).decide(ChainHead::At(9)),
        RunDecision::Advance {
            duplicates: 0,
            head: 12
        }
    );
}

#[test]
fn r4_a_run_whose_last_event_is_the_head_is_a_whole_duplicate() {
    assert_eq!(
        chained_run(&[(9, 10), (10, 11), (11, 12)]).decide(ChainHead::At(12)),
        RunDecision::Duplicate
    );
}

#[test]
fn r4_a_head_on_the_first_event_makes_a_one_event_duplicate_prefix() {
    assert_eq!(
        chained_run(&[(9, 10), (10, 11), (11, 12)]).decide(ChainHead::At(10)),
        RunDecision::Advance {
            duplicates: 1,
            head: 12
        }
    );
}

#[test]
fn r4_a_head_past_the_first_event_makes_a_longer_duplicate_prefix() {
    assert_eq!(
        chained_run(&[(9, 10), (10, 11), (11, 12)]).decide(ChainHead::At(11)),
        RunDecision::Advance {
            duplicates: 2,
            head: 12
        }
    );
}

#[test]
fn r4_a_run_behind_the_head_is_a_violation_carrying_the_committed_head() {
    assert_eq!(
        chained_run(&[(9, 10), (10, 11), (11, 12)]).decide(ChainHead::At(20)),
        RunDecision::SequenceViolation { last_sequence: 20 }
    );
}

#[test]
fn r4_a_run_that_does_not_reach_back_to_the_head_is_a_violation() {
    assert_eq!(
        chained_run(&[(9, 10), (10, 11)]).decide(ChainHead::At(5)),
        RunDecision::SequenceViolation { last_sequence: 5 }
    );
}

#[test]
fn r4_a_single_event_run_advances_duplicates_or_violates_on_its_own_numbers() {
    assert_eq!(
        chained_run(&[(7, 8)]).decide(ChainHead::At(7)),
        RunDecision::Advance {
            duplicates: 0,
            head: 8
        }
    );
    assert_eq!(
        chained_run(&[(7, 8)]).decide(ChainHead::At(8)),
        RunDecision::Duplicate
    );
    assert_eq!(
        chained_run(&[(3, 4)]).decide(ChainHead::At(8)),
        RunDecision::SequenceViolation { last_sequence: 8 }
    );
}

#[test]
fn r4_a_single_event_linking_to_the_head_without_moving_past_it_is_a_violation() {
    // A single publish is not gated by R3, so the decision itself refuses a
    // link that would move the head backwards.
    assert_eq!(
        chained_run(&[(8, 5)]).decide(ChainHead::At(8)),
        RunDecision::SequenceViolation { last_sequence: 8 }
    );
}

// -- R5: monotonic run vs the committed head --

#[test]
fn r5_an_absent_head_lets_the_whole_monotonic_run_advance() {
    assert_eq!(
        monotonic_run(&[4, 5, 6]).decide(ChainHead::Absent),
        RunDecision::Advance {
            duplicates: 0,
            head: 6
        }
    );
}

#[test]
fn r5_leading_events_at_or_below_the_head_are_a_duplicate_prefix() {
    assert_eq!(
        monotonic_run(&[4, 5, 6, 7]).decide(ChainHead::At(5)),
        RunDecision::Advance {
            duplicates: 2,
            head: 7
        }
    );
}

#[test]
fn r5_a_run_entirely_past_the_head_advances_with_no_duplicates() {
    assert_eq!(
        monotonic_run(&[6, 9]).decide(ChainHead::At(5)),
        RunDecision::Advance {
            duplicates: 0,
            head: 9
        }
    );
}

#[test]
fn r5_a_run_entirely_at_or_below_the_head_is_a_whole_duplicate() {
    assert_eq!(
        monotonic_run(&[3, 4, 5]).decide(ChainHead::At(5)),
        RunDecision::Duplicate
    );
}

#[test]
fn r5_a_single_event_monotonic_run_advances_or_duplicates_and_never_violates() {
    assert_eq!(
        monotonic_run(&[6]).decide(ChainHead::At(5)),
        RunDecision::Advance {
            duplicates: 0,
            head: 6
        }
    );
    assert_eq!(
        monotonic_run(&[2]).decide(ChainHead::At(5)),
        RunDecision::Duplicate
    );
}
