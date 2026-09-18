//! Unit tests for `Service::list` (ADR-0005/ADR-0004 collection read).

use std::sync::Arc;

use credstore_sdk::{
    CredentialPatch, CredentialStatus, CredentialWrite, Fallback as SdkFallback, InheritanceStatus,
    OwnerId, PatchField, SecretRef, SecretType, SecretValue, SharingMode, TenantId, ValueId,
};
use time::OffsetDateTime;
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, SortDir};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{CredStoreMetricsPort, NoopMetrics};
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::resolver::TenantDirectory;
use crate::domain::secret::model::{Fallback, SecretRow, SecretStatus};
use crate::domain::secret::repo::SecretRepo;
use crate::domain::secret::service::{GcSettings, ListSettings, Service};
use crate::domain::secret::test_support::*;

fn key(s: &str) -> SecretRef {
    SecretRef::new(s).expect("valid ref")
}

fn create_only() -> crate::domain::secret::model::PutPrecondition {
    crate::domain::secret::model::PutPrecondition::CreateOnly
}

fn write_typed(sharing: SharingMode, value: &str, type_name: &str) -> CredentialWrite {
    CredentialWrite {
        secret_type: Some(SecretType::from_name(type_name).expect("known type").into()),
        sharing,
        fallback: SdkFallback::Inherit,
        expires_at: None,
        secret: Some(SecretValue::from(value)),
    }
}

fn write_generic(sharing: SharingMode, value: &str) -> CredentialWrite {
    write_typed(sharing, value, "generic")
}

fn patch_suppress() -> CredentialPatch {
    CredentialPatch {
        secret_type: None,
        sharing: None,
        fallback: Some(SdkFallback::None),
        expires_at: PatchField::Absent,
        secret: PatchField::Null,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "test builder threading every Service::new dependency plus list settings through"
)]
fn service_with(
    repo: Arc<dyn SecretRepo>,
    plugin: Arc<FakePlugin>,
    dir: Arc<dyn TenantDirectory>,
    enforcer: authz_resolver_sdk::PolicyEnforcer,
    max_limit: u64,
    secret_mode_cap: u64,
) -> Service {
    Service::new(
        repo,
        dir,
        enforcer,
        Arc::new(FakePluginSelector::new(plugin)) as Arc<dyn PluginSelector>,
        catalog_type_resolver(),
        Arc::new(NoopMetrics),
        GcSettings {
            pending_max_age_secs: 3600,
            batch_size: 256,
        },
        ListSettings {
            max_limit,
            secret_mode_cap,
        },
    )
}

fn default_service(repo: Arc<dyn SecretRepo>, dir: Arc<dyn TenantDirectory>) -> Service {
    service_with(repo, FakePlugin::new(), dir, mock_enforcer(), 200, 25)
}

/// Like [`service_with`], but with a caller-supplied metrics port (for
/// asserting counters `NoopMetrics` swallows) and the same fixed
/// `max_limit`/`secret_mode_cap` [`default_service`] uses.
fn service_with_metrics(
    repo: Arc<dyn SecretRepo>,
    plugin: Arc<FakePlugin>,
    dir: Arc<dyn TenantDirectory>,
    enforcer: authz_resolver_sdk::PolicyEnforcer,
    metrics: Arc<dyn CredStoreMetricsPort>,
) -> Service {
    Service::new(
        repo,
        dir,
        enforcer,
        Arc::new(FakePluginSelector::new(plugin)) as Arc<dyn PluginSelector>,
        catalog_type_resolver(),
        metrics,
        GcSettings {
            pending_max_age_secs: 3600,
            batch_size: 256,
        },
        ListSettings {
            max_limit: 200,
            secret_mode_cap: 25,
        },
    )
}

fn references_of(page: &toolkit_odata::Page<credstore_sdk::CredentialListItem>) -> Vec<String> {
    page.items
        .iter()
        .map(|i| i.credential.reference.as_ref().to_owned())
        .collect()
}

fn filter_expr(raw: &str) -> toolkit_odata::ast::Expr {
    toolkit_odata::parse_filter_string(raw)
        .expect("valid OData syntax")
        .into_expr()
}

