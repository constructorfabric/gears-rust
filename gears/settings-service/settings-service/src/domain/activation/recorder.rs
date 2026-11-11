// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-outcomes:p1
//! The Activation Outcome Recorder: what the consumer's report does to the
//! activation state — the server half of the SDK's `report_outcome`.
//!
//! A report is decided against the reporting instance's own state — the
//! subscriber from the caller's security context, the instance from the call,
//! never a sibling's. It settles an `awaiting` or a `silent` state against the
//! expectation recorded when the change committed: the expected value is read
//! back verbatim and never recomputed. The decision is taken on a row read in
//! one transaction and applied by an update guarded on the `change_ref` the
//! decision read and on the state still being `awaiting` or `silent`, together
//! with the `activation` audit record of the transition — so a decision on a
//! stale read can only fail to apply, never overwrite a newer expectation or a
//! settled row, and a repeated report makes no second transition. A stale
//! report re-arms the instance's `silent` state of the current change, and
//! every report of a subscribed instance ends its silence for the key last.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use settings_service_sdk::SettingKey;
use toolkit_db::secure::DBRunner;
use toolkit_db::{DBProvider, DbError};
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use tracing::Instrument as _;
use uuid::Uuid;

use super::subscriptions::{ActivationRepository, require_instance_id};
use super::{
    ActivationRead, ActivationState, ActivationStatus, InstanceKey, NotifyMetrics, Subscription,
    build_items, calling_subscriber, tenant_text,
};
use crate::audit::{ActorTenant, AuditOperation, AuditRecord, AuditSink};
use crate::domain::access::AccessRepository;
use crate::domain::category::visibility::{domain_visibility, is_visible};
use crate::domain::declaration::DeclarationRepository;
use crate::domain::error::DomainError;
use crate::domain::resolution::{ScopeTarget, ValueResolver};
use crate::domain::validation::guards::MAX_SERIALIZED_BYTES;
use crate::domain::value::ValueRepository;
use crate::field;

/// The longest `detail` a report keeps: 1 KiB, longer input truncated.
pub const DETAIL_LIMIT_BYTES: usize = 1024;

/// The longest setting key a report may name: 1 KiB. A key is a GTS identifier
/// the gear composes from short parts; anything this long names no setting.
pub const KEY_LIMIT_BYTES: usize = 1024;

/// What a redacted field prints as: values, hashes and details never reach a
/// log line through `Debug`.
const REDACTED: &str = "<redacted>";

/// What the consumer reports, as the SDK hands it over — raw, so the Recorder
/// is the one place that refuses malformed input.
#[domain_model]
#[derive(Clone, PartialEq, Eq)]
pub struct BackResponse {
    /// The reporting consumer instance: the SDK client always sets it, so a
    /// report without one came past the client and is refused.
    pub instance_id: Option<Uuid>,
    /// The change the notification named for the key: a UUID.
    pub change_ref: String,
    /// The tenant the consumer applied the change for: a UUID.
    pub tenant: String,
    /// The setting key.
    pub key: String,
    /// `success` or `failed`.
    pub status: String,
    /// The value the consumer applied, in the encoding of the expected value:
    /// canonical JSON, or the salted hash for a `secret`-trait setting. Never
    /// logged.
    pub applied_value: Option<String>,
    /// The failure detail, free of values and secrets by contract.
    pub detail: Option<String>,
}

impl fmt::Debug for BackResponse {
    /// `applied_value` and `detail` only say whether they are present.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BackResponse")
            .field("instance_id", &self.instance_id)
            .field("change_ref", &self.change_ref)
            .field("tenant", &self.tenant)
            .field("key", &self.key)
            .field("status", &self.status)
            .field("applied_value", &redacted(self.applied_value.as_ref()))
            .field("detail", &redacted(self.detail.as_ref()))
            .finish()
    }
}

fn redacted(field: Option<&String>) -> Option<&'static str> {
    field.map(|_| REDACTED)
}

/// What `receive_response` answers.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportResult {
    /// The report settled the state: one transition and one audit record.
    Applied,
    /// The report changed nothing: no state, a terminal state, a stale
    /// `change_ref` or a state settled or replaced concurrently.
    Ignored,
    /// The reporting instance holds no subscription of the key at the tenant
    /// — forgotten, evicted by the registration cap, unsubscribed or purged
    /// with its declaration: nothing was written.
    NotSubscribed,
}

