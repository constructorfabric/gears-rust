// Created: 2026-09-07 by Constructor Tech
//! The resolver over the real repositories and an in-memory database.
//!
//! The tree is `root → a → b`, with `c` a sibling of `a` and `s` a standalone
//! child of `a`. Rows are written through the value repository directly: the
//! write path is a later feature, and the resolver's contract is about what it
//! reads.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use settings_service_sdk::EffectiveSource;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::resolution::{ScopeTarget, TenantHierarchy, scope_class};
use settings_service_sdk::SettingKey;

use crate::test_support::{ResolutionHarness as Harness, SECRET};

fn tenant(id: Uuid) -> ScopeTarget {
    ScopeTarget::Tenant(id)
}

#[tokio::test]
async fn a_global_setting_reads_its_platform_row_and_is_inherited_by_tenants() {
    let h = Harness::new().await;
    let d = h
        .declare("proxy_enabled", scope_class::GLOBAL, json!(false))
        .await;
    h.set(d, h.tree.root, json!(true)).await;

    let at_platform = h
        .resolve("proxy_enabled", ScopeTarget::Platform)
        .await
        .expect("resolves");
    assert_eq!(at_platform.value, json!(true));
    assert_eq!(at_platform.source, EffectiveSource::OwnOverride);
    assert_eq!(at_platform.source_scope.as_deref(), Some("/"));
    assert_eq!(at_platform.scope, "/");

    // A tenant is served the platform value read-only: not its own override.
    let at_b = h
        .resolve("proxy_enabled", tenant(h.tree.b))
        .await
        .expect("resolves");
    assert_eq!(at_b.value, json!(true));
    assert_eq!(at_b.source, EffectiveSource::Inherited);
    assert_eq!(at_b.source_scope.as_deref(), Some("/"));
    assert!(
        at_b.own_row.is_none(),
        "a tenant has no row of its own for a global setting"
    );
    assert_eq!(
        h.hierarchy.chain_calls(),
        0,
        "a global setting never asks for ancestry"
    );
}

#[tokio::test]
async fn a_global_setting_without_a_platform_row_is_its_schema_default() {
    let h = Harness::new().await;
    h.declare("proxy_enabled", scope_class::GLOBAL, json!(false))
        .await;
    let v = h
        .resolve("proxy_enabled", ScopeTarget::Platform)
        .await
        .expect("resolves");
    assert_eq!(v.value, json!(false));
    assert_eq!(v.source, EffectiveSource::SchemaDefault);
    assert_eq!(v.source_scope, None);
    assert_eq!(v.trail.len(), 1);
    assert!(!v.trail[0].has_override);
}

#[tokio::test]
async fn a_cascading_setting_prefers_the_deepest_valid_override() {
    let h = Harness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let t = &h.tree;

    // Nothing anywhere: the Schema Default with a null source scope.
    let v = h.resolve("strict", tenant(t.b)).await.expect("resolves");
    assert_eq!(
        (v.source, v.source_scope.clone()),
        (EffectiveSource::SchemaDefault, None)
    );
    assert_eq!(
        v.trail.iter().map(|e| e.tenant_id).collect::<Vec<_>>(),
        vec![t.root, t.a, t.b]
    );

    h.set(d, t.root, json!(true)).await;
    h.cache.invalidate_key(h.key("strict").as_str());
    let v = h.resolve("strict", tenant(t.b)).await.expect("resolves");
    assert_eq!(v.source, EffectiveSource::Inherited);
    assert_eq!(v.source_scope.as_deref(), Some("/"));

    // A deeper ancestor override wins over the platform one.
    h.set(d, t.a, json!(false)).await;
    h.cache.invalidate_key(h.key("strict").as_str());
    let v = h.resolve("strict", tenant(t.b)).await.expect("resolves");
    assert_eq!(v.value, json!(false));
    assert_eq!(v.source, EffectiveSource::Inherited);
    assert_eq!(v.source_scope, Some(format!("/tenants/{}", t.a)));
    let provided: Vec<Uuid> = v
        .trail
        .iter()
        .filter(|e| e.provided_value)
        .map(|e| e.tenant_id)
        .collect();
    assert_eq!(provided, vec![t.a]);

    // Its own row wins over every ancestor.
    h.set(d, t.b, json!(true)).await;
    h.cache.invalidate_key(h.key("strict").as_str());
    let v = h.resolve("strict", tenant(t.b)).await.expect("resolves");
    assert_eq!(v.source, EffectiveSource::OwnOverride);
    assert_eq!(v.source_scope, Some(format!("/tenants/{}", t.b)));
    assert!(v.own_row.is_some());

    // A sibling's row never enters the walk or the trail.
    h.set(d, t.c, json!(false)).await;
    h.cache.invalidate_key(h.key("strict").as_str());
    let v = h.resolve("strict", tenant(t.b)).await.expect("resolves");
    assert!(v.trail.iter().all(|e| e.tenant_id != t.c));
    assert_eq!(v.value, json!(true));
}

