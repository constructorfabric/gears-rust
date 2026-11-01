use std::sync::Arc;

use gts::GtsTypeId;
use license_enforcement_sdk::models::{
    Binding, Conditions, GrantState, SubjectRef, SubjectRefInput,
};
use serde_json::json;
use toolkit_odata::ODataQuery;
use uuid::Uuid;

use super::*;
use crate::test_support::{
    ADMIN, AllowAuthZ, DenyAuthZ, GHOST, SERVICE, ctx, env, env_with, env_with_plugin,
    env_with_window, input, tenant_subject, terms, token, user,
};

fn reasons(e: &DomainError) -> Vec<String> {
    match e {
        DomainError::InvalidArgument(v) => v.iter().map(|v| v.reason.clone()).collect(),
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

#[tokio::test]
async fn issue_grant_creates_an_active_root_and_replays() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let t = token();
    let issued = terms();

    let first = env
        .grants
        .issue(&c, tenant, vec![issued.clone()], false, t.clone())
        .await
        .unwrap();
    assert!(first.pack.is_none());
    let g = &first.grants[0];
    assert_eq!(g.state, GrantState::Active);
    assert_eq!(g.binding, Binding::None);
    assert_eq!(g.holder_tenant_id, tenant);
    assert_eq!(Some(g.evidence_id), first.evidence_id);
    assert_eq!(env.grants.get_grant(&c, g.id).await.unwrap(), *g);

    let replay = env
        .grants
        .issue(&c, tenant, vec![issued.clone()], false, t.clone())
        .await
        .unwrap();
    assert_eq!(replay, first);

    let mut other = issued;
    other.permissions.may_bind = false;
    let err = env
        .grants
        .issue(&c, tenant, vec![other], false, t)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::IdempotencyConflict), "{err:?}");
}

#[tokio::test]
async fn an_absent_id_is_allowed_an_empty_or_nul_one_is_not() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let fixed = |id: &str| {
        let mut t = terms();
        t.fixed_subject = Some(SubjectRefInput {
            gts_type: GtsTypeId::new(crate::test_support::USER),
            id: id.to_owned(),
        });
        t
    };
    let with_resource_id = |id: &str| {
        let mut t = terms();
        t.resource.id = Some(id.to_owned());
        t
    };
    for (t, expected) in [
        (with_resource_id(""), reason::REQUIRED),
        (with_resource_id("a\0b"), reason::INVALID_CHARACTER),
        (fixed(""), reason::REQUIRED),
        (fixed("x\0"), reason::INVALID_CHARACTER),
    ] {
        let err = env
            .grants
            .issue(&c, tenant, vec![t], false, token())
            .await
            .unwrap_err();
        assert_eq!(reasons(&err), vec![expected.to_owned()]);
    }
    let err = env
        .grants
        .issue(&c, tenant, vec![terms()], false, "t\0".to_owned())
        .await
        .unwrap_err();
    assert_eq!(reasons(&err), vec![reason::INVALID_CHARACTER.to_owned()]);

    // `terms()` has no resource id and no fixed subject.
    let g = env
        .grants
        .issue(&c, tenant, vec![terms()], false, token())
        .await
        .unwrap()
        .grants[0]
        .id;
    for (id, expected) in [("", reason::REQUIRED), ("x\0", reason::INVALID_CHARACTER)] {
        let err = env
            .grants
            .change_binding(&c, g, BindingChange::Bind(user(id)), token(), None)
            .await
            .unwrap_err();
        assert_eq!(reasons(&err), vec![expected.to_owned()]);
    }
}

#[tokio::test]
async fn issue_refuses_a_grant_that_awaits_acceptance() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let mut t = terms();
    t.permissions.requires_acceptance = true;
    let err = env
        .grants
        .issue(&ctx(tenant), tenant, vec![t], false, token())
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::NotImplemented(_)), "{err:?}");
}

#[tokio::test]
async fn issue_pack_counts_members() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let mut fixed = terms();
    fixed.fixed_subject = Some(input(user("alice")));

    let result = env
        .grants
        .issue(&c, tenant, vec![terms(), terms(), fixed], true, token())
        .await
        .unwrap();
    let pack = result.pack.unwrap();
    assert_eq!(pack.counts.len(), 1);
    assert_eq!((pack.counts[0].unbound, pack.counts[0].bound), (2, 1));
    assert!(result.grants.iter().all(|g| g.pack_id == Some(pack.id)));
    assert!(result.grants[2].binding_fixed);
    assert_eq!(env.grants.get_pack(&c, pack.id).await.unwrap(), pack);
}

