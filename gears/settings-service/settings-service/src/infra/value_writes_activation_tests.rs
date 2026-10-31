//! The write path with consumer activation bound: the key's mutation lock
//! around every change, and the activation state each committed change records
//! in its own transaction.
//!
//! The database is `SQLite` on one connection and the lock a table in this
//! process, so nothing here overlaps in time; the concurrent rows run on
//! `PostgreSQL` (`tests/pg_activation_state_test.rs`). What is pinned here is
//! the logic: which subscriptions a change affects, what is recorded for them,
//! what a refusal leaves behind.

use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;
use serde_json::{Value, json};
use settings_service_sdk::SettingKey;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{BatchChange, PausePoint, WriteCoordinator};
use crate::audit::{AuditOperation, StoredAuditRecord};
use crate::domain::activation::subscriptions::ActivationRepository;
use crate::domain::activation::{
    ActivationState, ActivationStatus, ExpectedValue, plaintext_text, secret_hash,
};
use crate::domain::coherence::CoherencePublisher;
use crate::domain::declaration::DeclarationRepository;
use crate::domain::error::DomainError;
use crate::domain::ports::NoMetrics;
use crate::domain::resolution::{ScopeTarget, scope_class};
use crate::domain::stepup::USER_SUBJECT_TYPE;
use crate::domain::value::ValueRepository;
use crate::domain::writes::{Change, ValueWriter, WriteActor};
use crate::infra::storage::activation_repo::ActivationRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::pending_secret_repo::PendingSecretRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::infra::type_validator::GtsTypeValidator;
use crate::test_support::{
    ActivationRig, AllowAllGate, BOOL, DenyAllGate, FixedStepUp, ReadAnswer, RecordingPublisher,
    RecordingSecrets, ResolutionHarness, SECRET, activation_rig, resolution_catalogue,
};

/// What a test's pause point reaches into once: the database, a declaration
/// and a tenant.
type Moving = Arc<std::sync::Mutex<Option<(Arc<Db>, Uuid, Uuid)>>>;

/// The database a test's pause point moves a row in.
type Db = toolkit_db::DBProvider<toolkit_db::DbError>;

struct Rig {
    base: ResolutionHarness,
    coordinator: Arc<WriteCoordinator>,
    activation: ActivationRig,
    secrets: Arc<RecordingSecrets>,
}

fn actor(tenant: Uuid) -> WriteActor {
    WriteActor {
        ctx: SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant)
            .subject_type(USER_SUBJECT_TYPE)
            .build()
            .expect("context"),
        request_id: "req".to_owned(),
        step_up_token: Some(SecretString::from("token".to_owned())),
        visibility: crate::domain::category::DomainVisibility::Unrestricted,
    }
}

fn text(tenant: Uuid) -> String {
    crate::domain::activation::tenant_text(tenant)
}

impl Rig {
    async fn new() -> Self {
        Self::with(
            Arc::new(AllowAllGate),
            Duration::from_millis(60),
            |coordinator| coordinator,
        )
        .await
    }

    async fn with(
        secret_gate: Arc<dyn crate::domain::ports::SecretResolveGate>,
        budget: Duration,
        shape: impl FnOnce(WriteCoordinator) -> WriteCoordinator,
    ) -> Self {
        Self::with_publisher(secret_gate, budget, shape, |_| {
            Arc::new(RecordingPublisher::default())
        })
        .await
    }

    async fn with_publisher(
        secret_gate: Arc<dyn crate::domain::ports::SecretResolveGate>,
        budget: Duration,
        shape: impl FnOnce(WriteCoordinator) -> WriteCoordinator,
        publisher: impl FnOnce(
            &Arc<crate::domain::activation::testing::InProcessLocks>,
        ) -> Arc<dyn crate::domain::ports::ChangePublisher>,
    ) -> Self {
        Self::over(
            ResolutionHarness::new().await,
            secret_gate,
            budget,
            shape,
            publisher,
        )
    }

    /// A rig whose coordinator ends each request through `hook`, built over
    /// the rig's own database.
    async fn with_hook(make: impl FnOnce(&Arc<Db>) -> Arc<CoherencePublisher>) -> Self {
        let base = ResolutionHarness::new().await;
        let hook = make(&base.db);
        Self::over(
            base,
            Arc::new(AllowAllGate),
            Duration::from_millis(60),
            |coordinator| coordinator.with_coherence(hook),
            |_| Arc::new(RecordingPublisher::default()),
        )
    }

    fn over(
        base: ResolutionHarness,
        secret_gate: Arc<dyn crate::domain::ports::SecretResolveGate>,
        budget: Duration,
        shape: impl FnOnce(WriteCoordinator) -> WriteCoordinator,
        publisher: impl FnOnce(
            &Arc<crate::domain::activation::testing::InProcessLocks>,
        ) -> Arc<dyn crate::domain::ports::ChangePublisher>,
    ) -> Self {
        let secrets = Arc::new(RecordingSecrets::default());
        let activation = activation_rig(&base, &secrets, secret_gate, budget);
        let writer = Arc::new(ValueWriter::new(
            ValueRepo,
            Arc::clone(&base.resolver),
            Arc::new(GtsTypeValidator::new(resolution_catalogue())),
            crate::infra::storage::audit_store::AuditStore,
            Arc::new(FixedStepUp::verified()),
            Arc::clone(&secrets) as Arc<dyn crate::domain::ports::SecretManager>,
            PendingSecretRepo,
            publisher(&activation.locks),
            Arc::new(NoMetrics),
        ));
        let coordinator = shape(
            WriteCoordinator::new(Arc::clone(&base.db), writer)
                .with_activation(activation.wiring.clone()),
        );
        Self {
            base,
            coordinator: Arc::new(coordinator),
            activation,
            secrets,
        }
    }

    fn key(&self, name: &str) -> SettingKey {
        self.base.key(name)
    }

    /// A public setting a caller may write without step-up.
    async fn declare(&self, name: &str, class: &str) -> Uuid {
        let id = self
            .base
            .declare_typed(name, class, json!(false), BOOL, "public")
            .await;
        self.clear_step_up(id, "public").await;
        id
    }

