use event_broker_sdk::{DbDeduplication, Producer};

fn main() {
    let _ = Producer::builder()
        .deduplication(DbDeduplication::stateless());
}