fn reason_of(err: &DomainError) -> &'static str {
    match err {
        DomainError::InvalidRequest { reason, .. } => reason,
        other => panic!("expected InvalidRequest, got {other:?}"),
    }
}

// ── metadata mode: own / inherited / overridden / suppressed ────────────────

#[tokio::test]
async fn metadata_page_reports_own_inherited_overridden_and_suppressed() {
    let t1 = Uuid::new_v4();
    let t3 = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir_t1 = Arc::new(FakeDir::single(t1));
    let dir_t3 = Arc::new(FakeDir::new(vec![t3, t1]));
    let enforcer = mock_enforcer();
    let svc_t1 = service_with(
        repo.clone(),
        plugin.clone(),
        dir_t1,
        enforcer.clone(),
        200,
        25,
    );
    let svc_t3 = service_with(repo.clone(), plugin.clone(), dir_t3, enforcer, 200, 25);

    let owner1 = Uuid::new_v4();
    let ctx1 = make_ctx(owner1, t1);
    let owner3 = Uuid::new_v4();
    let ctx3 = make_ctx(owner3, t3);

    // a-own: only T3 holds a row.
    svc_t3
        .put(
            &ctx3,
            &key("a-own"),
            write_generic(SharingMode::Tenant, "v-a"),
            create_only(),
        )
        .await
        .expect("a-own create");

    // b-inherited: only T1 (shared); T3 holds nothing.
    svc_t1
        .put(
            &ctx1,
            &key("b-inherited"),
            write_generic(SharingMode::Shared, "v-b"),
            create_only(),
        )
        .await
        .expect("b-inherited create");

    // c-overridden: T1 shares, T3 shadows with its own.
    svc_t1
        .put(
            &ctx1,
            &key("c-overridden"),
            write_generic(SharingMode::Shared, "v-c-parent"),
            create_only(),
        )
        .await
        .expect("c-overridden parent create");
    svc_t3
        .put(
            &ctx3,
            &key("c-overridden"),
            write_generic(SharingMode::Tenant, "v-c-own"),
            create_only(),
        )
        .await
        .expect("c-overridden own create");

    // d-suppressed: T1 shares, T3 suppresses.
    svc_t1
        .put(
            &ctx1,
            &key("d-suppressed"),
            write_generic(SharingMode::Shared, "v-d-parent"),
            create_only(),
        )
        .await
        .expect("d-suppressed parent create");
    svc_t3
        .put(
            &ctx3,
            &key("d-suppressed"),
            write_generic(SharingMode::Tenant, "v-d-own"),
            create_only(),
        )
        .await
        .expect("d-suppressed own create");
    let existing = svc_t3
        .get(&ctx3, &key("d-suppressed"))
        .await
        .expect("get")
        .expect("own row");
    svc_t3
        .patch(
            &ctx3,
            &key("d-suppressed"),
            patch_suppress(),
            crate::domain::secret::model::WritePrecondition::Version {
                id: existing.validator.expect("own validator").id,
                version: existing.validator.expect("own validator").version,
            },
        )
        .await
        .expect("suppress");

    let page = svc_t3.list(&ctx3, &ODataQuery::new()).await.expect("list");

    assert_eq!(
        references_of(&page),
        vec!["a-own", "b-inherited", "c-overridden", "d-suppressed"]
    );

    let by_ref = |r: &str| {
        page.items
            .iter()
            .find(|i| i.credential.reference.as_ref() == r)
            .unwrap_or_else(|| panic!("{r} missing from page"))
    };

    let a = by_ref("a-own");
    assert_eq!(a.credential.inheritance, InheritanceStatus::Own);
    assert!(a.credential.owner_id.is_some());
    assert_eq!(a.credential.status, CredentialStatus::Active);

    let b = by_ref("b-inherited");
    assert_eq!(b.credential.inheritance, InheritanceStatus::Inherited);
    assert!(b.credential.owner_id.is_none());
    assert_eq!(b.credential.status, CredentialStatus::None);

    let c = by_ref("c-overridden");
    assert_eq!(c.credential.inheritance, InheritanceStatus::Overridden);
    assert!(c.credential.owner_id.is_some());

    let d = by_ref("d-suppressed");
    assert_eq!(d.credential.inheritance, InheritanceStatus::Suppressed);
    assert!(d.credential.owner_id.is_some());
    assert_eq!(d.credential.status, CredentialStatus::Declared);

    // Metadata mode never carries a value.
    assert!(page.items.iter().all(|i| i.secret.is_none()));
}

