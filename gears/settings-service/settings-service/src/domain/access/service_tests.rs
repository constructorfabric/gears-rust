// Created: 2026-09-07 by Constructor Tech
//! Set, clear, read and list over the resolution harness.

use std::sync::Arc;

use serde_json::json;
use settings_service_sdk::SettingKey;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{AccessActor, AccessService};
use crate::domain::access::{ABSENT_RESTRICTION_TAG, TenantAccess};
use crate::domain::error::DomainError;
use crate::domain::resolution::{ScopeTarget, scope_class};
use crate::infra::storage::access_repo::AccessRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::test_support::{FixedScope, RecordingAudit, ResolutionHarness};

type Service = AccessService<DeclarationRepo, AccessRepo, Arc<RecordingAudit>>;

struct Harness {
    base: ResolutionHarness,
    service: Service,
    audit: Arc<RecordingAudit>,
}

impl Harness {
    async fn new() -> Self {
        let base = ResolutionHarness::new().await;
        let audit = Arc::new(RecordingAudit::default());
        let service = AccessService::new(
            DeclarationRepo,
            AccessRepo,
            Arc::clone(&audit),
            Arc::clone(&base.hierarchy) as Arc<dyn crate::domain::resolution::TenantHierarchy>,
            Arc::new(FixedScope(base.tree.root)),
            Arc::clone(&base.cache),
        );
        Self {
            base,
            service,
            audit,
        }
    }

    fn key(&self, name: &str) -> SettingKey {
        self.base.key(name)
    }
}

fn actor(tenant: Uuid) -> AccessActor {
    AccessActor {
        ctx: SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant)
            .build()
            .expect("context"),
        request_id: "req".to_owned(),
    }
}

#[tokio::test]
async fn set_read_and_clear_round_trip_with_their_tags_and_records() {
    let h = Harness::new().await;
    h.base
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let t = &h.base.tree;
    let conn = h.base.db.conn().expect("connection");
    let root = actor(t.root);

    let fresh = h
        .service
        .read(&conn, &root, &h.key("strict"), t.b)
        .await
        .expect("reads");
    assert!(fresh.stored.is_none());
    assert_eq!(fresh.effective.access, TenantAccess::Overridable);
    assert_eq!(fresh.etag.as_str(), ABSENT_RESTRICTION_TAG);

    let set = h
        .service
        .set(
            &conn,
            &root,
            &h.key("strict"),
            t.a,
            TenantAccess::Hidden,
            Some(ABSENT_RESTRICTION_TAG),
        )
        .await
        .expect("sets");
    assert_eq!(
        set.stored.as_ref().map(|r| r.access),
        Some(TenantAccess::Hidden)
    );
    assert_ne!(set.etag.as_str(), ABSENT_RESTRICTION_TAG);

    // The descendant reads as hidden, supplied by its ancestor; it has no row.
    let below = h
        .service
        .read(&conn, &root, &h.key("strict"), t.b)
        .await
        .expect("reads");
    assert!(below.stored.is_none());
    assert_eq!(below.effective.access, TenantAccess::Hidden);
    assert_eq!(below.effective.supplied_by, Some(t.a));

    // A stale or missing tag stores nothing.
    assert!(matches!(
        h.service
            .set(
                &conn,
                &root,
                &h.key("strict"),
                t.a,
                TenantAccess::ReadOnly,
                Some(ABSENT_RESTRICTION_TAG)
            )
            .await,
        Err(DomainError::PreconditionFailed { .. })
    ));
    assert!(matches!(
        h.service
            .set(
                &conn,
                &root,
                &h.key("strict"),
                t.a,
                TenantAccess::ReadOnly,
                None
            )
            .await,
        Err(DomainError::PreconditionRequired { .. })
    ));

    let cleared = h
        .service
        .clear(&conn, &root, &h.key("strict"), t.a, Some(set.etag.as_str()))
        .await
        .expect("clears");
    assert!(cleared.stored.is_none());
    assert_eq!(cleared.effective.access, TenantAccess::Overridable);
    assert_eq!(cleared.etag.as_str(), ABSENT_RESTRICTION_TAG);
    // Clearing again is a no-op that still needs the absent-state tag.
    assert!(
        h.service
            .clear(
                &conn,
                &root,
                &h.key("strict"),
                t.a,
                Some(ABSENT_RESTRICTION_TAG)
            )
            .await
            .is_ok()
    );
    assert_eq!(h.audit.operations(), vec!["create", "remove"]);
}