    async fn declare_secret(&self, name: &str) -> Uuid {
        let id = self
            .base
            .declare_typed(name, scope_class::CASCADING, json!(""), SECRET, "secret")
            .await;
        self.clear_step_up(id, "secret").await;
        id
    }

    async fn clear_step_up(&self, id: Uuid, classification: &str) {
        use crate::domain::declaration::DeclarationMetadata;
        let conn = self.base.db.conn().expect("connection");
        DeclarationRepo
            .update_metadata(
                &conn,
                &AccessScope::allow_all(),
                id,
                DeclarationMetadata {
                    mode: "standard".to_owned(),
                    display_name: None,
                    description: None,
                    domain_affinity: None,
                    licence_feature: None,
                    data_classification: classification.to_owned(),
                    requires_step_up: false,
                    anonymous_exposable: false,
                },
                None,
                true,
            )
            .await
            .expect("metadata");
    }

    async fn set(
        &self,
        who: &WriteActor,
        name: &str,
        tenant: Option<Uuid>,
        value: Value,
        if_match: Option<&str>,
    ) -> Result<crate::domain::writes::Committed, DomainError> {
        self.coordinator
            .change(
                who,
                &self.key(name),
                tenant,
                Change::Set(value),
                if_match,
                "set",
            )
            .await
    }

    async fn revert(
        &self,
        who: &WriteActor,
        name: &str,
        tenant: Option<Uuid>,
        if_match: Option<&str>,
    ) -> Result<crate::domain::writes::Committed, DomainError> {
        self.coordinator
            .change(
                who,
                &self.key(name),
                tenant,
                Change::Revert,
                if_match,
                "revert",
            )
            .await
    }

    /// Register a subscription as the Subscription Manager would have.
    async fn subscribe(&self, subscriber: &str, name: &str, tenant: Uuid) {
        let conn = self.base.db.conn().expect("connection");
        ActivationRepo
            .register(&conn, subscriber, self.key(name).as_str(), &text(tenant))
            .await
            .expect("registered");
    }

    async fn state(&self, subscriber: &str, name: &str, tenant: Uuid) -> Option<ActivationState> {
        let conn = self.base.db.conn().expect("connection");
        ActivationRepo
            .state(&conn, subscriber, self.key(name).as_str(), &text(tenant))
            .await
            .expect("state read")
    }

    /// The audit records of a key at a tenant, newest first.
    async fn records(&self, name: &str, tenant: Uuid) -> Vec<StoredAuditRecord> {
        let conn = self.base.db.conn().expect("connection");
        crate::infra::storage::audit_store::AuditStore
            .history(
                &conn,
                &AccessScope::allow_all(),
                self.key(name).as_str(),
                tenant,
                &toolkit_odata::ODataQuery::default(),
            )
            .await
            .expect("history")
            .items
    }

    async fn secret_uses(&self, name: &str, tenant: Uuid) -> Vec<StoredAuditRecord> {
        self.records(name, tenant)
            .await
            .into_iter()
            .filter(|r| r.operation == AuditOperation::SecretUse)
            .collect()
    }

    async fn rows(&self, declaration: Uuid) -> usize {
        let conn = self.base.db.conn().expect("connection");
        ValueRepo
            .find_all(&conn, &AccessScope::allow_all(), declaration)
            .await
            .expect("rows")
            .len()
    }

    /// Settle a state as a report would have, to see a later change reset it.
    async fn settle(&self, subscriber: &str, name: &str, tenant: Uuid, status: &str) {
        use crate::infra::storage::entity::activation_state::{Column, Entity};
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use toolkit_db::secure::SecureUpdateExt;
        let conn = self.base.db.conn().expect("connection");
        let now = crate::infra::storage::clock::now();
        Entity::update_many()
            .col_expr(Column::Status, Expr::value(status))
            .col_expr(Column::AppliedValue, Expr::value("reported"))
            .col_expr(Column::Detail, Expr::value("a detail"))
            .col_expr(Column::ReportedAt, Expr::value(now))
            .col_expr(Column::LastNotifiedAt, Expr::value(now))
            .filter(Column::Subscriber.eq(subscriber))
            .filter(Column::Key.eq(self.key(name).as_str()))
            .filter(Column::Tenant.eq(text(tenant)))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await
            .expect("settled");
    }
}

fn is_unavailable<T: std::fmt::Debug>(outcome: &Result<T, DomainError>) -> bool {
    matches!(outcome, Err(DomainError::Unavailable { .. }))
}

