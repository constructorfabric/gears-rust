// Created: 2026-11-18 by Constructor Tech
//! Tests of the bounded read (`get_secret_within`, ADR-0009): the value is
//! served only when its row belongs to the caller-named bound or to a tenant
//! below it on the caller's chain, and the validator `Secret` carries is the
//! REST `ETag` one.
//!
//! Chain used throughout: `tenant` (the caller) under `partner` under `root`.

use std::sync::Arc;

use credstore_sdk::{OwnerId, SecretRef, SecretType, SecretValidator, SharingMode, TenantId};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::audit::{AuditOperation, AuditOutcome};
use crate::domain::ports::metrics::ReadOutcome;
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::secret::etag::weak_etag;
use crate::domain::secret::model::{Fallback, HealFlags, SecretRow, SecretStatus};
use crate::domain::secret::service::{ListSettings, Service};
use crate::domain::secret::test_support::*;

fn key(s: &str) -> SecretRef {
    SecretRef::new(s).expect("valid ref")
}

struct World {
    svc: Service,
    repo: Arc<FakeSecretRepo>,
    plugin: Arc<FakePlugin>,
    audit: Arc<RecordingAudit>,
    metrics: Arc<FakeMetrics>,
    root: Uuid,
    partner: Uuid,
    tenant: Uuid,
    subject: Uuid,
}

impl World {
    fn ctx(&self) -> toolkit_security::SecurityContext {
        make_ctx(self.subject, self.tenant)
    }

    /// Builds (does not store) a row; the value goes to the fake store when
    /// `value` is given, otherwise the row is `declared`.
    fn build(&self, tenant: Uuid, sharing: SharingMode, value: Option<&str>) -> SecretRow {
        let owner = if tenant == self.tenant {
            self.subject
        } else {
            Uuid::new_v4()
        };
        let mut row = SecretRow {
            id: Uuid::new_v4(),
            tenant_id: TenantId(tenant),
            reference: "k".to_owned(),
            sharing,
            owner_id: OwnerId(owner),
            status: SecretStatus::Active,
            version: 3,
            updated_at: OffsetDateTime::now_utc(),
            secret_type_uuid: SecretType::generic().uuid(),
            expires_at: None,
            value_version: None,
            fallback: Fallback::Inherit,
            heal: HealFlags::default(),
        };
        match value {
            Some(v) => row.value_version = Some(self.plugin.seed(&row.store_key(), v.as_bytes())),
            None => row.status = SecretStatus::Declared,
        }
        row
    }

    /// Builds and stores a row.
    fn row(&self, tenant: Uuid, sharing: SharingMode, value: Option<&str>) -> SecretRow {
        let row = self.build(tenant, sharing, value);
        self.repo.seed(row.clone());
        row
    }

    async fn within(&self, bound: Uuid) -> Result<Option<credstore_sdk::Secret>, DomainError> {
        self.svc
            .get_secret_within(&self.ctx(), &key("k"), TenantId(bound))
            .await
    }
}

fn world_with(enforcer: authz_resolver_sdk::PolicyEnforcer, visible_len: Option<usize>) -> World {
    let (root, partner, tenant, subject) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let audit = RecordingAudit::new();
    let metrics = FakeMetrics::new();
    let mut dir = FakeDir::new(vec![tenant, partner, root]);
    if let Some(n) = visible_len {
        dir = dir.with_visible_len(n);
    }
    let svc = Service::new(
        repo.clone(),
        Arc::new(dir),
        enforcer,
        Arc::new(FakePluginSelector::new(plugin.clone())) as Arc<dyn PluginSelector>,
        catalog_type_resolver(),
        metrics.clone(),
        ListSettings { max_limit: 200 },
    )
    .with_audit(audit.clone());
    World {
        svc,
        repo,
        plugin,
        audit,
        metrics,
        root,
        partner,
        tenant,
        subject,
    }
}

fn world() -> World {
    world_with(mock_enforcer(), None)
}

fn served(secret: Option<credstore_sdk::Secret>) -> credstore_sdk::Secret {
    secret.expect("a value is served")
}

fn bytes(secret: &credstore_sdk::Secret) -> &[u8] {
    secret.secret.as_bytes()
}

#[tokio::test]
async fn own_active_row_within_its_own_bound_is_served_with_a_strong_validator() {
    let w = world();
    let own = w.row(w.tenant, SharingMode::Tenant, Some("mine"));

    let secret = served(w.within(w.tenant).await.expect("read"));
    assert_eq!(bytes(&secret), b"mine");
    match secret.validator {
        SecretValidator::Strong(v) => assert_eq!((v.id, v.version), (own.id, own.version)),
        other @ SecretValidator::Weak(_) => panic!("expected Strong, got {other:?}"),
    }
}

