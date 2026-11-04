//! Tests for the category service's decidable rules.
//!
//! # Why this file is small
//!
//! Every service operation takes a [`DBRunner`](toolkit_db::secure::DBRunner),
//! and `toolkit-db` exposes no public constructor for one — `Db`, `SecureConn`
//! and `DbConn` are all sealed so a raw database handle cannot escape the
//! framework. That is a deliberate security property, and its consequence is
//! that a gear cannot drive its own services from a unit test, even against a
//! stub repository. The one exception is the `foreign_key_path` module, which
//! drives the service over the harness's real connection with a fake
//! repository -- the connection is only passed through, never constructed here.
//!
//! So the rules that can be stated without a connection are extracted and
//! pinned here. The orchestration around them — that the precondition is
//! evaluated before the orphan guard, that a refused delete never reaches the
//! repository — is exercised by the E2E suite against a real database, and is
//! recorded in the FEATURE's acceptance criteria rather than here.

use crate::domain::error::DomainError;

#[test]
fn select_is_refused_rather_than_ignored() {
    // A caller whose projection was silently dropped receives every field
    // believing it asked for two -- the same failure the declared filter
    // surface exists to prevent.
    let query = toolkit_odata::ODataQuery {
        select: Some(vec!["key".to_owned(), "name".to_owned()]),
        ..Default::default()
    };
    match crate::domain::odata::reject_unsupported_options(&query, "categories") {
        Err(DomainError::Validation { field, code, .. }) => {
            assert_eq!(field, "$select");
            assert_eq!(code, crate::field::ODATA_UNSUPPORTED_OPTION);
        }
        other => panic!("expected $select to be refused, got {other:?}"),
    }
}

#[test]
fn a_query_without_select_is_accepted() {
    let query = toolkit_odata::ODataQuery::default();
    assert!(crate::domain::odata::reject_unsupported_options(&query, "categories").is_ok());
}

mod transactional {
    //! The mutation and its record share one commit, over a real database.

    use std::sync::Arc;

    use toolkit_odata::ODataQuery;
    use toolkit_security::{AccessScope, SecurityContext};
    use uuid::Uuid;

    use crate::audit::AuditOperation;
    use crate::domain::category::service::Actor;
    use crate::domain::category::{
        CategoryDraft, CategoryKey, CategoryPatch, CategoryRepository, CategoryService, Patch,
    };
    use crate::domain::error::DomainError;
    use crate::infra::storage::audit_store::AuditStore;
    use crate::infra::storage::category_repo::CategoryRepo;
    use crate::test_support::{FailingSink, sqlite_provider};

    fn draft(slug: &str) -> CategoryDraft {
        CategoryDraft {
            key: CategoryKey::parse(slug).expect("slug"),
            name: slug.to_owned(),
            description: None,
            domain_affinity: None,
            sort_order: 0,
            icon: None,
        }
    }

    #[tokio::test]
    async fn the_row_write_itself_is_conditional_on_the_version_the_tag_was_compared_against() {
        let db = sqlite_provider().await;
        let conn = db.conn().expect("connection");
        let all = AccessScope::allow_all();
        let created = CategoryRepo
            .insert(&conn, &all, draft("network"))
            .await
            .expect("inserts");
        let stale = created.updated_at - time::Duration::seconds(1);
        let patch = || CategoryPatch {
            name: Some("Networking".to_owned()),
            description: Patch::Keep,
            domain_affinity: Patch::Keep,
            sort_order: Some(0),
            icon: Patch::Keep,
        };

        // The comparison ran against a read; a row that moved since finds no
        // match at the write, and the writer gets the same `412` a stale tag
        // gets.
        let refused = CategoryRepo
            .update(&conn, &all, created.id, patch(), stale)
            .await
            .expect_err("moved");
        assert!(
            matches!(refused, DomainError::PreconditionFailed { .. }),
            "{refused:?}"
        );
        let refused = CategoryRepo
            .delete(&conn, &all, created.id, stale)
            .await
            .expect_err("moved");
        assert!(
            matches!(refused, DomainError::PreconditionFailed { .. }),
            "{refused:?}"
        );
        let kept = CategoryRepo
            .find(&conn, &all, created.id)
            .await
            .expect("lookup")
            .expect("row");
        assert_eq!(kept.name, "network", "untouched");

        // At the version read, the write lands and the tag moves with it.
        let updated = CategoryRepo
            .update(&conn, &all, created.id, patch(), created.updated_at)
            .await
            .expect("current version");
        assert_ne!(updated.etag, created.etag);
        CategoryRepo
            .delete(&conn, &all, updated.id, updated.updated_at)
            .await
            .expect("current version");
    }