// ── type clamp / authorization ───────────────────────────────────────────────

#[tokio::test]
async fn a_denied_types_references_never_appear() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let denied_gts = SecretType::from_name("api-key")
        .expect("known")
        .gts_id()
        .to_owned();
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    // Create both credentials under a permissive enforcer: `type_deny_enforcer`
    // denies every action on the api-key type, including the `write`/
    // `write_secret` the create itself would need.
    let svc_setup = service_with(
        repo.clone(),
        plugin.clone(),
        dir.clone(),
        mock_enforcer(),
        200,
        25,
    );
    svc_setup
        .put(
            &ctx,
            &key("allowed-generic"),
            write_generic(SharingMode::Tenant, "v1"),
            create_only(),
        )
        .await
        .expect("create generic");
    svc_setup
        .put(
            &ctx,
            &key("denied-api-key"),
            write_typed(SharingMode::Tenant, "v2", "api-key"),
            create_only(),
        )
        .await
        .expect("create api-key");

    let (enforcer, _resolver) = type_deny_enforcer(vec![denied_gts]);
    let svc = service_with(repo, plugin, dir, enforcer, 200, 25);
    let page = svc.list(&ctx, &ODataQuery::new()).await.expect("list");
    assert_eq!(references_of(&page), vec!["allowed-generic"]);
}

#[tokio::test]
async fn reference_in_and_type_eq_clamp_the_result() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = default_service(repo, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    for (name, type_name) in [("r1", "generic"), ("r2", "generic"), ("r3", "api-key")] {
        svc.put(
            &ctx,
            &key(name),
            write_typed(SharingMode::Tenant, "v", type_name),
            create_only(),
        )
        .await
        .expect("create");
    }

    let by_ref_in = ODataQuery::new().with_filter(filter_expr("reference in ('r1', 'r3')"));
    let page = svc.list(&ctx, &by_ref_in).await.expect("list");
    assert_eq!(references_of(&page), vec!["r1", "r3"]);

    let generic_gts = SecretType::from_name("generic")
        .expect("known")
        .gts_id()
        .to_owned();
    let by_type = ODataQuery::new().with_filter(filter_expr(&format!("type eq '{generic_gts}'")));
    let page = svc.list(&ctx, &by_type).await.expect("list");
    assert_eq!(references_of(&page), vec!["r1", "r2"]);
}

