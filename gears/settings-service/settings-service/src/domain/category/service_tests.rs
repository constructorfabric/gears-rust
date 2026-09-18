// Created: 2026-08-13 by Constructor Tech
//! Tests for the category service's decidable rules.
//!
//! # Why this file is small
//!
//! Every service operation takes a [`DBRunner`](toolkit_db::secure::DBRunner),
//! and `toolkit-db` exposes no public constructor for one — `Db`, `SecureConn`
//! and `DbConn` are all sealed so a raw database handle cannot escape the
//! framework. That is a deliberate security property, and its consequence is
//! that a gear cannot drive its own services from a unit test, even against a
//! stub repository.
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
        CategoryDraft, CategoryKey, CategoryRepository, CategoryService,
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
