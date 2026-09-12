// Created: 2026-09-07 by Constructor Tech
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

use serde_json::Value;
use settings_service_sdk::SettingKey;
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{image_of, value_state_tag};
use crate::api::precondition;
use crate::audit::{AuditOperation, AuditRecord, AuditSink, AuditValue};
use crate::domain::access::{AccessRepository, TenantAccess};
use crate::domain::declaration::{Declaration, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::ports::{ChangePublisher, SecretManager, ValueEvent, WriteMetrics};
use crate::domain::resolution::{EffectiveValue, ScopeTarget, ValueResolver, scope_class};
use crate::domain::stepup::{StepUpSubject, StepUpVerifier, USER_SUBJECT_TYPE};
use crate::domain::validation::{FieldViolation, TypeValidator};
use crate::domain::value::{ValueDraft, ValueRepository};

/// Who is writing, with what proof.
#[derive(Debug, Clone)]
pub struct WriteActor {
    /// The authenticated caller.
    pub ctx: SecurityContext,
    /// The request the write belongs to.
    pub request_id: String,
    /// The step-up token presented, when any.
    pub step_up_token: Option<String>,
}

impl WriteActor {
    /// The subject id as recorded and audited.
    #[must_use]
    pub fn subject(&self) -> String {
        self.ctx.subject_id().to_string()
    }

    /// Whether the caller is a human session rather than a service principal.
    #[must_use]
    pub fn is_interactive(&self) -> bool {
        self.ctx.subject_type() == Some(USER_SUBJECT_TYPE)
    }

    /// Who a step-up assertion must be bound to.
    #[must_use]
    pub fn step_up_subject(&self) -> StepUpSubject {
        StepUpSubject {
            subject_id: self.ctx.subject_id(),
            session_sub: self
                .ctx
                .bearer_token()
                .and_then(|t| session_sub(secrecy::ExposeSecret::expose_secret(t))),
        }
    }
}

/// The `sub` of an unverified JWT payload, if the token is one. Used only to
/// bind a step-up token to the session it must confirm; the session token
/// itself was verified by authentication.
fn session_sub(bearer: &str) -> Option<String> {
    use base64::Engine;
    let payload = bearer.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("sub")?.as_str().map(str::to_owned)
}

/// What a change does to the scope's own row.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// Store this value.
    Set(Value),
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
#[derive(Debug, Clone, PartialEq)]
pub enum Staged {
    /// A value to store: inline, or as the reference of an entry just created.
    Set {
        /// The inline value; `None` for a secret.
        inline: Option<Value>,
        /// The reference of the entry created for this write; `None` inline.
        secret_ref: Option<String>,
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
    /// How many descendants a walk examines at most.
    pub const NODE_BUDGET: usize = 5_000;
}

/// The writer.
pub struct ValueWriter<D, V, A, S> {
    values: V,
    resolver: Arc<ValueResolver<D, V, A>>,
    validator: Arc<dyn TypeValidator>,
    sink: S,
    step_up: Arc<dyn StepUpVerifier>,
    secrets: Arc<dyn SecretManager>,
    publisher: Arc<dyn ChangePublisher>,
    metrics: Arc<dyn WriteMetrics>,
}

fn denied() -> DomainError {
    DomainError::Unauthorized {
        resource: settings_service_sdk::gts::VALUE_SCHEMA,
    }
}

impl<D, V, A, S> ValueWriter<D, V, A, S>
where
    D: DeclarationRepository,
    V: ValueRepository + Clone,
    A: AccessRepository,
    S: AuditSink,
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
    /// first, without consulting step-up.
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
        // manages a restricted descendant.
        if caller != root {
            let own = self
                .resolver
                .effective_access(conn, declaration.id, ScopeTarget::Tenant(caller))
                .await?;
            if own.access != TenantAccess::Overridable {
                return Err(denied());
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-8
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-9
        Ok(Gated {
            declaration,
            target: ScopeTarget::Tenant(tenant_id).normalize(root),
            tenant_id,
            root,
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
            .verify(actor.step_up_token.as_deref(), &subject)
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
                let requirement = self.step_up.requirement();
                Err(DomainError::StepUpRequired {
                    reason: refusal.code(),
                    max_age_seconds: requirement.max_age.as_secs(),
                    acr_values: requirement.acr_values.clone(),
                })
            }
        }
    }

    /// Whether any of these declarations requires step-up — the batch asks
    /// once for the request.
    #[must_use]
    pub fn any_requires_step_up(declarations: &[Declaration]) -> bool {
        declarations.iter().any(|d| d.requires_step_up)
    }

    /// Stage a change: validate the value and, for a secret, put the plaintext
    /// in the store. Runs before the transaction, which the store cannot join.
    ///
    /// # Errors
    /// [`DomainError::Validation`] for an invalid value;
    /// [`DomainError::Unavailable`] when the store cannot answer, in which case
    /// nothing was stored anywhere.
    pub async fn stage(&self, gated: &Gated, change: Change) -> Result<Staged, DomainError> {
        let declaration = &gated.declaration;
        let value = match change {
            Change::Set(value) => value,
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
        // takes it and hands back a reference unique to this write, and with
        // nothing bound the write is unavailable rather than stored in clear.
        // The store cannot join the row's transaction, so this runs before it;
        // a transaction that then fails releases the entry again.
        if declaration.has_secret_trait {
            let secret_ref = self
                .secrets
                .store_secret(&declaration.key, gated.tenant_id, &value)
                .await?;
            return Ok(Staged::Set {
                inline: None,
                secret_ref: Some(secret_ref),
            });
        }
        Ok(Staged::Set {
            inline: Some(value),
            secret_ref: None,
        })
        // @cpt-end:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-4
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-1
    }

    /// Release the entry a staged secret created, when its transaction did not
    /// commit. Logged, never failed: the refusal already stands.
    pub async fn discard(&self, gated: &Gated, staged: &Staged) {
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-7
        if let Staged::Set {
            secret_ref: Some(reference),
            ..
        } = staged
            && let Err(err) = self
                .secrets
                .delete_secret(&gated.declaration.key, gated.tenant_id, reference)
                .await
        {
            tracing::warn!(
                key = %gated.declaration.key,
                tenant = %gated.tenant_id,
                %err,
                "secret entry of a refused write not released; the reference is orphaned"
            );
        }
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-7
    }

    /// Commit a staged change inside the caller's transaction: the tag check,
    /// the row, and its audit record.
    ///
    /// # Errors
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
        let declaration = &gated.declaration;
        let scope = AccessScope::allow_all();
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-2
        // @cpt-begin:cpt-cf-settings-service-algo-value-writes-commit:p1:inst-vw-commit-3
        // Inside the caller's transaction, which spans this change alone. The
        // tag is compared here and the row written below in the same
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
            Staged::Set { inline, secret_ref } => {
                // @cpt-begin:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-6
                // @cpt-dod:cpt-cf-settings-service-dod-secret-values-reference-only:p1
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
                                .update(conn, &scope, row.id, inline, secret_ref, &actor.subject())
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
                    .delete(conn, &scope, declaration.id, gated.tenant_id)
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
                %err,
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
    ) {
        self.publisher
            .publish(ValueEvent::ChangeFailed {
                key: key.to_owned(),
                tenant_id,
                actor: actor.subject(),
                reason: reason.to_string(),
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