#[tokio::test]
async fn sharing_filter_is_applied_after_reduction() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = default_service(repo, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    svc.put(
        &ctx,
        &key("shared-one"),
        write_generic(SharingMode::Shared, "v"),
        create_only(),
    )
    .await
    .expect("create shared");
    svc.put(
        &ctx,
        &key("tenant-one"),
        write_generic(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create tenant");

    let query = ODataQuery::new().with_filter(filter_expr("sharing eq 'shared'"));
    let page = svc.list(&ctx, &query).await.expect("list");
    assert_eq!(references_of(&page), vec!["shared-one"]);
}

#[tokio::test]
async fn full_page_reflects_only_the_permitted_types_step_1_clamp() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let generic_uuid = SecretType::generic().uuid();
    let api_key_gts = SecretType::from_name("api-key")
        .expect("known")
        .gts_id()
        .to_owned();

    let svc_setup = service_with(
        repo.clone(),
        plugin.clone(),
        dir.clone(),
        mock_enforcer(),
        200,
        25,
    );
    for name in ["a1", "a2"] {
        svc_setup
            .put(
                &ctx,
                &key(name),
                write_generic(SharingMode::Tenant, "v"),
                create_only(),
            )
            .await
            .expect("create type A");
    }
    for name in ["b1", "b2", "b3"] {
        svc_setup
            .put(
                &ctx,
                &key(name),
                write_typed(SharingMode::Tenant, "v", "api-key"),
                create_only(),
            )
            .await
            .expect("create type B");
    }

    // Only type A (generic) is permitted.
    let (enforcer, _resolver) = type_deny_enforcer(vec![api_key_gts]);
    let svc = service_with(repo.clone(), plugin, dir, enforcer, 200, 25);
    let page = svc
        .list(&ctx, &ODataQuery::new().with_limit(2))
        .await
        .expect("list");
    assert_eq!(references_of(&page), vec!["a1", "a2"]);
    assert_eq!(page.page_info.next_cursor, None);

    let clamps = repo.list_candidate_references_type_clamps();
    assert_eq!(clamps.len(), 1, "step 1 must run exactly once");
    assert_eq!(
        clamps[0],
        Some(vec![generic_uuid]),
        "step 1's type clamp must be exactly the PDP-permitted set"
    );
}

#[tokio::test]
async fn empty_permitted_type_set_returns_empty_page_without_running_step_1() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let svc_setup = service_with(
        repo.clone(),
        plugin.clone(),
        dir.clone(),
        mock_enforcer(),
        200,
        25,
    );
    svc_setup
        .put(
            &ctx,
            &key("only-ref"),
            write_generic(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect("create");

    let svc = service_with(repo.clone(), plugin, dir, deny_enforcer(), 200, 25);
    let page = svc.list(&ctx, &ODataQuery::new()).await.expect("list");
    assert!(page.items.is_empty());
    assert_eq!(page.page_info.next_cursor, None);
    assert!(
        repo.list_candidate_references_type_clamps().is_empty(),
        "step 1 must not run when nothing is permitted"
    );
}

#[tokio::test]
async fn caller_type_filter_intersects_with_the_pdp_permitted_set() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let generic_gts = SecretType::generic().gts_id().to_owned();
    let generic_uuid = SecretType::generic().uuid();
    let api_key_gts = SecretType::from_name("api-key")
        .expect("known")
        .gts_id()
        .to_owned();

    let svc_setup = service_with(
        repo.clone(),
        plugin.clone(),
        dir.clone(),
        mock_enforcer(),
        200,
        25,
    );
    svc_setup
        .put(
            &ctx,
            &key("a1"),
            write_generic(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect("create type A");
    svc_setup
        .put(
            &ctx,
            &key("b1"),
            write_typed(SharingMode::Tenant, "v", "api-key"),
            create_only(),
        )
        .await
        .expect("create type B");

    // The caller's own filter names both types; the PDP permits only A.
    let (enforcer, _resolver) = type_deny_enforcer(vec![api_key_gts.clone()]);
    let svc = service_with(repo.clone(), plugin, dir, enforcer, 200, 25);
    let filter = ODataQuery::new().with_filter(filter_expr(&format!(
        "type in ('{generic_gts}', '{api_key_gts}')"
    )));
    let page = svc.list(&ctx, &filter).await.expect("list");
    assert_eq!(references_of(&page), vec!["a1"]);

    let clamps = repo.list_candidate_references_type_clamps();
    assert_eq!(clamps.len(), 1);
    assert_eq!(
        clamps[0],
        Some(vec![generic_uuid]),
        "the clamp is the caller's filter intersected with the PDP-permitted set"
    );
}

#[tokio::test]
async fn a_winner_of_an_unpermitted_type_is_an_invariant_violation_not_a_denial() {
    let child = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::new(vec![child, parent]));
    let ctx = make_ctx(owner, child);

    let denied_type_uuid = SecretType::generic().uuid();
    let denied_gts = SecretType::generic().gts_id().to_owned();
    let permitted_type_uuid = SecretType::from_name("api-key").expect("known").uuid();

    let now = OffsetDateTime::now_utc();
    // Own tenant, active row of the DENIED type — nearer in the chain, so
    // reduction picks it as the winner.
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(child),
        reference: "r".to_owned(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(owner),
        status: SecretStatus::Active,
        version: 1,
        updated_at: now,
        secret_type_uuid: denied_type_uuid,
        expires_at: None,
        value_id: Some(ValueId::new_v4()),
        value_fp: Some(vec![0u8; 32]),
        fp_key_id: Some(1),
        fallback: Fallback::Inherit,
    });
    // Ancestor, shared row of the PERMITTED type — this is what makes the
    // reference a step-1 candidate under the permitted-type clamp.
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(parent),
        reference: "r".to_owned(),
        sharing: SharingMode::Shared,
        owner_id: OwnerId(owner),
        status: SecretStatus::Active,
        version: 1,
        updated_at: now,
        secret_type_uuid: permitted_type_uuid,
        expires_at: None,
        value_id: Some(ValueId::new_v4()),
        value_fp: Some(vec![0u8; 32]),
        fp_key_id: Some(1),
        fallback: Fallback::Inherit,
    });

    let (enforcer, _resolver) = type_deny_enforcer(vec![denied_gts]);
    let metrics = FakeMetrics::new();
    let svc = service_with_metrics(repo, plugin, dir, enforcer, metrics.clone());

    let page = svc.list(&ctx, &ODataQuery::new()).await.expect("list");
    assert!(
        page.items.is_empty(),
        "the winner's type is not in the permitted set"
    );
    assert_eq!(metrics.list_type_invariant_violation_total(), 1);
    assert_eq!(
        metrics.cross_tenant_denied_count(),
        0,
        "this is the override-type-consistency invariant, not a cross-tenant denial"
    );
}

