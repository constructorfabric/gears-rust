use event_broker_sdk::{DbProducer, DirectDeduplication};

fn main() {
    let _ = DbProducer::builder().deduplication(DirectDeduplication::stateless());
}
