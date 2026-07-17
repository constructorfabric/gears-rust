use event_broker_sdk::{DbDeduplication, Producer, ProducerMode};

fn main() {
    let _ = Producer::builder()
        .deduplication(DbDeduplication::managed(ProducerMode::Chained).key("orders"));
}