// ── pagination ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn cursor_round_trip_across_two_pages_has_no_duplicates_or_gaps() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = default_service(repo, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let names = ["r1", "r2", "r3", "r4", "r5"];
    for name in names {
        svc.put(
            &ctx,
            &key(name),
            write_generic(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect("create");
    }

    let mut seen = Vec::new();
    let mut query = ODataQuery::new().with_limit(2);
    loop {
        let page = svc.list(&ctx, &query).await.expect("list");
        seen.extend(references_of(&page));
        match &page.page_info.next_cursor {
            Some(token) => {
                let cursor = toolkit_odata::CursorV1::decode(token).expect("decodable cursor");
                query = ODataQuery::new().with_limit(2).with_cursor(cursor);
            }
            None => break,
        }
        assert!(
            seen.len() <= names.len() + 1,
            "pagination did not terminate"
        );
    }

    assert_eq!(seen, names.to_vec(), "no duplicates, no gaps, in order");
}

#[tokio::test]
async fn orderby_other_than_reference_is_rejected() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = default_service(repo, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let query = ODataQuery::new().with_order(ODataOrderBy(vec![OrderKey {
        field: "updated_at".to_owned(),
        dir: SortDir::Asc,
    }]));
    let err = svc.list(&ctx, &query).await.expect_err("must be rejected");
    assert_eq!(reason_of(&err), "INVALID_ORDERBY_FIELD");
}

// ── secret mode ───────────────────────────────────────────────────────────────

fn secret_mode_query(filter_raw: &str) -> ODataQuery {
    ODataQuery::new()
        .with_select(vec!["reference".to_owned(), "secret".to_owned()])
        .with_filter(filter_expr(filter_raw))
}

#[tokio::test]
async fn secret_mode_rejects_limit_and_cursor() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = default_service(repo, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let with_limit = secret_mode_query("reference eq 'r'").with_limit(10);
    let err = svc
        .list(&ctx, &with_limit)
        .await
        .expect_err("limit must be rejected in secret mode");
    assert_eq!(reason_of(&err), "SECRET_MODE_NO_PAGINATION");
}

#[tokio::test]
async fn secret_mode_over_cap_fails_closed() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = service_with(repo, plugin, dir, mock_enforcer(), 200, 2);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    for name in ["r1", "r2", "r3"] {
        svc.put(
            &ctx,
            &key(name),
            write_generic(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect("create");
    }

    let query = secret_mode_query("reference in ('r1', 'r2', 'r3')");
    let err = svc.list(&ctx, &query).await.expect_err("must fail closed");
    assert_eq!(reason_of(&err), "TOO_MANY_MATCHES");
}

#[tokio::test]
async fn secret_mode_cap_counts_only_permitted_type_references() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let api_key_gts = SecretType::from_name("api-key")
        .expect("known")
        .gts_id()
        .to_owned();

    let svc_setup = service_with(
        repo.clone(),
        plugin.clone(),
        dir.clone(),
        mock_enforcer(),
        200,
        2,
    );
    for name in ["b1", "b2", "b3"] {
        svc_setup
            .put(
                &ctx,
                &key(name),
                write_typed(SharingMode::Tenant, "v", "api-key"),
                create_only(),
            )
            .await
            .expect("create type B");
    }
    svc_setup
        .put(
            &ctx,
            &key("a1"),
            write_generic(SharingMode::Tenant, "v-a"),
            create_only(),
        )
        .await
        .expect("create type A");

    // Cap is 2, and 4 references exist in total, but only "a1" (type A) is
    // permitted — the cap must count that one reference, not all four.
    let (enforcer, _resolver) = type_deny_enforcer(vec![api_key_gts]);
    let svc = service_with(repo, plugin, dir, enforcer, 200, 2);
    let query = secret_mode_query("reference in ('a1', 'b1', 'b2', 'b3')");
    let page = svc
        .list(&ctx, &query)
        .await
        .expect("must not fail closed: only one reference is permitted");
    assert_eq!(references_of(&page), vec!["a1"]);
}

#[tokio::test]
async fn secret_mode_items_carry_values_and_evaluate_read_secret_once_per_type() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let (enforcer, resolver) = type_recording_enforcer();
    let svc = service_with(repo, plugin, dir, enforcer, 200, 25);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    svc.put(
        &ctx,
        &key("r1"),
        write_generic(SharingMode::Tenant, "value-one"),
        create_only(),
    )
    .await
    .expect("create r1");
    svc.put(
        &ctx,
        &key("r2"),
        write_generic(SharingMode::Tenant, "value-two"),
        create_only(),
    )
    .await
    .expect("create r2");

    let query = secret_mode_query("reference in ('r1', 'r2')");
    let page = svc.list(&ctx, &query).await.expect("list");

    assert_eq!(page.page_info.next_cursor, None);
    assert_eq!(page.items.len(), 2);
    for item in &page.items {
        let value = item.secret.as_ref().expect("value present");
        let expected = if item.credential.reference.as_ref() == "r1" {
            "value-one"
        } else {
            "value-two"
        };
        assert_eq!(value.as_bytes(), expected.as_bytes());
    }

    // Both `r1`/`r2` share the `generic` type, so `read_secret` is
    // evaluated exactly once for it, not once per item.
    let read_secret_evals = resolver
        .seen_actions()
        .into_iter()
        .filter(|a| a == "read_secret")
        .count();
    assert_eq!(read_secret_evals, 1);
}