fn batch_set(key: SettingKey, value: Value) -> BatchChange {
    BatchChange {
        key,
        tenant: None,
        op: None,
        value: Some(value),
        if_match: Some("absent".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// What a change records
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_set_writes_one_awaiting_state_per_affected_subscription_and_none_for_a_shadowed_one() {
    // `flag` cascades; `b` holds an override of its own, `a` does not.
    let h = Rig::new().await;
    h.declare("flag", scope_class::CASCADING).await;
    let (root, a, b) = (h.base.tree.root, h.base.tree.a, h.base.tree.b);
    h.set(&actor(b), "flag", None, json!(true), Some("absent"))
        .await
        .expect("the override at b");
    for tenant in [root, a, b] {
        h.subscribe("gear", "flag", tenant).await;
    }

    let committed = h
        .set(&actor(root), "flag", None, json!(true), Some("absent"))
        .await
        .expect("the set at the root");

    let change_ref = h.records("flag", root).await[0].id;
    for tenant in [root, a] {
        let state = h
            .state("gear", "flag", tenant)
            .await
            .unwrap_or_else(|| panic!("a state at {tenant}"));
        assert_eq!(state.status, ActivationStatus::Awaiting);
        assert_eq!(state.expected_value, "true", "the canonical encoding");
        assert_eq!(state.change_ref, change_ref, "the audit record's id");
        assert_eq!(state.applied_value, None);
        assert_eq!(state.last_notified_at, None);
    }
    assert!(
        h.state("gear", "flag", b).await.is_none(),
        "a nearer override shadows the change: no state for `b`"
    );
    assert_eq!(
        committed.change_set_id,
        h.records("flag", root).await[0].change_set_id.unwrap()
    );
}

#[tokio::test]
async fn a_write_of_an_identical_value_affects_nothing() {
    let h = Rig::new().await;
    h.declare("flag", scope_class::CASCADING).await;
    let root = h.base.tree.root;
    let first = h
        .set(&actor(root), "flag", None, json!(true), Some("absent"))
        .await
        .expect("first write");
    h.subscribe("gear", "flag", root).await;

    let same = h
        .set(&actor(root), "flag", None, json!(true), Some(&first.etag))
        .await
        .expect("the same value again");
    assert!(
        h.state("gear", "flag", root).await.is_none(),
        "true to true changes nothing a consumer holds"
    );

    h.set(&actor(root), "flag", None, json!(false), Some(&same.etag))
        .await
        .expect("a different value");
    let state = h.state("gear", "flag", root).await.expect("now affected");
    assert_eq!(state.expected_value, "false");
}

#[tokio::test]
async fn a_local_key_affects_only_the_written_tenant_and_a_global_one_every_subscription() {
    let h = Rig::new().await;
    h.declare("local", scope_class::LOCAL).await;
    h.declare("global", scope_class::GLOBAL).await;
    let (root, a, c) = (h.base.tree.root, h.base.tree.a, h.base.tree.c);
    for tenant in [root, a] {
        h.subscribe("gear", "local", tenant).await;
    }
    for tenant in [root, a, c] {
        h.subscribe("gear", "global", tenant).await;
    }

    h.set(&actor(a), "local", None, json!(true), Some("absent"))
        .await
        .expect("a local write at a");
    assert!(h.state("gear", "local", a).await.is_some());
    assert!(
        h.state("gear", "local", root).await.is_none(),
        "a local value is never inherited"
    );

    h.set(&actor(root), "global", None, json!(true), Some("absent"))
        .await
        .expect("a global write at the root");
    for tenant in [root, a, c] {
        let state = h.state("gear", "global", tenant).await;
        assert_eq!(
            state.map(|s| s.expected_value),
            Some("true".to_owned()),
            "a global value is the effective value at every tenant"
        );
    }
}

#[tokio::test]
async fn an_overwrite_replaces_the_expectation_the_change_ref_and_the_report_fields() {
    let h = Rig::new().await;
    h.declare("flag", scope_class::CASCADING).await;
    let root = h.base.tree.root;
    h.subscribe("gear", "flag", root).await;
    let mut tag = "absent".to_owned();
    let mut previous_ref = None;
    for (status, value) in [("succeeded", true), ("failed", false), ("awaiting", true)] {
        let committed = h
            .set(&actor(root), "flag", None, json!(value), Some(&tag))
            .await
            .expect("a write");
        tag = committed.etag;
        let state = h.state("gear", "flag", root).await.expect("state");
        assert_eq!(state.status, ActivationStatus::Awaiting);
        assert_eq!(state.expected_value, value.to_string());
        assert_eq!(state.applied_value, None, "cleared by the overwrite");
        assert_eq!(state.detail, None);
        assert_eq!(state.reported_at, None);
        assert_eq!(state.last_notified_at, None);
        assert_ne!(Some(state.change_ref), previous_ref, "a newer change_ref");
        assert_eq!(state.change_ref, h.records("flag", root).await[0].id);
        previous_ref = Some(state.change_ref);
        h.settle("gear", "flag", root, status).await;
    }
}

#[tokio::test]
async fn within_one_batch_the_last_change_reaching_a_subscription_wins_its_row() {
    // Both scopes reach the subscription at `a`: the root's value is inherited
    // there, then `a` is written itself. Each change runs under its own lock in
    // its own transaction, in the request's order.
    let h = Rig::new().await;
    h.declare("flag", scope_class::CASCADING).await;
    let (root, a) = (h.base.tree.root, h.base.tree.a);
    h.subscribe("gear", "flag", a).await;

    let outcome = h
        .coordinator
        .batch(
            &actor(root),
            vec![
                batch_set(h.key("flag"), json!(true)),
                BatchChange {
                    tenant: Some(a),
                    ..batch_set(h.key("flag"), json!(false))
                },
            ],
        )
        .await
        .expect("the batch runs");

    assert!(
        outcome.results.iter().all(Result::is_ok),
        "{:?}",
        outcome.results
    );
    let state = h.state("gear", "flag", a).await.expect("state");
    assert_eq!(
        state.change_ref,
        h.records("flag", a).await[0].id,
        "the row names the last change"
    );
    assert_eq!(state.expected_value, "false");
    assert_eq!(
        outcome.activation.pairs().len(),
        1,
        "one pair, remembered once however many changes wrote it"
    );
    assert_eq!(outcome.activation.pairs()[0].subscriber, "gear");
    assert_eq!(outcome.activation.pairs()[0].tenant, text(a));
}

#[tokio::test]
async fn the_expected_value_of_a_secret_is_the_hash_salted_by_the_states_own_tenant() {
    let h = Rig::new().await;
    h.declare_secret("pw").await;
    let (root, a) = (h.base.tree.root, h.base.tree.a);
    h.subscribe("gear", "pw", root).await;
    h.subscribe("gear", "pw", a).await;

    let first = h
        .set(&actor(root), "pw", None, json!("hunter2"), Some("absent"))
        .await
        .expect("an inline secret");

    let key = h.key("pw");
    let hash = |tenant: Uuid| {
        secret_hash(
            key.as_str(),
            &text(tenant),
            &SecretString::from("hunter2".to_owned()),
        )
        .expect("hashes")
    };
    let at_root = h.state("gear", "pw", root).await.expect("state");
    let at_a = h.state("gear", "pw", a).await.expect("state");
    assert_eq!(at_root.expected_value, hash(root));
    assert_eq!(at_a.expected_value, hash(a));
    assert_ne!(
        at_root.expected_value, at_a.expected_value,
        "salted by tenant"
    );
    assert!(
        h.secret_uses("pw", root).await.is_empty(),
        "an inline plaintext is in hand: nothing is fetched"
    );

    // The same plaintext again: every write of a secret counts, and the hash
    // across the two changes is equal.
    h.set(
        &actor(root),
        "pw",
        None,
        json!("hunter2"),
        Some(&first.etag),
    )
    .await
    .expect("the same secret again");
    let again = h.state("gear", "pw", root).await.expect("state");
    assert_ne!(
        again.change_ref, at_root.change_ref,
        "a secret write always affects"
    );
    assert_eq!(again.expected_value, at_root.expected_value);
}

// ---------------------------------------------------------------------------
// The secret fetches
// ---------------------------------------------------------------------------

/// A secret at the root with an override at `a`, subscribers at `a` and `b`,
/// and the tag of the override.
async fn inherited_secret(h: &Rig) -> (Uuid, String) {
    let id = h.declare_secret("pw").await;
    let (root, a, b) = (h.base.tree.root, h.base.tree.a, h.base.tree.b);
    h.set(&actor(root), "pw", None, json!("base"), Some("absent"))
        .await
        .expect("the ancestor's secret");
    let own = h
        .set(&actor(a), "pw", None, json!("mine"), Some("absent"))
        .await
        .expect("the override at a");
    h.subscribe("gear", "pw", a).await;
    h.subscribe("gear", "pw", b).await;
    (id, own.etag)
}

#[tokio::test]
async fn a_revert_of_a_secret_override_hashes_the_fallback_fetched_once_and_audited() {
    let h = Rig::new().await;
    let (_, tag) = inherited_secret(&h).await;
    let (root, a, b) = (h.base.tree.root, h.base.tree.a, h.base.tree.b);
    // A replica's cache holding the previous ancestor secret: the fallback is
    // read through the freshness form, never from it.
    let key = h.key("pw");
    {
        let conn = h.base.db.conn().expect("connection");
        h.base
            .resolver
            .resolve(&conn, &key, ScopeTarget::Platform)
            .await
            .expect("warms the cache");
        let declaration = DeclarationRepo
            .find_by_key(&conn, &AccessScope::allow_all(), key.as_str())
            .await
            .expect("lookup")
            .expect("declared");
        let row = ValueRepo
            .find_one(&conn, &AccessScope::allow_all(), declaration.id, root)
            .await
            .expect("lookup")
            .expect("the root's row");
        h.secrets.seed("rotated-ref", "rotated");
        ValueRepo
            .update(
                &conn,
                &AccessScope::allow_all(),
                row.id,
                None,
                Some("rotated-ref".to_owned()),
                "rotation",
                row.last_change_at,
            )
            .await
            .expect("the ancestor secret rotated behind this replica's cache");
    }
    let before = h.secret_uses("pw", a).await.len();

    h.revert(&actor(a), "pw", None, Some(&tag))
        .await
        .expect("the revert");

    let expected = |tenant: Uuid| {
        ExpectedValue::of_secret(
            key.as_str(),
            &text(tenant),
            &SecretString::from("rotated".to_owned()),
        )
        .expect("hashes")
        .into_string()
    };
    assert_eq!(
        h.state("gear", "pw", a).await.map(|s| s.expected_value),
        Some(expected(a)),
        "the hash of the committed ancestor secret, not the cached one"
    );
    assert_eq!(
        h.state("gear", "pw", b).await.map(|s| s.expected_value),
        Some(expected(b)),
        "salted by the state's own tenant"
    );
    let uses = h.secret_uses("pw", a).await;
    assert_eq!(uses.len(), before + 1, "one fetch, one `secret_use` record");
    assert_eq!(
        uses[0].actor,
        crate::infra::service_identity::service_principal().to_string(),
        "attributed to the service's own identity"
    );
    assert_eq!(uses[0].post_image, Some(crate::audit::AuditValue::Masked));
}

#[tokio::test]
async fn a_revert_whose_fallback_is_the_default_fetches_nothing() {
    let h = Rig::new().await;
    h.declare_secret("pw").await;
    let a = h.base.tree.a;
    let own = h
        .set(&actor(a), "pw", None, json!("mine"), Some("absent"))
        .await
        .expect("the only value");
    h.subscribe("gear", "pw", a).await;

    h.revert(&actor(a), "pw", None, Some(&own.etag))
        .await
        .expect("the revert");

    let key = h.key("pw");
    let state = h.state("gear", "pw", a).await.expect("state");
    assert_eq!(
        state.expected_value,
        secret_hash(key.as_str(), &text(a), &plaintext_text(&json!(""))).expect("hashes"),
        "the default's value, hashed"
    );
    assert!(
        h.secret_uses("pw", a).await.is_empty(),
        "no fetch, no record"
    );
}

#[tokio::test]
async fn a_fetch_the_service_is_not_decided_for_refuses_the_change_and_stores_nothing() {
    let gate = Arc::new(DenyAllGate::default());
    let h = Rig::with(
        Arc::clone(&gate) as Arc<dyn crate::domain::ports::SecretResolveGate>,
        Duration::from_millis(60),
        |c| c,
    )
    .await;
    let (declaration, tag) = inherited_secret(&h).await;
    let (a, b) = (h.base.tree.a, h.base.tree.b);

    let outcome = h.revert(&actor(a), "pw", None, Some(&tag)).await;

    assert!(is_unavailable(&outcome), "refused 503: {outcome:?}");
    assert_eq!(gate.asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(h.rows(declaration).await, 2, "the override is still there");
    assert!(h.state("gear", "pw", a).await.is_none());
    assert!(h.state("gear", "pw", b).await.is_none());
    assert!(h.secret_uses("pw", a).await.is_empty());
    assert!(
        !h.activation.locks.is_held(h.key("pw").as_str()),
        "the key's lock is released"
    );
}

#[tokio::test]
async fn a_fetch_the_store_cannot_answer_refuses_the_change_with_the_lock_released() {
    let h = Rig::new().await;
    let (declaration, tag) = inherited_secret(&h).await;
    let a = h.base.tree.a;
    h.secrets.go_down();

    let outcome = h.revert(&actor(a), "pw", None, Some(&tag)).await;

    assert!(is_unavailable(&outcome), "{outcome:?}");
    assert_eq!(h.rows(declaration).await, 2);
    assert!(h.state("gear", "pw", a).await.is_none());
    assert!(!h.activation.locks.is_held(h.key("pw").as_str()));
}

#[tokio::test]
async fn the_secret_use_record_of_a_fetch_stays_when_the_changes_transaction_rolls_back() {
    // Between the fetch and the transaction the row moves: the commit's own tag
    // check then refuses the revert, after the fetch happened.
    let base_hold: Moving = Arc::default();
    let hook_state = Arc::clone(&base_hold);
    let pause: PausePoint = Arc::new(move |_key: &str| {
        let hook_state = Arc::clone(&hook_state);
        Box::pin(async move {
            let Some((db, declaration, tenant)) = hook_state.lock().expect("hook").take() else {
                return;
            };
            let conn = db.conn().expect("connection");
            let scope = AccessScope::allow_all();
            let row = ValueRepo
                .find_one(&conn, &scope, declaration, tenant)
                .await
                .expect("lookup")
                .expect("the override");
            ValueRepo
                .update(
                    &conn,
                    &scope,
                    row.id,
                    None,
                    row.secret_ref.clone(),
                    "someone else",
                    row.last_change_at,
                )
                .await
                .expect("another writer got there first");
        })
    });
    let h = Rig::with(Arc::new(AllowAllGate), Duration::from_millis(60), |c| {
        c.with_pause_point(pause)
    })
    .await;
    let (declaration, tag) = inherited_secret(&h).await;
    let a = h.base.tree.a;
    *base_hold.lock().expect("hook") = Some((Arc::clone(&h.base.db), declaration, a));
    let before = h.secret_uses("pw", a).await.len();

    let outcome = h.revert(&actor(a), "pw", None, Some(&tag)).await;

    assert!(
        matches!(outcome, Err(DomainError::PreconditionFailed { .. })),
        "{outcome:?}"
    );
    assert_eq!(
        h.secret_uses("pw", a).await.len(),
        before + 1,
        "the fetch's record stays"
    );
    assert_eq!(
        h.rows(declaration).await,
        2,
        "nothing of the change is stored"
    );
    assert!(h.state("gear", "pw", a).await.is_none());
    assert!(!h.activation.locks.is_held(h.key("pw").as_str()));
}

#[tokio::test]
async fn a_batch_set_adopting_a_staged_secret_fetches_it_once_and_hashes_per_tenant() {
    let h = Rig::new().await;
    h.declare_secret("pw").await;
    let root = h.base.tree.root;
    let (a, b) = (h.base.tree.a, h.base.tree.b);
    h.subscribe("gear", "pw", a).await;
    h.subscribe("gear", "pw", b).await;
    let admin = actor(a);
    let pending = h
        .coordinator
        .stage_secret(&admin, &h.key("pw"), None, json!("staged-value"))
        .await
        .expect("staged");
    let before = h.secret_uses("pw", a).await.len();

    let outcome = h
        .coordinator
        .batch(
            &admin,
            vec![batch_set(
                h.key("pw"),
                json!({ "pending_id": pending.id.to_string() }),
            )],
        )
        .await
        .expect("the batch runs");
    assert!(outcome.results[0].is_ok(), "{:?}", outcome.results[0]);

    let key = h.key("pw");
    for tenant in [a, b] {
        let state = h.state("gear", "pw", tenant).await.expect("state");
        assert_eq!(
            state.expected_value,
            secret_hash(
                key.as_str(),
                &text(tenant),
                &SecretString::from("staged-value".to_owned())
            )
            .expect("hashes")
        );
    }
    assert_eq!(h.secret_uses("pw", a).await.len(), before + 1, "one fetch");
    let _ = root;
}

#[tokio::test]
async fn a_staged_adoption_whose_fetch_the_service_is_not_decided_for_is_refused_and_keeps_its_token()
 {
    let gate = Arc::new(DenyAllGate::default());
    let h = Rig::with(
        Arc::clone(&gate) as Arc<dyn crate::domain::ports::SecretResolveGate>,
        Duration::from_millis(60),
        |c| c,
    )
    .await;
    let declaration = h.declare_secret("pw").await;
    let a = h.base.tree.a;
    h.subscribe("gear", "pw", a).await;
    let admin = actor(a);
    let pending = h
        .coordinator
        .stage_secret(&admin, &h.key("pw"), None, json!("staged-value"))
        .await
        .expect("staged");

    let outcome = h
        .coordinator
        .batch(
            &admin,
            vec![batch_set(
                h.key("pw"),
                json!({ "pending_id": pending.id.to_string() }),
            )],
        )
        .await
        .expect("the batch runs");

    assert!(
        is_unavailable(&outcome.results[0]),
        "{:?}",
        outcome.results[0]
    );
    assert_eq!(gate.asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(h.rows(declaration).await, 0, "nothing stored");
    assert!(h.state("gear", "pw", a).await.is_none());
    assert!(h.secret_uses("pw", a).await.is_empty());
    assert!(!h.activation.locks.is_held(h.key("pw").as_str()));
    // The token is still claimable: the refusal consumed nothing.
    let conn = h.base.db.conn().expect("connection");
    assert!(
        crate::domain::secrets::pending::PendingSecretRepository::find(
            &PendingSecretRepo,
            &conn,
            &AccessScope::allow_all(),
            pending.id
        )
        .await
        .expect("lookup")
        .is_some()
    );
}

// ---------------------------------------------------------------------------
// Failing closed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tenant_resolver_that_cannot_answer_fails_a_cascading_write_closed() {
    let h = Rig::new().await;
    let declaration = h.declare("flag", scope_class::CASCADING).await;
    let (root, b) = (h.base.tree.root, h.base.tree.b);
    h.subscribe("gear", "flag", b).await;
    h.base.hierarchy.set_unavailable(true);

    let outcome = h
        .set(&actor(root), "flag", None, json!(true), Some("absent"))
        .await;

    assert!(is_unavailable(&outcome), "{outcome:?}");
    assert_eq!(h.rows(declaration).await, 0, "nothing is stored");
    assert!(h.state("gear", "flag", b).await.is_none());
    assert!(!h.activation.locks.is_held(h.key("flag").as_str()));
}

#[tokio::test]
async fn a_stale_tag_stores_no_state_and_releases_the_lock_for_the_next_mutation() {
    // The row moves between the candidate step and the transaction.
    let moved: Arc<std::sync::Mutex<Option<Arc<Db>>>> = Arc::default();
    let hook_db = Arc::clone(&moved);
    let pause: PausePoint = Arc::new(move |_key: &str| {
        let hook_db = Arc::clone(&hook_db);
        Box::pin(async move {
            let Some(db) = hook_db.lock().expect("hook").take() else {
                return;
            };
            let conn = db.conn().expect("connection");
            let scope = AccessScope::allow_all();
            let declarations = DeclarationRepo
                .find_by_key_prefix(&conn, &scope, "")
                .await
                .expect("declarations");
            let rows = ValueRepo
                .find_all(&conn, &scope, declarations[0].id)
                .await
                .expect("rows");
            ValueRepo
                .update(
                    &conn,
                    &scope,
                    rows[0].id,
                    Some(json!(false)),
                    None,
                    "someone else",
                    rows[0].last_change_at,
                )
                .await
                .expect("moved");
        })
    });
    let h = Rig::with(Arc::new(AllowAllGate), Duration::from_millis(60), |c| {
        c.with_pause_point(pause)
    })
    .await;
    h.declare("flag", scope_class::CASCADING).await;
    let root = h.base.tree.root;
    h.subscribe("gear", "flag", root).await;
    let first = h
        .set(&actor(root), "flag", None, json!(true), Some("absent"))
        .await
        .expect("first");
    let state_before = h.state("gear", "flag", root).await.expect("state");
    *moved.lock().expect("hook") = Some(Arc::clone(&h.base.db));

    let outcome = h
        .set(&actor(root), "flag", None, json!(false), Some(&first.etag))
        .await;

    assert!(
        matches!(outcome, Err(DomainError::PreconditionFailed { .. })),
        "{outcome:?}"
    );
    assert_eq!(
        h.state("gear", "flag", root).await,
        Some(state_before),
        "the rolled-back change wrote no state"
    );
    let key = h.key("flag");
    assert!(!h.activation.locks.is_held(key.as_str()), "released");
    assert_eq!(
        h.activation
            .locks
            .released
            .load(std::sync::atomic::Ordering::SeqCst),
        h.activation.locks.asks_for(key.as_str()),
        "every acquisition was released"
    );
}

// ---------------------------------------------------------------------------
// The lock
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_wait_that_runs_out_refuses_a_single_change_with_nothing_stored() {
    let h = Rig::new().await;
    let declaration = h.declare("flag", scope_class::CASCADING).await;
    let root = h.base.tree.root;
    h.subscribe("gear", "flag", root).await;
    let owner = h.activation.locks.hold(h.key("flag").as_str());

    let outcome = h
        .set(&actor(root), "flag", None, json!(true), Some("absent"))
        .await;

    assert!(is_unavailable(&outcome), "{outcome:?}");
    assert_eq!(h.rows(declaration).await, 0);
    assert!(h.state("gear", "flag", root).await.is_none());
    assert!(
        h.activation.locks.is_held(h.key("flag").as_str()),
        "the lock is still its owner's"
    );
    drop(owner);
}

#[tokio::test]
async fn a_batch_item_whose_lock_is_held_is_unavailable_and_its_neighbours_commit() {
    let h = Rig::new().await;
    let mut declarations = Vec::new();
    for name in ["one", "two", "three"] {
        declarations.push(h.declare(name, scope_class::CASCADING).await);
    }
    let root = h.base.tree.root;
    let _owner = h.activation.locks.hold(h.key("two").as_str());

    let outcome = h
        .coordinator
        .batch(
            &actor(root),
            ["one", "two", "three"]
                .into_iter()
                .map(|name| batch_set(h.key(name), json!(true)))
                .collect(),
        )
        .await
        .expect("the batch runs");

    assert!(outcome.results[0].is_ok());
    assert!(
        is_unavailable(&outcome.results[1]),
        "{:?}",
        outcome.results[1]
    );
    assert!(
        outcome.results[2].is_ok(),
        "later changes are still attempted"
    );
    assert_eq!(h.rows(declarations[0]).await, 1);
    assert_eq!(
        h.rows(declarations[1]).await,
        0,
        "nothing stored for the item"
    );
    assert_eq!(h.rows(declarations[2]).await, 1);
}

#[tokio::test]
async fn a_batch_never_waits_on_a_lock_past_the_requests_remaining_deadline() {
    // The lock's own budget is long; the request deadline is spent.
    let h = Rig::with(Arc::new(AllowAllGate), Duration::from_secs(60), |c| {
        c.with_request_deadline(Duration::ZERO)
    })
    .await;
    let declaration = h.declare("held", scope_class::CASCADING).await;
    let root = h.base.tree.root;
    let _owner = h.activation.locks.hold(h.key("held").as_str());

    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        h.coordinator
            .batch(&actor(root), vec![batch_set(h.key("held"), json!(true))]),
    )
    .await
    .expect("the cap bounds the wait, not the lock's budget")
    .expect("the batch runs");

    assert!(is_unavailable(&outcome.results[0]));
    assert_eq!(h.rows(declaration).await, 0);
}

#[tokio::test]
async fn changes_of_different_keys_never_wait_for_each_other() {
    let h = Rig::new().await;
    h.declare("held", scope_class::CASCADING).await;
    let free = h.declare("free", scope_class::CASCADING).await;
    let root = h.base.tree.root;
    let _owner = h.activation.locks.hold(h.key("held").as_str());

    h.set(&actor(root), "free", None, json!(true), Some("absent"))
        .await
        .expect("a held lock on another key blocks nothing");

    assert_eq!(h.rows(free).await, 1);
    assert_eq!(h.activation.locks.asks_for(h.key("held").as_str()), 0);
}

#[tokio::test]
async fn a_change_refused_before_the_lock_never_contends_for_it() {
    // Refused by its tag in staging, before the lock is asked for.
    let h = Rig::new().await;
    h.declare("flag", scope_class::CASCADING).await;
    let root = h.base.tree.root;
    let first = h
        .set(&actor(root), "flag", None, json!(true), Some("absent"))
        .await
        .expect("first");
    let asked = h.activation.locks.asks_for(h.key("flag").as_str());

    let refused = h
        .set(&actor(root), "flag", None, json!(false), Some("stale"))
        .await;

    assert!(matches!(
        refused,
        Err(DomainError::PreconditionFailed { .. })
    ));
    assert_eq!(h.activation.locks.asks_for(h.key("flag").as_str()), asked);

    // Refused by validation: a string is no boolean.
    let invalid = h
        .set(
            &actor(root),
            "flag",
            None,
            json!("not a boolean"),
            Some(&first.etag),
        )
        .await;
    assert!(
        matches!(invalid, Err(DomainError::Validation { .. })),
        "{invalid:?}"
    );
    assert_eq!(h.activation.locks.asks_for(h.key("flag").as_str()), asked);

    // Refused by the Credential Store's put of an inline secret.
    h.declare_secret("pw").await;
    h.secrets.go_down();
    let unstored = h
        .set(&actor(root), "pw", None, json!("hunter2"), Some("absent"))
        .await;
    assert!(is_unavailable(&unstored), "{unstored:?}");
    assert_eq!(
        h.activation.locks.asks_for(h.key("pw").as_str()),
        0,
        "a refused put never contended for the lock"
    );
}

#[tokio::test]
async fn the_lock_is_released_before_the_event_and_the_superseded_entry_are_published() {
    // The event is published, and the entry the change superseded released,
    // after the key's lock is let go: neither lengthens the hold.
    struct Probe {
        locks: Arc<crate::domain::activation::testing::InProcessLocks>,
        held_at_publish: std::sync::Mutex<Vec<bool>>,
    }
    #[async_trait::async_trait]
    impl crate::domain::ports::ChangePublisher for Probe {
        async fn publish(&self, event: crate::domain::ports::ValueEvent) {
            if let crate::domain::ports::ValueEvent::Changed { key, .. } = event {
                self.held_at_publish
                    .lock()
                    .expect("probe")
                    .push(self.locks.is_held(&key));
            }
        }
    }
    let probe: Arc<std::sync::Mutex<Option<Arc<Probe>>>> = Arc::default();
    let sink = Arc::clone(&probe);
    let h = Rig::with_publisher(
        Arc::new(AllowAllGate),
        Duration::from_millis(60),
        |coordinator| coordinator,
        move |locks| {
            let made = Arc::new(Probe {
                locks: Arc::clone(locks),
                held_at_publish: std::sync::Mutex::default(),
            });
            *sink.lock().expect("probe") = Some(Arc::clone(&made));
            made
        },
    )
    .await;
    let probe = probe.lock().expect("probe").clone().expect("built");
    h.declare_secret("pw").await;
    let root = h.base.tree.root;
    let held_at_release = Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let locks = Arc::clone(&h.activation.locks);
        let key = h.key("pw");
        let seen = Arc::clone(&held_at_release);
        *h.secrets.on_delete.lock().expect("hook") = Some(Box::new(move || {
            seen.lock().expect("seen").push(locks.is_held(key.as_str()));
        }));
    }
    let first = h
        .set(&actor(root), "pw", None, json!("one"), Some("absent"))
        .await
        .expect("first");
    // The second write supersedes the first's entry, which is released.
    h.set(&actor(root), "pw", None, json!("two"), Some(&first.etag))
        .await
        .expect("second");

    assert_eq!(
        *probe.held_at_publish.lock().expect("probe"),
        vec![false, false],
        "the lock was free when each event was published"
    );
    assert_eq!(
        *held_at_release.lock().expect("seen"),
        vec![false],
        "and when the superseded entry was released"
    );
}

#[tokio::test]
async fn a_revert_or_remove_with_no_override_of_its_own_fetches_no_fallback() {
    // `pw` has a value at the root only: a revert at `a` is doomed to
    // `NotFound`, and costs no fallback fetch, so no `secret_use` record.
    let h = Rig::new().await;
    h.declare_secret("pw").await;
    let (root, a) = (h.base.tree.root, h.base.tree.a);
    h.set(&actor(root), "pw", None, json!("base"), Some("absent"))
        .await
        .expect("the ancestor's secret");
    h.subscribe("gear", "pw", a).await;

    let reverted = h.revert(&actor(a), "pw", None, Some("absent")).await;
    let removed = h
        .coordinator
        .change(
            &actor(a),
            &h.key("pw"),
            None,
            Change::Remove,
            Some("absent"),
            "remove_value",
        )
        .await;

    assert!(
        matches!(reverted, Err(DomainError::NotFound { .. })),
        "{reverted:?}"
    );
    assert!(
        matches!(removed, Err(DomainError::NotFound { .. })),
        "{removed:?}"
    );
    assert!(
        h.secret_uses("pw", a).await.is_empty(),
        "no secret_use record for a doomed change"
    );
    assert!(h.state("gear", "pw", a).await.is_none());
}

#[tokio::test]
async fn every_value_operation_takes_the_key_lock_once_and_releases_it() {
    let h = Rig::new().await;
    h.declare("flag", scope_class::CASCADING).await;
    let (root, a) = (h.base.tree.root, h.base.tree.a);
    let key = h.key("flag");
    let set = h
        .set(&actor(root), "flag", None, json!(true), Some("absent"))
        .await
        .expect("set");
    assert_eq!(h.activation.locks.asks_for(key.as_str()), 1);

    let cloned = h
        .coordinator
        .clone_value(&actor(root), &key, Some(root), Some(a), Some("absent"))
        .await
        .expect("clone");
    assert_eq!(h.activation.locks.asks_for(key.as_str()), 2);

    h.revert(&actor(root), "flag", None, Some(&set.etag))
        .await
        .expect("revert");
    assert_eq!(h.activation.locks.asks_for(key.as_str()), 3);

    // `remove_value` of the copy `clone` left at `a`.
    h.coordinator
        .change(
            &actor(root),
            &key,
            Some(a),
            Change::Remove,
            Some(&cloned.etag),
            "remove_value",
        )
        .await
        .expect("remove_value");
    assert_eq!(h.activation.locks.asks_for(key.as_str()), 4);

    assert!(!h.activation.locks.is_held(key.as_str()));
    assert_eq!(
        h.activation
            .locks
            .released
            .load(std::sync::atomic::Ordering::SeqCst),
        4
    );
}

#[tokio::test]
async fn without_activation_bound_a_change_takes_no_lock_and_records_no_state() {
    // A build before consumer activation: the coordinator knows nothing of it.
    let base = ResolutionHarness::new().await;
    let secrets = Arc::new(RecordingSecrets::default());
    let writer = Arc::new(ValueWriter::new(
        ValueRepo,
        Arc::clone(&base.resolver),
        Arc::new(GtsTypeValidator::new(resolution_catalogue())),
        crate::infra::storage::audit_store::AuditStore,
        Arc::new(FixedStepUp::verified()),
        Arc::clone(&secrets) as Arc<dyn crate::domain::ports::SecretManager>,
        PendingSecretRepo,
        Arc::new(RecordingPublisher::default()),
        Arc::new(NoMetrics),
    ));
    let coordinator = WriteCoordinator::new(Arc::clone(&base.db), writer);
    let id = base
        .declare_typed("flag", scope_class::CASCADING, json!(false), BOOL, "public")
        .await;
    let rig = Rig {
        base,
        coordinator: Arc::new(coordinator),
        activation: activation_rig(
            &ResolutionHarness::new().await,
            &secrets,
            Arc::new(AllowAllGate),
            Duration::from_millis(10),
        ),
        secrets,
    };
    rig.clear_step_up(id, "public").await;
    let root = rig.base.tree.root;
    rig.subscribe("gear", "flag", root).await;

    rig.set(&actor(root), "flag", None, json!(true), Some("absent"))
        .await
        .expect("commits");

    assert!(rig.state("gear", "flag", root).await.is_none());
    assert_eq!(rig.activation.locks.asks_for(rig.key("flag").as_str()), 0);
}

#[tokio::test]
async fn the_read_gate_answers_are_the_managers_alone() {
    // The coordinator never consults the registration decision.
    let h = Rig::new().await;
    h.declare("flag", scope_class::CASCADING).await;
    h.activation.gate.answer(ReadAnswer::Deny);
    let root = h.base.tree.root;
    h.set(&actor(root), "flag", None, json!(true), Some("absent"))
        .await
        .expect("a write is not a registration");
    assert_eq!(
        h.activation
            .gate
            .asked
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

// ---------------------------------------------------------------------------
// The end-of-request notification
// ---------------------------------------------------------------------------

mod notifying {
    use std::sync::Arc;

    use super::{Rig, actor, text};
    use crate::domain::activation::testing::{
        FakeNotifications, FakeScanLock, RecordingNotifyMetrics,
    };
    use crate::domain::activation::{
        ActivationNotifier, NotificationGateway, NotificationStore, NotifyMetrics, ScanLock,
    };
    use crate::domain::coherence::testing::{RecordingGateway, publisher as coherence_double};
    use crate::domain::coherence::{BroadcastGateway, BroadcastMetrics, CoherencePublisher};
    use crate::domain::resolution::scope_class;
    use crate::infra::storage::notification_store::DbNotificationStore;

    struct Wired {
        rig: Rig,
        broadcast: Arc<RecordingGateway>,
        notifications: Arc<FakeNotifications>,
    }

    async fn wired() -> Wired {
        let broadcast = Arc::new(RecordingGateway::ready(uuid::Uuid::from_u128(0xa0)));
        let notifications = FakeNotifications::ready();
        let rig = {
            let broadcast = Arc::clone(&broadcast);
            let notifications = Arc::clone(&notifications);
            Rig::with_hook(move |db| {
                let notifier = Arc::new(ActivationNotifier::new(
                    Arc::new(DbNotificationStore::new(Arc::clone(db)))
                        as Arc<dyn NotificationStore>,
                    notifications as Arc<dyn NotificationGateway>,
                    Arc::new(FakeScanLock::default()) as Arc<dyn ScanLock>,
                    Arc::new(RecordingNotifyMetrics::default()) as Arc<dyn NotifyMetrics>,
                    std::time::Duration::from_secs(30),
                ));
                let (_, metrics) = coherence_double(&broadcast);
                Arc::new(
                    CoherencePublisher::new(
                        broadcast as Arc<dyn BroadcastGateway>,
                        metrics as Arc<dyn BroadcastMetrics>,
                    )
                    .with_notifier(notifier),
                )
            })
            .await
        };
        Wired {
            rig,
            broadcast,
            notifications,
        }
    }

    #[tokio::test]
    async fn a_change_that_wrote_states_publishes_the_pairs_notification_after_its_broadcast() {
        let w = wired().await;
        w.rig.declare("flag", scope_class::CASCADING).await;
        let (root, a) = (w.rig.base.tree.root, w.rig.base.tree.a);
        w.rig.subscribe("gear", "flag", root).await;
        w.rig.subscribe("gear", "flag", a).await;
        w.rig.subscribe("other-gear", "flag", a).await;

        let committed = w
            .rig
            .set(
                &actor(root),
                "flag",
                None,
                serde_json::json!(true),
                Some("absent"),
            )
            .await
            .expect("the set commits");
        assert_eq!(committed.key, w.rig.key("flag").as_str());

        // The broadcast is the hook's first publish; the three pairs the change
        // wrote follow, one `change_notification` each.
        let events = w.broadcast.events();
        assert_eq!(
            events[0].type_id.as_ref() as &str,
            settings_service_sdk::gts::CACHE_INVALIDATE_EVENT_TYPE
        );
        let notified: Vec<(String, String)> = events[1..]
            .iter()
            .map(|e| {
                let data = e.data.clone().expect("a body");
                (
                    data["subscriber"].as_str().unwrap().to_owned(),
                    data["tenant"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let mut expected = vec![
            ("gear".to_owned(), text(a)),
            ("gear".to_owned(), text(root)),
            ("other-gear".to_owned(), text(a)),
        ];
        expected.sort();
        let mut sorted = notified.clone();
        sorted.sort();
        assert_eq!(
            sorted, expected,
            "one notification per pair the change wrote a state for"
        );
        assert_eq!(notified.len(), 3);
        // The accepted publish stamped every state.
        for (subscriber, tenant) in [("gear", root), ("gear", a), ("other-gear", a)] {
            let state = w
                .rig
                .state(subscriber, "flag", tenant)
                .await
                .expect("state");
            assert!(state.last_notified_at.is_some(), "{subscriber} at {tenant}");
        }
        assert!(
            w.notifications.events().is_empty(),
            "the loop's session is not used"
        );
    }

    #[tokio::test]
    async fn a_change_that_affects_no_subscription_publishes_no_notification() {
        let w = wired().await;
        w.rig.declare("flag", scope_class::CASCADING).await;
        let root = w.rig.base.tree.root;
        w.rig
            .set(
                &actor(root),
                "flag",
                None,
                serde_json::json!(true),
                Some("absent"),
            )
            .await
            .expect("the set commits");
        assert_eq!(w.broadcast.events().len(), 1, "the broadcast only");
    }
}