/// A report that passed the input rules.
#[domain_model]
#[derive(Clone, PartialEq, Eq)]
struct Report {
    instance_id: Uuid,
    change_ref: Uuid,
    tenant: Uuid,
    key: String,
    success: bool,
    applied_value: Option<String>,
    detail: Option<String>,
}

impl fmt::Debug for Report {
    /// `applied_value` and `detail` only say whether they are present.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Report")
            .field("instance_id", &self.instance_id)
            .field("change_ref", &self.change_ref)
            .field("tenant", &self.tenant)
            .field("key", &self.key)
            .field("success", &self.success)
            .field("applied_value", &redacted(self.applied_value.as_ref()))
            .field("detail", &redacted(self.detail.as_ref()))
            .finish()
    }
}

/// What the outcome rules decide for an `awaiting` or a `silent` state.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The consumer runs the value currently expected.
    Succeeded,
    /// The consumer failed to apply the current change, or applied a different
    /// value for it.
    Failed,
    /// The report answers an earlier change.
    Ignored,
}

/// The transition a report writes, guarded on the state still naming the
/// `change_ref` the decision read and still being `awaiting` or `silent`.
#[domain_model]
#[derive(Clone, PartialEq, Eq)]
pub struct Transition {
    /// The subscriber of the state.
    pub subscriber: String,
    /// The reporting instance: the state is its own.
    pub instance_id: Uuid,
    /// The setting key.
    pub key: String,
    /// The subscription's tenant, a bare lowercase UUID string.
    pub tenant: String,
    /// The `change_ref` the decision read: the guard.
    pub change_ref: Uuid,
    /// The new status: `succeeded` or `failed`.
    pub status: ActivationStatus,
    /// The reported value, as reported; absent when the report carried none.
    pub applied_value: Option<String>,
    /// The consumer's detail on a `failed` status, absent otherwise.
    pub detail: Option<String>,
}

impl fmt::Debug for Transition {
    /// `applied_value` and `detail` only say whether they are present.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transition")
            .field("subscriber", &self.subscriber)
            .field("instance_id", &self.instance_id)
            .field("key", &self.key)
            .field("tenant", &self.tenant)
            .field("change_ref", &self.change_ref)
            .field("status", &self.status)
            .field("applied_value", &redacted(self.applied_value.as_ref()))
            .field("detail", &redacted(self.detail.as_ref()))
            .finish()
    }
}

/// Apply the outcome rules to an `awaiting` or a `silent` state and a report:
/// a `silent` state is decided exactly as an `awaiting` one.
///
/// `applied_value` matches only when it is present and byte-equal to the
/// expected value. The expected value is canonical JSON — or 64 lowercase hex
/// characters for a `secret`-trait row — by construction, since `record_change`
/// encodes it and nothing else writes it, so equality already implies that
/// the applied value is well formed, and a value that is missing, malformed or
/// merely a different serialization of the same value does not match.
#[must_use]
pub fn decide(
    state: &ActivationState,
    report_change_ref: Uuid,
    success: bool,
    applied: Option<&str>,
) -> Outcome {
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-1
    let matches = applied.is_some_and(|value| value == state.expected_value);
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-1
    let current = report_change_ref == state.change_ref;
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-2
    if success && matches {
        // Whatever `change_ref` the report names: the consumer runs the value
        // currently expected.
        return Outcome::Succeeded;
    }
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-2
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-3
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-4
    // A `success` with a different, missing or malformed value, and a `failed`,
    // each answer the current change or an earlier one.
    if current {
        Outcome::Failed
    } else {
        Outcome::Ignored
    }
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-4
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-3
}

/// `detail` cut to [`DETAIL_LIMIT_BYTES`] at a character boundary.
#[must_use]
pub fn truncated(mut detail: String) -> String {
    if detail.len() > DETAIL_LIMIT_BYTES {
        let mut end = DETAIL_LIMIT_BYTES;
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        detail.truncate(end);
    }
    detail
}

