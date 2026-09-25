// Created: 2026-09-07 by Virtuozzo International GmbH
// @cpt-dod:cpt-cf-settings-service-dod-value-writes-gates:p1
// @cpt-dod:cpt-cf-settings-service-dod-value-writes-atomicity:p1
// @cpt-dod:cpt-cf-settings-service-dod-value-writes-stale:p1
// @cpt-dod:cpt-cf-settings-service-dod-value-writes-ordering:p1
// @cpt-dod:cpt-cf-settings-service-dod-value-writes-impact:p1
// @cpt-dod:cpt-cf-settings-service-dod-value-writes-secret-routing:p1
// @cpt-dod:cpt-cf-settings-service-dod-gear-foundation-authz-stepup:p1
// @cpt-dod:cpt-cf-settings-service-dod-tenant-access-consumption:p1
//! The Value Writer: two gates in order, then one transaction per change.

use std::sync::Arc;

use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use settings_service_sdk::SettingKey;
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{image_of, value_state_tag};
use crate::audit::{AuditOperation, AuditRecord, AuditSink, AuditValue};
use crate::domain::access::{AccessRepository, TenantAccess};
use crate::domain::declaration::{Declaration, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::ports::{ChangePublisher, SecretManager, ValueEvent, WriteMetrics};
use crate::domain::precondition;
use crate::domain::resolution::{EffectiveValue, ScopeTarget, ValueResolver, scope_class};
use crate::domain::secrets::pending::{
    PENDING_SECRET_TTL, PendingSecretDraft, PendingSecretRepository, invalid_pending,
};
use crate::domain::stepup::StepUpRefusal;
use crate::domain::stepup::{
    INTERACTIVE_SUBJECT_TYPES, StepUpSubject, StepUpVerifier, unverified_payload,
};
use crate::domain::validation::{FieldViolation, TypeValidator};
use crate::domain::value::{ValueDraft, ValueRepository};
use crate::log_text::LogSafe;

/// Who is writing, with what proof.
#[derive(Debug, Clone)]
pub struct WriteActor {
    /// The authenticated caller.
    pub ctx: SecurityContext,
    /// The request the write belongs to.
    pub request_id: String,
    /// The step-up token presented, when any — the `X-Step-Up-Token` header,
    /// or the session's own bearer when it is fresh enough to serve. Kept
    /// wrapped: a `{:?}` of the actor prints `[REDACTED]`, the bytes are
    /// zeroed when the actor drops, and they are exposed in one place, the
    /// call to the verifier. A live token replays an elevated action for the
    /// length of its freshness window, so it never sits in a plain `String`
    /// that a log line or a panic message could carry.
    pub step_up_token: Option<SecretString>,
}

impl WriteActor {
    /// The subject id as recorded and audited.
    #[must_use]
    pub fn subject(&self) -> String {
        self.ctx.subject_id().to_string()
    }

    /// Whether the caller is a human session rather than a service principal.
    /// An unlabelled subject is not: absence of a label is no evidence of a
    /// person.
    #[must_use]
    pub fn is_interactive(&self) -> bool {
        self.ctx
            .subject_type()
            .is_some_and(|t| INTERACTIVE_SUBJECT_TYPES.contains(&t))
    }

    /// Who a step-up assertion must be bound to.
    #[must_use]
    pub fn step_up_subject(&self) -> StepUpSubject {
        StepUpSubject {
            subject_id: self.ctx.subject_id(),
            session_sub: self
                .ctx
                .bearer_token()
                .and_then(|t| session_sub(t.expose_secret())),
        }
    }
}

/// The `sub` of an unverified JWT payload, if the token is one. Used only to
/// bind a step-up token to the session it must confirm; the session token
/// itself was verified by authentication.
fn session_sub(bearer: &str) -> Option<String> {
    unverified_payload(bearer)?
        .get("sub")?
        .as_str()
        .map(str::to_owned)
}

/// What a change does to the scope's own row.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// Store this value.
    Set(Value),
    /// Adopt the entry a stage created earlier, by its reference: the plaintext
    /// was validated and stored then, and travels nowhere now.
    AdoptSecret {
        /// The reference of the entry the stage created.
        secret_ref: String,
        /// The token's row, consumed inside the commit.
        pending_id: Uuid,
    },
    /// Clear the override so the scope falls back.
    Revert,
    /// Remove the scope's own row.
    Remove,
}