#[tokio::test]
async fn secret_mode_omits_items_of_a_refused_type() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let denied_gts = SecretType::from_name("api-key")
        .expect("known")
        .gts_id()
        .to_owned();
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let svc_setup = service_with(
        repo.clone(),
        plugin.clone(),
        dir.clone(),
        mock_enforcer(),
        200,
        25,
    );
    svc_setup
        .put(
            &ctx,
            &key("allowed"),
            write_generic(SharingMode::Tenant, "v-ok"),
            create_only(),
        )
        .await
        .expect("create allowed");
    svc_setup
        .put(
            &ctx,
            &key("denied"),
            write_typed(SharingMode::Tenant, "v-no", "api-key"),
            create_only(),
        )
        .await
        .expect("create denied");

    let (enforcer, _resolver) = type_deny_enforcer(vec![denied_gts]);
    let svc = service_with(repo, plugin, dir, enforcer, 200, 25);
    let query = secret_mode_query("reference in ('allowed', 'denied')");
    let page = svc.list(&ctx, &query).await.expect("list");
    assert_eq!(references_of(&page), vec!["allowed"]);
    assert_eq!(
        page.items[0].secret.as_ref().expect("value").as_bytes(),
        b"v-ok"
    );
}

#[tokio::test]
async fn secret_mode_with_record_field_selected_also_requires_list_per_type() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let svc_setup = service_with(
        repo.clone(),
        plugin.clone(),
        dir.clone(),
        mock_enforcer(),
        200,
        25,
    );
    svc_setup
        .put(
            &ctx,
            &key("r1"),
            write_generic(SharingMode::Tenant, "v1"),
            create_only(),
        )
        .await
        .expect("create r1");

    // A pure secret-only selector evaluates `read_secret` alone.
    let (enforcer, resolver) = type_recording_enforcer();
    let svc = service_with(repo.clone(), plugin.clone(), dir.clone(), enforcer, 200, 25);
    let secret_only = secret_mode_query("reference eq 'r1'");
    let page = svc.list(&ctx, &secret_only).await.expect("list");
    assert_eq!(references_of(&page), vec!["r1"]);
    let seen = resolver.seen_actions();
    assert!(seen.contains(&"read_secret".to_owned()));
    assert!(
        !seen.contains(&"list".to_owned()),
        "a secret-only selector must not evaluate list: {seen:?}"
    );

    // Selecting a record-only field alongside `secret` needs `list` too.
    let (enforcer2, resolver2) = type_recording_enforcer();
    let svc2 = service_with(
        repo.clone(),
        plugin.clone(),
        dir.clone(),
        enforcer2,
        200,
        25,
    );
    let combined = ODataQuery::new()
        .with_select(vec![
            "reference".to_owned(),
            "sharing".to_owned(),
            "secret".to_owned(),
        ])
        .with_filter(filter_expr("reference eq 'r1'"));
    let page2 = svc2.list(&ctx, &combined).await.expect("list");
    assert_eq!(references_of(&page2), vec!["r1"]);
    let seen2 = resolver2.seen_actions();
    assert!(seen2.contains(&"read_secret".to_owned()));
    assert!(seen2.contains(&"list".to_owned()));

    // And a caller denied `list` (but holding `read_secret`) loses the item
    // entirely once a record field rides with `secret` — dropped, not
    // returned without its record fields (ADR-0004 Amendment A).
    let (enforcer3, _resolver3) = action_deny_enforcer(
        SecretType::generic().gts_id().to_owned(),
        crate::domain::authz::actions::LIST,
    );
    let svc3 = service_with(repo, plugin, dir, enforcer3, 200, 25);
    let page3 = svc3.list(&ctx, &combined).await.expect("list");
    assert!(
        page3.items.is_empty(),
        "list denied must drop the type entirely when a record field rides with secret"
    );
}