#[tokio::test]
async fn a_local_setting_never_inherits() {
    let h = Harness::new().await;
    let d = h
        .declare("cpu_share", scope_class::LOCAL, json!(false))
        .await;
    h.set(d, h.tree.a, json!(true)).await;

    let at_b = h
        .resolve("cpu_share", tenant(h.tree.b))
        .await
        .expect("resolves");
    assert_eq!(at_b.source, EffectiveSource::SchemaDefault);
    assert_eq!(at_b.trail.len(), 1, "only the requested scope is inspected");

    let at_a = h
        .resolve("cpu_share", tenant(h.tree.a))
        .await
        .expect("resolves");
    assert_eq!(
        (at_a.value.clone(), at_a.source),
        (json!(true), EffectiveSource::OwnOverride)
    );
    assert_eq!(
        h.hierarchy.chain_calls(),
        0,
        "a local setting never asks for ancestry"
    );
}

#[tokio::test]
async fn a_flagged_override_is_skipped_never_served_and_never_an_error() {
    let h = Harness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let t = &h.tree;
    h.set(d, t.a, json!(true)).await;
    h.set_flagged(d, t.b, json!("not-a-bool")).await;

    let v = h
        .resolve("strict", tenant(t.b))
        .await
        .expect("a flagged row is not an error");
    assert_eq!(v.value, json!(true), "the nearest valid ancestor is served");
    assert_eq!(v.source, EffectiveSource::Inherited);
    let own = v
        .own_row
        .as_ref()
        .expect("the flagged row is still reported as the scope's own");
    assert!(own.needs_review);
    assert_eq!(
        own.needs_review_detail.as_deref(),
        Some("no longer validates")
    );
    assert!(
        v.trail
            .iter()
            .any(|e| e.tenant_id == t.b && e.needs_review && !e.provided_value)
    );

    // With no valid ancestor the walk ends at the Schema Default.
    let d2 = h
        .declare("other", scope_class::CASCADING, json!(false))
        .await;
    h.set_flagged(d2, t.b, json!("bad")).await;
    let v = h.resolve("other", tenant(t.b)).await.expect("resolves");
    assert_eq!(
        (v.value.clone(), v.source),
        (json!(false), EffectiveSource::SchemaDefault)
    );

    // A local setting's flagged row falls straight through to the default.
    let d3 = h
        .declare("local_flag", scope_class::LOCAL, json!(false))
        .await;
    h.set_flagged(d3, t.a, json!("bad")).await;
    let v = h
        .resolve("local_flag", tenant(t.a))
        .await
        .expect("resolves");
    assert_eq!(v.source, EffectiveSource::SchemaDefault);
    assert!(v.own_row.as_ref().is_some_and(|o| o.needs_review));
}

#[tokio::test]
async fn retired_not_found_and_unavailable_are_distinct_and_never_a_default() {
    let h = Harness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;

    match h.resolve("ghost", tenant(h.tree.b)).await {
        Err(DomainError::NotFound { resource }) => assert_eq!(resource, "declaration"),
        other => panic!("expected not-found, got {other:?}"),
    }

    h.retire(d).await;
    match h.resolve("strict", tenant(h.tree.b)).await {
        Err(DomainError::Retired { key }) => assert_eq!(key, h.key("strict").to_string()),
        other => panic!("expected retired, got {other:?}"),
    }

    let d2 = h
        .declare("other", scope_class::CASCADING, json!(false))
        .await;
    h.set(d2, h.tree.root, json!(true)).await;
    h.hierarchy.set_unavailable(true);
    match h.resolve("other", tenant(h.tree.b)).await {
        Err(DomainError::Unavailable { .. }) => {}
        other => panic!("expected unavailable rather than a substituted default, got {other:?}"),
    }
    // A global setting needs no ancestry and still resolves.
    let d3 = h.declare("global", scope_class::GLOBAL, json!(false)).await;
    h.set(d3, h.tree.root, json!(true)).await;
    assert_eq!(
        h.resolve("global", tenant(h.tree.b))
            .await
            .expect("resolves")
            .value,
        json!(true)
    );
}