#[tokio::test]
async fn only_a_reachable_strict_descendant_can_be_restricted_and_overridable_is_not_a_value() {
    let h = Harness::new().await;
    h.base
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let t = &h.base.tree;
    let conn = h.base.db.conn().expect("connection");
    for (caller, target) in [(t.a, t.a), (t.b, t.a), (t.a, t.c), (t.a, t.s)] {
        assert!(
            matches!(
                h.service
                    .set(
                        &conn,
                        &actor(caller),
                        &h.key("strict"),
                        target,
                        TenantAccess::ReadOnly,
                        Some(ABSENT_RESTRICTION_TAG)
                    )
                    .await,
                Err(DomainError::Unauthorized { .. })
            ),
            "{caller} -> {target}"
        );
    }
    assert!(matches!(
        h.service
            .set(
                &conn,
                &actor(t.root),
                &h.key("strict"),
                t.a,
                TenantAccess::Overridable,
                Some(ABSENT_RESTRICTION_TAG)
            )
            .await,
        Err(DomainError::Validation { .. })
    ));
    assert!(matches!(
        h.service
            .read(&conn, &actor(t.root), &h.key("ghost"), t.a)
            .await,
        Err(DomainError::NotFound { .. })
    ));
}

#[tokio::test]
async fn a_hidden_caller_sees_nothing_and_a_restriction_evicts_the_subtree() {
    let h = Harness::new().await;
    let d = h
        .base
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let t = &h.base.tree;
    let conn = h.base.db.conn().expect("connection");
    h.base.set(d, t.root, json!(true)).await;
    for tenant in [t.a, t.b, t.c] {
        h.base
            .resolve("strict", ScopeTarget::Tenant(tenant))
            .await
            .expect("cached");
    }

    h.service
        .set(
            &conn,
            &actor(t.root),
            &h.key("strict"),
            t.a,
            TenantAccess::Hidden,
            Some(ABSENT_RESTRICTION_TAG),
        )
        .await
        .expect("sets");
    h.service
        .evict(&h.key("strict"), t.a)
        .await
        .expect("evicts");
    let key = h.key("strict");
    assert!(
        h.base.cache.get(key.as_str(), t.a).is_none()
            && h.base.cache.get(key.as_str(), t.b).is_none()
    );
    assert!(
        h.base.cache.get(key.as_str(), t.c).is_some(),
        "the sibling branch keeps its entry"
    );

    // The hidden tenant cannot even read its own access: absent, not forbidden.
    assert!(matches!(
        h.service
            .read(&conn, &actor(t.b), &h.key("strict"), t.b)
            .await,
        Err(DomainError::NotFound { .. })
    ));
    // Its value still resolves: access gates the caller, not the value.
    assert_eq!(
        h.base
            .resolve("strict", ScopeTarget::Tenant(t.b))
            .await
            .expect("resolves")
            .value,
        json!(true)
    );
}

#[tokio::test]
async fn the_list_covers_the_callers_subtree_without_standalone_branches() {
    let h = Harness::new().await;
    h.base
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let t = &h.base.tree;
    let conn = h.base.db.conn().expect("connection");
    let root = actor(t.root);
    for tenant in [t.a, t.b, t.c] {
        h.service
            .set(
                &conn,
                &root,
                &h.key("strict"),
                tenant,
                TenantAccess::ReadOnly,
                Some(ABSENT_RESTRICTION_TAG),
            )
            .await
            .expect("sets");
    }
    // `s` is standalone: root cannot restrict it, so no row can exist for it.
    let all: Vec<Uuid> = h
        .service
        .list(&conn, &root, &h.key("strict"))
        .await
        .expect("lists")
        .into_iter()
        .map(|r| r.tenant_id)
        .collect();
    assert_eq!(all.len(), 3);
    assert!(all.contains(&t.a) && all.contains(&t.b) && all.contains(&t.c));

    // `a` sees its own row and `b`'s, not the sibling `c`'s.
    let from_a: Vec<Uuid> = h
        .service
        .list(&conn, &actor(t.a), &h.key("strict"))
        .await
        .expect("lists")
        .into_iter()
        .map(|r| r.tenant_id)
        .collect();
    assert_eq!(from_a.len(), 2);
    assert!(from_a.contains(&t.a) && from_a.contains(&t.b));
}
