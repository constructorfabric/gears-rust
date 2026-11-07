use event_broker_sdk::{DirectProducer, OutboxRegistration};

fn main() {
    let _ = DirectProducer::builder().registration(OutboxRegistration::Stateless);
}