    #[tokio::test]
    async fn a_category_mutation_leaves_exactly_one_record_in_the_same_commit() {
        let db = sqlite_provider().await;
        let root = Uuid::new_v4();
        let svc = Arc::new(CategoryService::new(CategoryRepo, AuditStore));
        let ctx = SecurityContext::anonymous();
        let created = db
            .db()
            .transaction_ref_mapped::<_, _, DomainError>(|tx| {
                let svc = Arc::clone(&svc);
                let ctx = ctx.clone();
                Box::pin(async move {
                    svc.create(
                        tx,
                        &AccessScope::allow_all(),
                        draft("network"),
                        Actor {
                            ctx: &ctx,
                            request_id: "req-1",
                        },
                    )
                    .await
                })
            })
            .await
            .expect("creates");

        let conn = db.conn().expect("connection");
        let page = AuditStore
            .history(
                &conn,
                &AccessScope::allow_all(),
                created.key.as_str(),
                root,
                &ODataQuery::default(),
            )
            .await
            .expect("history");
        assert_eq!(page.items.len(), 1);
        let record = &page.items[0];
        assert_eq!(record.operation, AuditOperation::Create);
        assert_eq!(
            record.tenant_id, None,
            "a category is platform-wide and sits at no scope, so it borrows none"
        );
        assert_eq!(record.request_id, "req-1");
        assert!(record.pre_image.is_none() && record.post_image.is_some());
    }

    #[tokio::test]
    async fn a_record_that_cannot_be_written_rolls_the_mutation_back() {
        let db = sqlite_provider().await;
        let svc = Arc::new(CategoryService::new(CategoryRepo, FailingSink));
        let ctx = SecurityContext::anonymous();
        let outcome = db
            .db()
            .transaction_ref_mapped::<_, _, DomainError>(|tx| {
                let svc = Arc::clone(&svc);
                let ctx = ctx.clone();
                Box::pin(async move {
                    svc.create(
                        tx,
                        &AccessScope::allow_all(),
                        draft("network"),
                        Actor {
                            ctx: &ctx,
                            request_id: "req-2",
                        },
                    )
                    .await
                })
            })
            .await;
        assert!(
            matches!(outcome, Err(DomainError::Unavailable { .. })),
            "{outcome:?}"
        );

        // Neither the row nor the record: the change the platform could not
        // record never took effect.
        let conn = db.conn().expect("connection");
        let found = CategoryRepo
            .find_by_key(
                &conn,
                &AccessScope::allow_all(),
                &CategoryKey::parse("network").expect("slug"),
            )
            .await
            .expect("lookup");
        assert!(found.is_none());
    }
}

mod foreign_key_path {
    //! The foreign key refuses a delete the advisory check let through; the
    //! service passes the refusal on and asks nothing more of the refused
    //! transaction -- the recount belongs after the rollback.

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use toolkit_db::secure::DBRunner;
    use toolkit_odata::ODataQuery;

    use crate::domain::odata::Listing;
    use toolkit_security::{AccessScope, SecurityContext};
    use uuid::Uuid;

    use crate::domain::category::service::Actor;
    use crate::domain::category::visibility::DomainVisibility;
    use crate::domain::category::{
        Category, CategoryDraft, CategoryKey, CategoryPatch, CategoryRepository, CategoryService,
    };
    use crate::domain::error::DomainError;
    use crate::domain::precondition::ETag;
    use crate::test_support::{RecordingAudit, ResolutionHarness};