#[tokio::test]
async fn a_value_from_the_partner_is_above_a_bound_at_the_caller() {
    let w = world();
    w.row(w.partner, SharingMode::Shared, Some("partner-value"));

    let err = w.within(w.tenant).await.expect_err("above the bound");
    assert!(matches!(err, DomainError::OwnerAboveBound), "{err:?}");
    assert_eq!(w.plugin.get_calls(), 0, "the backend is not read");
    assert_eq!(
        w.metrics.last_read_outcome(),
        Some(ReadOutcome::OwnerAboveBound)
    );
}

#[tokio::test]
async fn a_value_from_the_partner_is_served_with_a_weak_validator_at_a_bound_on_the_partner() {
    let w = world();
    w.row(w.partner, SharingMode::Shared, Some("partner-value"));

    let secret = served(w.within(w.partner).await.expect("read"));
    assert_eq!(bytes(&secret), b"partner-value");
    assert!(matches!(secret.validator, SecretValidator::Weak(_)));
}

#[tokio::test]
async fn a_value_from_the_root_is_above_a_bound_on_the_partner() {
    let w = world();
    w.row(w.root, SharingMode::Shared, Some("root-value"));

    let err = w.within(w.partner).await.expect_err("above the bound");
    assert!(matches!(err, DomainError::OwnerAboveBound), "{err:?}");
    // A bound on the root itself reaches it.
    assert_eq!(
        bytes(&served(w.within(w.root).await.expect("read"))),
        b"root-value"
    );
}

#[tokio::test]
async fn a_declared_own_row_with_inherit_does_not_lower_the_owner_of_the_value() {
    let w = world();
    let own = w.row(w.tenant, SharingMode::Tenant, None);
    w.row(w.partner, SharingMode::Shared, Some("partner-value"));

    let err = w
        .within(w.tenant)
        .await
        .expect_err("the value is the partner's");
    assert!(matches!(err, DomainError::OwnerAboveBound), "{err:?}");

    let secret = served(w.within(w.partner).await.expect("read"));
    assert_eq!(bytes(&secret), b"partner-value");
    match secret.validator {
        SecretValidator::Strong(v) => {
            assert_eq!(
                (v.id, v.version),
                (own.id, own.version),
                "the own row's validator"
            );
        }
        other @ SecretValidator::Weak(_) => panic!("expected Strong, got {other:?}"),
    }
}

#[tokio::test]
async fn a_suppressing_row_above_the_bound_is_a_miss_not_an_error() {
    let w = world();
    let mut suppress = w.build(w.partner, SharingMode::Shared, None);
    suppress.fallback = Fallback::None;
    w.repo.seed(suppress);

    assert!(w.within(w.tenant).await.expect("no error").is_none());
}

#[tokio::test]
async fn a_decisive_row_of_another_type_is_a_miss_whatever_the_bound() {
    let w = world();
    w.row(w.tenant, SharingMode::Private, None);
    let mut other = w.build(w.partner, SharingMode::Shared, Some("type-b"));
    other.secret_type_uuid = SecretType::from_name("api-key").expect("known").uuid();
    w.repo.seed(other);

    for bound in [w.tenant, w.partner] {
        let got = w.within(bound).await.expect("no error");
        assert!(got.is_none(), "bound {bound}");
    }
}

#[tokio::test]
async fn above_the_bound_wins_over_expiry_and_within_it_expiry_is_reported() {
    let w = world();
    let mut row = w.build(w.partner, SharingMode::Shared, Some("old"));
    row.expires_at = Some(OffsetDateTime::now_utc() - time::Duration::hours(1));
    w.repo.seed(row);

    let above = w.within(w.tenant).await.expect_err("above");
    assert!(matches!(above, DomainError::OwnerAboveBound), "{above:?}");
    let within = w.within(w.partner).await.expect_err("expired");
    assert!(matches!(within, DomainError::SecretExpired), "{within:?}");
}

#[tokio::test]
async fn a_bound_off_the_chain_is_not_on_chain() {
    let w = world();
    w.row(w.partner, SharingMode::Shared, Some("v"));

    let err = w
        .within(Uuid::new_v4())
        .await
        .expect_err("not on the chain");
    assert!(matches!(err, DomainError::BoundNotOnChain), "{err:?}");
    // Nothing is read for it and nothing is audited.
    assert_eq!(w.plugin.get_calls(), 0);
    assert!(w.audit.events().is_empty());
}

