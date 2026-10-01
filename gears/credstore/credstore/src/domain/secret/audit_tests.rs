//! Audit tests for `cpt-cf-credstore-nfr-audit`: which service operations
//! publish an audit event, what the event carries, and that a broker that is
//! absent or rejecting changes neither the result nor the work done.

use std::sync::Arc;

use credstore_sdk::{
    CredentialPatch, CredentialWrite, Fallback as SdkFallback, PatchField, SecretRef, SecretType,
    SecretValue, SharingMode,
};
use toolkit_odata::ODataQuery;
use uuid::Uuid;

use crate::domain::ports::audit::{AuditEvent, AuditOperation, AuditOutcome, AuditSink};
use crate::domain::ports::metrics::CredStoreMetricsPort;
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::secret::model::{PutPrecondition, WritePrecondition};
use crate::domain::secret::service::{ListSettings, Service};
use crate::domain::secret::test_support::*;
use crate::infra::audit::EventBrokerAuditSink;
use crate::infra::audit::tests::{Behavior, FakeBroker};

const SECRET: &str = "s3cr3t-value-do-not-leak";

fn key(s: &str) -> SecretRef {
    SecretRef::new(s).expect("valid ref")
}

fn generic_gts() -> String {
    SecretType::generic().gts_id().to_owned()
}

fn write(secret: Option<&str>, sharing: SharingMode) -> CredentialWrite {
    CredentialWrite {
        secret_type: Some(SecretType::generic().into()),
        sharing,
        fallback: SdkFallback::Inherit,
        expires_at: None,
        secret: secret.map(SecretValue::from),
    }
}

fn replace(secret: Option<&str>, sharing: SharingMode) -> CredentialWrite {
    CredentialWrite {
        secret_type: None,
        ..write(secret, sharing)
    }
}

fn patch_secret(secret: PatchField<SecretValue>) -> CredentialPatch {
    CredentialPatch {
        secret_type: None,
        sharing: None,
        fallback: None,
        expires_at: PatchField::Absent,
        secret,
    }
}

fn patch_fallback_only() -> CredentialPatch {
    CredentialPatch {
        fallback: Some(SdkFallback::None),
        ..patch_secret(PatchField::Absent)
    }
}

struct Fixture {
    svc: Service,
    audit: Arc<RecordingAudit>,
    ctx: toolkit_security::SecurityContext,
    subject: Uuid,
    tenant: Uuid,
}

fn fixture() -> Fixture {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let audit = RecordingAudit::new();
    let svc = service_with_sink(tenant, audit.clone(), Arc::new(NoopMetrics));
    Fixture {
        svc,
        audit,
        ctx: make_ctx(subject, tenant),
        subject,
        tenant,
    }
}

fn service_with_sink(
    tenant: Uuid,
    sink: Arc<dyn AuditSink>,
    metrics: Arc<dyn CredStoreMetricsPort>,
) -> Service {
    Service::new(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        Arc::new(FakePluginSelector::new(FakePlugin::new())) as Arc<dyn PluginSelector>,
        catalog_type_resolver(),
        metrics,
        ListSettings {
            max_limit: 200,
            secret_mode_cap: 25,
        },
    )
    .with_audit(sink)
}

fn assert_no_secret(events: &[AuditEvent]) {
    assert!(!format!("{events:?}").contains(SECRET));
}