#[tokio::test]
async fn issue_validates_terms() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);

    let mut unknown = terms();
    unknown.resource.gts_type = GtsTypeId::new("gts.cf.core.lic.res.v1~test.le.app.nope.v1~");
    let mut not_admitted = terms();
    not_admitted.subject_type = GtsTypeId::new(SERVICE);
    let mut conditions = terms();
    conditions.conditions = Some(Conditions(json!({"plan": "pro"})));
    let mut window = terms();
    window.window.expires_at = window.window.not_before;
    // UTC of this instant is in year 10000, which `time` does not represent.
    let mut year_10000 = terms();
    year_10000.window.expires_at = time::PrimitiveDateTime::new(
        time::Date::from_calendar_date(9999, time::Month::December, 31).unwrap(),
        time::Time::from_hms(23, 0, 0).unwrap(),
    )
    .assume_offset(time::UtcOffset::from_hms(-5, 0, 0).unwrap());
    // UTC of this instant is in year -1, which RFC 3339 does not write.
    let mut year_minus_1 = terms();
    year_minus_1.window.not_before = time::PrimitiveDateTime::new(
        time::Date::from_calendar_date(0, time::Month::January, 1).unwrap(),
        time::Time::MIDNIGHT,
    )
    .assume_offset(time::UtcOffset::from_hms(0, 1, 0).unwrap());
    let mut ended = terms();
    ended.window.expires_at = OffsetDateTime::now_utc() - Duration::hours(1);
    let mut fixed_unknown_type = terms();
    fixed_unknown_type.fixed_subject = Some(SubjectRefInput {
        gts_type: GtsTypeId::new(GHOST),
        id: "alice".to_owned(),
    });
    let mut fixed_wrong_type = terms();
    fixed_wrong_type.fixed_subject = Some(SubjectRefInput {
        gts_type: GtsTypeId::new(tenant_subject()),
        id: tenant.to_string(),
    });

    let cases = [
        (unknown, reason::TYPE_NOT_REGISTERED),
        (not_admitted, reason::SUBJECT_NOT_ADMITTED),
        (conditions, reason::RULES_INVALID),
        (window, reason::INVALID_WINDOW),
        (year_10000, reason::INVALID_WINDOW),
        (ended, reason::INVALID_WINDOW),
        (year_minus_1, reason::INVALID_WINDOW),
        (fixed_unknown_type, reason::TYPE_NOT_REGISTERED),
        (fixed_wrong_type, reason::TYPE_NOT_DERIVED),
    ];
    for (t, expected) in cases {
        let err = env
            .grants
            .issue(&c, tenant, vec![t], false, token())
            .await
            .unwrap_err();
        assert_eq!(reasons(&err), vec![expected.to_owned()]);
    }
    let err = env
        .grants
        .issue(&c, tenant, vec![], true, token())
        .await
        .unwrap_err();
    assert_eq!(reasons(&err), vec![reason::REQUIRED.to_owned()]);

    // `admin` derives from the admitted `user`.
    let mut derived = terms();
    derived.subject_type = GtsTypeId::new(ADMIN);
    env.grants
        .issue(&c, tenant, vec![derived], false, token())
        .await
        .unwrap();
}

#[tokio::test]
async fn requests_over_size_limits_are_invalid_arguments() {
    use license_enforcement_sdk::limits::{
        MAX_GRANTS_PER_ISSUE, MAX_ID_LEN, MAX_IDEMPOTENCY_TOKEN_LEN, MAX_RULES_LEN,
    };
    let env = env().await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let too_large = || vec![reason::TOO_LARGE.to_owned()];

    let err = env
        .grants
        .issue(
            &c,
            tenant,
            vec![terms()],
            false,
            "t".repeat(MAX_IDEMPOTENCY_TOKEN_LEN + 1),
        )
        .await
        .unwrap_err();
    assert_eq!(reasons(&err), too_large());
    let err = env
        .grants
        .issue(
            &c,
            tenant,
            vec![terms(); MAX_GRANTS_PER_ISSUE + 1],
            true,
            token(),
        )
        .await
        .unwrap_err();
    assert_eq!(reasons(&err), too_large());
    let mut long_id = terms();
    long_id.resource.id = Some("r".repeat(MAX_ID_LEN + 1));
    let mut long_rules = terms();
    long_rules.conditions = Some(Conditions(json!({ "k": "v".repeat(MAX_RULES_LEN) })));
    for t in [long_id, long_rules] {
        let err = env
            .grants
            .issue(&c, tenant, vec![t], false, token())
            .await
            .unwrap_err();
        assert_eq!(reasons(&err), too_large());
    }

    let g = env
        .grants
        .issue(&c, tenant, vec![terms()], false, token())
        .await
        .unwrap()
        .grants
        .remove(0);
    let err = env
        .grants
        .change_binding(
            &c,
            g.id,
            BindingChange::Bind(user(&"a".repeat(MAX_ID_LEN + 1))),
            token(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(reasons(&err), too_large());
}

#[tokio::test]
async fn empty_token_is_an_invalid_argument() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let required = || vec![reason::REQUIRED.to_owned()];

    let err = env
        .grants
        .issue(&c, tenant, vec![terms()], false, String::new())
        .await
        .unwrap_err();
    assert_eq!(reasons(&err), required());

    let g = env
        .grants
        .issue(&c, tenant, vec![terms()], false, token())
        .await
        .unwrap()
        .grants
        .remove(0);
    let err = env
        .grants
        .change_binding(&c, g.id, BindingChange::Unbind, String::new(), None)
        .await
        .unwrap_err();
    assert_eq!(reasons(&err), required());
}

#[tokio::test]
async fn issue_is_refused_by_the_pdp() {
    let env = env_with(Arc::new(DenyAuthZ)).await;
    let tenant = Uuid::now_v7();
    let err = env
        .grants
        .issue(&ctx(tenant), tenant, vec![terms()], false, token())
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::PermissionDenied), "{err:?}");
}

