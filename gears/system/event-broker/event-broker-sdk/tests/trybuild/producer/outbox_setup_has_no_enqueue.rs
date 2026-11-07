use event_broker_sdk::{EventV1, OutboxSetup};
use toolkit_db::secure::DbConn;

async fn enqueue(setup: &OutboxSetup, conn: &DbConn<'_>, event: EventV1) {
    let _ = setup.enqueue(conn, event).await;
}

fn main() {}