#[tokio::test]
async fn an_isolation_barrier_cuts_the_visible_chain_but_not_the_walk() {
    // The partner is a barrier: the caller sees itself and the partner.
    let w = world_with(mock_enforcer(), Some(2));
    w.row(w.partner, SharingMode::Shared, Some("partner-value"));

    assert_eq!(
        bytes(&served(
            w.within(w.partner).await.expect("the barrier itself")
        )),
        b"partner-value"
    );
    let above = w.within(w.root).await.expect_err("above the barrier");
    assert!(matches!(above, DomainError::BoundNotOnChain), "{above:?}");
    let random = w.within(Uuid::new_v4()).await.expect_err("random id");
    assert!(matches!(random, DomainError::BoundNotOnChain), "{random:?}");
    assert_eq!(
        format!("{above:?}"),
        format!("{random:?}"),
        "one answer for both"
    );
}

#[tokio::test]
async fn a_self_managed_caller_sees_only_itself() {
    let w = world_with(mock_enforcer(), Some(1));
    w.row(w.partner, SharingMode::Shared, Some("partner-value"));

    let own_bound = w.within(w.tenant).await.expect_err("the value is above");
    assert!(
        matches!(own_bound, DomainError::OwnerAboveBound),
        "{own_bound:?}"
    );
    let parent = w
        .within(w.partner)
        .await
        .expect_err("parent is not visible");
    assert!(matches!(parent, DomainError::BoundNotOnChain), "{parent:?}");

    // Inheritance is unchanged: an unbounded read still walks through the barrier.
    let unbounded = served(w.svc.get_secret(&w.ctx(), &key("k")).await.expect("read"));
    assert_eq!(bytes(&unbounded), b"partner-value");
}

#[tokio::test]
async fn a_denied_caller_gets_access_denied_whatever_the_bound() {
    let w = world_with(deny_enforcer(), None);
    w.row(w.partner, SharingMode::Shared, Some("v"));

    for bound in [w.tenant, w.root, Uuid::new_v4()] {
        let err = w.within(bound).await.expect_err("denied");
        assert!(matches!(err, DomainError::AccessDenied { .. }), "{err:?}");
    }
}

#[tokio::test]
async fn a_record_the_pdp_constraints_exclude_is_a_miss_not_above_the_bound() {
    let w = world_with(reference_enforcer(&["other"], None), None);
    w.row(w.partner, SharingMode::Shared, Some("v"));

    assert!(w.within(w.tenant).await.expect("no error").is_none());
    assert!(w.audit.events().is_empty());
}

#[tokio::test]
async fn owner_above_bound_is_audited_once_as_a_failed_read() {
    let w = world();
    w.row(w.partner, SharingMode::Shared, Some("v"));

    w.within(w.tenant).await.expect_err("above the bound");
    let events = w.audit.events();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].operation, AuditOperation::Read);
    assert_eq!(events[0].outcome, AuditOutcome::Failure);
}

#[tokio::test]
async fn the_inherited_validator_is_the_opaque_weak_etag_of_the_winning_row() {
    let w = world();
    let row = w.row(w.partner, SharingMode::Shared, Some("v"));

    let secret = served(w.svc.get_secret(&w.ctx(), &key("k")).await.expect("read"));
    let SecretValidator::Weak(weak) = &secret.validator else {
        panic!("expected Weak, got {:?}", secret.validator);
    };
    // The same text the REST `ETag` carries for this read.
    assert_eq!(weak.as_str(), weak_etag(w.tenant, "k", row.id, row.version));
    assert!(weak.as_str().starts_with("W/\""));
    assert!(!weak.as_str().contains(&row.id.to_string()), "no row id");

    // A bounded read at a sufficient bound serves the very same validator.
    let bounded = served(w.within(w.partner).await.expect("read"));
    assert_eq!(bounded.validator, secret.validator);
}

#[tokio::test]
async fn an_own_row_gives_a_strong_validator_without_a_bound_too() {
    let w = world();
    let own = w.row(w.tenant, SharingMode::Tenant, Some("mine"));

    let secret = served(w.svc.get_secret(&w.ctx(), &key("k")).await.expect("read"));
    assert_eq!(
        secret.validator,
        SecretValidator::Strong(credstore_sdk::Validator {
            id: own.id,
            version: own.version
        })
    );
}