/// A change after staging: validated, and with a secret already in the store.
///
/// The Credential Store cannot join the row's transaction, so a secret is
/// stored before it opens, under a reference unique to this write. What the
/// transaction then persists is the reference; the plaintext is gone from here.
/// Whether a stage judges the caller's `If-Match` before the store leg.
#[derive(Debug, Clone, Copy)]
pub enum StagePrecondition<'a> {
    /// A write: the tag the caller presented, judged against the current row
    /// before the plaintext goes anywhere, and again by the commit.
    Judge(Option<&'a str>),
    /// A secret staged ahead of its write: nothing to judge yet, since the
    /// tag belongs to the batch that adopts the stage.
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Staged {
    /// A value to store: inline, or as the reference of an entry just created.
    Set {
        /// The inline value; `None` for a secret.
        inline: Option<Value>,
        /// The reference of the entry created for this write; `None` inline.
        secret_ref: Option<String>,
        /// The row consumed inside the commit: the intent recorded before
        /// this write created its entry, or the token of the stage it adopts.
        /// `None` inline.
        pending_id: Option<Uuid>,
        /// Whether the entry was adopted from an earlier stage rather than
        /// created by this write — then it is not this write's to release.
        adopted: bool,
    },
    /// Fall back to the inherited value.
    Revert,
    /// Remove the override.
    Remove,
}

/// Whether step-up still has to be verified for this change, or was verified
/// once for the whole request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepUpPolicy {
    /// Verify here when the declaration requires it.
    Verify,
    /// Already verified once for the request.
    AlreadyVerified,
}

/// A change that passed every gate.
#[derive(Debug, Clone)]
pub struct Gated {
    /// The declaration written.
    pub declaration: Declaration,
    /// The target scope.
    pub target: ScopeTarget,
    /// The target as a tenant id.
    pub tenant_id: Uuid,
    /// The root tenant.
    pub root: Uuid,
    /// The caller's root-to-self chain as the gate resolved it, empty for the
    /// platform caller: what the commit derives access from again.
    pub caller_chain: Vec<Uuid>,
    /// Whether a verified step-up stands behind this change — verified at the
    /// gate, or once for the request by a caller that said so.
    pub step_up_verified: bool,
}

/// A committed change.
#[derive(Debug, Clone, PartialEq)]
pub struct Committed {
    /// The setting key.
    pub key: String,
    /// The target as a tenant id.
    pub tenant_id: Uuid,
    /// The target scope path.
    pub scope: String,
    /// The declaration's scope class, for the eviction that follows.
    pub scope_class: String,
    /// The declaration's classification, for masking the response.
    pub data_classification: String,
    /// The image before, when a row existed.
    pub old_value: Option<Value>,
    /// The image after, when a row remains.
    pub new_value: Option<Value>,
    /// The new value state tag of the scope.
    pub etag: String,
    /// What the change was recorded as.
    pub operation: AuditOperation,
    /// The change set it belongs to.
    pub change_set_id: Uuid,
    /// The store reference a removed or reverted secret row carried, released
    /// after the commit; `None` when nothing left the store's care.
    pub released_secret: Option<String>,
}

/// The read-only report of `validate`.
#[derive(Debug, Clone)]
pub struct ValidationReport {
    /// Field-level detail; empty when valid.
    pub violations: Vec<FieldViolation>,
    /// The current effective value at the target.
    pub effective: Arc<EffectiveValue>,
    /// For a cascading setting, the descendants the change would affect.
    pub impact: Option<ImpactReport>,
}

/// One descendant whose effective value would change.
#[derive(Debug, Clone, PartialEq)]
pub struct ImpactEntry {
    /// The descendant.
    pub tenant_id: Uuid,
    /// Its scope path.
    pub scope: String,
    /// Its effective value today.
    pub current: Value,
}

/// The bounded impact report.
#[derive(Debug, Clone, PartialEq)]
pub struct ImpactReport {
    /// The first `limit` affected descendants in traversal order.
    pub changed: Vec<ImpactEntry>,
    /// How many would change, up to the node budget.
    pub total_changed: usize,
    /// How many descendants were examined.
    pub scanned: usize,
    /// Whether the node budget or `limit` was hit.
    pub truncated: bool,
}

impl ImpactReport {
    /// The report of a setting nothing below inherits.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            changed: Vec::new(),
            total_changed: 0,
            scanned: 0,
            truncated: false,
        }
    }

    /// The default page size.
    pub const DEFAULT_LIMIT: usize = 100;
    /// The largest page size.
    pub const MAX_LIMIT: usize = 500;
    /// How many descendants a walk examines at most: the shared subtree budget.
    pub const NODE_BUDGET: usize = crate::domain::resolution::SUBTREE_BUDGET;
}

/// The writer.
pub struct ValueWriter<D, V, A, S, P> {
    values: V,
    resolver: Arc<ValueResolver<D, V, A>>,
    validator: Arc<dyn TypeValidator>,
    sink: S,
    step_up: Arc<dyn StepUpVerifier>,
    secrets: Arc<dyn SecretManager>,
    pending: P,
    publisher: Arc<dyn ChangePublisher>,
    metrics: Arc<dyn WriteMetrics>,
}

fn denied() -> DomainError {
    DomainError::Unauthorized {
        resource: settings_service_sdk::gts::VALUE_SCHEMA,
    }
}

