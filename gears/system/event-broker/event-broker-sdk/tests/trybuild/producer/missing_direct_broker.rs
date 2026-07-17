use event_broker_sdk::{DirectDeduplication, Producer, ProducerIdentity};

fn main() {
    let _ = Producer::builder()
        .security_context(toolkit_security::SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("orders"))
        .deduplication(DirectDeduplication::stateless())
        .topics(["orders"])
        .event_type_patterns(["orders.*"])
        .build();
}
