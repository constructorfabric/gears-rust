use toolkit_gts::gts_id;
use uuid::Uuid;

use crate::api::ProducerMode;
use crate::ids::ProducerId;

use super::direct::{ProducerSequenceCursor, advance_cursor, producer_meta_for_last};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.sdk.producer.unit_orders.v1");
const PARTITION: u32 = 2;

#[test]
fn monotonic_batch_advances_sequence_between_prepared_events() {
    let second = second_batch_meta(ProducerMode::Monotonic);

    assert_eq!(second.sequence, Some(1));
    assert_eq!(second.previous, None);
}

#[test]
fn chained_batch_advances_previous_between_prepared_events() {
    let second = second_batch_meta(ProducerMode::Chained);

    assert_eq!(second.sequence, Some(1));
    assert_eq!(second.previous, Some(0));
}

fn second_batch_meta(mode: ProducerMode) -> crate::models::ProducerMeta {
    let producer_id = ProducerId(Uuid::from_u128(1));
    let first = producer_meta_for_last(mode, producer_id, -1, PARTITION).unwrap();
    let mut cursor = ProducerSequenceCursor::new();
    advance_cursor(&mut cursor, TOPIC, PARTITION, Some(&first));
    let last = cursor
        .get(&(producer_id, TOPIC.to_owned(), PARTITION))
        .copied()
        .unwrap();

    producer_meta_for_last(mode, producer_id, last, PARTITION).unwrap()
}
