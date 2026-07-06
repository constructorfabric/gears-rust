use super::*;
use crate::infra::db::Migrator;
use sea_orm_migration::MigratorTrait;
use serde_json::json;
use time::Duration;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};

async fn buffer() -> SeaStreamEventBuffer {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("connect sqlite::memory:");
    toolkit_db::migration_runner::run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("apply migrations");
    SeaStreamEventBuffer::new(Arc::new(DBProvider::new(db)))
}

fn far_future() -> OffsetDateTime {
    OffsetDateTime::now_utc() + Duration::hours(1)
}

#[tokio::test]
async fn append_then_read_since_returns_ordered_tail() {
    let buf = buffer().await;
    let mid = Uuid::new_v4();
    for seq in 0..4u64 {
        buf.append(mid, seq, json!({ "seq": seq }), far_future())
            .await
            .expect("append");
    }
    // Full read.
    let all = buf.read_since(mid, None).await.expect("read all");
    assert_eq!(
        all.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    // Tail after seq 1.
    let tail = buf.read_since(mid, Some(1)).await.expect("read tail");
    assert_eq!(tail.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2, 3]);
    // Isolated per message.
    let other = buf
        .read_since(Uuid::new_v4(), None)
        .await
        .expect("read other");
    assert!(other.is_empty());
}

#[tokio::test]
async fn delete_expired_removes_only_past_ttl() {
    let buf = buffer().await;
    let mid = Uuid::new_v4();
    let past = OffsetDateTime::now_utc() - Duration::minutes(1);
    buf.append(mid, 0, json!({}), past).await.unwrap();
    buf.append(mid, 1, json!({}), far_future()).await.unwrap();
    let removed = buf.delete_expired(OffsetDateTime::now_utc()).await.unwrap();
    assert_eq!(removed, 1);
    let left = buf.read_since(mid, None).await.unwrap();
    assert_eq!(left.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1]);
}