#[tokio::test]
async fn an_explicit_null_is_told_from_an_unset_value_by_source_alone() {
    let h = Harness::new().await;
    let d = h.declare("nullable", scope_class::LOCAL, Value::Null).await;
    let unset = h
        .resolve("nullable", tenant(h.tree.a))
        .await
        .expect("resolves");
    h.set(d, h.tree.b, Value::Null).await;
    let set = h
        .resolve("nullable", tenant(h.tree.b))
        .await
        .expect("resolves");
    assert_eq!(unset.value, set.value, "the values are indistinguishable");
    assert_eq!(unset.source, EffectiveSource::SchemaDefault);
    assert_eq!(set.source, EffectiveSource::OwnOverride);
}

#[tokio::test]
async fn a_secret_row_resolves_to_its_handle_never_plaintext() {
    let h = Harness::new().await;
    let d = h
        .declare_typed(
            "api_token",
            scope_class::CASCADING,
            json!(""),
            SECRET,
            "secret",
        )
        .await;
    h.set_secret(d, h.tree.root, "credstore:abc").await;
    let v = h
        .resolve("api_token", tenant(h.tree.b))
        .await
        .expect("resolves");
    assert_eq!(v.value, json!("credstore:abc"));
    assert!(v.secret_backed);
    assert_eq!(v.traits["secret"], json!(true));
    assert_eq!(v.data_classification, "secret");
}

#[tokio::test]
async fn a_second_read_is_served_from_cache_and_invalidation_re_resolves() {
    let h = Harness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let first = h
        .resolve("strict", tenant(h.tree.b))
        .await
        .expect("resolves");
    assert_eq!(h.hierarchy.chain_calls(), 1);

    // A row written behind the cache's back is invisible until eviction: the
    // second read touched neither the database nor the resolver.
    h.set(d, h.tree.a, json!(true)).await;
    let second = h
        .resolve("strict", tenant(h.tree.b))
        .await
        .expect("resolves");
    assert!(
        Arc::ptr_eq(&first, &second),
        "the very entry populated by the first read"
    );
    assert_eq!(h.hierarchy.chain_calls(), 1);

    h.cache.invalidate(
        h.key("strict").as_str(),
        scope_class::CASCADING,
        Some(h.tree.a),
    );
    let third = h
        .resolve("strict", tenant(h.tree.b))
        .await
        .expect("resolves");
    assert_eq!(
        third.value,
        json!(true),
        "descendants re-resolve after a key-wide eviction"
    );
    assert_eq!(h.hierarchy.chain_calls(), 2);
}

#[tokio::test]
async fn an_entry_past_the_ttl_is_re_resolved() {
    let h = Harness::with_ttl(Duration::ZERO).await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    h.resolve("strict", tenant(h.tree.b))
        .await
        .expect("resolves");
    h.set(d, h.tree.b, json!(true)).await;
    std::thread::sleep(Duration::from_millis(2));
    let v = h
        .resolve("strict", tenant(h.tree.b))
        .await
        .expect("resolves");
    assert_eq!(
        v.value,
        json!(true),
        "a missed invalidation heals within the time-to-live"
    );
}

#[tokio::test]
async fn a_bulk_read_shares_one_ancestry_and_answers_every_key() {
    let h = Harness::new().await;
    let good = h
        .declare("good", scope_class::CASCADING, json!(false))
        .await;
    let retired = h
        .declare("retired", scope_class::CASCADING, json!(false))
        .await;
    h.set(good, h.tree.a, json!(true)).await;
    h.retire(retired).await;

    let conn = h.db.conn().expect("connection");
    let keys = vec![h.key("good"), h.key("ghost"), h.key("retired")];
    let outcomes = h
        .resolver
        .resolve_bulk(&conn, &keys, tenant(h.tree.b))
        .await;

    assert_eq!(outcomes.len(), 3);
    assert!(matches!(&outcomes[0].1, Ok(v) if v.value == json!(true)));
    assert!(matches!(&outcomes[1].1, Err(DomainError::NotFound { .. })));
    assert!(matches!(&outcomes[2].1, Err(DomainError::Retired { .. })));
    assert_eq!(
        h.hierarchy.chain_calls(),
        1,
        "one ancestry lookup for the whole batch"
    );
}