#[tokio::test]
async fn issue_for_another_tenant_fails_closed_without_tenant_resolver() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let err = env
        .grants
        .issue(&ctx(tenant), Uuid::now_v7(), vec![terms()], false, token())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::DependencyUnavailable(_)),
        "{err:?}"
    );
}

/// Answers `is_ancestor` with a fixed error; nothing else is called.
struct FailingResolver(fn() -> tenant_resolver_sdk::TenantResolverError);

#[async_trait::async_trait]
impl tenant_resolver_sdk::TenantResolverClient for FailingResolver {
    async fn get_tenant(
        &self,
        _: &SecurityContext,
        _: tenant_resolver_sdk::TenantId,
    ) -> Result<tenant_resolver_sdk::TenantInfo, tenant_resolver_sdk::TenantResolverError> {
        unreachable!()
    }

    async fn get_root_tenant(
        &self,
        _: &SecurityContext,
    ) -> Result<tenant_resolver_sdk::TenantInfo, tenant_resolver_sdk::TenantResolverError> {
        unreachable!()
    }

    async fn get_tenants(
        &self,
        _: &SecurityContext,
        _: &[tenant_resolver_sdk::TenantId],
        _: &tenant_resolver_sdk::GetTenantsOptions,
    ) -> Result<Vec<tenant_resolver_sdk::TenantInfo>, tenant_resolver_sdk::TenantResolverError>
    {
        unreachable!()
    }

    async fn get_ancestors(
        &self,
        _: &SecurityContext,
        _: tenant_resolver_sdk::TenantId,
        _: &tenant_resolver_sdk::GetAncestorsOptions,
    ) -> Result<tenant_resolver_sdk::GetAncestorsResponse, tenant_resolver_sdk::TenantResolverError>
    {
        unreachable!()
    }

    async fn get_descendants(
        &self,
        _: &SecurityContext,
        _: tenant_resolver_sdk::TenantId,
        _: &tenant_resolver_sdk::GetDescendantsOptions,
    ) -> Result<tenant_resolver_sdk::GetDescendantsResponse, tenant_resolver_sdk::TenantResolverError>
    {
        unreachable!()
    }

    async fn is_ancestor(
        &self,
        _: &SecurityContext,
        _: tenant_resolver_sdk::TenantId,
        _: tenant_resolver_sdk::TenantId,
        _: &tenant_resolver_sdk::IsAncestorOptions,
    ) -> Result<bool, tenant_resolver_sdk::TenantResolverError> {
        Err((self.0)())
    }
}

#[tokio::test]
async fn issue_for_an_unknown_holder_is_an_invalid_argument() {
    use tenant_resolver_sdk::{TenantId, TenantResolverClient, TenantResolverError};

    let cases = [
        (
            FailingResolver(|| TenantResolverError::TenantNotFound {
                tenant_id: TenantId(Uuid::nil()),
            }),
            true,
        ),
        (FailingResolver(|| TenantResolverError::Unauthorized), true),
        (
            FailingResolver(|| TenantResolverError::ServiceUnavailable("down".into())),
            false,
        ),
    ];
    for (resolver, invalid) in cases {
        let env = env().await;
        env.hub
            .register::<dyn TenantResolverClient>(Arc::new(resolver));
        let tenant = Uuid::now_v7();
        let err = env
            .grants
            .issue(&ctx(tenant), Uuid::now_v7(), vec![terms()], false, token())
            .await
            .unwrap_err();
        if invalid {
            assert_eq!(reasons(&err), [reason::HOLDER_NOT_IN_SUBTREE]);
        } else {
            assert!(
                matches!(err, DomainError::DependencyUnavailable(_)),
                "{err:?}"
            );
        }
    }
}

