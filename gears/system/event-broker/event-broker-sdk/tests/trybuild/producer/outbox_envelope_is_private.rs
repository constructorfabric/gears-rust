use event_broker_sdk::producer::ProducerOutboxEnvelope;

fn main() {
    let _ = std::mem::size_of::<ProducerOutboxEnvelope>();
}
