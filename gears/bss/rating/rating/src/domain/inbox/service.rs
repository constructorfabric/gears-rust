//! The inbox service: accept a fact (feature 01 §2.1) and release a quarantined entry (§2.2).

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use serde_json::json;
use toolkit_db::Db;
use toolkit_db::secure::DbTx;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{
    AcceptOutcome, Appliers, InboundFact, InboxEntry, InboxRepo, InboxSource, InboxTransition,
    ProducerPolicy, Quarantine, ReleaseResolution,
};
use crate::authz::{self, Operation, SelectorRead};
use crate::domain::audit::{AuditEntry, OperatorAudit, OperatorReason};
use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;
use crate::domain::exception::{ExceptionKey, ExceptionRegister, SubjectKind};
use crate::domain::ports::{Clock, Dependency, FailureCode, RatingMetricsPort, TenantGrant};

/// The audit `target_kind` of an inbox entry.
pub const INBOX_TARGET_KIND: &str = "inbox_entry";

/// What the inbox is built from.
#[domain_model]
pub struct InboxParts {
    /// The gear's database; the inbox opens its own transactions.
    pub db: Db,
    /// The PEP releases are authorized through.
    pub enforcer: PolicyEnforcer,
    /// The inbox table.
    pub repo: Arc<dyn InboxRepo>,
    /// The apply step of each source whose owning feature is built.
    pub appliers: Appliers,
    /// The producer identity each source's event path accepts.
    pub producers: ProducerPolicy,
    /// Rating's service grant per tenant.
    pub grant: Arc<dyn TenantGrant>,
    /// The exception register quarantines are recorded in.
    pub exceptions: ExceptionRegister,
    /// The audit helper releases are recorded with.
    pub audit: OperatorAudit,
    /// `rating_inbox_quarantined_total` and the PDP's `rating_dependency_errors_total`.
    pub metrics: Arc<dyn RatingMetricsPort>,
    /// The time source.
    pub clock: Arc<dyn Clock>,
}

/// An operator's release of a quarantined entry, validated whatever entry point it came through.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCommand {
    entry_id: Uuid,
    resolution: ReleaseResolution,
    reason: OperatorReason,
    request_digest: Sha256Hex,
    request_id: Option<String>,
}

impl ReleaseCommand {
    /// A release of `entry_id` with the operator's `reason` and the caller's request id. The
    /// request digest of the audit row is computed here, over the entry, the trimmed reason and
    /// the resolution.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] for a reason [`OperatorReason::parse`] refuses.
    pub fn new(
        entry_id: Uuid,
        resolution: ReleaseResolution,
        reason: &str,
        request_id: Option<String>,
    ) -> Result<Self, DomainError> {
        let reason = OperatorReason::parse(reason)?;
        let request_digest = Sha256Hex::of_json(&json!({
            "entry_id": entry_id,
            "reason": reason.as_str(),
            "resolution": resolution.as_str(),
        }));
        Ok(Self {
            entry_id,
            resolution,
            reason,
            request_digest,
            request_id,
        })
    }

    /// The entry released.
    #[must_use]
    pub const fn entry_id(&self) -> Uuid {
        self.entry_id
    }
}

/// The inbox for every non-usage upstream fact.
#[domain_model]
#[derive(Clone)]
pub struct Inbox {
    parts: Arc<InboxParts>,
}

impl Inbox {
    /// An inbox over `parts`.
    #[must_use]
    pub fn new(parts: InboxParts) -> Self {
        Self {
            parts: Arc::new(parts),
        }
    }

