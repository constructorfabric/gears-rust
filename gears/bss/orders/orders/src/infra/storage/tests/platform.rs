//! Owner-provided outbox exercised with the actual restricted Orders runtime role.
use super::*;
use toolkit_db::outbox::{
    LeasedMessageHandler, MessageResult, Outbox, OutboxMessage, Partitions, Record,
};

struct HoldForInspection;
#[async_trait::async_trait]
impl LeasedMessageHandler for HoldForInspection {
    async fn handle(&self, _: &OutboxMessage) -> MessageResult {
        MessageResult::Retry
    }
}

#[tokio::test]
async fn owner_outbox_enqueue_shares_runtime_transaction_and_rolls_back() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    let handle = Outbox::builder(db.clone())
        .queue("orders-storage-test", Partitions::of(1))
        .leased(HoldForInspection)
        .start()
        .await?;
    let outbox = handle.outbox().clone();
    let failed: anyhow::Result<()> = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let scope = toolkit_security::AccessScope::for_resources(vec![u(1)]);
                let row = repo::insert_order(tx, &scope, order(1)).await?;
                repo::LockedOrder::acquire(tx, &scope, &row)
                    .await?
                    .insert_order_version(version(1, 1, None))
                    .await?;
                outbox
                    .enqueue(
                        tx,
                        Record::to("orders-storage-test", 0)
                            .payload(b"rolled back".to_vec(), "text/plain")
                            .build()?,
                    )
                    .await?
                    .discard();
                anyhow::bail!("deliberate rollback after owner enqueue");
            })
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__order")
            .await?,
        0
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM toolkit_outbox_body")
            .await?,
        0
    );
    let outbox = handle.outbox().clone();
    toolkit_db::outbox::in_transaction(&db, move |tx| {
        Box::pin(async move {
            let scope = toolkit_security::AccessScope::for_resources(vec![u(1)]);
            let row = repo::insert_order(tx, &scope, order(1)).await?;
            repo::LockedOrder::acquire(tx, &scope, &row)
                .await?
                .insert_order_version(version(1, 1, None))
                .await?;
            let wake = outbox
                .enqueue(
                    tx,
                    Record::to("orders-storage-test", 0)
                        .payload(b"committed".to_vec(), "text/plain")
                        .build()?,
                )
                .await?;
            anyhow::Ok(((), wake))
        })
    })
    .await?;
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__order")
            .await?,
        1
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM toolkit_outbox_body")
            .await?,
        1
    );
    let (_, private) = pg.role("private").await?;
    assert!(
        private
            .execute_unprepared(
                "INSERT INTO toolkit_outbox_body(payload,payload_type) VALUES('x','text/plain')"
            )
            .await
            .is_err()
    );
    handle.stop().await;
    Ok(())
}