#[tokio::test]
async fn the_trail_carries_setter_identity_for_the_administrative_projection() {
    let h = Harness::new().await;
    let decl = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    h.set(decl, h.tree.a, json!(true)).await;
    let value = h
        .resolve("strict", tenant(h.tree.b))
        .await
        .expect("resolves");
    let at_a = value
        .trail
        .iter()
        .find(|e| e.tenant_id == h.tree.a)
        .expect("a is on the trail");
    assert_eq!(
        at_a.set_by.as_deref(),
        Some(format!("admin-of-{}", h.tree.a).as_str())
    );
    assert!(at_a.last_change_at.is_some());
    assert_eq!(value.resolved_row_last_change_at, at_a.last_change_at);
    let at_b = value
        .trail
        .iter()
        .find(|e| e.tenant_id == h.tree.b)
        .expect("b is on the trail");
    assert_eq!(at_b.set_by, None);
}

mod access {
    //! Effective access over the harness: the strictest row on the chain.

    use serde_json::json;
    use toolkit_security::AccessScope;

    use crate::domain::access::{AccessRepository, RestrictionDraft, TenantAccess};
    use crate::domain::resolution::{ScopeTarget, scope_class};
    use crate::infra::storage::access_repo::AccessRepo;
    use crate::test_support::ResolutionHarness;

    #[tokio::test]
    async fn a_fresh_setting_is_overridable_everywhere_and_creates_no_row() {
        let h = ResolutionHarness::new().await;
        let d = h.declare("strict", scope_class::GLOBAL, json!(false)).await;
        let conn = h.db.conn().expect("connection");
        for target in [ScopeTarget::Platform, ScopeTarget::Tenant(h.tree.b)] {
            let access = h
                .resolver
                .effective_access(&conn, d, target)
                .await
                .expect("resolves");
            assert_eq!(access.access, TenantAccess::Overridable);
            assert_eq!(access.supplied_by, None);
        }
        assert!(
            AccessRepo
                .find_one(&conn, &AccessScope::allow_all(), d, h.tree.b)
                .await
                .expect("lookup")
                .is_none(),
            "reading creates nothing"
        );
        assert!(
            h.hierarchy.chain_calls() >= 1,
            "access walks the chain even for a global setting"
        );
    }

    #[tokio::test]
    async fn the_strictest_ancestor_row_wins_and_siblings_are_unaffected() {
        let h = ResolutionHarness::new().await;
        let d = h
            .declare("strict", scope_class::CASCADING, json!(false))
            .await;
        let conn = h.db.conn().expect("connection");
        let all = AccessScope::allow_all();
        let t = &h.tree;
        for (tenant, access) in [(t.a, TenantAccess::Hidden), (t.b, TenantAccess::ReadOnly)] {
            AccessRepo
                .upsert(
                    &conn,
                    &all,
                    RestrictionDraft {
                        declaration_id: d,
                        tenant_id: tenant,
                        access,
                        set_by: "root-admin".to_owned(),
                    },
                )
                .await
                .expect("row");
        }
        let at_b = h
            .resolver
            .effective_access(&conn, d, ScopeTarget::Tenant(t.b))
            .await
            .expect("resolves");
        assert_eq!(
            at_b.access,
            TenantAccess::Hidden,
            "the ancestor's hidden dominates"
        );
        assert_eq!(at_b.supplied_by, Some(t.a));
        let at_c = h
            .resolver
            .effective_access(&conn, d, ScopeTarget::Tenant(t.c))
            .await
            .expect("resolves");
        assert_eq!(
            at_c.access,
            TenantAccess::Overridable,
            "a sibling branch is untouched"
        );

        // The value still resolves for the hidden tenant: access gates the
        // caller, never the value, and the in-process reader is not gated.
        h.set(d, t.a, json!(true)).await;
        let value = h
            .resolve("strict", ScopeTarget::Tenant(t.b))
            .await
            .expect("resolves");
        assert_eq!(value.value, json!(true));
    }
}

