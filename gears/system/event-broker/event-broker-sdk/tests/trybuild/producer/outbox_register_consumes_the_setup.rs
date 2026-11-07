use event_broker_sdk::OutboxSetup;
use toolkit_db::outbox::OutboxBuilder;

fn wire(setup: OutboxSetup, a: OutboxBuilder, b: OutboxBuilder) {
    let _ = setup.register(a);
    let _ = setup.start(b);
}

fn main() {}
