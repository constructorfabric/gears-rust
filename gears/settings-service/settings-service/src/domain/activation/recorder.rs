// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-outcomes:p1
//! The Activation Outcome Recorder: what the consumer's report does to the
//! activation state — the server half of the SDK's `report_outcome`.
//!
//! A report settles an `awaiting` state against the expectation recorded when
//! the change committed: the expected value is read back verbatim and never
//! recomputed. The decision is taken on a row read in one transaction and
//! applied by an update guarded on the `change_ref` the decision read and on
//! the state still being `awaiting`, together with the `activation` audit
//! record of the transition — so a decision on a stale read can only fail to
//! apply, never overwrite a newer expectation or a settled row, and a repeated
//! report makes no second transition.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use settings_service_sdk::SettingKey;
use toolkit_db::secure::DBRunner;
use toolkit_db::{DBProvider, DbError};
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::subscriptions::ActivationRepository;
use super::{
    ActivationItem, ActivationRead, ActivationState, ActivationStatus, NotifyMetrics,
    subscriber_of, tenant_text,
};
use crate::audit::{AuditOperation, AuditRecord, AuditSink};
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
}

/// A report that passed the input rules.
#[domain_model]
#[derive(Clone, PartialEq, Eq)]
struct Report {
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
            .field("change_ref", &self.change_ref)
            .field("tenant", &self.tenant)
            .field("key", &self.key)
            .field("success", &self.success)
            .field("applied_value", &redacted(self.applied_value.as_ref()))
            .field("detail", &redacted(self.detail.as_ref()))
            .finish()
    }
}

/// What the outcome rules decide for an `awaiting` state.
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
/// `change_ref` the decision read and still being `awaiting`.
#[domain_model]
#[derive(Clone, PartialEq, Eq)]
pub struct Transition {
    /// The subscriber of the state.
    pub subscriber: String,
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
            .field("key", &self.key)
            .field("tenant", &self.tenant)
            .field("change_ref", &self.change_ref)
            .field("status", &self.status)
            .field("applied_value", &redacted(self.applied_value.as_ref()))
            .field("detail", &redacted(self.detail.as_ref()))
            .finish()
    }
}

/// Apply the outcome rules to an `awaiting` state and a report.
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