#[tokio::test]
async fn a_flagged_override_stays_in_storage_and_is_what_the_administrative_listing_shows() {
    let h = Harness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let t = &h.tree;

    // One flagged row and one sound row on the same setting.
    h.set_flagged(d, t.a, json!("no longer a boolean")).await;
    h.set(d, t.c, json!(true)).await;

    // The resolver skips the flagged row without serving or deleting it: `a`
    // falls through to the Schema Default while the row is still there.
    let resolved = h
        .resolve("strict", ScopeTarget::Tenant(t.a))
        .await
        .expect("resolves");
    assert_eq!(resolved.value, json!(false));
    assert_eq!(resolved.source, EffectiveSource::SchemaDefault);

    // The administrative listing is the other half: it reads rows rather than
    // resolved values, so the flagged one is exactly what it reports, with the
    // detail that explains it.
    let conn = h.db.conn().expect("connection");
    let mut tenants = h.hierarchy.descendants(t.root).await.expect("descendants");
    tenants.push(t.root);
    let flagged = h
        .resolver
        .flagged_overrides(&conn, &[d], &tenants)
        .await
        .expect("listing");
    assert_eq!(flagged.len(), 1);
    assert_eq!(flagged[0].tenant_id, t.a);
    assert_eq!(flagged[0].value, Some(json!("no longer a boolean")));
    assert!(flagged[0].needs_review);
    assert!(flagged[0].needs_review_detail.is_some());
}

#[tokio::test]
async fn the_flagged_listing_stops_at_a_standalone_descendant() {
    let h = Harness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let t = &h.tree;

    // `s` is standalone below `a`, and holds a flagged row of its own.
    h.set_flagged(d, t.a, json!("bad at a")).await;
    h.set_flagged(d, t.s, json!("bad at s")).await;

    // The listing walks the caller's subtree as the hierarchy reports it, and
    // the hierarchy does not traverse into a standalone tenant from above.
    let conn = h.db.conn().expect("connection");
    let mut tenants = h.hierarchy.descendants(t.root).await.expect("descendants");
    tenants.push(t.root);
    assert!(!tenants.contains(&t.s), "the subtree stops at the seam");
    let flagged = h
        .resolver
        .flagged_overrides(&conn, &[d], &tenants)
        .await
        .expect("listing");
    let listed: Vec<uuid::Uuid> = flagged.iter().map(|r| r.tenant_id).collect();
    assert_eq!(listed, vec![t.a]);

    // The row is there; it is simply not this caller's to see. Asked for
    // directly, the standalone tenant's own listing reports it.
    let own = h
        .resolver
        .flagged_overrides(&conn, &[d], &[t.s])
        .await
        .expect("listing");
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].value, Some(json!("bad at s")));
}

#[tokio::test]
async fn a_key_stale_after_a_category_rename_is_absent_exactly_as_one_never_declared_is() {
    let h = Harness::new().await;
    let declared = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let _ = declared;
    let t = &h.tree;

    // The category slug rides inside the key, so a setting filed under a
    // renamed category answers at its new key and no alias is kept. Nothing
    // resolves the old spelling, and nothing distinguishes it from a spelling
    // that was never declared: both are the same absence.
    let stale = SettingKey::contributed(
        "cf",
        "demo",
        "renamed_away",
        "strict",
        std::num::NonZeroU32::MIN,
    )
    .expect("well-formed");
    let never = SettingKey::contributed(
        "cf",
        "demo",
        "renamed_away",
        "never_existed",
        std::num::NonZeroU32::MIN,
    )
    .expect("well-formed");

    let conn = h.db.conn().expect("connection");
    let stale_outcome = h
        .resolver
        .resolve(&conn, &stale, ScopeTarget::Tenant(t.a))
        .await;
    let never_outcome = h
        .resolver
        .resolve(&conn, &never, ScopeTarget::Tenant(t.a))
        .await;
    assert!(
        matches!(&stale_outcome, Err(DomainError::NotFound { resource }) if *resource == "declaration"),
        "{stale_outcome:?}"
    );
    assert!(
        matches!(&never_outcome, Err(DomainError::NotFound { resource }) if *resource == "declaration"),
        "{never_outcome:?}"
    );
    assert_eq!(
        format!("{:?}", stale_outcome.err()),
        format!("{:?}", never_outcome.err()),
        "the two absences are indistinguishable"
    );
}
