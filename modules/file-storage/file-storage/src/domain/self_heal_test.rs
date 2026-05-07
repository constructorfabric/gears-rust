#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use file_storage_sdk::{FileInfo, FileMetaUpdate, FileStatus};
    use modkit_db::secure::DBRunner;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::domain::error::DomainError;
    use crate::domain::etag::compose;
    use crate::domain::repo::{
        ChangeStatusOutcome, DeleteOutcome, FilesRepo, InsertPendingArgs, ListFilesArgs,
        ListFilesPage, MutationOutcome, PersistenceFields,
    };
    use crate::domain::self_heal::{SelfHealOutcome, sync_etag_from_backend};

    /// Hand-rolled FilesRepo. The only method exercised by the self-heal
    /// primitive is `repair_etag_system_context`, but the trait is wide so
    /// every method must be defined — others are unimplemented!.
    struct FakeRepo {
        repair_outcome: Mutex<MutationOutcome>,
        repair_calls: Mutex<u32>,
        last_old_etag: Mutex<String>,
        last_new_etag: Mutex<String>,
    }

    impl FakeRepo {
        fn new(outcome: MutationOutcome) -> Self {
            Self {
                repair_outcome: Mutex::new(outcome),
                repair_calls: Mutex::new(0),
                last_old_etag: Mutex::new(String::new()),
                last_new_etag: Mutex::new(String::new()),
            }
        }
        fn calls(&self) -> u32 {
            *self.repair_calls.lock().unwrap()
        }
        fn last_etags(&self) -> (String, String) {
            (
                self.last_old_etag.lock().unwrap().clone(),
                self.last_new_etag.lock().unwrap().clone(),
            )
        }
    }

    #[async_trait]
    impl FilesRepo for FakeRepo {
        async fn insert_pending<C: DBRunner>(
            &self,
            _r: &C,
            _a: InsertPendingArgs,
        ) -> Result<FileInfo, DomainError> {
            unimplemented!()
        }
        async fn get_by_id<C: DBRunner>(
            &self,
            _r: &C,
            _t: Uuid,
            _id: Uuid,
        ) -> Result<Option<FileInfo>, DomainError> {
            unimplemented!()
        }
        async fn get_by_id_system<C: DBRunner>(
            &self,
            _r: &C,
            _id: Uuid,
        ) -> Result<Option<FileInfo>, DomainError> {
            unimplemented!()
        }
        async fn get_persistence_fields<C: DBRunner>(
            &self,
            _r: &C,
            _id: Uuid,
        ) -> Result<Option<PersistenceFields>, DomainError> {
            unimplemented!()
        }
        async fn update_metadata_etag_conditional<C: DBRunner>(
            &self,
            _r: &C,
            _t: Uuid,
            _id: Uuid,
            _e: &str,
            _u: &FileMetaUpdate,
            _h: Option<&str>,
            _now: OffsetDateTime,
        ) -> Result<ChangeStatusOutcome, DomainError> {
            unimplemented!()
        }
        async fn change_status_with_supersession<C: DBRunner>(
            &self,
            _r: &C,
            _t: Uuid,
            _id: Uuid,
            _e: &str,
            _target: FileStatus,
            _h: &str,
            _now: OffsetDateTime,
        ) -> Result<ChangeStatusOutcome, DomainError> {
            unimplemented!()
        }
        async fn delete_etag_conditional<C: DBRunner>(
            &self,
            _r: &C,
            _t: Uuid,
            _id: Uuid,
            _e: &str,
        ) -> Result<DeleteOutcome, DomainError> {
            unimplemented!()
        }
        async fn repair_etag_system_context<C: DBRunner>(
            &self,
            _r: &C,
            _id: Uuid,
            old_etag: &str,
            new_etag: &str,
        ) -> Result<MutationOutcome, DomainError> {
            *self.repair_calls.lock().unwrap() += 1;
            *self.last_old_etag.lock().unwrap() = old_etag.to_owned();
            *self.last_new_etag.lock().unwrap() = new_etag.to_owned();
            Ok(*self.repair_outcome.lock().unwrap())
        }
        async fn list_paginated<C: DBRunner>(
            &self,
            _r: &C,
            _a: ListFilesArgs,
        ) -> Result<ListFilesPage, DomainError> {
            unimplemented!()
        }
    }

    /// Spin up a SQLite in-memory DBRunner-compatible connection. The
    /// in-source module wires a real DBRunner implementor (DbConn).
    async fn make_runner() -> modkit_db::Db {
        use modkit_db::{ConnectOpts, connect_db};
        let opts = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        connect_db("sqlite::memory:", opts)
            .await
            .expect("connect in-memory sqlite")
    }

    // ── No-op when row.etag already matches derived ─────────────────────────

    #[tokio::test]
    async fn already_consistent_when_derived_matches_current() {
        let derived = compose("hash-x", 5);
        let repo = FakeRepo::new(MutationOutcome::Applied);
        let db = make_runner().await;
        let conn = db.conn().expect("get conn");

        let outcome = sync_etag_from_backend(&repo, &conn, Uuid::new_v4(), &derived, "hash-x", 5)
            .await
            .unwrap();

        assert!(matches!(outcome, SelfHealOutcome::AlreadyConsistent));
        assert_eq!(repo.calls(), 0, "repair UPDATE must NOT fire when consistent");
    }

    // ── Repaired when divergent + Applied ───────────────────────────────────

    #[tokio::test]
    async fn repaired_when_diverged_and_repair_succeeds() {
        let current = "stale-etag".to_owned();
        let repo = FakeRepo::new(MutationOutcome::Applied);
        let db = make_runner().await;
        let conn = db.conn().expect("get conn");

        let outcome =
            sync_etag_from_backend(&repo, &conn, Uuid::new_v4(), &current, "fresh-hash", 9)
                .await
                .unwrap();

        match outcome {
            SelfHealOutcome::Repaired { derived } => {
                assert_eq!(derived, compose("fresh-hash", 9));
                let (old, new) = repo.last_etags();
                assert_eq!(old, "stale-etag");
                assert_eq!(new, derived);
            }
            other => panic!("expected Repaired, got: {other:?}"),
        }
        assert_eq!(repo.calls(), 1);
    }

    // ── Raced when divergent + NoMatch ──────────────────────────────────────

    #[tokio::test]
    async fn raced_when_repair_loses_to_concurrent_writer() {
        let current = "stale-etag".to_owned();
        let repo = FakeRepo::new(MutationOutcome::NoMatch);
        let db = make_runner().await;
        let conn = db.conn().expect("get conn");

        let outcome =
            sync_etag_from_backend(&repo, &conn, Uuid::new_v4(), &current, "fresh-hash", 1)
                .await
                .unwrap();

        assert!(matches!(outcome, SelfHealOutcome::Raced));
        assert_eq!(repo.calls(), 1, "repair was attempted but lost the race");
    }
}