fn invalid(field_name: &str, message: &str) -> DomainError {
    DomainError::Validation {
        field: field_name.to_owned(),
        code: field::VALIDATION,
        message: message.to_owned(),
    }
}

/// The one answer of the four cases of an authorized caller that cannot read
/// the activation: it carries nothing that tells them apart.
fn absent() -> DomainError {
    DomainError::NotFound {
        resource: "declaration",
    }
}

/// A read of the gear's own tables that failed is the store being unavailable,
/// not an internal fault the caller cannot act on. That holds for an integrity
/// fault of a declaration or restriction row the activation read looks at too:
/// it is answered `503` here, where the history keeps its own mapping.
fn store_down(err: DomainError) -> DomainError {
    match err {
        DomainError::Internal { diagnostic } => {
            DomainError::dependency_unavailable("database", "read the setting", diagnostic)
        }
        other => other,
    }
}

/// The input rules: an `instance_id` first, then a UUID `tenant` and
/// `change_ref`, a status of `success` or `failed`, no NUL byte in the text
/// fields and a key within [`KEY_LIMIT_BYTES`]; the detail truncated. An
/// `applied_value` above the value size cap is a value that does not match and
/// is dropped here, so it is never stored.
fn parse(response: BackResponse) -> Result<Report, DomainError> {
    // A report without its instance came past the SDK client: the typed
    // invalid-input error, before any other rule, nothing changed.
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-22
    let instance_id = require_instance_id(response.instance_id)?;
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-22
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-5
    let change_ref = Uuid::parse_str(&response.change_ref)
        .map_err(|_| invalid("change_ref", "`change_ref` is not a UUID"))?;
    let tenant = Uuid::parse_str(&response.tenant)
        .map_err(|_| invalid("tenant", "`tenant` is not a UUID"))?;
    let success = match response.status.as_str() {
        "success" => true,
        "failed" => false,
        _ => {
            return Err(invalid(
                "status",
                "`status` is neither `success` nor `failed`",
            ));
        }
    };
    if response.key.len() > KEY_LIMIT_BYTES {
        return Err(invalid("key", "`key` is longer than a setting key can be"));
    }
    for (name, text) in [
        ("key", Some(&response.key)),
        ("detail", response.detail.as_ref()),
        ("applied_value", response.applied_value.as_ref()),
    ] {
        if text.is_some_and(|text| text.contains('\0')) {
            return Err(invalid(name, "the field holds a NUL byte"));
        }
    }
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-5
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-6
    let applied_value = response
        .applied_value
        .filter(|value| value.len() <= MAX_SERIALIZED_BYTES);
    Ok(Report {
        instance_id,
        change_ref,
        tenant,
        key: response.key,
        success,
        applied_value,
        detail: response.detail.map(truncated),
    })
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-6
}

/// Why the report's transaction failed: a statement of its body, already
/// mapped by the repository or the sink, or the transaction itself.
enum TxError {
    Statement(DomainError),
    Transaction(DbError),
}

impl From<DbError> for TxError {
    fn from(err: DbError) -> Self {
        Self::Transaction(err)
    }
}

impl From<TxError> for DomainError {
    /// A transaction that cannot begin or commit is a database outage the
    /// consumer retries; a statement keeps the error it was mapped to.
    fn from(err: TxError) -> Self {
        match err {
            TxError::Statement(err) => err,
            TxError::Transaction(err) => {
                Self::dependency_unavailable("database", "run the report's transaction", err)
            }
        }
    }
}

/// Why a report of a subscribed instance settled nothing: the `result` it is
/// counted under on `settings_notify_reports_total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unsettled {
    /// The instance holds no state: it was not expected for the current
    /// change.
    NoRow,
    /// The state was already `succeeded` or `failed` when read.
    Repeat,
    /// A decision on an expectation no longer current: an earlier
    /// `change_ref` not accounted, or a guarded update that matched nothing.
    Stale,
}

impl Unsettled {
    const fn result(self) -> &'static str {
        match self {
            Self::NoRow => "ignored_no_row",
            Self::Repeat => "ignored_repeat",
            Self::Stale => "ignored_stale",
        }
    }
}