    /// The sources whose owning features are built.
    pub fn applied_sources(&self) -> impl Iterator<Item = InboxSource> + '_ {
        self.parts.appliers.keys().copied()
    }

    /// Step 1 of accepting a fact, before its transaction: verifies the producer identity for the
    /// source and checks the payload's owning tenant against Rating's grant. Returns the scope,
    /// narrowed to that tenant, every write of the fact runs under.
    ///
    /// # Errors
    ///
    /// [`DomainError::ProducerNotVerified`] or [`DomainError::OutsideGrant`]: the fact is refused
    /// and nothing is written. [`DomainError::AuthzUnavailable`] when the PDP is down.
    pub async fn admit(&self, fact: &InboundFact) -> Result<AccessScope, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-flow-foundation-inbox-accept:p1:inst-ia-verify
        let key = &fact.key;
        self.parts
            .producers
            .verify(key.source(), &fact.origin)
            .inspect_err(|_| {
                tracing::warn!(
                    target: "bss_rating.inbox",
                    tenant_id = %key.tenant_id(),
                    source = %key.source(),
                    business_id = key.business_id(),
                    version = key.version(),
                    origin = ?fact.origin,
                    "inbox: producer identity does not verify; fact refused"
                );
            })?;
        let scope = self.parts.grant.scope_for(key.tenant_id()).await;
        self.observe_pdp(&scope);
        scope
        // @cpt-end:cpt-cf-bss-rating-flow-foundation-inbox-accept:p1:inst-ia-verify
    }

    /// Steps 2–5 of accepting a fact, inside the caller's transaction under the scope [`admit`]
    /// returned: a new key is stored `accepted` and applied by the owning feature in the same
    /// transaction; the same key and digest is a no-op; the same key with another digest is
    /// stored quarantined, and nothing is applied.
    ///
    /// A quarantine pages only once it is committed: the caller passes the outcome to
    /// [`Self::observe_committed`] after its commit ([`Self::accept_one`] does).
    ///
    /// [`admit`]: Self::admit
    ///
    /// # Errors
    ///
    /// [`DomainError::SourceNotApplicable`] for a source no feature applies (nothing written);
    /// [`DomainError::Concurrent`] when a concurrent transaction stored the key first (retry the
    /// transaction: the retry finds the winner's entry); the applier's refusal; a storage failure.
    #[tracing::instrument(
        name = "rating.inbox.accept",
        skip_all,
        fields(
            tenant_id = %fact.key.tenant_id(),
            source = %fact.key.source(),
            business_id = fact.key.business_id(),
            version = fact.key.version(),
        )
    )]
    pub async fn accept(
        &self,
        runner: &DbTx<'_>,
        scope: &AccessScope,
        fact: &InboundFact,
    ) -> Result<AcceptOutcome, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-flow-foundation-inbox-accept:p1:inst-ia-accept
        let existing = self
            .parts
            .repo
            .find_by_key(runner, scope, &fact.key)
            .await?;
        // @cpt-end:cpt-cf-bss-rating-flow-foundation-inbox-accept:p1:inst-ia-accept
        // @cpt-begin:cpt-cf-bss-rating-flow-foundation-inbox-accept:p1:inst-ia-duplicate
        if let Some(same) = existing.iter().find(|e| &e.payload_digest == fact.digest()) {
            return Ok(AcceptOutcome::Duplicate(same.clone()));
        }
        // @cpt-end:cpt-cf-bss-rating-flow-foundation-inbox-accept:p1:inst-ia-duplicate
        // @cpt-begin:cpt-cf-bss-rating-flow-foundation-inbox-accept:p1:inst-ia-conflict
        // The primary entry is the key's first arrival, whether or not it was applied.
        let primary = existing
            .iter()
            .find(|e| !matches!(e.quarantine, Some(Quarantine::DigestConflict { .. })));
        if let Some(primary) = primary {
            // @cpt-begin:cpt-cf-bss-rating-state-foundation-inbox:p1:inst-is-quarantine
            let quarantine = Quarantine::DigestConflict {
                conflict_of: primary.entry_id,
            };
            // @cpt-end:cpt-cf-bss-rating-state-foundation-inbox:p1:inst-is-quarantine
            return self.quarantine(runner, scope, fact, quarantine).await;
        }
        // @cpt-end:cpt-cf-bss-rating-flow-foundation-inbox-accept:p1:inst-ia-conflict
        if !fact.schema_supported {
            return self
                .quarantine(runner, scope, fact, Quarantine::UnsupportedSchemaVersion)
                .await;
        }
        // @cpt-begin:cpt-cf-bss-rating-flow-foundation-inbox-accept:p1:inst-ia-new
        let applier = self.applier(fact.key.source())?;
        let now = self.parts.clock.now();
        // @cpt-begin:cpt-cf-bss-rating-state-foundation-inbox:p1:inst-is-accept
        let mut entry = InboxEntry::arriving(fact, now, None);
        self.parts.repo.insert(runner, scope, &entry).await?;
        // @cpt-end:cpt-cf-bss-rating-state-foundation-inbox:p1:inst-is-accept
        applier.apply(runner, scope, &entry).await?;
        // @cpt-begin:cpt-cf-bss-rating-state-foundation-inbox:p1:inst-is-apply
        let apply = InboxTransition::ApplyAccepted { at: now };
        let moved = self
            .parts
            .repo
            .transition(runner, scope, entry.entry_id, apply)
            .await?;
        if !moved {
            return Err(DomainError::Concurrent(
                "the inbox entry left `accepted` before it was applied".to_owned(),
            ));
        }
        entry.state = apply.to();
        entry.applied_at = apply.applied_at();
        // @cpt-end:cpt-cf-bss-rating-state-foundation-inbox:p1:inst-is-apply
        // @cpt-begin:cpt-cf-bss-rating-flow-foundation-inbox-accept:p1:inst-ia-return
        Ok(AcceptOutcome::Applied(entry))
        // @cpt-end:cpt-cf-bss-rating-flow-foundation-inbox-accept:p1:inst-ia-return
        // @cpt-end:cpt-cf-bss-rating-flow-foundation-inbox-accept:p1:inst-ia-new
    }

    /// Admits and accepts one fact in a transaction of its own; a lost insert race is retried
    /// once, and the retry finds the winner's entry. A committed quarantine pages.
    ///
    /// # Errors
    ///
    /// As [`Self::admit`] and [`Self::accept`].
    pub async fn accept_one(&self, fact: InboundFact) -> Result<AcceptOutcome, DomainError> {
        let scope = self.admit(&fact).await?;
        let fact = Arc::new(fact);
        let mut retried = false;
        loop {
            let (inbox, scope, fact) = (self.clone(), scope.clone(), Arc::clone(&fact));
            let result = self
                .parts
                .db
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move { inbox.accept(tx, &scope, &fact).await })
                })
                .await;
            match result {
                Err(DomainError::Concurrent(_)) if !retried => retried = true,
                Ok(outcome) => {
                    self.observe_committed(&outcome);
                    return Ok(outcome);
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Pages a committed quarantine: an error log and `rating_inbox_quarantined_total{source}`,
    /// which the inbox-quarantine alert watches. Call it after the transaction of
    /// [`Self::accept`] committed, never inside it, so a rolled-back quarantine never pages.
    pub fn observe_committed(&self, outcome: &AcceptOutcome) {
        let AcceptOutcome::Quarantined(entry) = outcome else {
            return;
        };
        self.parts.metrics.inbox_quarantined(entry.key.source());
        tracing::error!(
            target: "bss_rating.inbox.quarantine",
            tenant_id = %entry.key.tenant_id(),
            source = %entry.key.source(),
            business_id = entry.key.business_id(),
            version = entry.key.version(),
            entry_id = %entry.entry_id,
            reason = entry.quarantine.map(|q| q.reason().as_str()),
            "inbox entry quarantined; nothing was applied"
        );
    }

    /// Releases a quarantined entry: `apply` hands its payload to the owning feature and marks it
    /// `applied`, `discard` marks it `discarded` and never applies it. The release is audited
    /// with actor, subject and request id in the same transaction. Releasing a quarantined entry
    /// again to the state it was released to answers that entry again (naturally idempotent);
    /// an entry that was never quarantined cannot be released.
    ///
    /// # Errors
    ///
    /// [`DomainError::PermissionDenied`] without `usage × release`; [`DomainError::NotFound`] for
    /// an entry absent or outside the caller's scope; [`DomainError::NotQuarantined`];
    /// [`DomainError::SourceNotApplicable`] to apply a source no feature applies;
    /// [`DomainError::Concurrent`] when a concurrent release won.
    pub async fn release(
        &self,
        ctx: &SecurityContext,
        command: ReleaseCommand,
    ) -> Result<InboxEntry, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-flow-foundation-inbox-release:p1:inst-ir-authorize
        let scope =
            authz::authorize(&self.parts.enforcer, ctx, Operation::InboxRelease, None).await;
        self.observe_pdp(&scope);
        let scope = scope?;
        // @cpt-end:cpt-cf-bss-rating-flow-foundation-inbox-release:p1:inst-ir-authorize
        let inbox = self.clone();
        let actor = ctx.subject_id();
        self.parts
            .db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move { inbox.release_in(tx, &scope, actor, command).await })
            })
            .await
    }

    async fn release_in(
        &self,
        runner: &DbTx<'_>,
        scope: &AccessScope,
        actor: Uuid,
        command: ReleaseCommand,
    ) -> Result<InboxEntry, DomainError> {
        let mut entry = self
            .parts
            .repo
            .find_by_id(runner, scope, command.entry_id)
            .await?
            .ok_or(DomainError::NotFound {
                what: "inbox entry",
            })?;
        // Writes land in the entry's tenant only, whatever else the caller's scope spans.
        let scope = authz::narrow_to_tenant(scope, entry.key.tenant_id(), SelectorRead::Object)?;
        // A repeat of a release that already moved the entry: both halves matter, as an entry
        // that was never quarantined is never released.
        let target = command.resolution.target_state();
        if entry.quarantine.is_some() && entry.state == target {
            return Ok(entry);
        }
        // @cpt-begin:cpt-cf-bss-rating-flow-foundation-inbox-release:p1:inst-ir-state
        entry.state.release(entry.entry_id, command.resolution)?;
        // @cpt-end:cpt-cf-bss-rating-flow-foundation-inbox-release:p1:inst-ir-state
        // @cpt-begin:cpt-cf-bss-rating-flow-foundation-inbox-release:p1:inst-ir-apply
        let transition = match command.resolution {
            ReleaseResolution::Apply => {
                self.applier(entry.key.source())?
                    .apply(runner, &scope, &entry)
                    .await?;
                InboxTransition::ApplyQuarantined {
                    at: self.parts.clock.now(),
                }
            }
            ReleaseResolution::Discard => InboxTransition::DiscardQuarantined,
        };
        // A conditional write: a concurrent or stale writer (another operator, a zombie leader)
        // matches no row and commits nothing. No lease fence is compared (ADR-0008).
        // @cpt-begin:cpt-cf-bss-rating-algo-foundation-lease:p1:inst-ls-fence
        let moved = self
            .parts
            .repo
            .transition(runner, &scope, entry.entry_id, transition)
            .await?;
        if !moved {
            return Err(DomainError::Concurrent(
                "a concurrent release moved the inbox entry first".to_owned(),
            ));
        }
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-lease:p1:inst-ls-fence
        entry.state = transition.to();
        entry.applied_at = transition.applied_at();
        if let Some(quarantine) = entry.quarantine {
            let key = ExceptionKey::new(
                entry.key.tenant_id(),
                SubjectKind::Inbox,
                entry.entry_id.to_string(),
                quarantine.reason().as_str(),
            )?;
            self.parts
                .exceptions
                .resolve(
                    runner,
                    &scope,
                    &key,
                    &format!("released:{}", command.resolution.as_str()),
                )
                .await?;
        }
        self.parts
            .audit
            .record(
                runner,
                &scope,
                &AuditEntry {
                    tenant_id: entry.key.tenant_id(),
                    actor,
                    operation_id: Operation::InboxRelease.operation_id(),
                    target_kind: INBOX_TARGET_KIND,
                    target_id: entry.entry_id.to_string(),
                    request_digest: command.request_digest,
                    reason: command.reason,
                    approved_by: None,
                    request_id: command.request_id,
                },
            )
            .await?;
        Ok(entry)
        // @cpt-end:cpt-cf-bss-rating-flow-foundation-inbox-release:p1:inst-ir-apply
    }

    /// Stores `fact` as a quarantined entry and opens its exception; it pages once committed
    /// ([`Self::observe_committed`]).
    async fn quarantine(
        &self,
        runner: &DbTx<'_>,
        scope: &AccessScope,
        fact: &InboundFact,
        quarantine: Quarantine,
    ) -> Result<AcceptOutcome, DomainError> {
        let entry = InboxEntry::arriving(fact, self.parts.clock.now(), Some(quarantine));
        self.parts.repo.insert(runner, scope, &entry).await?;
        let key = ExceptionKey::new(
            entry.key.tenant_id(),
            SubjectKind::Inbox,
            entry.entry_id.to_string(),
            quarantine.reason().as_str(),
        )?;
        let (source, business_id, version) = (
            entry.key.source(),
            entry.key.business_id(),
            entry.key.version(),
        );
        let detail = match quarantine {
            Quarantine::DigestConflict { conflict_of } => format!(
                "{source} {business_id} v{version}: digest {} conflicts with entry {conflict_of}",
                entry.payload_digest
            ),
            Quarantine::UnsupportedSchemaVersion => {
                format!("{source} {business_id} v{version}: unsupported schema version")
            }
        };
        self.parts
            .exceptions
            .record(runner, scope, &key, &detail)
            .await?;
        Ok(AcceptOutcome::Quarantined(entry))
    }

    /// Counts a PDP outage behind a scope request.
    fn observe_pdp(&self, scope: &Result<AccessScope, DomainError>) {
        if matches!(scope, Err(DomainError::AuthzUnavailable(..))) {
            self.parts
                .metrics
                .dependency_error(Dependency::Pdp, FailureCode::EvaluationFailed);
        }
    }

    fn applier(&self, source: InboxSource) -> Result<&Arc<dyn super::InboxApplier>, DomainError> {
        self.parts
            .appliers
            .get(&source)
            .ok_or(DomainError::SourceNotApplicable {
                inbox_source: source,
            })
    }
}
