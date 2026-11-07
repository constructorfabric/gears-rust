use event_broker_sdk::{OutboxRegistration, OutboxSetup};

async fn build(db: toolkit_db::Db, broker: std::sync::Arc<dyn event_broker_sdk::EventBrokerApi>) {
    let _ = OutboxSetup::builder()
        .broker(broker)
        .db(db)
        .security_context(toolkit_security::SecurityContext::anonymous())
        .source("orders")
        .registration(OutboxRegistration::Stateless)
        .topics(["orders"])
        .event_type_patterns(["orders.*"])
        .build()
        .await;
}

fn main() {}