#[tokio::test]
async fn list_orders_only_by_id() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    for _ in 0..2 {
        env.grants
            .issue(&c, tenant, vec![terms()], false, token())
            .await
            .unwrap();
    }
    let ordered = |raw: &str| {
        ODataQuery::new()
            .with_order(toolkit::api::odata::parse_orderby(raw).unwrap())
            .with_limit(1)
    };
    for field in [
        "binding_subject_id",
        "binding_subject_type",
        "pack_id",
        "parent_grant_id",
        "state",
    ] {
        let err = env
            .grants
            .list_grants(&c, &ordered(&format!("{field} asc")))
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                DomainError::Query(toolkit_odata::Error::InvalidOrderByField(_))
            ),
            "{field}: {err:?}"
        );
        let canonical = format!("{:?}", toolkit_canonical_errors::CanonicalError::from(err));
        assert!(canonical.contains("INVALID_ORDERBY_FIELD"), "{canonical}");
    }
    let first = env
        .grants
        .list_grants(&c, &ordered("id asc"))
        .await
        .unwrap();
    let cursor = first.page_info.next_cursor.clone().expect("a second page");
    let second = env
        .grants
        .list_grants(
            &c,
            &ODataQuery::new()
                .with_cursor(toolkit_odata::CursorV1::decode(&cursor).unwrap())
                .with_order(toolkit_odata::ODataOrderBy::empty()),
        )
        .await
        .unwrap();
    assert!(first.items[0].id < second.items[0].id);
}

#[tokio::test]
async fn type_filters_ignore_minor_versions() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let g = env
        .grants
        .issue(&c, tenant, vec![terms()], false, token())
        .await
        .unwrap()
        .grants
        .remove(0);
    env.grants
        .change_binding(&c, g.id, BindingChange::Bind(user("alice")), token(), None)
        .await
        .unwrap();
    let filtered = |raw: &str| {
        ODataQuery::new().with_filter(toolkit_odata::parse_filter_string(raw).unwrap().into_expr())
    };
    for raw in [
        "resource_type eq 'gts.cf.core.lic.res.v1.2~test.le.app.seat.v1.7~'",
        "binding_subject_type in ('gts.cf.core.lic.subj.v1~test.le.app.user.v1.3~')",
    ] {
        let page = env.grants.list_grants(&c, &filtered(raw)).await.unwrap();
        assert_eq!(page.items.len(), 1, "{raw}");
    }
    let err = env
        .grants
        .list_grants(&c, &filtered("contains(resource_type, 'seat')"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            DomainError::Query(toolkit_odata::Error::InvalidFilter(_))
        ),
        "{err:?}"
    );
    // PostgreSQL does not take a NUL: the filter is refused before the store.
    let err = env
        .grants
        .list_grants(&c, &filtered("binding_subject_id eq 'a\u{0}b'"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            DomainError::Query(toolkit_odata::Error::InvalidFilter(_))
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn the_issued_window_is_the_stored_one() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let minus_five = time::UtcOffset::from_hms(-5, 0, 0).unwrap();
    let mut t = terms();
    t.window.expires_at =
        (t.window.expires_at.replace_nanosecond(123_456_789).unwrap()).to_offset(minus_five);
    let issued = env
        .grants
        .issue(&c, tenant, vec![t], false, token())
        .await
        .unwrap()
        .grants
        .remove(0);
    assert_eq!(
        issued.validity.window().expires_at.nanosecond(),
        123_456_000
    );
    assert_eq!(
        issued.validity.window().expires_at.offset(),
        time::UtcOffset::UTC
    );
    assert_eq!(env.grants.get_grant(&c, issued.id).await.unwrap(), issued);

    // RFC 3339 does not write an offset with seconds; the gear stores UTC.
    let mut odd = terms();
    odd.window.expires_at = odd
        .window
        .expires_at
        .to_offset(time::UtcOffset::from_hms(5, 30, 15).unwrap());
    env.grants
        .issue(&c, tenant, vec![odd], false, token())
        .await
        .unwrap();

    // Both ends fall into one microsecond, so the stored window is empty.
    let mut empty = terms();
    let start = OffsetDateTime::now_utc().replace_nanosecond(100).unwrap() + Duration::hours(1);
    empty.window.not_before = start;
    empty.window.expires_at = start + Duration::nanoseconds(800);
    let err = env
        .grants
        .issue(&c, tenant, vec![empty], false, token())
        .await
        .unwrap_err();
    assert_eq!(reasons(&err), vec![reason::INVALID_WINDOW.to_owned()]);
}

#[tokio::test]
async fn bind_and_unbind() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let g = env
        .grants
        .issue(&c, tenant, vec![terms()], false, token())
        .await
        .unwrap()
        .grants[0]
        .clone();

    let t = token();
    let bound = env
        .grants
        .change_binding(
            &c,
            g.id,
            BindingChange::Bind(user("alice")),
            t.clone(),
            Some(1),
        )
        .await
        .unwrap();
    assert_eq!(
        bound.binding,
        Binding::Subject {
            subject: user("alice")
        }
    );
    assert_eq!(bound.version, 2);
    let replay = env
        .grants
        .change_binding(&c, g.id, BindingChange::Bind(user("alice")), t, Some(1))
        .await
        .unwrap();
    assert_eq!(replay, bound);

    let err = env
        .grants
        .change_binding(&c, g.id, BindingChange::Bind(user("bob")), token(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::InvalidTransition(_)), "{err:?}");

    let err = env
        .grants
        .change_binding(&c, g.id, BindingChange::Unbind, token(), Some(1))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::VersionMismatch), "{err:?}");

    let unbound = env
        .grants
        .change_binding(&c, g.id, BindingChange::Unbind, token(), Some(2))
        .await
        .unwrap();
    assert_eq!(unbound.binding, Binding::None);
    assert_eq!(env.grants.get_grant(&c, g.id).await.unwrap(), unbound);

    let err = env
        .grants
        .change_binding(&c, g.id, BindingChange::Unbind, token(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::InvalidTransition(_)), "{err:?}");
}

