// Created: 2026-09-07 by Constructor Tech
//! The effective-access rule, the state tag, the target check and eviction.

use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    ABSENT_RESTRICTION_TAG, EffectiveAccess, Restriction, TenantAccess, evict_access_change,
    may_restrict, restriction_tag, strictest,
};
use crate::domain::resolution::EffectiveCache;
use crate::test_support::FakeHierarchy;

fn row(tenant: Uuid, access: TenantAccess) -> Restriction {
    Restriction {
        id: Uuid::new_v4(),
        declaration_id: Uuid::nil(),
        tenant_id: tenant,
        access,
        set_by: "root-admin".to_owned(),
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::from_unix_timestamp(42).expect("in range"),
    }
}

struct Tree {
    root: Uuid,
    child: Uuid,
    grandchild: Uuid,
    sibling: Uuid,
    sealed: Uuid,
}

impl Tree {
    fn new() -> Self {
        Self {
            root: Uuid::new_v4(),
            child: Uuid::new_v4(),
            grandchild: Uuid::new_v4(),
            sibling: Uuid::new_v4(),
            sealed: Uuid::new_v4(),
        }
    }

    fn hierarchy(&self) -> FakeHierarchy {
        FakeHierarchy::default()
            .with_tenant(self.root, None)
            .with_tenant(self.child, Some(self.root))
            .with_tenant(self.grandchild, Some(self.child))
            .with_tenant(self.sibling, Some(self.root))
            .with_tenant(self.sealed, Some(self.child))
            .with_standalone(self.sealed)
    }
}

#[test]
fn no_row_is_overridable_and_the_strictest_row_wins() {
    assert_eq!(strictest(&[]), EffectiveAccess::OVERRIDABLE);
    let (ancestor, descendant) = (Uuid::new_v4(), Uuid::new_v4());
    let effective = strictest(&[
        row(ancestor, TenantAccess::Hidden),
        row(descendant, TenantAccess::ReadOnly),
    ]);
    assert_eq!(effective.access, TenantAccess::Hidden);
    assert_eq!(
        effective.supplied_by,
        Some(ancestor),
        "the hiding ancestor supplies it"
    );
    assert!(effective.is_hidden());
    assert!(TenantAccess::Overridable < TenantAccess::ReadOnly);
    assert!(TenantAccess::ReadOnly < TenantAccess::Hidden);
}

#[test]
fn the_tag_follows_the_row_and_the_absent_state_is_distinct() {
    let stored = restriction_tag(Some(&row(Uuid::new_v4(), TenantAccess::ReadOnly)));
    let absent = restriction_tag(None);
    assert_eq!(absent.as_str(), ABSENT_RESTRICTION_TAG);
    assert_ne!(stored, absent);
    assert_eq!(
        stored.as_str(),
        OffsetDateTime::from_unix_timestamp(42)
            .expect("in range")
            .unix_timestamp_nanos()
            .to_string()
    );
}

#[tokio::test]
async fn only_a_reachable_strict_descendant_may_be_restricted() {
    let tree = Tree::new();
    let hierarchy = tree.hierarchy();
    assert!(
        may_restrict(&hierarchy, tree.child, tree.grandchild)
            .await
            .expect("answers")
    );
    assert!(
        may_restrict(&hierarchy, tree.root, tree.grandchild)
            .await
            .expect("answers")
    );
    for (caller, target) in [
        (tree.child, tree.child),
        (tree.grandchild, tree.child),
        (tree.child, tree.sibling),
        (tree.child, tree.sealed),
        (tree.root, tree.sealed),
    ] {
        assert!(
            !may_restrict(&hierarchy, caller, target)
                .await
                .expect("answers"),
            "{caller} -> {target}"
        );
    }
}

#[tokio::test]
async fn an_access_change_evicts_the_tenant_and_its_descendants_only() {
    let tree = Tree::new();
    let hierarchy = tree.hierarchy();
    let cache = EffectiveCache::new(Duration::from_secs(30));
    for tenant in [tree.root, tree.child, tree.grandchild, tree.sibling] {
        cache.populate(Arc::new(crate::domain::resolution::cache::tests_entry(
            "k", tenant,
        )));
    }
    evict_access_change(&cache, &hierarchy, "k", tree.child)
        .await
        .expect("evicts");
    assert!(cache.get("k", tree.child).is_none() && cache.get("k", tree.grandchild).is_none());
    assert!(cache.get("k", tree.root).is_some() && cache.get("k", tree.sibling).is_some());
}
