//! `SeaOrmFilesRepository` unit tests against SQLite `:memory:`.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use std::collections::BTreeMap;

    use file_storage_sdk::{FileMetaUpdate, FileStatus};
    use modkit_db::migration_runner::run_migrations_for_testing;
    use modkit_db::{ConnectOpts, DBProvider, connect_db};
    use sea_orm_migration::MigratorTrait;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::domain::etag::compose;
    use crate::domain::repo::{
        ChangeStatusOutcome, FilesRepo, InsertPendingArgs, ListFilesArgs, MutationOutcome,
    };
    use crate::infra::storage::migrations::Migrator;
    use crate::infra::storage::sea_orm_repo::SeaOrmFilesRepository;

    type Provider = DBProvider<modkit_db::DbError>;

    async fn build() -> (Arc<Provider>, SeaOrmFilesRepository) {
        let opts = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = connect_db("sqlite::memory:", opts)
            .await
            .expect("connect sqlite");
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("migrations");
        (Arc::new(DBProvider::new(db)), SeaOrmFilesRepository::new())
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
    }

    fn args(tenant_id: Uuid, file_id: Uuid, file_path: &str) -> InsertPendingArgs {
        InsertPendingArgs {
            file_id,
            tenant_id,
            backend_id: Uuid::nil(),
            file_path: file_path.to_owned(),
            owner_id: tenant_id,
            name: "f.bin".to_owned(),
            gts_file_type: "gts.cf.fstorage.file.type.v1~doc.v1~".to_owned(),
            mime_type: "application/octet-stream".to_owned(),
            etag_pinned: "etag-pending".to_owned(),
            upload_expires_at: None,
            custom_metadata_json: "{}".to_owned(),
            now: now(),
        }
    }

    /// Promote a row to `uploaded` and return the new etag.
    async fn promote_to_uploaded(
        repo: &SeaOrmFilesRepository,
        provider: &Provider,
        tenant_id: Uuid,
        file_id: Uuid,
        old_etag: &str,
        content_hash: &str,
    ) -> String {
        let conn = provider.conn().expect("conn");
        let outcome = repo
            .change_status_with_supersession(
                &conn,
                tenant_id,
                file_id,
                old_etag,
                FileStatus::Uploaded,
                content_hash,
                now(),
            )
            .await
            .expect("promote");
        match outcome {
            ChangeStatusOutcome::Applied(info) => info.etag,
            ChangeStatusOutcome::NoMatch => panic!("promotion did not match"),
        }
    }

    // ── insert_pending / get_by_id / get_by_id_system ────────────────────────

    #[tokio::test]
    async fn insert_pending_persists_row_with_pending_status() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        let info = repo
            .insert_pending(&conn, args(tid, fid, "p"))
            .await
            .expect("insert");
        assert_eq!(info.file_id, fid);
        assert_eq!(info.owner.tenant_id, tid);
        assert!(matches!(info.status, FileStatus::PendingUpload));
        assert_eq!(info.etag, "etag-pending");
    }

    #[tokio::test]
    async fn get_by_id_returns_row_for_owning_tenant() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        repo.insert_pending(&conn, args(tid, fid, "p")).await.unwrap();
        let row = repo.get_by_id(&conn, tid, fid).await.unwrap();
        assert!(row.is_some(), "row not found for owning tenant");
    }

    #[tokio::test]
    async fn get_by_id_returns_none_for_other_tenant() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let owner = Uuid::now_v7();
        let other = Uuid::now_v7();
        let fid = Uuid::now_v7();
        repo.insert_pending(&conn, args(owner, fid, "p"))
            .await
            .unwrap();
        let row = repo.get_by_id(&conn, other, fid).await.unwrap();
        assert!(row.is_none(), "cross-tenant lookup leaked row");
    }

    #[tokio::test]
    async fn get_by_id_system_finds_row_regardless_of_tenant() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        repo.insert_pending(&conn, args(tid, fid, "p")).await.unwrap();
        let row = repo.get_by_id_system(&conn, fid).await.unwrap();
        assert!(row.is_some(), "system lookup must find by id alone");
    }

    #[tokio::test]
    async fn get_persistence_fields_returns_revision_zero_for_pending() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        repo.insert_pending(&conn, args(tid, fid, "p")).await.unwrap();
        let pf = repo
            .get_persistence_fields(&conn, fid)
            .await
            .unwrap()
            .expect("fields");
        assert_eq!(pf.meta_revision, 0);
    }

    #[tokio::test]
    async fn get_persistence_fields_returns_none_for_unknown() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let pf = repo.get_persistence_fields(&conn, Uuid::now_v7()).await.unwrap();
        assert!(pf.is_none());
    }

    // ── update_metadata_etag_conditional — uploaded-only branches ────────────

    #[tokio::test]
    async fn update_metadata_no_match_when_status_pending() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        repo.insert_pending(&conn, args(tid, fid, "p")).await.unwrap();
        let upd = FileMetaUpdate {
            name: Some("renamed".to_owned()),
            mime_type: None,
            custom_metadata: None,
        };
        let outcome = repo
            .update_metadata_etag_conditional(&conn, tid, fid, "etag-pending", &upd, None, now())
            .await
            .unwrap();
        assert!(matches!(outcome, ChangeStatusOutcome::NoMatch));
    }

    #[tokio::test]
    async fn update_metadata_no_match_when_etag_stale() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        repo.insert_pending(&conn, args(tid, fid, "p")).await.unwrap();
        let etag = promote_to_uploaded(&repo, &provider, tid, fid, "etag-pending", "h").await;
        let upd = FileMetaUpdate {
            name: Some("renamed".to_owned()),
            mime_type: None,
            custom_metadata: None,
        };
        let stale = format!("{etag}-stale");
        let outcome = repo
            .update_metadata_etag_conditional(&conn, tid, fid, &stale, &upd, None, now())
            .await
            .unwrap();
        assert!(matches!(outcome, ChangeStatusOutcome::NoMatch));
    }

    #[tokio::test]
    async fn update_metadata_applies_all_fields_and_bumps_revision() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        repo.insert_pending(&conn, args(tid, fid, "p")).await.unwrap();
        let etag = promote_to_uploaded(&repo, &provider, tid, fid, "etag-pending", "h").await;

        let mut custom = BTreeMap::new();
        custom.insert("k".to_owned(), "v".to_owned());
        let upd = FileMetaUpdate {
            name: Some("renamed".to_owned()),
            mime_type: Some("image/png".to_owned()),
            custom_metadata: Some(custom),
        };
        let outcome = repo
            .update_metadata_etag_conditional(&conn, tid, fid, &etag, &upd, None, now())
            .await
            .unwrap();
        let info = match outcome {
            ChangeStatusOutcome::Applied(info) => info,
            ChangeStatusOutcome::NoMatch => panic!("expected applied"),
        };
        assert_eq!(info.meta.name, "renamed");
        assert_eq!(info.meta.mime_type, "image/png");
        assert_ne!(info.etag, etag, "etag must change after metadata bump");
    }

    // ── change_status_with_supersession ──────────────────────────────────────

    #[tokio::test]
    async fn change_status_returns_no_match_when_etag_stale() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        repo.insert_pending(&conn, args(tid, fid, "p")).await.unwrap();
        let outcome = repo
            .change_status_with_supersession(
                &conn,
                tid,
                fid,
                "wrong-etag",
                FileStatus::Uploaded,
                "h",
                now(),
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ChangeStatusOutcome::NoMatch));
    }

    #[tokio::test]
    async fn change_status_returns_no_match_for_unknown_id() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let outcome = repo
            .change_status_with_supersession(
                &conn,
                Uuid::now_v7(),
                Uuid::now_v7(),
                "etag",
                FileStatus::Uploaded,
                "h",
                now(),
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ChangeStatusOutcome::NoMatch));
    }

    // ── delete_etag_conditional ──────────────────────────────────────────────

    #[tokio::test]
    async fn delete_returns_applied_with_keys_when_match() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        let mut a = args(tid, fid, "p");
        let backend_id = Uuid::now_v7();
        a.backend_id = backend_id;
        repo.insert_pending(&conn, a).await.unwrap();
        let etag = promote_to_uploaded(&repo, &provider, tid, fid, "etag-pending", "h").await;

        let outcome = repo
            .delete_etag_conditional(&conn, tid, fid, &etag)
            .await
            .unwrap();
        assert!(matches!(outcome.outcome, MutationOutcome::Applied));
        assert_eq!(outcome.backend_id, Some(backend_id));
        assert!(repo.get_by_id(&conn, tid, fid).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_returns_no_match_when_etag_stale() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        repo.insert_pending(&conn, args(tid, fid, "p")).await.unwrap();
        promote_to_uploaded(&repo, &provider, tid, fid, "etag-pending", "h").await;

        let outcome = repo
            .delete_etag_conditional(&conn, tid, fid, "stale")
            .await
            .unwrap();
        assert!(matches!(outcome.outcome, MutationOutcome::NoMatch));
        assert!(outcome.backend_id.is_none());
    }

    #[tokio::test]
    async fn delete_returns_no_match_when_status_not_uploaded() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        repo.insert_pending(&conn, args(tid, fid, "p")).await.unwrap();
        let outcome = repo
            .delete_etag_conditional(&conn, tid, fid, "etag-pending")
            .await
            .unwrap();
        assert!(matches!(outcome.outcome, MutationOutcome::NoMatch));
    }

    #[tokio::test]
    async fn delete_returns_no_match_for_unknown_id() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let outcome = repo
            .delete_etag_conditional(&conn, Uuid::now_v7(), Uuid::now_v7(), "etag")
            .await
            .unwrap();
        assert!(matches!(outcome.outcome, MutationOutcome::NoMatch));
        assert!(outcome.backend_id.is_none());
    }

    // ── repair_etag_system_context ───────────────────────────────────────────

    #[tokio::test]
    async fn repair_etag_updates_when_match() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        repo.insert_pending(&conn, args(tid, fid, "p")).await.unwrap();
        let outcome = repo
            .repair_etag_system_context(&conn, fid, "etag-pending", "etag-repaired")
            .await
            .unwrap();
        assert!(matches!(outcome, MutationOutcome::Applied));
        let updated = repo.get_by_id_system(&conn, fid).await.unwrap().unwrap();
        assert_eq!(updated.etag, "etag-repaired");
    }

    #[tokio::test]
    async fn repair_etag_returns_no_match_when_stale() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        repo.insert_pending(&conn, args(tid, fid, "p")).await.unwrap();
        let outcome = repo
            .repair_etag_system_context(&conn, fid, "wrong-etag", "etag-x")
            .await
            .unwrap();
        assert!(matches!(outcome, MutationOutcome::NoMatch));
    }

    // ── list_paginated — filter / cursor branches ────────────────────────────

    #[tokio::test]
    async fn list_paginated_filters_by_owner_id() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let user_a = Uuid::now_v7();
        let user_b = Uuid::now_v7();

        let mut a1 = args(tid, Uuid::now_v7(), "a1");
        a1.owner_id = user_a;
        let mut a2 = args(tid, Uuid::now_v7(), "a2");
        a2.owner_id = user_a;
        let mut b1 = args(tid, Uuid::now_v7(), "b1");
        b1.owner_id = user_b;
        for ins in [a1.clone(), a2.clone(), b1.clone()] {
            repo.insert_pending(&conn, ins).await.unwrap();
        }

        let page = repo
            .list_paginated(
                &conn,
                ListFilesArgs {
                    tenant_id: tid,
                    owner_id: Some(user_a),
                    backend_id: None,
                    mime_type: None,
                    gts_file_type: None,
                    created_after: None,
                    created_before: None,
                    cursor: None,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 2);
        assert!(page.items.iter().all(|f| f.owner.owner_id == user_a));
        assert!(page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn list_paginated_returns_next_cursor_when_more_pages() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();

        for i in 0..4 {
            let mut a = args(tid, Uuid::now_v7(), &format!("p{i}"));
            a.now = OffsetDateTime::from_unix_timestamp(1_700_000_000 + i64::from(i)).unwrap();
            repo.insert_pending(&conn, a).await.unwrap();
        }

        let page = repo
            .list_paginated(
                &conn,
                ListFilesArgs {
                    tenant_id: tid,
                    owner_id: None,
                    backend_id: None,
                    mime_type: None,
                    gts_file_type: None,
                    created_after: None,
                    created_before: None,
                    cursor: None,
                    limit: 2,
                },
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 2);
        assert!(page.next_cursor.is_some());

        let page2 = repo
            .list_paginated(
                &conn,
                ListFilesArgs {
                    tenant_id: tid,
                    owner_id: None,
                    backend_id: None,
                    mime_type: None,
                    gts_file_type: None,
                    created_after: None,
                    created_before: None,
                    cursor: page.next_cursor,
                    limit: 2,
                },
            )
            .await
            .unwrap();
        assert_eq!(page2.items.len(), 1);
        assert!(page2.next_cursor.is_none());
    }

    #[tokio::test]
    async fn list_paginated_applies_mime_and_gts_and_backend_and_time_filters() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let primary_backend = Uuid::now_v7();
        let secondary_backend = Uuid::now_v7();

        // matching row
        let mut hit = args(tid, Uuid::now_v7(), "hit");
        hit.mime_type = "image/png".to_owned();
        hit.gts_file_type = "gts.cf.fstorage.file.type.v1~photo.v1~".to_owned();
        hit.backend_id = primary_backend;
        hit.now = OffsetDateTime::from_unix_timestamp(1_700_000_500).unwrap();

        let mut miss_mime = args(tid, Uuid::now_v7(), "missm");
        miss_mime.mime_type = "text/plain".to_owned();
        miss_mime.backend_id = primary_backend;

        let mut miss_backend = args(tid, Uuid::now_v7(), "missb");
        miss_backend.backend_id = secondary_backend;

        let mut miss_early = args(tid, Uuid::now_v7(), "misse");
        miss_early.now = OffsetDateTime::from_unix_timestamp(1_600_000_000).unwrap();
        miss_early.backend_id = primary_backend;

        for ins in [hit.clone(), miss_mime, miss_backend, miss_early] {
            repo.insert_pending(&conn, ins).await.unwrap();
        }

        let page = repo
            .list_paginated(
                &conn,
                ListFilesArgs {
                    tenant_id: tid,
                    owner_id: None,
                    backend_id: Some(primary_backend),
                    mime_type: Some("image/png".to_owned()),
                    gts_file_type: Some("gts.cf.fstorage.file.type.v1~photo.v1~".to_owned()),
                    created_after: Some(
                        OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
                    ),
                    created_before: Some(
                        OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap(),
                    ),
                    cursor: None,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1, "exactly the matching row");
        assert_eq!(page.items[0].meta.name, "f.bin");
    }

    #[tokio::test]
    async fn list_paginated_other_tenant_invisible() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let owner = Uuid::now_v7();
        let other = Uuid::now_v7();
        repo.insert_pending(&conn, args(owner, Uuid::now_v7(), "x"))
            .await
            .unwrap();

        let page = repo
            .list_paginated(
                &conn,
                ListFilesArgs {
                    tenant_id: other,
                    owner_id: None,
                    backend_id: None,
                    mime_type: None,
                    gts_file_type: None,
                    created_after: None,
                    created_before: None,
                    cursor: None,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert!(page.items.is_empty(), "cross-tenant rows must not leak");
    }

    /// Etag composition smoke.
    #[tokio::test]
    async fn promoted_etag_matches_compose_contract() {
        let (provider, repo) = build().await;
        let conn = provider.conn().unwrap();
        let tid = Uuid::now_v7();
        let fid = Uuid::now_v7();
        repo.insert_pending(&conn, args(tid, fid, "p")).await.unwrap();
        let etag = promote_to_uploaded(&repo, &provider, tid, fid, "etag-pending", "h").await;
        assert_eq!(etag, compose("h", 1));
    }
}
