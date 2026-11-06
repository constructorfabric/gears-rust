use event_broker_sdk::{DbDeduplication, DbProducer, ProducerMode};

fn main() {
    let _ = DbProducer::builder().deduplication(DbDeduplication::managed(ProducerMode::Chained));
}