#[tokio::test]
async fn bind_accepts_derived_subject_types_only() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let g = env
        .grants
        .issue(&c, tenant, vec![terms()], false, token())
        .await
        .unwrap()
        .grants[0]
        .clone();

    let tenant_subject = SubjectRef {
        gts_type: GtsTypeId::new(tenant_subject()),
        id: tenant.to_string(),
    };
    let err = env
        .grants
        .change_binding(&c, g.id, BindingChange::Bind(tenant_subject), token(), None)
        .await
        .unwrap_err();
    assert_eq!(reasons(&err), vec![reason::TYPE_NOT_DERIVED.to_owned()]);

    let ghost = SubjectRef {
        gts_type: GtsTypeId::new(GHOST),
        id: "alice".to_owned(),
    };
    let err = env
        .grants
        .change_binding(&c, g.id, BindingChange::Bind(ghost), token(), None)
        .await
        .unwrap_err();
    assert_eq!(reasons(&err), vec![reason::TYPE_NOT_REGISTERED.to_owned()]);

    let admin = SubjectRef {
        gts_type: GtsTypeId::new(ADMIN),
        id: "root".to_owned(),
    };
    let bound = env
        .grants
        .change_binding(&c, g.id, BindingChange::Bind(admin.clone()), token(), None)
        .await
        .unwrap();
    assert_eq!(bound.binding, Binding::Subject { subject: admin });
}

#[tokio::test]
async fn bind_respects_fixed_binding_and_may_bind() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let mut fixed = terms();
    fixed.fixed_subject = Some(input(user("alice")));
    let mut no_bind = terms();
    no_bind.permissions.may_bind = false;
    let issued = env
        .grants
        .issue(&c, tenant, vec![fixed, no_bind], true, token())
        .await
        .unwrap();

    for g in &issued.grants {
        let err = env
            .grants
            .change_binding(&c, g.id, BindingChange::Unbind, token(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::InvalidTransition(_)), "{err:?}");
    }
}

#[tokio::test]
async fn reads_are_scoped_to_the_tenant() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let g = env
        .grants
        .issue(&ctx(a), a, vec![terms()], false, token())
        .await
        .unwrap()
        .grants[0]
        .clone();
    env.grants
        .issue(&ctx(b), b, vec![terms()], false, token())
        .await
        .unwrap();

    let page = env
        .grants
        .list_grants(&ctx(a), &ODataQuery::new())
        .await
        .unwrap();
    assert_eq!(page.items, vec![g.clone()]);
    let err = env.grants.get_grant(&ctx(b), g.id).await.unwrap_err();
    assert!(matches!(err, DomainError::NotFound { .. }), "{err:?}");
}

#[tokio::test]
async fn bind_refused_by_the_pdp_is_permission_denied() {
    let env = env_with(Arc::new(DenyAuthZ)).await;
    let err = env
        .grants
        .change_binding(
            &ctx(Uuid::now_v7()),
            GrantId(Uuid::now_v7()),
            BindingChange::Unbind,
            token(),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::PermissionDenied), "{err:?}");
}

/// The `issue` plugin, with the external id and the replaced evidence taken
/// from the token: `"1/b>a"` is evidence `b` that replaces `a`.
struct ChainPlugin;

#[async_trait::async_trait]
impl GrantAdmissionPluginV1 for ChainPlugin {
    fn info(&self) -> license_enforcement_sdk::PluginInfo {
        le_issue_admission_plugin::IssueAdmissionPlugin.info()
    }

    fn intakes(&self) -> license_enforcement_sdk::Intakes {
        le_issue_admission_plugin::IssueAdmissionPlugin.intakes()
    }