/// What the transaction came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settled {
    /// No subscription of the instance: nothing was written.
    NotSubscribed,
    /// Nothing settled, for this reason.
    Ignored(Unsettled),
    /// A transition and its record.
    Applied {
        status: ActivationStatus,
        /// From the state's `changed_at` to the report.
        waited: Duration,
        /// The row the guarded update settled was `silent`: read from that
        /// row, not from the decision's read.
        late: bool,
    },
}

impl Settled {
    /// The `result` the report is counted under on
    /// `settings_notify_reports_total`.
    const fn result(self) -> &'static str {
        match self {
            Self::NotSubscribed => "not_subscribed",
            Self::Ignored(why) => why.result(),
            Self::Applied { .. } => "applied",
        }
    }
}

/// The Recorder over the settings database, the activation repository and the
/// audit sink.
#[domain_model]
pub struct ActivationRecorder<R, A> {
    db: Arc<DBProvider<DbError>>,
    repo: Arc<R>,
    audit: Arc<A>,
    metrics: Arc<dyn NotifyMetrics>,
}

impl<R, A> ActivationRecorder<R, A>
where
    R: ActivationRepository + 'static,
    A: AuditSink + 'static,
{
    /// A Recorder over this database, repository and audit sink.
    pub fn new(
        db: Arc<DBProvider<DbError>>,
        repo: R,
        audit: A,
        metrics: Arc<dyn NotifyMetrics>,
    ) -> Self {
        Self {
            db,
            repo: Arc::new(repo),
            audit: Arc::new(audit),
            metrics,
        }
    }

    /// Record an outcome the consumer reported through the SDK's
    /// `report_outcome`: the subscriber is the caller's, never an argument,
    /// and the instance the call's, trusted within the subscriber's identity.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] when the caller's subject is of an
    /// interactive subject type, with nothing read or changed — terminal, the
    /// consumer does not repeat it; [`DomainError::Validation`] with
    /// [`crate::field::INSTANCE_ID_REQUIRED`] for a report without an
    /// `instance_id`, and for a tenant or a `change_ref` that is not a
    /// UUID, a status that is neither `success` nor `failed`, a NUL byte in the
    /// key, detail or value, or a key above [`KEY_LIMIT_BYTES`], with nothing
    /// changed; [`DomainError::Unavailable`] when the transaction cannot begin
    /// or commit, and whatever error the repository or the audit sink maps a
    /// failed statement to, with nothing changed — the consumer may repeat the
    /// report, which is idempotent per subscriber, instance, key, tenant and
    /// `change_ref`.
    ///
    /// Runs inside an `activation.report` span — a child of the caller's, so
    /// the SDK's delivery span in process — whose only attributes are the
    /// reported `outcome` (`success` or `failed`), the `result` it is counted
    /// under on `settings_notify_reports_total`, and the `status` an applied
    /// report settled the state to; never a key, a value, a hash or an instance.
    pub async fn receive_response(
        &self,
        ctx: &SecurityContext,
        response: BackResponse,
    ) -> Result<ReportResult, DomainError> {
        let span = tracing::info_span!(
            "activation.report",
            outcome = tracing::field::Empty,
            result = tracing::field::Empty,
            status = tracing::field::Empty,
        );
        self.received(ctx, response, &span)
            .instrument(span.clone())
            .await
    }

    async fn received(
        &self,
        ctx: &SecurityContext,
        response: BackResponse,
        span: &tracing::Span,
    ) -> Result<ReportResult, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-4
        // A subscriber is a gear's service principal: a person is refused here,
        // before the report is parsed and before any row is read or written.
        let subscriber = calling_subscriber(ctx)?;
        // The subscriber acted from its own tenant: the history's identity
        // rule shows it only on the path from a reader down to the record.
        let actor_tenant = ActorTenant::of(ctx.subject_tenant_id());
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-4
        let report = parse(response)?;
        span.record("outcome", if report.success { "success" } else { "failed" });
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-7
        let repo = Arc::clone(&self.repo);
        let audit = Arc::clone(&self.audit);
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-15
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-16
        // The transaction commits when the body returns, and rolls back with
        // its error when the append, the update or the commit fails: nothing
        // changed, and the call answers the error, counted on no metric.
        let settled = self
            .db
            .db()
            .transaction_ref_mapped::<_, Settled, TxError>(move |tx| {
                Box::pin(async move {
                    settle(
                        repo.as_ref(),
                        audit.as_ref(),
                        tx,
                        &subscriber,
                        actor_tenant,
                        &report,
                    )
                    .await
                    .map_err(TxError::Statement)
                })
            })
            .await
            .map_err(DomainError::from)?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-16
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-15
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-7
        span.record("result", settled.result());
        if let Settled::Applied { status, .. } = settled {
            span.record("status", status.as_str());
        }
        Ok(self.counted(settled))
    }

    /// The activation state of the current value of `key` at `tenant`: one item
    /// per subscriber, ordered by subscriber, with no value of any kind.
    ///
    /// `scope` is the caller's `AccessScope` of the `read` decision on the
    /// value resource; the caller took that decision first and a denial never
    /// reaches here, so the answer to it is the same for every key and tenant.
    /// What follows it is the second question, answered
    /// [`DomainError::NotFound`] in four indistinguishable cases: `tenant`
    /// outside the caller's subtree (a standalone descendant and the tenants
    /// below it included), an undeclared key, a declaration outside the
    /// caller's administrative domain, and a key hidden from the caller. A
    /// retired declaration is read like an active one. `tenant` defaults to the
    /// caller's own.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] as above; [`DomainError::Unavailable`] when the
    /// Tenant Resolver or the database cannot answer at any step that asks —
    /// decided at the subtree test before its `NotFound`, and never read as one.
    pub async fn activation_status<D, V, X>(
        &self,
        resolver: &ValueResolver<D, V, X>,
        ctx: &SecurityContext,
        scope: &AccessScope,
        key: &SettingKey,
        tenant: Option<Uuid>,
    ) -> Result<ActivationRead, DomainError>
    where
        D: DeclarationRepository,
        V: ValueRepository + Clone,
        X: AccessRepository,
    {
        // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-3
        // The caller's own tenant when none is named; a `Uuid` renders as a
        // bare lowercase string, which is the normalisation.
        let caller = ctx.subject_tenant_id();
        let tenant = tenant.unwrap_or(caller);
        // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-3
        let conn = self.db.conn().map_err(|err| {
            DomainError::dependency_unavailable("database", "open a connection", err)
        })?;
        // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-6
        // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-7
        // The subtree test: a resolver that cannot answer fails the read here,
        // before any `NotFound` is decided; a tenant the resolver does not
        // know is outside the subtree like any other.
        if tenant != caller {
            let hierarchy = resolver.hierarchy();
            let inside = match hierarchy.is_within_subtree(caller, tenant).await {
                Ok(inside) => inside,
                Err(DomainError::NotFound { .. }) => false,
                Err(err) => return Err(err),
            };
            // Outside the subtree is decided: the standalone question is
            // asked only for a tenant inside it, so a resolver failing on
            // that second question never turns a settled `NotFound` into 503.
            if !inside {
                return Err(absent());
            }
            // A tenant the resolver does not know is no more a standalone
            // one than it is inside: the one `NotFound`.
            let standalone = match hierarchy.is_standalone(tenant).await {
                Ok(standalone) => standalone,
                Err(DomainError::NotFound { .. }) => true,
                Err(err) => return Err(err),
            };
            if standalone {
                return Err(absent());
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-7
        // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-6
        // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-8
        // Retired passes: the declaration is read whatever its status.
        let declaration = resolver
            .find_declaration(&conn, key)
            .await
            .map_err(store_down)?
            .filter(|d| is_visible(&domain_visibility(scope), d.domain_affinity.as_deref()))
            .ok_or_else(absent)?;
        // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-8
        // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-9
        // Whose access decides is the caller's, over its own root-to-self
        // chain, as the value read and the history decide it.
        let root = resolver.root_tenant().await?;
        let hidden = resolver
            .effective_access(
                &conn,
                declaration.id,
                ScopeTarget::Tenant(caller).normalize(root),
            )
            .await
            .map_err(store_down)?
            .is_hidden();
        if hidden {
            return Err(absent());
        }
        // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-9
        // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-10
        // One statement: the rows of `(key, tenant)` joined to their
        // subscriptions for the label by an outer join.
        let rows = self
            .repo
            .activation_rows(&conn, key.as_str(), &tenant_text(tenant))
            .await
            .map_err(store_down)?;
        // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-10
        // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-13
        // One indexed read of the silent subscriptions, which the instances a
        // change left out are found among.
        let silent = self
            .repo
            .silent_subscriptions(&conn, key.as_str(), &tenant_text(tenant))
            .await
            .map_err(store_down)?;
        // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-13
        // The reader's own tenant decides the identities: the root tenant has
        // nothing above it and is shown them; every other reader is not.
        // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-16
        Ok(ActivationRead {
            tenant,
            items: build_items(rows, &silent),
        }
        .disclosed_to(caller == root))
        // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-16
    }

    /// The metrics of a committed report, and the answer.
    fn counted(&self, settled: Settled) -> ReportResult {
        match settled {
            // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-23
            Settled::NotSubscribed => {
                self.metrics.report("not_subscribed");
                ReportResult::NotSubscribed
            }
            // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-23
            // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-26
            Settled::Ignored(why) => {
                self.metrics.report(why.result());
                ReportResult::Ignored
            }
            // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-26
            Settled::Applied {
                status,
                waited,
                late,
            } => {
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-17
                self.metrics.reaction(status.as_str());
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-17
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-20
                self.metrics.response_latency(waited, late);
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-20
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-18
                self.metrics.report("applied");
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-18
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-19
                ReportResult::Applied
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-19
            }
        }
    }
}

/// The body of the report's transaction: read the instance's subscription and
/// state, decide, apply the transition with its record or the re-arm, and end
/// the instance's silence last. `NotSubscribed` writes nothing.
async fn settle<R, A, C>(
    repo: &R,
    audit: &A,
    tx: &C,
    subscriber: &str,
    actor_tenant: ActorTenant,
    report: &Report,
) -> Result<Settled, DomainError>
where
    R: ActivationRepository,
    A: AuditSink,
    C: DBRunner,
{
    // The reporting instance's own subscription and state, never a sibling's:
    // both are keyed on the subscriber of the caller and the instance of the
    // call, so an instance of another gear is never found.
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-24
    let instance = InstanceKey {
        subscriber: subscriber.to_owned(),
        instance_id: report.instance_id,
        key: report.key.clone(),
        tenant: tenant_text(report.tenant),
    };
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-24
    let Some(subscription) = repo.find(tx, &instance).await? else {
        // Forgotten, evicted, unsubscribed or purged: nothing is written.
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-23
        return Ok(Settled::NotSubscribed);
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-23
    };
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-8
    let settled = match repo.state(tx, &instance).await? {
        // Not expected for the current change — silent while a sibling was,
        // or registered after it — and it stays out of it.
        None => Settled::Ignored(Unsettled::NoRow),
        Some(state) => {
            account(repo, audit, tx, &subscription, actor_tenant, report, &state).await?
        }
    };
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-8
    // The transaction's last statement, after its row statements, whatever the
    // report decided: the instance's silence ends unless it holds a `silent`
    // state at this moment — one a concurrent call of silence made, not the
    // one its own re-arm just returned to `awaiting`. No record.
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-27
    repo.clear_silent(tx, &instance).await?;
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-27
    Ok(settled)
}

/// The decision on the instance's state, and the statements it writes before
/// the clear of the instance's silence: the guarded transition with its
/// `activation` record, or the re-arm of a stale report.
async fn account<R, A, C>(
    repo: &R,
    audit: &A,
    tx: &C,
    subscription: &Subscription,
    actor_tenant: ActorTenant,
    report: &Report,
    state: &ActivationState,
) -> Result<Settled, DomainError>
where
    R: ActivationRepository,
    A: AuditSink,
    C: DBRunner,
{
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-9
    if matches!(
        state.status,
        ActivationStatus::Succeeded | ActivationStatus::Failed
    ) {
        // A report never moves a settled row; only the next change resets it.
        // A repeat re-arms nothing: no call of silence makes a settled row
        // `silent`.
        return Ok(Settled::Ignored(Unsettled::Repeat));
    }
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-9
    let instance = state.instance_key();
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-10
    // An `awaiting` and a `silent` state are decided alike: silence is not
    // terminal, so a late report is accounted as a timely one.
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-6
    let status = match decide(
        state,
        report.change_ref,
        report.success,
        report.applied_value.as_deref(),
    ) {
        Outcome::Succeeded => ActivationStatus::Succeeded,
        Outcome::Failed => ActivationStatus::Failed,
        Outcome::Ignored => {
            // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-25
            // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-10
            // The instance re-enters: its `silent` state of the current change
            // goes back to `awaiting`, due at the next scan. Issued whether
            // the state was read `awaiting` or `silent`, the guard on `silent`
            // and the decided `change_ref` matching only a `silent` row — one
            // a call of silence made after the read included — and leaving a
            // row a change replaced or a report settled meanwhile alone. No
            // record; the earlier `silent` record stays.
            repo.rearm(tx, &instance, state.change_ref).await?;
            // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-10
            // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-25
            return Ok(Settled::Ignored(Unsettled::Stale));
        }
    };
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-6
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-10
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-11
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-5
    // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-2
    // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-3
    // Only `status`, `applied_value`, `detail`, `reported_at` and `updated_at`
    // are written — a `silent` state keeps its `silent_since`;
    // `expected_value` and `change_ref` are never touched. The detail is the
    // consumer's on a `failed` status and absent otherwise.
    // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-8
    // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-9
    let detail = if status == ActivationStatus::Failed {
        report.detail.clone()
    } else {
        None
    };
    let transition = Transition {
        subscriber: instance.subscriber.clone(),
        instance_id: instance.instance_id,
        key: instance.key.clone(),
        tenant: instance.tenant.clone(),
        change_ref: state.change_ref,
        status,
        applied_value: report.applied_value.clone(),
        detail: detail.clone(),
    };
    let reported_at = repo.report(tx, &transition).await?;
    // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-9
    // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-8
    // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-3
    // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-2
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-5
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-11
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-12
    let Some(reported_at) = reported_at else {
        // A concurrent report settled the row, or a concurrent change replaced
        // its expectation and its own notification follows: a decision on an
        // expectation no longer current, with no record.
        return Ok(Settled::Ignored(Unsettled::Stale));
    };
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-12
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-13
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-14
    // The transition's record, in the update's transaction: the key at the
    // row's tenant under the shared formatter, the subscriber as a public
    // actor, the reporting instance with its subscription's label at the time
    // of the report, the outcome of the new status, a freshly minted request
    // id, the row's `change_ref`, and no pre-value or post-value. A failed
    // append is the call's error and rolls the update back.
    let mut record = AuditRecord::new(
        &state.key,
        Some(report.tenant),
        &instance.subscriber,
        actor_tenant,
        AuditOperation::Activation,
        Uuid::new_v4().to_string(),
    )
    .by_module()
    .with_change_ref(state.change_ref)
    .with_instance(instance.instance_id, subscription.instance_label.clone());
    if status == ActivationStatus::Failed {
        record = record.failed();
        if let Some(detail) = detail {
            record = record.with_detail(detail);
        }
    }
    audit.append(tx, &AccessScope::allow_all(), record).await?;
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-14
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-13
    // The latency's baseline is the state's `changed_at`, which only the
    // change that set the expectation writes: no stamp, call of silence or
    // re-arm moves it.
    let waited = Duration::try_from(reported_at - state.changed_at).unwrap_or(Duration::ZERO);
    // A `silent` state settled is a late report — the state the guarded update
    // settled, not the one the decision read: a call of silence committing
    // between the two leaves the read `awaiting` and the settled row `silent`.
    // The update keeps `silent_since`, which only a call of silence sets — a
    // change's overwrite and a re-arm clear it — so the settled row carries one
    // exactly when it was `silent`. Read in the update's transaction, which
    // holds that row until it commits, so it is the row the update wrote.
    let late = repo
        .state(tx, &instance)
        .await?
        .is_some_and(|settled| settled.silent_since.is_some());
    Ok(Settled::Applied {
        status,
        waited,
        late,
    })
}

#[cfg(test)]
#[path = "recorder_tests.rs"]
mod recorder_tests;

#[cfg(test)]
#[path = "status_tests.rs"]
mod status_tests;