    /// Answers 0 to the first count (the pre-check passes), 3 afterwards, and
    /// refuses the delete the way `ON DELETE RESTRICT` does.
    struct RacedRepo {
        category: Category,
        counts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl CategoryRepository for RacedRepo {
        async fn find<C: DBRunner>(
            &self,
            _conn: &C,
            _scope: &AccessScope,
            _id: Uuid,
        ) -> Result<Option<Category>, DomainError> {
            Ok(Some(self.category.clone()))
        }

        async fn find_by_key<C: DBRunner>(
            &self,
            _conn: &C,
            _scope: &AccessScope,
            _key: &CategoryKey,
        ) -> Result<Option<Category>, DomainError> {
            unimplemented!()
        }

        async fn insert<C: DBRunner>(
            &self,
            _conn: &C,
            _scope: &AccessScope,
            _draft: CategoryDraft,
        ) -> Result<Category, DomainError> {
            unimplemented!()
        }

        async fn update<C: DBRunner>(
            &self,
            _conn: &C,
            _scope: &AccessScope,
            _id: Uuid,
            _patch: CategoryPatch,
            _expected: time::OffsetDateTime,
        ) -> Result<Category, DomainError> {
            unimplemented!()
        }

        async fn delete<C: DBRunner>(
            &self,
            _conn: &C,
            _scope: &AccessScope,
            _id: Uuid,
            _expected: time::OffsetDateTime,
        ) -> Result<(), DomainError> {
            Err(DomainError::Conflict {
                detail: "the category still has declarations referencing it".to_owned(),
            })
        }

        async fn list<C: DBRunner>(
            &self,
            _conn: &C,
            _scope: &AccessScope,
            _visibility: &DomainVisibility,
            _query: &ODataQuery,
        ) -> Result<Listing<Category>, DomainError> {
            unimplemented!()
        }

        async fn count_referencing_declarations<C: DBRunner>(
            &self,
            _conn: &C,
            _id: Uuid,
        ) -> Result<u64, DomainError> {
            let call = self.counts.fetch_add(1, Ordering::SeqCst);
            Ok(if call == 0 { 0 } else { 3 })
        }
    }

    #[tokio::test]
    async fn a_refused_delete_issues_no_further_statement_on_the_transaction() {
        // A failed statement leaves a PostgreSQL transaction unusable, so the
        // refusal is returned as the repository gave it and the only count
        // taken is the pre-check's.
        let h = ResolutionHarness::new().await;
        let conn = h.db.conn().expect("connection");
        let counts = Arc::new(AtomicUsize::new(0));
        let category = Category {
            id: Uuid::new_v4(),
            key: CategoryKey::parse("network").expect("slug"),
            name: "network".to_owned(),
            description: None,
            domain_affinity: None,
            sort_order: 0,
            icon: None,
            etag: ETag::new("\"v1\""),
            updated_at: time::OffsetDateTime::now_utc(),
        };
        let id = category.id;
        let svc = CategoryService::new(
            RacedRepo {
                category,
                counts: Arc::clone(&counts),
            },
            Arc::new(RecordingAudit::default()),
        );
        let ctx = SecurityContext::anonymous();

        let refused = svc
            .delete(
                &conn,
                &AccessScope::allow_all(),
                id,
                Some("\"v1\""),
                Actor {
                    ctx: &ctx,
                    request_id: "req-1",
                },
            )
            .await
            .expect_err("the constraint refuses");

        match refused {
            DomainError::Conflict { detail } => {
                assert_eq!(detail, "the category still has declarations referencing it");
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
        assert_eq!(
            counts.load(Ordering::SeqCst),
            1,
            "only the pre-check counts; the recount happens after the rollback"
        );
    }
}

mod refusal_with_count {
    //! The recount after a refused delete is an enrichment: it may sharpen the
    //! refusal, never replace it with something worse.

    use crate::domain::category::service::refusal_with_count;
    use crate::domain::error::DomainError;

    fn original() -> DomainError {
        DomainError::Conflict {
            detail: "the category still has declarations referencing it".to_owned(),
        }
    }

    #[test]
    fn a_positive_count_is_named_in_the_refusal() {
        match refusal_with_count(original(), Some(3)) {
            DomainError::Conflict { detail } => {
                assert!(detail.contains("3 declaration"), "{detail}");
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    #[test]
    fn a_zero_count_keeps_the_original_refusal() {
        // The racing declaration vanished again.
        match refusal_with_count(original(), Some(0)) {
            DomainError::Conflict { detail } => {
                assert_eq!(detail, "the category still has declarations referencing it");
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    #[test]
    fn a_failed_count_keeps_the_original_refusal() {
        match refusal_with_count(original(), None) {
            DomainError::Conflict { detail } => {
                assert_eq!(detail, "the category still has declarations referencing it");
            }
            other => panic!("a failed recount must not change the refusal, got {other:?}"),
        }
    }
}