impl<D, V, A, S, P> ValueWriter<D, V, A, S, P>
where
    D: DeclarationRepository,
    V: ValueRepository + Clone,
    A: AccessRepository,
    S: AuditSink,
    P: PendingSecretRepository,
{
    /// Build the writer over the resolver and its ports.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        values: V,
        resolver: Arc<ValueResolver<D, V, A>>,
        validator: Arc<dyn TypeValidator>,
        sink: S,
        step_up: Arc<dyn StepUpVerifier>,
        secrets: Arc<dyn SecretManager>,
        pending: P,
        publisher: Arc<dyn ChangePublisher>,
        metrics: Arc<dyn WriteMetrics>,
    ) -> Self {
        Self {
            values,
            resolver,
            validator,
            sink,
            step_up,
            secrets,
            pending,
            publisher,
            metrics,
        }
    }

    /// The resolver this writer reads through.
    #[must_use]
    pub fn resolver(&self) -> &Arc<ValueResolver<D, V, A>> {
        &self.resolver
    }

    /// The step-up verifier, for the challenge a refusal carries.
    #[must_use]
    pub fn step_up(&self) -> &Arc<dyn StepUpVerifier> {
        &self.step_up
    }

    /// The Secret Manager, for the entries a sweep releases.
    #[must_use]
    pub fn secrets(&self) -> &Arc<dyn SecretManager> {
        &self.secrets
    }

    /// The declaration at a key as a write sees it: absent or hidden from the
    /// caller is not-found, retired is the distinct retired outcome.
    ///
    /// # Errors
    /// [`DomainError::NotFound`], [`DomainError::Retired`], or a read failure.
    pub async fn declaration_for_write<C: DBRunner>(
        &self,
        conn: &C,
        actor: &WriteActor,
        key: &SettingKey,
        root: Uuid,
    ) -> Result<Declaration, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-2
        let declaration =
            self.resolver
                .find_declaration(conn, key)
                .await?
                .ok_or(DomainError::NotFound {
                    resource: "declaration",
                })?;
        let caller = ScopeTarget::Tenant(actor.ctx.subject_tenant_id()).normalize(root);
        if self
            .resolver
            .effective_access(conn, declaration.id, caller)
            .await?
            .is_hidden()
        {
            return Err(DomainError::NotFound {
                resource: "declaration",
            });
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-2
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-3
        if declaration.status == "retired" {
            return Err(DomainError::Retired {
                key: declaration.key.clone(),
            });
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-3
        Ok(declaration)
    }

    /// The write gate order, after authorization — which the caller decided
    /// first, without consulting step-up: the declaration, the target, the
    /// caller's own access, and step-up last.
    ///
    /// # Errors
    /// The first gate's refusal: not-found or retired for the declaration,
    /// [`DomainError::Unauthorized`] for a service principal, a target outside
    /// the subtree or a caller whose own access is not overridable,
    /// [`DomainError::StepUpRequired`] for a missing or stale step-up,
    /// [`DomainError::Conflict`] for a tenant-scoped write to a global setting.
    pub async fn gate<C: DBRunner>(
        &self,
        conn: &C,
        actor: &WriteActor,
        key: &SettingKey,
        requested: Option<Uuid>,
        policy: StepUpPolicy,
        operation: &'static str,
    ) -> Result<Gated, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-1
        // Authorization was decided by the caller before this point; nothing
        // here runs for an unauthorized caller, step-up included.
        let root = self.resolver.root_tenant().await?;
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-1
        let declaration = self.declaration_for_write(conn, actor, key, root).await?;
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-6
        let caller = actor.ctx.subject_tenant_id();
        let tenant_id = requested.unwrap_or(caller);
        if tenant_id != caller {
            let hierarchy = self.resolver.hierarchy();
            if !hierarchy.is_within_subtree(caller, tenant_id).await?
                || hierarchy.is_standalone(tenant_id).await?
            {
                return Err(denied());
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-6
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-7
        if declaration.scope_class == scope_class::GLOBAL && tenant_id != root {
            return Err(DomainError::Conflict {
                detail: format!(
                    "`{key}` is a global setting: nobody writes a tenant-scoped value for it"
                ),
            });
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-7
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-8
        // The caller's own access, never the target's: an overridable ancestor
        // manages a restricted descendant. The chain is resolved once, here,
        // and travels with the change: the commit reads the rows again over it.
        let caller_chain = if caller == root {
            Vec::new()
        } else {
            let chain = self.resolver.chain_of(caller).await?;
            let own = self
                .resolver
                .access_on_chain(conn, declaration.id, &chain)
                .await?;
            if own.access != TenantAccess::Overridable {
                return Err(denied());
            }
            chain
        };
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-8
        // Step-up comes last, once the write is otherwise the caller's to
        // make: a challenge is issued only for a write that would go through
        // with it — authorize the action, then challenge (RFC 9470) — so a
        // caller without rights on the target learns nothing about what the
        // setting would have asked of it. When the declaration requires it, a
        // verified step-up stands behind the change once this block is
        // through: verified here, or once for the request by the caller that
        // chose `AlreadyVerified`.
        let step_up_verified = declaration.requires_step_up;
        if declaration.requires_step_up {
            // @cpt-begin:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-4
            // A setting that needs a person to confirm it is by definition not
            // one a machine may set: refused before any validation.
            if !actor.is_interactive() {
                self.metrics.step_up(operation, "service_principal");
                return Err(denied());
            }
            // @cpt-end:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-4
            // @cpt-begin:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-5
            // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-6
            if policy == StepUpPolicy::Verify {
                self.verify_step_up(actor, operation).await?;
            }
            // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-6
            // @cpt-end:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-5
        }
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-9
        Ok(Gated {
            declaration,
            target: ScopeTarget::Tenant(tenant_id).normalize(root),
            tenant_id,
            root,
            caller_chain,
            step_up_verified,
        })
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-9
    }

    /// The target of a read-only write-path operation: the caller's own
    /// tenant, or a descendant that is not standalone.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] for anything else.
    pub async fn target_for(
        &self,
        actor: &WriteActor,
        requested: Option<Uuid>,
    ) -> Result<(ScopeTarget, Uuid, Uuid), DomainError> {
        let root = self.resolver.root_tenant().await?;
        let caller = actor.ctx.subject_tenant_id();
        let tenant_id = requested.unwrap_or(caller);
        if tenant_id != caller {
            let hierarchy = self.resolver.hierarchy();
            if !hierarchy.is_within_subtree(caller, tenant_id).await?
                || hierarchy.is_standalone(tenant_id).await?
            {
                return Err(denied());
            }
        }
        Ok((
            ScopeTarget::Tenant(tenant_id).normalize(root),
            tenant_id,
            root,
        ))
    }

    /// Verify step-up once, counting the outcome.
    ///
    /// # Errors
    /// [`DomainError::StepUpRequired`] carrying the challenge's parameters.
    pub async fn verify_step_up(
        &self,
        actor: &WriteActor,
        operation: &'static str,
    ) -> Result<(), DomainError> {
        let subject = actor.step_up_subject();
        match self
            .step_up
            .verify(
                actor
                    .step_up_token
                    .as_ref()
                    .map(ExposeSecret::expose_secret),
                &subject,
            )
            .await
        {
            Ok(()) => {
                // @cpt-begin:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-6
                self.metrics.step_up(operation, "verified");
                Ok(())
                // @cpt-end:cpt-cf-settings-service-algo-value-writes-step-up:p1:inst-vw-su-6
            }
            Err(refusal) => {
                self.metrics.step_up(operation, refusal.code());
                match refusal {
                    // Not a verdict on the token: the platform's AuthN could
                    // not be asked. Unavailable tells the client to retry; a
                    // challenge would tell the person to re-authenticate
                    // against a dependency that is down.
                    StepUpRefusal::Unavailable(detail) => Err(DomainError::Unavailable {
                        detail: format!("step-up could not be verified: {detail}"),
                    }),
                    refusal => Err(self.challenge(&refusal)),
                }
            }
        }
    }

    /// The challenge a write without a verified step-up behind it is answered
    /// with: the deployment's freshness window and assurance levels.
    fn challenge(&self, refusal: &StepUpRefusal) -> DomainError {
        let requirement = self.step_up.requirement();
        DomainError::StepUpRequired {
            reason: refusal.code(),
            max_age_seconds: requirement.max_age.as_secs(),
            acr_values: requirement.acr_values.clone(),
        }
    }

    /// Whether any of these declarations requires step-up — the batch asks
    /// once for the request.
    #[must_use]
    pub fn any_requires_step_up(declarations: &[Declaration]) -> bool {
        declarations.iter().any(|d| d.requires_step_up)
    }

    /// Stage a change: validate the value and, for a secret, put the plaintext
    /// in the store. Runs before the transaction, which the store cannot join;
    /// `conn` is for the intent row a secret leaves behind first.
    ///
    /// # Errors
    /// [`DomainError::Validation`] for an invalid value;
    /// [`DomainError::Unavailable`] when the store cannot answer — the intent
    /// row stays for the sweep, since the entry may exist without an answer;
    /// [`DomainError`] when the row cannot be written, nothing stored anywhere.
    pub async fn stage<C: DBRunner>(
        &self,
        conn: &C,
        gated: &Gated,
        actor: &WriteActor,
        change: Change,
        precondition: StagePrecondition<'_>,
    ) -> Result<Staged, DomainError> {
        let declaration = &gated.declaration;
        // The tag is judged before the plaintext goes anywhere: a stale or
        // missing one is the cheapest refusal and the one a concurrent editor
        // trips most, and refusing it here leaves no store entry to release,
        // no intent row to sweep and no compensating delete to fail. It is a
        // preview of the check the commit makes again under its lock, which
        // is the one that decides; a row that moves in between is refused
        // there, having cost one store entry — the race, not the rule.
        if let StagePrecondition::Judge(if_match) = precondition {
            let current = self
                .values
                .find_one(
                    conn,
                    &AccessScope::allow_all(),
                    declaration.id,
                    gated.tenant_id,
                )
                .await?;
            precondition::evaluate(if_match, &value_state_tag(current.as_ref()))?;
        }
        let value = match change {
            Change::Set(value) => value,
            Change::AdoptSecret {
                secret_ref,
                pending_id,
            } => {
                // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-8
                // Validated and stored when it was staged; the store leg is
                // skipped and the reference goes on to the commit as any
                // secret's would, the token's row consumed there.
                return Ok(Staged::Set {
                    inline: None,
                    secret_ref: Some(secret_ref),
                    pending_id: Some(pending_id),
                    adopted: true,
                });
                // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-8
            }
            Change::Revert => return Ok(Staged::Revert),
            Change::Remove => return Ok(Staged::Remove),
        };
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-1
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-1
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-2
        // A secret is still a typed value: validated like any other, before
        // anything is stored anywhere.
        self.validator
            .validate_value(&declaration.value_type_id, &value)
            .await?
            .into_result()?;
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-2
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-1
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-4
        // Plaintext never reaches `value` for a secret: the Secret Manager
        // takes it under a reference unique to this write, and with nothing
        // bound the write is unavailable rather than stored in clear. The
        // store cannot join the row's transaction, so this runs before it.
        //
        // The intent first, durable, then the entry. The row names the
        // reference the create is about to use, so a create whose answer never
        // arrives — the entry may or may not exist — leaves the sweep
        // something to reclaim within the pending window instead of an orphan
        // nobody can find. A committed write deletes the row inside its
        // transaction; a refused one deletes it with the entry.
        if declaration.has_secret_trait {
            let secret_ref = self
                .secrets
                .mint_reference(&declaration.key, gated.tenant_id);
            let intent = self
                .pending
                .insert(
                    conn,
                    &AccessScope::allow_all(),
                    PendingSecretDraft {
                        declaration_id: declaration.id,
                        tenant_id: gated.tenant_id,
                        subject_id: actor.subject(),
                        secret_ref: secret_ref.clone(),
                        expires_at: OffsetDateTime::now_utc() + PENDING_SECRET_TTL,
                    },
                )
                .await?;
            self.secrets
                .store_secret(&declaration.key, gated.tenant_id, &secret_ref, &value)
                .await?;
            return Ok(Staged::Set {
                inline: None,
                secret_ref: Some(secret_ref),
                pending_id: Some(intent.id),
                adopted: false,
            });
        }
        Ok(Staged::Set {
            inline: Some(value),
            secret_ref: None,
            pending_id: None,
            adopted: false,
        })
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-4
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-1
    }

    /// Release the entry this write created, and the intent row that named
    /// it, when its transaction did not commit. Logged, never failed: the
    /// refusal already stands, and a row left behind is the sweep's.
    ///
    /// An adopted entry is left alone: the stage created it, the token still
    /// names it, and a commit that failed left both in place for the retry —
    /// or for the sweep, once the token expires.
    pub async fn discard<C: DBRunner>(&self, conn: &C, gated: &Gated, staged: &Staged) {
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-7
        let Staged::Set {
            secret_ref: Some(reference),
            pending_id,
            adopted: false,
            ..
        } = staged
        else {
            return;
        };
        // The entry first, the row only once the entry is gone: a release
        // that fails leaves the sweep its handle on the entry.
        if let Err(err) = self
            .secrets
            .delete_secret(&gated.declaration.key, gated.tenant_id, reference)
            .await
        {
            tracing::warn!(
                key = %gated.declaration.key,
                tenant = %gated.tenant_id,
                err = %LogSafe(&err),
                "secret entry of a refused write not released; its intent row stays for the sweep"
            );
            return;
        }
        if let Some(id) = pending_id
            && let Err(err) = self
                .pending
                .delete(conn, &AccessScope::allow_all(), *id)
                .await
        {
            tracing::warn!(
                pending_id = %id,
                err = %LogSafe(&err),
                "intent row of a refused write not removed; the sweep finds nothing to release"
            );
        }
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-7
    }

    /// Commit a staged change inside the caller's transaction: the tag check,
    /// the row, and its audit record.
    ///
    /// # Errors
    /// [`DomainError::Retired`] when the declaration retired since the gate,
    /// [`DomainError::PreconditionRequired`] / [`DomainError::PreconditionFailed`]
    /// on the tag, [`DomainError::NotFound`] removing a row that is not there,
    /// [`DomainError::Unavailable`] when the database or the audit sink cannot
    /// answer — the caller's transaction rolls back.
    pub async fn commit_in<C: DBRunner>(
        &self,
        conn: &C,
        gated: &Gated,
        staged: &Staged,
        if_match: Option<&str>,
        actor: &WriteActor,
        change_set_id: Uuid,
    ) -> Result<Committed, DomainError> {
        let scope = AccessScope::allow_all();
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-2
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-3
        // Inside the caller's transaction, which spans this change alone. The
        // gate read the declaration outside it; it is read again here under a
        // share lock held to the commit. A retire or a major upgrade already
        // under way holds the row for update, so this read waits for it and
        // sees `retired` — the write lands nowhere rather than on a retired
        // declaration, where the upgrade's copy would never find it. One that
        // starts later waits for this commit, and copies or retains this value.
        let live = self
            .resolver
            .lock_declaration(conn, gated.declaration.id)
            .await?
            .ok_or(DomainError::NotFound {
                resource: "declaration",
            })?;
        if live.status == "retired" {
            return Err(DomainError::Retired { key: live.key });
        }
        // From here on the declaration is the row as it is inside this
        // transaction, not the gate's snapshot. The classification stamped on
        // the row and the secret trait that masks the audit images come from
        // it: a reclassification committed between the gate and here resynced
        // only the rows that existed then, and this row did not; one under way
        // waits for this commit and resyncs it with the rest.
        let declaration = &live;
        // Two more decisions the gate took from outside the transaction are
        // taken again here, under the same lock. A restriction set or cleared
        // takes the declaration row for update, so one already under way is
        // seen once it commits, and one that starts later waits for this
        // commit: the caller's own access is derived again from the rows on
        // the chain the gate resolved — hidden is absent, as at the gate. And
        // a step-up requirement switched on since the gate finds no verified
        // step-up behind this write; the retry meets the gate that sees it.
        if !gated.caller_chain.is_empty() {
            let own = self
                .resolver
                .access_on_chain(conn, declaration.id, &gated.caller_chain)
                .await?;
            if own.is_hidden() {
                return Err(DomainError::NotFound {
                    resource: "declaration",
                });
            }
            if own.access != TenantAccess::Overridable {
                return Err(denied());
            }
        }
        if declaration.requires_step_up && !gated.step_up_verified {
            return Err(self.challenge(&StepUpRefusal::Missing));
        }
        // The tag is compared here and the row written below in the same
        // transaction, so a value that moved in between is the other writer's.
        let current = self
            .values
            .find_one(conn, &scope, declaration.id, gated.tenant_id)
            .await?;
        precondition::evaluate(if_match, &value_state_tag(current.as_ref()))?;
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-3
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-2
        let old_value = current.as_ref().map(image_of);
        let mut released_secret = None;
        let (stored, operation) = match staged {
            Staged::Set {
                inline,
                secret_ref,
                pending_id,
                adopted,
            } => {
                // @cpt-begin:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-6
                // @cpt-dod:cpt-cf-settings-service-dod-secret-values-reference-only:p1
                // The row behind the secret — the write's own intent, or the
                // token of the stage it adopts — is consumed here, inside the
                // transaction, by one statement that re-asserts its expiry: a
                // commit that lands takes it with it, one that does not leaves
                // it. A row gone or past its expiry is the sweep's, its entry
                // released or about to be, and no row commits pointing at it.
                if let Some(id) = pending_id
                    && !self
                        .pending
                        .claim(conn, &scope, *id, OffsetDateTime::now_utc())
                        .await?
                {
                    return Err(if *adopted {
                        invalid_pending()
                    } else {
                        DomainError::Unavailable {
                            detail: "the secret staged for this write expired before it \
                                     committed; retry the write"
                                .to_owned(),
                        }
                    });
                }
                // The row takes the reference of the entry created for this
                // write; the entry it held before is released after the commit,
                // once nothing can point at it any more.
                if let Some(previous) = current.as_ref().and_then(|row| row.secret_ref.as_deref())
                    && secret_ref.as_deref() != Some(previous)
                {
                    released_secret = Some(previous.to_owned());
                }
                let inline = inline.clone();
                let secret_ref = secret_ref.clone();
                // @cpt-end:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-6
                // @cpt-begin:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-5
                // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-classification-sync:p1:inst-tvv-sync-1
                // @cpt-begin:cpt-cf-settings-service-state-typed-value-validation-review:p1:inst-tvv-state-2
                // A valid re-set clears `needs_review`; the unique index guards
                // the first insert so two first writers cannot both land. The
                // row takes the declaration's classification as it is written,
                // so masking reads one column and the two never disagree.
                match &current {
                    Some(row) => (
                        Some(
                            self.values
                                .update(
                                    conn,
                                    &scope,
                                    row.id,
                                    inline,
                                    secret_ref,
                                    &actor.subject(),
                                    row.last_change_at,
                                )
                                .await?,
                        ),
                        AuditOperation::Change,
                    ),
                    None => (
                        Some(
                            self.values
                                .insert(
                                    conn,
                                    &scope,
                                    ValueDraft {
                                        declaration_id: declaration.id,
                                        tenant_id: gated.tenant_id,
                                        value: inline,
                                        secret_ref,
                                        data_classification: declaration
                                            .data_classification
                                            .clone(),
                                        needs_review: false,
                                        needs_review_detail: None,
                                        set_by: actor.subject(),
                                    },
                                )
                                .await?,
                        ),
                        AuditOperation::Create,
                    ),
                }
                // @cpt-end:cpt-cf-settings-service-state-typed-value-validation-review:p1:inst-tvv-state-2
                // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-classification-sync:p1:inst-tvv-sync-1
                // @cpt-end:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-5
            }
            Staged::Revert | Staged::Remove => {
                let Some(row) = &current else {
                    return Err(DomainError::NotFound { resource: "value" });
                };
                // @cpt-begin:cpt-cf-settings-service-flow-secret-values-remove:p1:inst-sv-remove-1
                // @cpt-begin:cpt-cf-settings-service-flow-secret-values-remove:p1:inst-sv-remove-2
                // The reference leaves the transaction with the outcome; the
                // store is touched only once the row is durably gone.
                if declaration.has_secret_trait {
                    released_secret = row.secret_ref.clone();
                }
                // @cpt-end:cpt-cf-settings-service-flow-secret-values-remove:p1:inst-sv-remove-2
                // @cpt-end:cpt-cf-settings-service-flow-secret-values-remove:p1:inst-sv-remove-1
                self.values
                    .delete(
                        conn,
                        &scope,
                        declaration.id,
                        gated.tenant_id,
                        row.last_change_at,
                    )
                    .await?;
                let operation = if matches!(staged, Staged::Revert) {
                    AuditOperation::Revert
                } else {
                    AuditOperation::Remove
                };
                (None, operation)
            }
        };
        let new_value = stored.as_ref().map(image_of);
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-6
        // The record in the same transaction, images masked by classification;
        // a sink that cannot write rolls the change back with it.
        let mut record = AuditRecord::new(
            declaration.key.as_str(),
            Some(gated.tenant_id),
            actor.subject(),
            operation,
            actor.request_id.clone(),
        )
        .with_change_set(change_set_id);
        if let Some(old) = &old_value {
            record = record.with_pre_image(AuditValue::record(
                old.clone(),
                declaration.has_secret_trait,
            ));
        }
        if let Some(new) = &new_value {
            record = record.with_post_image(AuditValue::record(
                new.clone(),
                declaration.has_secret_trait,
            ));
        }
        self.sink.append(conn, &scope, record).await?;
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-6
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-7
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-10
        // The caller commits; a failed commit is its unavailability, nothing
        // stored. What comes back is the old value, the new value, the scope
        // and the new tag.
        Ok(Committed {
            key: declaration.key.clone(),
            tenant_id: gated.tenant_id,
            scope: crate::domain::resolution::scope_path(gated.tenant_id, gated.root),
            scope_class: declaration.scope_class.clone(),
            data_classification: declaration.data_classification.clone(),
            old_value,
            new_value,
            etag: value_state_tag(stored.as_ref()).as_str().to_owned(),
            operation,
            change_set_id,
            released_secret,
        })
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-10
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-7
    }

    /// What follows a durable commit, in this order: evict, then publish.
    pub async fn after_commit(&self, committed: &Committed, actor: &WriteActor) {
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-8
        self.resolver.cache().invalidate(
            &committed.key,
            &committed.scope_class,
            Some(committed.tenant_id),
        );
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-8
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-9
        self.publisher
            .publish(ValueEvent::Changed {
                key: committed.key.clone(),
                tenant_id: committed.tenant_id,
                actor: actor.subject(),
                change_set_id: committed.change_set_id,
            })
            .await;
        self.metrics.value_write("committed");
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-9
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-remove:p1:inst-sv-remove-3
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-8
        // @cpt-dod:cpt-cf-settings-service-dod-secret-values-cleanup:p1
        // The committed row is the truth; an entry the store will not release
        // is an orphan to log, never a reason to fail a change that happened.
        // Removed, reverted or superseded: the same release.
        if let Some(reference) = &committed.released_secret
            && let Err(err) = self
                .secrets
                .delete_secret(&committed.key, committed.tenant_id, reference)
                .await
        {
            tracing::warn!(
                key = %committed.key,
                tenant = %committed.tenant_id,
                err = %LogSafe(&err),
                "secret entry not released after the change; the reference is orphaned"
            );
        }
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-8
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-remove:p1:inst-sv-remove-3
    }

    /// What follows a rejection: a durable notification and a count.
    pub async fn after_rejection(
        &self,
        key: &str,
        tenant_id: Uuid,
        actor: &WriteActor,
        reason: &DomainError,
        change_set_id: Uuid,
    ) {
        // An internal fault's diagnostic stays in this process's log; the event
        // may travel further than the log does.
        if let Some(diagnostic) = reason.internal_diagnostic() {
            tracing::error!(
                %key,
                %tenant_id,
                diagnostic = %LogSafe(diagnostic),
                "value change failed internally"
            );
        }
        self.publisher
            .publish(ValueEvent::ChangeFailed {
                key: key.to_owned(),
                tenant_id,
                actor: actor.subject(),
                reason: reason.event_reason(),
                change_set_id,
            })
            .await;
        self.metrics.value_write("rejected");
    }

    /// The read-only report: validity with field-level detail, the current
    /// effective value, and the impact for a cascading setting. Stores nothing
    /// and emits no record; the same answer for the same inputs.
    ///
    /// # Errors
    /// [`DomainError`] when the resolver or the walk cannot answer.
    pub async fn validate<C: DBRunner>(
        &self,
        conn: &C,
        declaration: &Declaration,
        target: ScopeTarget,
        value: &Value,
        limit: Option<usize>,
    ) -> Result<ValidationReport, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-5
        let violations = self
            .validator
            .validate_value(&declaration.value_type_id, value)
            .await?
            .violations;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-5
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-6
        let key = SettingKey::parse(&declaration.key).map_err(|e| DomainError::Internal {
            diagnostic: format!("stored key `{}` does not parse: {e}", declaration.key),
        })?;
        let effective = self.resolver.resolve(conn, &key, target).await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-6
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-7
        let impact = if declaration.scope_class == scope_class::CASCADING {
            Some(self.impact(conn, declaration, target, value, limit).await?)
        } else {
            None
        };
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-7
        Ok(ValidationReport {
            violations,
            effective,
            impact,
        })
    }

    /// The bounded impact walk: which descendants would see a different
    /// effective value under `candidate` set at `target`.
    ///
    /// # Errors
    /// [`DomainError`] when the tenant resolver or the resolver cannot answer.
    pub async fn impact<C: DBRunner>(
        &self,
        conn: &C,
        declaration: &Declaration,
        target: ScopeTarget,
        candidate: &Value,
        limit: Option<usize>,
    ) -> Result<ImpactReport, DomainError> {
        if declaration.scope_class != scope_class::CASCADING {
            return Ok(ImpactReport::empty());
        }
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-impact:p1:inst-vw-imp-walk-1
        let limit = limit
            .unwrap_or(ImpactReport::DEFAULT_LIMIT)
            .clamp(1, ImpactReport::MAX_LIMIT);
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-impact:p1:inst-vw-imp-walk-1
        let root = self.resolver.root_tenant().await?;
        let target_tenant = target.tenant_id(root);
        let key = SettingKey::parse(&declaration.key).map_err(|e| DomainError::Internal {
            diagnostic: format!("stored key `{}` does not parse: {e}", declaration.key),
        })?;
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-impact:p1:inst-vw-imp-walk-2
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-impact:p1:inst-vw-imp-walk-3
        // Breadth-first, barriers respected: a standalone descendant and
        // everything below it is never listed and never counted, since a bare
        // count still says the tenant exists and differs.
        let (descendants, budget_hit) = self
            .resolver
            .hierarchy()
            .descendants_bfs(target_tenant, ImpactReport::NODE_BUDGET)
            .await?;
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-impact:p1:inst-vw-imp-walk-3
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-impact:p1:inst-vw-imp-walk-2
        let mut report = ImpactReport::empty();
        for descendant in descendants {
            report.scanned += 1;
            // @cpt-begin:cpt-cf-settings-service-algo-value-writes-impact:p1:inst-vw-imp-walk-4
            // A descendant keeps its value when a row at or below itself,
            // deeper than the target, already supplies it; otherwise it takes
            // the candidate and changes when the candidate differs.
            let current = self
                .resolver
                .resolve(conn, &key, ScopeTarget::Tenant(descendant))
                .await?;
            let shielded = match current.source_scope.as_deref() {
                None => false,
                Some(source) => {
                    let source_tenant = current
                        .trail
                        .iter()
                        .find(|e| e.scope == source)
                        .map(|e| e.tenant_id);
                    let chain: Vec<Uuid> = current.trail.iter().map(|e| e.tenant_id).collect();
                    let target_pos = chain.iter().position(|t| *t == target_tenant);
                    let source_pos = source_tenant.and_then(|s| chain.iter().position(|t| *t == s));
                    matches!((target_pos, source_pos), (Some(t), Some(s)) if s > t)
                }
            };
            if !shielded && current.value != *candidate {
                report.total_changed += 1;
                if report.changed.len() < limit {
                    report.changed.push(ImpactEntry {
                        tenant_id: descendant,
                        scope: current.scope.clone(),
                        current: current.value.clone(),
                    });
                }
            }
            // @cpt-end:cpt-cf-settings-service-algo-value-writes-impact:p1:inst-vw-imp-walk-4
        }
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-impact:p1:inst-vw-imp-walk-5
        report.truncated = budget_hit || report.total_changed > report.changed.len();
        Ok(report)
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-impact:p1:inst-vw-imp-walk-5
    }
}

impl ScopeTarget {
    /// A tenant target whose id is the root is platform scope.
    #[must_use]
    pub fn normalize(self, root: Uuid) -> Self {
        match self {
            Self::Tenant(id) if id == root => Self::Platform,
            other => other,
        }
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;