#[tokio::test]
async fn get_secret_publishes_one_read_event_naming_everything_but_the_secret() {
    let f = fixture();
    f.svc
        .put(
            &f.ctx,
            &key("db"),
            write(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("create");
    let before = f.audit.events().len();

    let secret = f.svc.get_secret(&f.ctx, &key("db")).await.expect("read");
    assert!(secret.is_some());

    let events = f.audit.events();
    assert_eq!(events.len() - before, 1);
    assert_eq!(
        events[before],
        AuditEvent {
            subject_id: f.subject,
            tenant_id: f.tenant,
            reference: "db".to_owned(),
            secret_type: generic_gts(),
            operation: AuditOperation::Read,
            outcome: AuditOutcome::Success,
        }
    );
    assert_no_secret(&events);
}

#[tokio::test]
async fn a_read_without_the_secret_publishes_nothing() {
    let f = fixture();
    f.svc
        .put(
            &f.ctx,
            &key("db"),
            write(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("create");
    let before = f.audit.events().len();

    assert!(f.svc.get(&f.ctx, &key("db")).await.expect("get").is_some());
    assert!(
        f.svc
            .resolve_credential(&f.ctx, &key("db"))
            .await
            .expect("resolve")
            .is_some()
    );
    let fields = ["reference".to_owned(), "type".to_owned()];
    assert!(
        f.svc
            .get_item(&f.ctx, &key("db"), Some(&fields))
            .await
            .expect("metadata projection")
            .is_some()
    );
    let metadata_page = ODataQuery::new();
    f.svc.list(&f.ctx, &metadata_page).await.expect("list");
    // A miss discloses nothing and is not a secret read.
    assert!(
        f.svc
            .get_secret(&f.ctx, &key("absent"))
            .await
            .expect("miss")
            .is_none()
    );

    assert_eq!(f.audit.events().len(), before);
}

#[tokio::test]
async fn point_read_with_secret_and_record_fields_publishes_one_read_event() {
    let f = fixture();
    f.svc
        .put(
            &f.ctx,
            &key("db"),
            write(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("create");
    let before = f.audit.events().len();

    let fields = [
        "reference".to_owned(),
        "status".to_owned(),
        "secret".to_owned(),
    ];
    let item = f
        .svc
        .get_item(&f.ctx, &key("db"), Some(&fields))
        .await
        .expect("read")
        .expect("found");
    assert!(item.value.is_some());

    let events = f.audit.events();
    assert_eq!(events.len() - before, 1);
    assert_eq!(events[before].operation, AuditOperation::Read);
}

#[tokio::test]
async fn secret_mode_publishes_one_event_per_secret_returned() {
    let f = fixture();
    for name in ["a", "b", "c"] {
        f.svc
            .put(
                &f.ctx,
                &key(name),
                write(Some(SECRET), SharingMode::Tenant),
                PutPrecondition::CreateOnly,
            )
            .await
            .expect("create");
    }
    let before = f.audit.events().len();

    let query = ODataQuery::new()
        .with_select(vec!["reference".to_owned(), "secret".to_owned()])
        .with_filter(
            toolkit_odata::parse_filter_string("reference in ('a', 'b', 'c')")
                .expect("filter")
                .into_expr(),
        );
    let page = f.svc.list(&f.ctx, &query).await.expect("secret mode");
    assert_eq!(page.items.len(), 3);

    let events = f.audit.events();
    let mut refs: Vec<_> = events[before..]
        .iter()
        .inspect(|e| {
            assert_eq!(e.operation, AuditOperation::Read);
            assert_eq!(e.outcome, AuditOutcome::Success);
            assert_eq!(e.tenant_id, f.tenant);
        })
        .map(|e| e.reference.clone())
        .collect();
    refs.sort();
    assert_eq!(refs, ["a", "b", "c"]);
    assert_no_secret(&events);
}

#[tokio::test]
async fn writes_that_carry_or_remove_a_secret_publish_one_event_each() {
    let f = fixture();
    let k = key("db");
    let tail = |f: &Fixture| f.audit.events().last().cloned().expect("an event");
    let count = |f: &Fixture| f.audit.events().len();

    f.svc
        .put(
            &f.ctx,
            &k,
            write(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("create");
    assert_eq!(count(&f), 1);
    assert_eq!(tail(&f).operation, AuditOperation::Create);

    f.svc
        .put(
            &f.ctx,
            &k,
            replace(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::Exists,
        )
        .await
        .expect("replace");
    assert_eq!(count(&f), 2);
    assert_eq!(tail(&f).operation, AuditOperation::Replace);

    f.svc
        .patch(
            &f.ctx,
            &k,
            patch_secret(PatchField::Set(SecretValue::from(SECRET))),
            WritePrecondition::Exists,
        )
        .await
        .expect("patch set");
    assert_eq!(count(&f), 3);
    assert_eq!(tail(&f).operation, AuditOperation::Replace);

    f.svc
        .patch(
            &f.ctx,
            &k,
            patch_secret(PatchField::Null),
            WritePrecondition::Exists,
        )
        .await
        .expect("patch remove");
    assert_eq!(count(&f), 4);
    assert_eq!(tail(&f).operation, AuditOperation::Remove);

    f.svc
        .put(
            &f.ctx,
            &k,
            replace(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::Exists,
        )
        .await
        .expect("set again");
    f.svc
        .put(
            &f.ctx,
            &k,
            replace(None, SharingMode::Tenant),
            PutPrecondition::Exists,
        )
        .await
        .expect("replace with null removes");
    assert_eq!(count(&f), 6);
    assert_eq!(tail(&f).operation, AuditOperation::Remove);

    f.svc
        .put(
            &f.ctx,
            &k,
            replace(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::Exists,
        )
        .await
        .expect("set a third time");
    f.svc
        .delete(&f.ctx, &k, WritePrecondition::Exists)
        .await
        .expect("delete");
    assert_eq!(count(&f), 8);
    let last = tail(&f);
    assert_eq!(last.operation, AuditOperation::Delete);
    assert_eq!(last.outcome, AuditOutcome::Success);
    assert_eq!(last.reference, "db");
    assert_eq!(last.subject_id, f.subject);
    assert_eq!(last.tenant_id, f.tenant);
    assert_eq!(last.secret_type, generic_gts());
    assert_no_secret(&f.audit.events());
}

#[tokio::test]
async fn metadata_only_writes_and_value_less_deletes_publish_nothing() {
    let f = fixture();
    let k = key("decl");

    // Declared (value-less) create, metadata-only patch, null over a
    // value-less record, then delete of the value-less record.
    f.svc
        .put(
            &f.ctx,
            &k,
            write(None, SharingMode::Tenant),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("declare");
    f.svc
        .patch(&f.ctx, &k, patch_fallback_only(), WritePrecondition::Exists)
        .await
        .expect("metadata patch");
    f.svc
        .put(
            &f.ctx,
            &k,
            replace(None, SharingMode::Tenant),
            PutPrecondition::Exists,
        )
        .await
        .expect("null over value-less");
    f.svc
        .delete(&f.ctx, &k, WritePrecondition::Exists)
        .await
        .expect("delete value-less");

    // Metadata-only patch of a record that holds a secret.
    let k2 = key("held");
    f.svc
        .put(
            &f.ctx,
            &k2,
            write(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("create");
    let after_create = f.audit.events().len();
    f.svc
        .patch(
            &f.ctx,
            &k2,
            patch_fallback_only(),
            WritePrecondition::Exists,
        )
        .await
        .expect("metadata patch of a held record");

    assert_eq!(after_create, 1);
    assert_eq!(f.audit.events().len(), 1);
}

#[tokio::test]
async fn a_failed_secret_write_is_published_with_outcome_failure() {
    let f = fixture();
    let k = key("db");
    f.svc
        .put(
            &f.ctx,
            &k,
            write(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("create");

    // Create-only over an existing record: authorized, then conflicts.
    f.svc
        .put(
            &f.ctx,
            &k,
            write(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect_err("conflict");

    let events = f.audit.events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].operation, AuditOperation::Create);
    assert_eq!(events[1].outcome, AuditOutcome::Failure);
}

#[tokio::test]
async fn a_denied_write_discloses_and_changes_nothing_and_is_not_audited() {
    let tenant = Uuid::new_v4();
    let audit = RecordingAudit::new();
    let svc = Service::new(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        deny_enforcer(),
        Arc::new(FakePluginSelector::new(FakePlugin::new())) as Arc<dyn PluginSelector>,
        catalog_type_resolver(),
        Arc::new(NoopMetrics),
        ListSettings {
            max_limit: 200,
            secret_mode_cap: 25,
        },
    )
    .with_audit(audit.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    svc.put(
        &ctx,
        &key("db"),
        write(Some(SECRET), SharingMode::Tenant),
        PutPrecondition::CreateOnly,
    )
    .await
    .expect_err("denied");

    assert!(audit.events().is_empty());
}

// ── best-effort: the broker never changes a result ───────────────────────────

/// Run the same read/write/delete script and return what each step answered.
async fn script(svc: &Service, ctx: &toolkit_security::SecurityContext) -> Vec<String> {
    let k = key("db");
    let mut out = Vec::new();
    out.push(format!(
        "{:?}",
        svc.put(
            ctx,
            &k,
            write(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::CreateOnly
        )
        .await
        .map(|o| o.created)
    ));
    out.push(format!(
        "{:?}",
        svc.get_secret(ctx, &k)
            .await
            .map(|s| s.map(|s| s.secret.as_bytes().to_vec()))
    ));
    out.push(format!(
        "{:?}",
        svc.patch(
            ctx,
            &k,
            patch_secret(PatchField::Null),
            WritePrecondition::Exists
        )
        .await
        .map(|v| v.version)
    ));
    out.push(format!(
        "{:?}",
        svc.delete(ctx, &k, WritePrecondition::Exists).await
    ));
    out
}

#[tokio::test]
async fn an_absent_broker_changes_no_result_and_counts_every_dropped_event() {
    let tenant = Uuid::new_v4();
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let reference = service_with_sink(tenant, RecordingAudit::new(), Arc::new(NoopMetrics));
    let expected = script(&reference, &ctx).await;

    let metrics = FakeMetrics::new();
    let sink = Arc::new(EventBrokerAuditSink::new(
        Arc::new(|| None),
        metrics.clone(),
        std::time::Duration::from_secs(1),
    ));
    let svc = service_with_sink(tenant, sink, metrics.clone());
    let actual = script(&svc, &ctx).await;

    assert_eq!(actual, expected);
    // create, read, remove, then the delete of a value-less record (none).
    assert_eq!(metrics.audit_publish_failed_total(), 3);
}

#[tokio::test]
async fn a_rejecting_broker_changes_no_result_and_is_counted() {
    let tenant = Uuid::new_v4();
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let reference = service_with_sink(tenant, RecordingAudit::new(), Arc::new(NoopMetrics));
    let expected = script(&reference, &ctx).await;

    let broker = FakeBroker::new(Behavior::Reject);
    let metrics = FakeMetrics::new();
    let resolver_broker = broker.clone();
    let sink = Arc::new(EventBrokerAuditSink::new(
        Arc::new(
            move || Some(resolver_broker.clone() as Arc<dyn event_broker_sdk::EventBrokerApi>),
        ),
        metrics.clone(),
        std::time::Duration::from_secs(1),
    ));
    let svc = service_with_sink(tenant, sink, metrics.clone());
    let actual = script(&svc, &ctx).await;

    assert_eq!(actual, expected);
    assert_eq!(broker.published().len(), 3);
    assert_eq!(metrics.audit_publish_failed_total(), 3);
    let published = format!("{:?}", broker.published());
    assert!(!published.contains(SECRET));
}

#[tokio::test(start_paused = true)]
async fn a_hanging_broker_delays_a_read_by_at_most_the_timeout() {
    let tenant = Uuid::new_v4();
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let broker = FakeBroker::new(Behavior::Hang);
    let metrics = FakeMetrics::new();
    let sink = Arc::new(EventBrokerAuditSink::new(
        Arc::new(move || Some(broker.clone() as Arc<dyn event_broker_sdk::EventBrokerApi>)),
        metrics.clone(),
        std::time::Duration::from_millis(500),
    ));
    let svc = service_with_sink(tenant, sink, metrics.clone());
    let k = key("db");

    svc.put(
        &ctx,
        &k,
        write(Some(SECRET), SharingMode::Tenant),
        PutPrecondition::CreateOnly,
    )
    .await
    .expect("create");
    let started = tokio::time::Instant::now();
    let secret = svc.get_secret(&ctx, &k).await.expect("read");

    assert!(secret.is_some());
    assert_eq!(started.elapsed(), std::time::Duration::from_millis(500));
    assert_eq!(metrics.audit_publish_failed_total(), 2);
}

/// Build a service sharing `repo` and `plugin` so a test can race the read.
fn racing_fixture() -> (Fixture, Arc<FakeSecretRepo>, Arc<FakePlugin>) {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let audit = RecordingAudit::new();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let svc = Service::new(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        Arc::new(FakePluginSelector::new(plugin.clone())) as Arc<dyn PluginSelector>,
        catalog_type_resolver(),
        Arc::new(NoopMetrics),
        ListSettings {
            max_limit: 200,
            secret_mode_cap: 25,
        },
    )
    .with_audit(audit.clone());
    (
        Fixture {
            svc,
            audit,
            ctx: make_ctx(subject, tenant),
            subject,
            tenant,
        },
        repo,
        plugin,
    )
}

#[tokio::test]
async fn a_read_whose_reread_finds_the_row_gone_is_404_and_not_audited() {
    let (f, repo, plugin) = racing_fixture();
    f.svc
        .put(
            &f.ctx,
            &key("db"),
            write(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("create");
    let before = f.audit.events().len();
    let row = repo.rows().into_iter().next().expect("row");
    plugin.fail_next_gets_with_not_found(1);
    repo.vanish_after_next_resolve(row.id);

    let err = f
        .svc
        .get_secret(&f.ctx, &key("db"))
        .await
        .expect_err("row gone on re-read");
    assert!(matches!(err, crate::domain::error::DomainError::NotFound));
    assert_eq!(f.audit.events().len(), before, "a 404 read is not audited");
}

#[tokio::test]
async fn a_read_that_resolved_a_record_but_got_no_value_is_audited_as_failure() {
    let (f, _repo, plugin) = racing_fixture();
    f.svc
        .put(
            &f.ctx,
            &key("db"),
            write(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("create");
    let before = f.audit.events().len();
    plugin.fail_next_gets_with_not_found(1);

    f.svc.get_secret(&f.ctx, &key("db")).await.expect_err("503");
    let events = f.audit.events();
    assert_eq!(events.len() - before, 1);
    assert_eq!(events[before].operation, AuditOperation::Read);
    assert_eq!(events[before].outcome, AuditOutcome::Failure);
}

#[tokio::test]
async fn a_read_ending_in_secret_expired_is_audited_as_failure() {
    let (f, repo, plugin) = racing_fixture();
    f.svc
        .put(
            &f.ctx,
            &key("db"),
            write(Some(SECRET), SharingMode::Tenant),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("create");
    repo.force_expire(repo.rows()[0].id);
    let before = f.audit.events().len();
    let gets_before = plugin.get_calls();

    let err = f
        .svc
        .get_secret(&f.ctx, &key("db"))
        .await
        .expect_err("expired");
    assert!(matches!(
        err,
        crate::domain::error::DomainError::SecretExpired
    ));
    assert_eq!(
        plugin.get_calls(),
        gets_before,
        "the expired secret is never fetched from the store"
    );
    let events = f.audit.events();
    assert_eq!(events.len() - before, 1);
    assert_eq!(events[before].operation, AuditOperation::Read);
    assert_eq!(events[before].outcome, AuditOutcome::Failure);
}