    async fn admit(
        &self,
        input: AdmissionInput,
        instant: OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Admission, AdmissionError> {
        let AdmissionInput::Issue {
            idempotency_token, ..
        } = &input
        else {
            unreachable!()
        };
        let ids = idempotency_token
            .split_once('/')
            .map_or(idempotency_token.as_str(), |(_, ids)| ids);
        let (external, replaces) = match ids.split_once('>') {
            Some((e, r)) => (e.to_owned(), Some(r.to_owned())),
            None => (ids.to_owned(), None),
        };
        let mut admission = le_issue_admission_plugin::IssueAdmissionPlugin
            .admit(input, instant, material)
            .await?;
        if let Some(e) = admission.evidence.as_mut() {
            e.external_id = Some(external);
            e.replaces = replaces;
        }
        Ok(admission)
    }

    async fn verify(
        &self,
        root_id: GrantId,
        evidence: &license_enforcement_sdk::StoredEvidence,
        instant: OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Verification, license_enforcement_sdk::PluginFault> {
        le_issue_admission_plugin::IssueAdmissionPlugin
            .verify(root_id, evidence, instant, material)
            .await
    }

    async fn health(&self) -> Result<(), license_enforcement_sdk::PluginFault> {
        Ok(())
    }
}

#[tokio::test]
async fn evidence_ids_and_replacement_are_checked() {
    let env = env_with_plugin(Arc::new(AllowAuthZ), Arc::new(ChainPlugin)).await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let issue = |token: &str| {
        env.grants
            .issue(&c, tenant, vec![terms()], false, token.to_owned())
    };

    let err = issue("1/b>a").await.unwrap_err();
    assert!(
        matches!(
            err,
            DomainError::EvidenceChainBroken(precondition::REPLACED_EVIDENCE_MISSING)
        ),
        "{err:?}"
    );
    issue("2/a").await.unwrap();
    let err = issue("3/a").await.unwrap_err();
    assert!(
        matches!(err, DomainError::EvidenceRejected(RejectionReason::Replay)),
        "{err:?}"
    );
    let err = issue("4/b>a").await.unwrap_err();
    assert!(matches!(err, DomainError::NotImplemented(_)), "{err:?}");
}

/// Allows everything; scopes a request on one resource to that resource id.
struct ResourceIdAuthZ;

#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for ResourceIdAuthZ {
    async fn evaluate(
        &self,
        ctx: toolkit_security::PlatformSecurityContext,
        request: authz_resolver_sdk::models::EvaluationRequest,
    ) -> Result<
        authz_resolver_sdk::models::EvaluationResponse,
        toolkit_canonical_errors::CanonicalError,
    > {
        use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
        let (true, Some(id)) = (request.context.require_constraints, request.resource.id) else {
            return AllowAuthZ.evaluate(ctx, request).await;
        };
        Ok(authz_resolver_sdk::models::EvaluationResponse {
            decision: true,
            context: authz_resolver_sdk::models::EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        toolkit_security::pep_properties::RESOURCE_ID,
                        [id],
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

#[tokio::test]
async fn get_pack_counts_members_when_scoped_to_the_pack_id() {
    let env = env_with(Arc::new(ResourceIdAuthZ)).await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);

    let pack = env
        .grants
        .issue(&c, tenant, vec![terms(), terms(), terms()], true, token())
        .await
        .unwrap()
        .pack
        .unwrap();
    assert_eq!(pack.counts[0].unbound, 3);
    assert_eq!(env.grants.get_pack(&c, pack.id).await.unwrap(), pack);
}

#[tokio::test]
async fn bind_in_another_tenant_answers_not_found() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let g = env
        .grants
        .issue(&ctx(a), a, vec![terms()], false, token())
        .await
        .unwrap()
        .grants[0]
        .clone();
    let err = env
        .grants
        .change_binding(&ctx(b), g.id, BindingChange::Unbind, token(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::NotFound { .. }), "{err:?}");
}

/// The `issue` plugin that writes the plugin material named by the token:
/// `"n/put:1:absent"` puts `1` if the key is absent, `"n/del::2"` deletes the
/// key if it holds `2`, an empty expectation is `Any`.
struct MaterialPlugin;

#[async_trait::async_trait]
impl GrantAdmissionPluginV1 for MaterialPlugin {
    fn info(&self) -> license_enforcement_sdk::PluginInfo {
        le_issue_admission_plugin::IssueAdmissionPlugin.info()
    }

    fn intakes(&self) -> license_enforcement_sdk::Intakes {
        le_issue_admission_plugin::IssueAdmissionPlugin.intakes()
    }

    async fn admit(
        &self,
        input: AdmissionInput,
        instant: OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Admission, AdmissionError> {
        use license_enforcement_sdk::{Expect, MaterialWrite};
        let AdmissionInput::Issue {
            idempotency_token, ..
        } = &input
        else {
            unreachable!()
        };
        let (_, spec) = idempotency_token.split_once('/').unwrap();
        let mut parts = spec.split(':');
        let (op, value, expect) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        let write = match op {
            "put" => MaterialWrite::put("k", value.as_bytes().to_vec()),
            _ => MaterialWrite::delete("k"),
        };
        let write = match expect {
            "" => write,
            "absent" => write.expecting(Expect::Absent),
            v => write.expecting(Expect::Equals(v.as_bytes().to_vec())),
        };
        let mut admission = le_issue_admission_plugin::IssueAdmissionPlugin
            .admit(input, instant, material)
            .await?;
        admission.material = vec![write];
        Ok(admission)
    }

    async fn verify(
        &self,
        root_id: GrantId,
        evidence: &license_enforcement_sdk::StoredEvidence,
        instant: OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Verification, license_enforcement_sdk::PluginFault> {
        le_issue_admission_plugin::IssueAdmissionPlugin
            .verify(root_id, evidence, instant, material)
            .await
    }

    async fn health(&self) -> Result<(), license_enforcement_sdk::PluginFault> {
        Ok(())
    }
}

#[tokio::test]
async fn material_writes_apply_only_when_the_expectation_holds() {
    let env = env_with_plugin(Arc::new(AllowAuthZ), Arc::new(MaterialPlugin)).await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let issue = |token: &str| {
        env.grants
            .issue(&c, tenant, vec![terms()], false, token.to_owned())
    };
    let conflict = |r: Result<IssueResult, DomainError>| {
        assert!(matches!(r, Err(DomainError::AdmissionConflict)), "{r:?}");
    };

    issue("1/put:1:absent").await.unwrap();
    conflict(issue("2/put:1:absent").await);
    conflict(issue("3/put:2:0").await);
    issue("4/put:2:1").await.unwrap();
    issue("5/put:3:").await.unwrap();
    conflict(issue("6/del::2").await);
    issue("7/del::3").await.unwrap();
    issue("8/put:1:absent").await.unwrap();
}

#[tokio::test]
async fn issue_with_a_token_reused_after_its_window_names_the_token() {
    let env = env_with_window(Duration::seconds(-1)).await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let t = token();
    env.grants
        .issue(&c, tenant, vec![terms()], false, t.clone())
        .await
        .unwrap();
    let err = env
        .grants
        .issue(&c, tenant, vec![terms()], false, t)
        .await
        .unwrap_err();
    assert_eq!(reasons(&err), vec![reason::TOKEN_REUSED.to_owned()]);
    let DomainError::InvalidArgument(v) = &err else {
        unreachable!()
    };
    assert_eq!(v[0].field, "idempotency_token");
}

/// The `issue` plugin, which admits a root whose window ends before it starts.
struct BadWindowPlugin;

#[async_trait::async_trait]
impl GrantAdmissionPluginV1 for BadWindowPlugin {
    fn info(&self) -> license_enforcement_sdk::PluginInfo {
        le_issue_admission_plugin::IssueAdmissionPlugin.info()
    }

    fn intakes(&self) -> license_enforcement_sdk::Intakes {
        le_issue_admission_plugin::IssueAdmissionPlugin.intakes()
    }

    async fn admit(
        &self,
        input: AdmissionInput,
        instant: OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Admission, AdmissionError> {
        let mut admission = le_issue_admission_plugin::IssueAdmissionPlugin
            .admit(input, instant, material)
            .await?;
        let window = &mut admission.roots[0].fields.terms.window;
        window.expires_at = window.not_before - Duration::days(1);
        Ok(admission)
    }

    async fn verify(
        &self,
        root_id: GrantId,
        evidence: &license_enforcement_sdk::StoredEvidence,
        instant: OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Verification, license_enforcement_sdk::PluginFault> {
        le_issue_admission_plugin::IssueAdmissionPlugin
            .verify(root_id, evidence, instant, material)
            .await
    }

    async fn health(&self) -> Result<(), license_enforcement_sdk::PluginFault> {
        Ok(())
    }
}

#[tokio::test]
async fn issue_validates_the_roots_of_the_plugin_not_the_request() {
    let env = env_with_plugin(Arc::new(AllowAuthZ), Arc::new(BadWindowPlugin)).await;
    let tenant = Uuid::now_v7();
    let err = env
        .grants
        .issue(&ctx(tenant), tenant, vec![terms()], false, token())
        .await
        .unwrap_err();
    assert_eq!(reasons(&err), vec![reason::INVALID_WINDOW.to_owned()]);
}

#[tokio::test]
async fn an_expired_token_is_free_for_another_payload() {
    let env = env_with_window(Duration::seconds(-1)).await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    let g = env
        .grants
        .issue(&c, tenant, vec![terms()], false, token())
        .await
        .unwrap()
        .grants[0]
        .clone();
    let t = token();
    env.grants
        .change_binding(
            &c,
            g.id,
            BindingChange::Bind(user("alice")),
            t.clone(),
            None,
        )
        .await
        .unwrap();
    let unbound = env
        .grants
        .change_binding(&c, g.id, BindingChange::Unbind, t, None)
        .await
        .unwrap();
    assert_eq!(unbound.binding, Binding::None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_retries_with_one_token_get_one_result() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let caller = ctx(tenant);
    let (issue_token, payload) = (token(), vec![terms()]);
    let issue = || {
        env.grants
            .issue(&caller, tenant, payload.clone(), false, issue_token.clone())
    };
    let (first, second) = tokio::join!(issue(), issue());
    let issued = first.unwrap();
    assert_eq!(issued, second.unwrap());

    let grant = issued.grants[0].id;
    let bind_token = token();
    let bind = || {
        env.grants.change_binding(
            &caller,
            grant,
            BindingChange::Bind(user("alice")),
            bind_token.clone(),
            None,
        )
    };
    let (first, second) = tokio::join!(bind(), bind());
    assert_eq!(first.unwrap(), second.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_binds_with_different_tokens_bind_once() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let caller = ctx(tenant);
    let grant = env
        .grants
        .issue(&caller, tenant, vec![terms()], false, token())
        .await
        .unwrap()
        .grants[0]
        .id;
    let bind = |id: &str| {
        env.grants
            .change_binding(&caller, grant, BindingChange::Bind(user(id)), token(), None)
    };
    let (alice, bob) = tokio::join!(bind("alice"), bind("bob"));
    let (won, lost) = match (alice, bob) {
        (Ok(g), Err(e)) | (Err(e), Ok(g)) => (g, e),
        other => panic!("exactly one bind must win: {other:?}"),
    };
    assert!(
        matches!(lost, DomainError::InvalidTransition(_)),
        "{lost:?}"
    );
    let stored = env.grants.get_grant(&caller, grant).await.unwrap();
    assert_eq!(stored.binding, won.binding);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_material_writes_with_one_expectation_apply_once() {
    let env = env_with_plugin(Arc::new(AllowAuthZ), Arc::new(MaterialPlugin)).await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    env.grants
        .issue(
            &c,
            tenant,
            vec![terms()],
            false,
            "0/put:1:absent".to_owned(),
        )
        .await
        .unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for n in 1..=8 {
        let (grants, c) = (Arc::clone(&env.grants), c.clone());
        tasks.spawn(async move {
            grants
                .issue(&c, tenant, vec![terms()], false, format!("{n}/put:2:1"))
                .await
        });
    }
    let results = tasks.join_all().await;
    let applied = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(applied, 1, "{results:?}");
    assert!(
        results
            .iter()
            .all(|r| matches!(r, Ok(_) | Err(DomainError::AdmissionConflict))),
        "{results:?}"
    );
}

#[test]
fn sdk_filters_have_the_limits_of_rest() {
    use license_enforcement_sdk::dto::ListQuery;
    let query = |filter: String| ListQuery {
        filter: Some(filter),
        ..ListQuery::default()
    };
    let field_reason = |e: toolkit_canonical_errors::CanonicalError| format!("{e:?}");
    let long = format!(
        "id eq '{}'",
        "x".repeat(toolkit::api::odata::MAX_FILTER_LEN)
    );
    let complex = vec!["a eq 1"; 700].join(" or ");
    for (filter, want) in [(long, "FILTER_TOO_LONG"), (complex, "FILTER_TOO_COMPLEX")] {
        let err = crate::domain::local_client::odata_query(&query(filter)).unwrap_err();
        assert!(field_reason(err).contains(want), "{want}");
    }
}

/// Admits through the issue plugin, but names one fixed root.
struct SameRoot(GrantId);

#[async_trait::async_trait]
impl GrantAdmissionPluginV1 for SameRoot {
    fn info(&self) -> license_enforcement_sdk::PluginInfo {
        le_issue_admission_plugin::IssueAdmissionPlugin.info()
    }

    fn intakes(&self) -> license_enforcement_sdk::Intakes {
        le_issue_admission_plugin::IssueAdmissionPlugin.intakes()
    }

    async fn admit(
        &self,
        input: license_enforcement_sdk::AdmissionInput,
        instant: time::OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Admission, license_enforcement_sdk::AdmissionError> {
        let mut admission = le_issue_admission_plugin::IssueAdmissionPlugin
            .admit(input, instant, material)
            .await?;
        for root in &mut admission.roots {
            root.id = self.0;
        }
        Ok(admission)
    }

    async fn verify(
        &self,
        root_id: GrantId,
        evidence: &license_enforcement_sdk::StoredEvidence,
        instant: time::OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Verification, license_enforcement_sdk::PluginFault> {
        le_issue_admission_plugin::IssueAdmissionPlugin
            .verify(root_id, evidence, instant, material)
            .await
    }

    async fn health(&self) -> Result<(), license_enforcement_sdk::PluginFault> {
        Ok(())
    }
}

#[tokio::test]
async fn a_root_that_is_already_stored_is_already_exists() {
    let root = GrantId(Uuid::now_v7());
    let env = env_with_plugin(Arc::new(AllowAuthZ), Arc::new(SameRoot(root))).await;
    let tenant = Uuid::now_v7();
    let c = ctx(tenant);
    env.grants
        .issue(&c, tenant, vec![terms()], false, token())
        .await
        .unwrap();
    let err = env
        .grants
        .issue(&c, tenant, vec![terms()], false, token())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::RootAlreadyAdmitted(id) if id == root),
        "{err:?}"
    );
    let canonical = toolkit_canonical_errors::CanonicalError::from(err);
    assert!(
        format!("{canonical:?}").contains(&root.to_string()),
        "{canonical:?}"
    );
}
