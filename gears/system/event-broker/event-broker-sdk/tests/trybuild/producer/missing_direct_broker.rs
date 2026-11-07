use event_broker_sdk::DirectProducer;

fn main() {
    let _ = DirectProducer::builder()
        .security_context(toolkit_security::SecurityContext::anonymous())
        .source("orders")
        .topics(["orders"])
        .event_type_patterns(["orders.*"])
        .build();
}