/// The input rules: a UUID `tenant` and `change_ref`, a status of `success` or
/// `failed`, no NUL byte in the text fields and a key within
/// [`KEY_LIMIT_BYTES`]; the detail truncated. An `applied_value` above the
/// value size cap is a value that does not match and is dropped here, so it is
/// never stored.
fn parse(response: BackResponse) -> Result<Report, DomainError> {
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

/// What the transaction came to.
#[derive(Clone, Copy)]
enum Settled {
    Ignored,
    Applied {
        status: ActivationStatus,
        waited: Duration,
    },
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
    /// `report_outcome`: the subscriber is the caller's, never an argument.
    ///
    /// # Errors
    /// [`DomainError::Validation`] for a tenant or a `change_ref` that is not a
    /// UUID, a status that is neither `success` nor `failed`, a NUL byte in the
    /// key, detail or value, or a key above [`KEY_LIMIT_BYTES`], with nothing
    /// changed; [`DomainError::Unavailable`] when the transaction cannot begin
    /// or commit, and whatever error the repository or the audit sink maps a
    /// failed statement to, with nothing changed — the consumer may repeat the
    /// report, which is idempotent per subscriber, key, tenant and
    /// `change_ref`.
    pub async fn receive_response(
        &self,
        ctx: &SecurityContext,
        response: BackResponse,
    ) -> Result<ReportResult, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-4
        let subscriber = subscriber_of(ctx);
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-4
        let report = parse(response)?;
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
                    settle(repo.as_ref(), audit.as_ref(), tx, &subscriber, &report)
                        .await
                        .map_err(TxError::Statement)
                })
            })
            .await
            .map_err(DomainError::from)?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-16
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-15
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-7
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
        let states = self
            .repo
            .states_of(&conn, key.as_str(), &tenant_text(tenant))
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-10
        Ok(ActivationRead {
            tenant,
            items: states.into_iter().map(ActivationItem::from).collect(),
        })
    }

    /// The metrics of a committed or ignored report, and the answer.
    fn counted(&self, settled: Settled) -> ReportResult {
        match settled {
            Settled::Ignored => {
                self.metrics.report("ignored");
                ReportResult::Ignored
            }
            Settled::Applied { status, waited } => {
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-17
                self.metrics.reaction(status.as_str());
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-17
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-20
                self.metrics.response_latency(waited);
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

/// The body of the report's transaction: read the row, decide, update under
/// the guard, append the audit record. Every `Ignored` return writes nothing.
async fn settle<R, A, C>(
    repo: &R,
    audit: &A,
    tx: &C,
    subscriber: &str,
    report: &Report,
) -> Result<Settled, DomainError>
where
    R: ActivationRepository,
    A: AuditSink,
    C: DBRunner,
{
    let tenant = tenant_text(report.tenant);
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-8
    let Some(state) = repo.state(tx, subscriber, &report.key, &tenant).await? else {
        return Ok(Settled::Ignored);
    };
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-8
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-9
    if state.status != ActivationStatus::Awaiting {
        // A report never moves a terminal row; only the next change resets it.
        return Ok(Settled::Ignored);
    }
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-9
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-10
    let status = match decide(
        &state,
        report.change_ref,
        report.success,
        report.applied_value.as_deref(),
    ) {
        Outcome::Succeeded => ActivationStatus::Succeeded,
        Outcome::Failed => ActivationStatus::Failed,
        Outcome::Ignored => return Ok(Settled::Ignored),
    };
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-10
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-11
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-5
    // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-2
    // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-3
    // Only `status`, `applied_value`, `detail`, `reported_at` and `updated_at`
    // are written; `expected_value` and `change_ref` are never touched. The
    // detail is the consumer's on a `failed` status and absent otherwise.
    let detail = if status == ActivationStatus::Failed {
        report.detail.clone()
    } else {
        None
    };
    let transition = Transition {
        subscriber: subscriber.to_owned(),
        key: report.key.clone(),
        tenant,
        change_ref: state.change_ref,
        status,
        applied_value: report.applied_value.clone(),
        detail: detail.clone(),
    };
    let reported_at = repo.report(tx, &transition).await?;
    // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-3
    // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-2
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-outcome:p1:inst-ca-outcome-5
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-11
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-12
    let Some(reported_at) = reported_at else {
        // A concurrent report settled the row, or a concurrent change replaced
        // its expectation and its own notification follows: no record.
        return Ok(Settled::Ignored);
    };
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-12
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-13
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-14
    // The transition's record, in the update's transaction: the key at the
    // row's tenant under the shared formatter, the subscriber as a public
    // actor, the outcome of the new status, a freshly minted request id, the
    // row's `change_ref`, and no pre-value or post-value. A failed append is
    // the call's error and rolls the update back.
    let mut record = AuditRecord::new(
        &state.key,
        Some(report.tenant),
        subscriber,
        AuditOperation::Activation,
        Uuid::new_v4().to_string(),
    )
    .by_module()
    .with_change_ref(state.change_ref);
    if status == ActivationStatus::Failed {
        record = record.failed();
        if let Some(detail) = detail {
            record = record.with_detail(detail);
        }
    }
    audit.append(tx, &AccessScope::allow_all(), record).await?;
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-14
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-13
    let waited = Duration::try_from(reported_at - state.updated_at).unwrap_or(Duration::ZERO);
    Ok(Settled::Applied { status, waited })
}

#[cfg(test)]
#[path = "recorder_tests.rs"]
mod recorder_tests;

#[cfg(test)]
#[path = "status_tests.rs"]
mod status_tests;