// ── secret mode: bounded-concurrency value reads ─────────────────────────────

#[tokio::test]
async fn secret_mode_reads_values_with_bounded_concurrency() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = service_with(repo.clone(), plugin.clone(), dir, mock_enforcer(), 200, 25);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let refs: Vec<String> = (0..6).map(|i| format!("r{i}")).collect();
    for name in &refs {
        svc.put(
            &ctx,
            &key(name),
            write_generic(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect("create");
    }
    // Delay every value read so overlapping in-flight reads are observable;
    // 6 references leave headroom under SECRET_READ_CONCURRENCY (8) to
    // actually overlap rather than merely queueing.
    for row in repo.rows() {
        plugin.set_delay_ms(&row.tenant_id, row.value_id.expect("value id"), 10);
    }

    let selector = refs
        .iter()
        .map(|r| format!("'{r}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let query = secret_mode_query(&format!("reference in ({selector})"));
    let page = svc.list(&ctx, &query).await.expect("list");
    assert_eq!(page.items.len(), 6);

    let max = plugin.max_in_flight();
    assert!(
        max >= 2,
        "expected overlapping in-flight reads, got max={max}"
    );
    assert!(
        max <= 8,
        "must not exceed SECRET_READ_CONCURRENCY (8), got max={max}"
    );
}

#[tokio::test]
async fn secret_mode_value_reads_preserve_reference_order() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = service_with(repo.clone(), plugin.clone(), dir, mock_enforcer(), 200, 25);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let refs: Vec<String> = (0..6).map(|i| format!("r{i}")).collect();
    for name in &refs {
        svc.put(
            &ctx,
            &key(name),
            write_generic(SharingMode::Tenant, name),
            create_only(),
        )
        .await
        .expect("create");
    }
    // Earlier references sleep longer than later ones, so completion order
    // inverts reference order; the response must still come back in
    // reference order regardless.
    for row in repo.rows() {
        let idx = refs
            .iter()
            .position(|r| *r == row.reference)
            .expect("known reference");
        let delay_ms = (refs.len() - idx) as u64 * 5;
        plugin.set_delay_ms(&row.tenant_id, row.value_id.expect("value id"), delay_ms);
    }

    let selector = refs
        .iter()
        .map(|r| format!("'{r}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let query = secret_mode_query(&format!("reference in ({selector})"));
    let page = svc.list(&ctx, &query).await.expect("list");

    assert_eq!(references_of(&page), refs);
    for item in &page.items {
        let expected = item.credential.reference.as_ref().to_owned();
        assert_eq!(
            item.secret.as_ref().expect("value present").as_bytes(),
            expected.as_bytes()
        );
    }
}

#[tokio::test]
async fn secret_mode_one_read_failure_fails_the_whole_request() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = service_with(repo.clone(), plugin.clone(), dir, mock_enforcer(), 200, 25);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    for name in ["r1", "r2", "r3"] {
        svc.put(
            &ctx,
            &key(name),
            write_generic(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect("create");
    }
    let failing_row = repo
        .rows()
        .into_iter()
        .find(|r| r.reference == "r2")
        .expect("r2 row");
    plugin.fail_get_for(
        &failing_row.tenant_id,
        failing_row.value_id.expect("value id"),
    );

    let query = secret_mode_query("reference in ('r1', 'r2', 'r3')");
    let err = svc
        .list(&ctx, &query)
        .await
        .expect_err("one backend read failure must fail the whole request");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn secret_mode_refused_item_is_omitted_others_present() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = service_with(repo.clone(), plugin.clone(), dir, mock_enforcer(), 200, 25);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    for name in ["r1", "r2", "r3"] {
        svc.put(
            &ctx,
            &key(name),
            write_generic(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect("create");
    }
    let refused_row = repo
        .rows()
        .into_iter()
        .find(|r| r.reference == "r2")
        .expect("r2 row");
    plugin.deny_get_for(
        &refused_row.tenant_id,
        refused_row.value_id.expect("value id"),
    );

    let query = secret_mode_query("reference in ('r1', 'r2', 'r3')");
    let page = svc.list(&ctx, &query).await.expect("list");
    assert_eq!(references_of(&page), vec!["r1", "r3"]);
}

#[test]
fn odata_errors_map_onto_the_domain_rejection_the_rest_boundary_renders() {
    use toolkit_odata::Error as ODataError;

    use super::map_odata_err;

    let invalid_request = |err: &ODataError| match map_odata_err(err) {
        DomainError::InvalidRequest { field, reason, .. } => (field, reason),
        other => panic!("expected InvalidRequest for {err:?}, got {other:?}"),
    };

    assert_eq!(
        invalid_request(&ODataError::OrderMismatch),
        ("$orderby", "ORDER_MISMATCH")
    );
    assert_eq!(
        invalid_request(&ODataError::FilterMismatch),
        ("$filter", "FILTER_MISMATCH")
    );
    for cursor_err in [
        ODataError::InvalidCursor,
        ODataError::CursorInvalidBase64,
        ODataError::CursorInvalidJson,
        ODataError::CursorInvalidVersion,
        ODataError::CursorInvalidKeys,
        ODataError::CursorInvalidFields,
        ODataError::CursorInvalidDirection,
    ] {
        assert_eq!(
            invalid_request(&cursor_err),
            ("cursor", "INVALID_CURSOR"),
            "{cursor_err:?}"
        );
    }
    assert_eq!(
        invalid_request(&ODataError::InvalidOrderByField("updated_at".to_owned())),
        ("$orderby", "INVALID_ORDERBY_FIELD")
    );
    assert_eq!(
        invalid_request(&ODataError::InvalidFilter("bad".to_owned())),
        ("$filter", "INVALID_FILTER")
    );
    assert_eq!(
        invalid_request(&ODataError::InvalidLimit),
        ("limit", "INVALID_LIMIT")
    );
    assert_eq!(
        invalid_request(&ODataError::OrderWithCursor),
        ("$orderby", "ORDER_WITH_CURSOR")
    );

    // The two the toolkit raises for its own failures, not the caller's:
    // internal, not a 400.
    for internal in [
        ODataError::Db("connection reset".to_owned()),
        ODataError::ParsingUnavailable("parser feature disabled"),
    ] {
        assert!(
            matches!(map_odata_err(&internal), DomainError::Internal { .. }),
            "{internal:?} must map to Internal"
        );
    }
}
