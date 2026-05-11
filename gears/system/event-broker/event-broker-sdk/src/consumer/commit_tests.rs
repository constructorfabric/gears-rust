use std::sync::atomic::Ordering;

use super::CommitHandle;

#[tokio::test]
async fn commit_handle_marks_offset_processed_for_async_commit_path() {
    let handle = CommitHandle::new(7, 42);

    assert!(!handle.committed.load(Ordering::Acquire));
    handle.commit().await.expect("commit should mark processed");

    assert!(handle.committed.load(Ordering::Acquire));
    assert_eq!(handle.partition, 7);
    assert_eq!(handle.offset, 42);
}

#[cfg(feature = "db")]
mod tx {
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use uuid::Uuid;

    use crate::consumer::{
        CommitOffsetInTx, Fallback, OffsetManagerError, OffsetStore, ResolvedPosition,
        TxCommitHandle,
    };
    use crate::ids::{ConsumerGroupId, TopicId};

    #[derive(Default)]
    struct RecordingTxOffsetManager {
        calls: Mutex<Vec<(ConsumerGroupId, TopicId, u32, i64)>>,
    }

    #[async_trait]
    impl OffsetStore for RecordingTxOffsetManager {
        async fn load_position(
            &self,
            _group: &ConsumerGroupId,
            _topic: &TopicId,
            _partition: u32,
        ) -> Result<ResolvedPosition, OffsetManagerError> {
            Ok(Fallback::Earliest.into())
        }
    }

    #[async_trait]
    impl CommitOffsetInTx for RecordingTxOffsetManager {
        async fn commit_in_tx<TX>(
            &self,
            _txn: &TX,
            group: &ConsumerGroupId,
            topic: &TopicId,
            partition: u32,
            offset: i64,
        ) -> Result<(), OffsetManagerError>
        where
            TX: toolkit_db::secure::DBRunner + Sync,
        {
            self.calls
                .lock()
                .expect("recording mutex")
                .push((*group, *topic, partition, offset));
            Ok(())
        }
    }

    #[tokio::test]
    async fn tx_commit_handle_persists_offset_and_marks_auto_commit_suppressed() {
        let db = toolkit_db::connect_db(
            "sqlite::memory:",
            toolkit_db::ConnectOpts {
                max_conns: Some(1),
                ..toolkit_db::ConnectOpts::default()
            },
        )
        .await
        .expect("sqlite db");

        let manager = Arc::new(RecordingTxOffsetManager::default());
        let group = ConsumerGroupId::new(Uuid::new_v4());
        let topic = TopicId::new(Uuid::new_v4());
        let handle = TxCommitHandle::new(5, 123, manager.clone(), group, topic);
        let committed = handle.committed.clone();

        assert!(!committed.load(Ordering::Acquire));
        db.transaction_ref(move |tx| {
            Box::pin(async move {
                handle
                    .commit_in_tx(tx)
                    .await
                    .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
                Ok(())
            })
        })
        .await
        .expect("transaction");

        assert!(committed.load(Ordering::Acquire));
        assert_eq!(
            manager.calls.lock().expect("recording mutex").as_slice(),
            &[(group, topic, 5, 123)]
        );
    }
}
