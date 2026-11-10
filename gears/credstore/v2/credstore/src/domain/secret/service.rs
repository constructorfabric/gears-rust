// Updated: 2026-10-06 by Constructor Tech
//! Credential-store domain service (ADR-0006: immutable value versions).
//!
//! Orchestrates authorization, typed-secret validation, hierarchy resolution
//! and the immutable-value write/read/delete protocols. Every side effect on
//! the value store is either announced in the database before it happens
//! (a write intent, inserted before `plugin.put`) or recorded as a cleanup
//! debt in the same transaction that learned it is needed (older versions
//! after a rotation, an unreferenced version after a lost write, a whole key
//! after a delete), and executed by the request that recorded it once the
//! commit is confirmed. There is no background work: the leftovers of an
//! interrupted request (an unexecuted debt, an expired intent) are healed by
//! a later request that touches the same record or reference ("Heal on
//! access", DESIGN section 6.2).

use std::sync::Arc;
use std::time::{Duration, Instant};

use authz_resolver_sdk::PolicyEnforcer;
use credstore_sdk::{
    CredStorePluginClientV2, Credential, CredentialPatch, CredentialWrite, OwnerId, PatchField,
    PutOutcome, Secret, SecretRef, SecretValue, SecretWrite, SharingMode, StoreKey, TenantId,
    Validator, ValueVersion,
};
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use authz_resolver_sdk::pep::ResourceType;

use crate::domain::authz::{self, actions, scope_for};
use crate::domain::error::{DomainError, plugin_error_outcome};
use crate::domain::ports::audit::{AuditEvent, AuditOperation, AuditOutcome, AuditSink, NoopAudit};
use crate::domain::ports::metrics::{
    CredStoreMetricsPort, Dep, DepOp, Outcome, ReadOutcome, ReadRetryOutcome, VerifyOp,
    VerifyOutcome,
};
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::resolver::TenantDirectory;
use time::OffsetDateTime;

use crate::domain::secret::list_filter;
use crate::domain::secret::model::{
    CleanupDebt, CleanupTask, DeleteVerification, Fallback, IntentCommit, NewDeclaredSecret,
    NewSecret, PutPrecondition, SecretRow, SecretStatus, WriteAttempt, WritePrecondition,
    WriteVerification,
};
use crate::domain::secret::reduce::{self, Reduced};
use crate::domain::secret::repo::SecretRepo;
use crate::domain::secret::type_resolver::{ResolvedSecretType, SecretTypeResolver};
use crate::domain::secret::typing;

/// Page size of the create-time downward type check's keyset walk over the
/// tenants holding a reference with another type.
const DESCENDANT_CHECK_PAGE: u64 = 100;

/// Collection-read settings (`Service::list`, ADR-0005/ADR-0004), from
/// `ListCfg`: the page-size cap (`limit`/`$top`).
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct ListSettings {
    pub max_limit: u64,
}

/// Secret-write settings (`WriteCfg`): how long a write intent is protected
/// from heal.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct WriteSettings {
    pub intent_lease: Duration,
}

impl Default for WriteSettings {
    fn default() -> Self {
        Self {
            intent_lease: Duration::from_mins(5),
        }
    }
}

/// `CredStore` domain service — get / put / delete with walk-up, the
/// immutable-value write protocol, and `AuthZ`.
#[domain_model]
pub struct Service {
    repo: Arc<dyn SecretRepo>,
    dir: Arc<dyn TenantDirectory>,
    enforcer: PolicyEnforcer,
    plugins: Arc<dyn PluginSelector>,
    types: Arc<dyn SecretTypeResolver>,
    metrics: Arc<dyn CredStoreMetricsPort>,
    audit: Arc<dyn AuditSink>,
    list: ListSettings,
    write: WriteSettings,
}

/// What a write is about to do to a secret, known once the write is
/// authorized. Carried out of `put_once`/`patch_once` so the one final
/// outcome (after any `If-Match: *` retry) is audited exactly once.
#[domain_model]
struct AuditTarget {
    operation: AuditOperation,
    secret_type: String,
}

/// Selection-aware answer to the point read (`Service::get_item`, ADR-0004,
/// "Read actions follow the projection"): the resolved [`Credential`], its decrypted value when the
/// projection named `secret` and the winner had one to serve, that value's
/// own validator (the row it was actually served from — a concurrent switch
/// can make this momentarily different from `credential.validator`), and the
/// weak-`ETag` source the REST layer needs whenever the caller holds no own
/// row.
#[domain_model]
#[derive(Debug)]
pub struct CredentialItem {
    pub credential: Credential,
    pub value: Option<SecretValue>,
    /// The validator of the row `value` was served from; `Some` iff `value`
    /// is.
    pub value_validator: Option<Validator>,
    pub weak_validator_source: Option<(Uuid, i64)>,
}

impl Service {
    /// Creates a new [`Service`].
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "one dependency/settings value per collaborator the domain service wires \
                  together; a builder would only move the same seven names into a second type"
    )]
    pub fn new(
        repo: Arc<dyn SecretRepo>,
        dir: Arc<dyn TenantDirectory>,
        enforcer: PolicyEnforcer,
        plugins: Arc<dyn PluginSelector>,
        types: Arc<dyn SecretTypeResolver>,
        metrics: Arc<dyn CredStoreMetricsPort>,
        list: ListSettings,
    ) -> Self {
        Self {
            repo,
            dir,
            enforcer,
            plugins,
            types,
            metrics,
            audit: Arc::new(NoopAudit),
            list,
            write: WriteSettings::default(),
        }
    }

    /// Route secret read/write audit events to `audit` (default: discard).
    #[must_use]
    pub fn with_audit(mut self, audit: Arc<dyn AuditSink>) -> Self {
        self.audit = audit;
        self
    }

    /// Use `write` for the write-intent lease (default:
    /// [`WriteSettings::default`]).
    #[must_use]
    pub fn with_write_settings(mut self, write: WriteSettings) -> Self {
        self.write = write;
        self
    }

    /// Report one audited secret operation. Infallible by contract: the sink
    /// owns its timeout, logging and metric, and nothing here can alter the
    /// reply of the operation being audited.
    async fn audit_event(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        secret_type: &str,
        operation: AuditOperation,
        outcome: AuditOutcome,
    ) {
        self.audit
            .record(AuditEvent {
                subject_id: ctx.subject_id(),
                tenant_id: ctx.subject_tenant_id(),
                reference: key.as_ref().to_owned(),
                secret_type: secret_type.to_owned(),
                operation,
                outcome,
            })
            .await;
    }

    /// Audit the final result of a write whose target is known.
    async fn audit_write<T>(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        target: Option<AuditTarget>,
        result: &Result<T, DomainError>,
    ) {
        if let Some(t) = target {
            let outcome = if result.is_ok() {
                AuditOutcome::Success
            } else {
                AuditOutcome::Failure
            };
            self.audit_event(ctx, key, &t.secret_type, t.operation, outcome)
                .await;
        }
    }

    /// Evaluate the PDP scope for `action`, recording it as a timed dependency.
    ///
    /// The PDP call gates every get/put/delete and drives 503s, so it is timed
    /// like the plugin and tenant-resolver dependencies.
    async fn scope_for_timed(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &str,
    ) -> Result<AccessScope, DomainError> {
        let t0 = Instant::now();
        let result = scope_for(&self.enforcer, ctx, resource, action).await;
        // A PDP *denial* (`AccessDenied`) is a normal authorization decision —
        // the dependency answered — not a health signal; counting it as an
        // error would inflate the PDP error rate and risk false outage alerts.
        // Only an evaluation failure/outage is a dependency error. Mirrors the
        // domain/transport split the type resolver makes.
        let outcome = match &result {
            Ok(_) | Err(DomainError::AccessDenied { .. }) => Outcome::Success,
            Err(_) => Outcome::Error,
        };
        self.metrics.dependency(
            Dep::Pdp,
            DepOp::Evaluate,
            outcome,
            t0.elapsed().as_secs_f64(),
        );
        result
    }

    /// Resolve the type of an **existing** row (`secret_type_uuid` was
    /// validated when the row was written). An `UNKNOWN_SECRET_TYPE`
    /// violation here means the type was deregistered while rows persist —
    /// an operational inconsistency, not a caller error — so it is remapped
    /// onto a retryable 503; registry outages propagate as 503 already.
    async fn resolve_stored(&self, type_uuid: Uuid) -> Result<ResolvedSecretType, DomainError> {
        match self.types.resolve(type_uuid).await {
            Err(DomainError::TypeViolation { detail, .. }) => {
                tracing::warn!(
                    uuid = %type_uuid,
                    detail = %detail,
                    "stored secret type no longer resolves in the types-registry"
                );
                Err(DomainError::ServiceUnavailable {
                    detail: "secret type is not resolvable".to_owned(),
                    retry_after: None,
                    cause: None,
                })
            }
            other => other,
        }
    }

    /// Retrieve the credential **record** (ADR-0004). See [`Self::get_item`]
    /// for the resolution/reduction/projection this delegates to; the SDK
    /// contract never needs the winning row's identity
    /// [`Self::resolve_credential`] carries for the REST layer's weak
    /// `ETag`.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AccessDenied`] when the PDP denies `read`
    /// outright or its scope excludes the caller's own tenant (decided before
    /// any lookup, whether or not the reference exists). A record the scope's
    /// type/reference constraints exclude is the canonical miss, `Ok(None)`.
    pub async fn get_record(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<Credential>, DomainError> {
        Ok(self
            .get_item(ctx, key, None)
            .await?
            .map(|item| item.credential))
    }

    /// Retrieve the credential **record** (ADR-0004), walking up the tenant
    /// hierarchy to determine the effective row and reducing it with the
    /// caller's own row (ADR-0005 "Reducing a reference to one item").
    /// Never carries the value — see [`Self::get_secret`]. Thin wrapper over
    /// [`Self::get_item`] with no projection (the unselected point read:
    /// `read` alone, exactly as `CredStoreClientV2::get_record`).
    ///
    /// Returns the assembled [`Credential`] together with the winning row's
    /// `(id, version)` whenever the caller holds no own row — the REST
    /// layer's weak-`ETag` source (ADR-0004, D4): `Credential::own`
    /// is `None` in that case (the caller has no row of its own to hold a
    /// real validator for), so the weak hash has to come from here instead.
    /// Not part of the SDK contract, hence not `CredStoreClientV2::get_record` itself.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AccessDenied`] when the PDP denies `read`
    /// outright or its scope excludes the caller's own tenant (decided before
    /// any lookup, whether or not the reference exists). A record the scope's
    /// type/reference constraints exclude is the canonical miss, `Ok(None)`.
    pub async fn resolve_credential(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<(Credential, Option<(Uuid, i64)>)>, DomainError> {
        Ok(self
            .get_item(ctx, key, None)
            .await?
            .map(|item| (item.credential, item.weak_validator_source)))
    }

    /// The projection-aware point read (`GET /credstore/v1/credentials/{ref}`,
    /// ADR-0004, "Read actions follow the projection" — mirror of the write path's per-action authorization for
    /// reads): resolves the candidates and reduces them exactly as
    /// [`Self::resolve_credential`] and the collection read do, evaluates
    /// the action(s) `fields` requires once each on the effective concrete
    /// type — `read` when `fields` is `None` or names any administrative
    /// record field (`sharing`/`status`/`fallback`/`inheritance`/`version`/
    /// `updated_at`/`owner_id`), `read_secret` when `fields` names `secret`,
    /// both when both — and reads the value via
    /// `read_value_for_row` only when `secret` is selected and the
    /// winner has one. Reads authorize first, like writes: the PDP is
    /// evaluated BEFORE any row lookup, so a denial on either required
    /// action, or a scope that excludes the caller's own tenant, is
    /// `AccessDenied` (403) whether or not the record exists, and a PDP
    /// outage is a 503 also for a reference that does not exist. After the
    /// lookup the decisive row is chosen unclamped; the record the caller
    /// sees is the caller's own row, else the decisive one, and the scope's
    /// type/reference constraints are applied to it: a record they do not
    /// admit is the canonical miss (`Ok(None)`). A value is served only from
    /// a decisive row of the same type as that record (ADR-0010,
    /// Inheritance): otherwise there is no value, the inheritance status is
    /// `own`, the row never yields `SecretExpired` and nothing is read or
    /// audited. A
    /// `secret`-only projection (no administrative field alongside it — the
    /// shape [`Self::get_secret`] uses) against a winner with no value to
    /// serve is the canonical miss too, exactly as the withdrawn
    /// `GET …/secret` was; the same value-less winner, projected together
    /// with an administrative field, is returned as a record with no
    /// `secret` instead.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AccessDenied`] if the PDP denies a required
    /// action or the scope excludes the caller's tenant (before any lookup).
    /// Returns [`DomainError::NotFound`] or [`DomainError::ServiceUnavailable`]
    /// from the value read, exactly as [`Self::get_secret`] documents, when
    /// `secret` is selected and the winner has one.
    /// Returns [`DomainError::SecretExpired`] when `secret` is selected and the
    /// decisive record is an expired `active` row (own override or inherited
    /// `shared` record) — only after the caller passed `read_secret`; a caller
    /// without it gets the usual miss. The metadata projection never fails
    /// because of expiry: it reports status `expired`.
    #[allow(
        clippy::cognitive_complexity,
        reason = "resolve -> reduce -> authorize(s) -> assemble -> conditionally read is \
                  inherently branchy; kept as one function for readability of the flow, \
                  mirroring resolve_credential/put/patch"
    )]
    pub async fn get_item(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        fields: Option<&[String]>,
    ) -> Result<Option<CredentialItem>, DomainError> {
        let result = self.read_item(ctx, key, fields).await;
        // Heal on access: a point read of a reference also heals the failed
        // creates of that reference (expired intents whose record has no
        // row), best effort and after the answer is known.
        self.heal_failed_creates(TenantId(ctx.subject_tenant_id()), key)
            .await;
        result
    }

    /// The body of [`Self::get_item`], before the failed-create heal.
    async fn read_item(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        fields: Option<&[String]>,
    ) -> Result<Option<CredentialItem>, DomainError> {
        if let Some(f) = fields {
            list_filter::validate_select(f)?;
        }

        // The projection-to-action rule (ADR-0004, "Read actions follow the
        // projection"): `read` is required unless
        // this is a pure value request (`secret` named, no administrative
        // field alongside it) — the shape `get_secret` uses; `read_secret`
        // is required whenever `secret` is named. At least one is always
        // `true`.
        let need_value = list_filter::secret_selected(fields);
        let need_admin = list_filter::admin_field_selected(fields);
        let need_read = !need_value || need_admin;
        let need_read_secret = need_value;

        let req = TenantId(ctx.subject_tenant_id());
        let subject = OwnerId(ctx.subject_id());

        // Reads authorize first, like writes: evaluate whichever of
        // `read`/`read_secret` the projection needs - ONE PDP evaluation per
        // action on the base credential type, never one per type - gated on
        // the caller's own tenant, BEFORE any row lookup. A full denial or a
        // scope that excludes the caller's own tenant is `AccessDenied`
        // (403) whether or not the record exists, and is not a read miss; a
        // PDP outage propagates, also for a reference that does not exist.
        let mut required: Vec<&str> = Vec::with_capacity(2);
        if need_read {
            required.push(actions::READ);
        }
        if need_read_secret {
            required.push(actions::READ_SECRET);
        }
        let scope = self.authorize_actions(ctx, req, &required).await?;
        let clamp = authz::row_clamp(&scope, req.0);

        let chain = self.dir.ancestor_chain(ctx, req).await?;

        let candidates = self
            .repo
            .resolve_candidates(req, subject, key, &chain)
            .await?;

        // Choose the decisive row unclamped, then the record the caller sees
        // (own row, else the decisive one) - shared with the collection read.
        let Some(reduced) = reduce::reduce_reference(&candidates, req, subject, &chain) else {
            // Nothing resolves and the caller holds no row at all.
            if need_read_secret {
                self.metrics.read_outcome(ReadOutcome::Miss);
            }
            return Ok(None);
        };
        let Reduced {
            own,
            winner,
            effective,
            value_row,
            inheritance,
        } = reduced;
        let now = OffsetDateTime::now_utc();

        // The PDP's type/reference constraints must admit the record the
        // caller sees. A record they do not admit is answered exactly as a
        // missing one.
        if !clamp.admits(effective.secret_type_uuid, &effective.reference) {
            if need_read_secret {
                self.metrics.read_outcome(ReadOutcome::Miss);
            }
            return Ok(None);
        }
        let resolved = self.resolve_stored(effective.secret_type_uuid).await?;

        let own_row = own.map(|o| reduce::own_row_of(o, now));

        // `own` is `None` here only when `winner` is `Some` (we already
        // returned `Ok(None)` above when both were absent), so this is the
        // weak-`ETag` source whenever the caller holds no own row.
        let weak_validator_source = if own.is_none() {
            winner.map(|w| (w.id, w.version))
        } else {
            None
        };

        let credential = Credential {
            reference: key.clone(),
            secret_type: resolved.gts_id,
            sharing: effective.sharing,
            inheritance,
            expires_at: effective.expires_at,
            own: own_row,
        };

        // Read the value only when it was asked for and the decisive row
        // (of the effective record's type) actually has one. This
        // re-resolves the row through `resolve_for_get` — the same single
        // targeted query `get_secret`'s classic implementation issued — and
        // carries this read's retry-once protocol (ADR-0006 §6.2); the
        // re-resolved row must be the expected one (same id), else the read
        // is a miss, so a concurrent switch can never serve a row the
        // authorization and the type rule were not evaluated on.
        let secret = if need_read_secret {
            if let Some(row) = value_row {
                self.read_secret_audited(
                    ctx,
                    req,
                    subject,
                    key,
                    &chain,
                    &credential.secret_type,
                    row.id,
                )
                .await?
            } else {
                // Nothing resolves, or the decisive row is of another type
                // than the record the caller sees: no value, and nothing is
                // read, expired-checked or audited.
                self.metrics.read_outcome(ReadOutcome::Miss);
                None
            }
        } else {
            None
        };

        if need_read_secret && !need_read && secret.is_none() {
            // A pure value request (no administrative field rode along) that
            // resolved to a value-less winner is the canonical miss — the
            // same 404 the withdrawn `GET …/secret` gave, not a 200 with no
            // `value`.
            return Ok(None);
        }

        let value_validator = secret.as_ref().map(|s| s.validator);
        let value = secret.map(|s| s.secret);
        Ok(Some(CredentialItem {
            credential,
            value,
            value_validator,
            weak_validator_source,
        }))
    }

    /// The authorized value read of a point read: re-resolve the winning
    /// row, read its value, and report the result to the audit sink. A
    /// secret actually returned is audited as a success; an error after
    /// authorization that follows a resolved record as a failure. A miss
    /// (nothing, a value-less winner, or a re-read that finds the row gone or
    /// a row other than `expected_id`, the decisive row of the effective
    /// record's type the caller authorized - 404) discloses nothing and is
    /// not audited.
    #[allow(
        clippy::too_many_arguments,
        reason = "the resolve_for_get key, the audit type and the expected decisive row id"
    )]
    async fn read_secret_audited(
        &self,
        ctx: &SecurityContext,
        req: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        chain: &[Uuid],
        secret_type: &str,
        expected_id: Uuid,
    ) -> Result<Option<Secret>, DomainError> {
        let mut debts_of = None;
        let read = async {
            let value_row = self.repo.resolve_for_get(req, subject, key, chain).await?;
            // Re-resolution guard: the row served must be the very row the
            // authorization and the type rule were evaluated on. A type and a
            // reference are immutable, so the same id implies the same type;
            // anything else (a concurrent switch) is a miss, nothing audited.
            if value_row.as_ref().is_none_or(|row| row.id != expected_id) {
                self.metrics.read_outcome(ReadOutcome::Miss);
                return Ok(None);
            }
            if let Some(row) = value_row.as_ref().filter(|row| row.heal.debts) {
                debts_of = Some(row.store_key());
            }
            // Expiry applies to the secret: the decisive record is expired,
            // so its secret is never served and resolution does not continue
            // to an ancestor's value. The caller is already authorized, so
            // this is safe to disclose (and is audited as a failed read).
            if value_row
                .as_ref()
                .is_some_and(|row| row.is_expired(OffsetDateTime::now_utc()))
            {
                self.metrics.read_outcome(ReadOutcome::Expired);
                return Err(DomainError::SecretExpired);
            }
            match &value_row {
                Some(row) if row.value_version.is_some() => {
                    let plugin = self.plugins.resolve().await?;
                    self.read_value_for_row(
                        &plugin,
                        ctx,
                        req,
                        subject,
                        key,
                        chain,
                        row,
                        secret_type,
                    )
                    .await
                }
                _ => {
                    // No winning row, or a value-less (declared/suppressed)
                    // one: nothing to read, and `read_value_for_row` was
                    // never called, so the miss is not yet recorded.
                    self.metrics.read_outcome(ReadOutcome::Miss);
                    Ok(None)
                }
            }
        }
        .await;
        // Heal on access: the record has pending debts; execute them best
        // effort now that the read is done, so a debt never delays or alters
        // the reply.
        if let Some(record) = debts_of {
            self.heal_record_debts(&record).await;
        }
        let outcome = match &read {
            Ok(Some(_)) => Some(AuditOutcome::Success),
            // `NotFound`: the re-read found the row gone or another record -
            // nothing resolved, nothing disclosed, nothing audited.
            Ok(None) | Err(DomainError::NotFound) => None,
            Err(_) => Some(AuditOutcome::Failure),
        };
        if let Some(outcome) = outcome {
            self.audit_event(ctx, key, secret_type, AuditOperation::Read, outcome)
                .await;
        }
        read
    }

    /// Retrieve the resolved **value** (ADR-0004), walking up the tenant
    /// hierarchy. A winning record with no value (`declared`, including the
    /// suppression case) is the canonical miss. Thin wrapper over
    /// [`Self::get_item`], projected to exactly `reference`, `type`,
    /// `expires_at` and `secret` — the shape the SDK's `Secret` envelope
    /// wraps, and the same projection `CredStoreLocalClient::get_secret`
    /// sends over the in-process trait (ADR-0004, "Read actions follow the projection").
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AccessDenied`] if the PDP denies `read_secret`
    /// or the scope excludes the caller's tenant (a full denial is 403,
    /// before any lookup, whether or not the reference exists); constraints
    /// that exclude the record give `Ok(None)`.
    /// Returns [`DomainError::SecretExpired`] if the decisive record is an
    /// expired `active` row (the answer never falls through to an ancestor).
    /// Returns [`DomainError::NotFound`] if the reference resolves to nothing
    /// (including a re-read that finds a different generation) after one
    /// retry.
    /// Returns [`DomainError::ServiceUnavailable`] if the plugin still has no
    /// value for a row's current `value_version` on the retry — by protocol a
    /// live pointer always names bytes that were durably written first, so
    /// this is a backend inconsistency, not absence.
    pub async fn get_secret(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<Secret>, DomainError> {
        let fields = [
            "reference".to_owned(),
            "type".to_owned(),
            "expires_at".to_owned(),
            "secret".to_owned(),
        ];
        let Some(item) = self.get_item(ctx, key, Some(&fields)).await? else {
            return Ok(None);
        };
        // `get_item` already applied the value-only miss rule above, so
        // reaching here with either absent is structurally unreachable
        // (`value_validator` is `Some` iff `value` is); guard rather than
        // unwrap.
        let (Some(value), Some(validator)) = (item.value, item.value_validator) else {
            return Ok(None);
        };
        Ok(Some(Secret {
            reference: key.clone(),
            secret_type: item.credential.secret_type,
            expires_at: item.credential.expires_at,
            secret: value,
            validator,
        }))
    }

    /// Read the store value named by `row.value_version` (ADR-0006, DESIGN
    /// section 4.6): `plugin.get(key, value_version)`; found -> reply; not
    /// found (a concurrent write switched the pointer and destroyed the old
    /// version) -> re-read the row once and, if `value_version` changed, `get`
    /// again; a second miss is 503. The shared tail of [`Self::get_secret`]
    /// and, per reduced winner, the collection read's `secret` selection
    /// (ADR-0004).
    ///
    /// Callers MUST have already checked `row.value_version.is_some()` (a
    /// `declared`/suppressed row has nothing to read, and never causes a store
    /// call), authorized `resolved_gts_id` (the PDP decision taken before any
    /// lookup and its constraints applied to the record the caller sees) and
    /// checked that `row` has the type of that record - this method performs
    /// neither the resolution nor the PDP gate nor the type rule, only the
    /// read.
    ///
    /// Returns `Ok(None)` (with the corresponding metric already recorded) on
    /// a legitimate miss: the retry finds the reference gone or switched to a
    /// value-less generation.
    #[allow(
        clippy::too_many_arguments,
        reason = "carries every field the retry protocol needs: identity, the row being read, \
                  and the type name for the returned Secret"
    )]
    async fn read_value_for_row(
        &self,
        plugin: &Arc<dyn CredStorePluginClientV2>,
        ctx: &SecurityContext,
        req: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        chain: &[Uuid],
        row: &SecretRow,
        resolved_gts_id: &str,
    ) -> Result<Option<Secret>, DomainError> {
        if row.value_version.is_none() {
            // Defensive: callers already filter out a value-less row before
            // calling this (nothing to authorize or read for it).
            return Ok(None);
        }

        let depth = chain
            .iter()
            .position(|c| *c == row.tenant_id.0)
            .unwrap_or(chain.len());
        self.metrics.walkup_depth(depth as u64);

        let Some((value, served_row)) = self
            .fetch_with_retry(plugin, ctx, req, subject, key, chain, row)
            .await?
        else {
            self.metrics.read_outcome(ReadOutcome::Miss);
            return Ok(None);
        };

        let is_inherited = served_row.tenant_id != req;
        self.metrics.read_outcome(if is_inherited {
            ReadOutcome::HitInherited
        } else {
            ReadOutcome::HitOwn
        });

        Ok(Some(Secret {
            reference: key.clone(),
            secret_type: resolved_gts_id.to_owned(),
            expires_at: served_row.expires_at,
            secret: value,
            validator: Validator {
                id: served_row.id,
                version: served_row.version,
            },
        }))
    }

    /// Read the store value for `row`'s `value_version`, re-reading the row
    /// once if the plugin reports it gone - the read landed a moment before a
    /// concurrent write switched the pointer and destroyed the old version.
    /// If the re-read row carries a *changed* `value_version`, `get` again; a
    /// second consecutive miss is a store inconsistency, not absence - by
    /// protocol the `put` always precedes the CAS that names it, and a
    /// version is destroyed only after the pointer left it - mapped onto
    /// [`DomainError::ServiceUnavailable`] (retryable), never a stale or empty
    /// value. A re-read row still naming the version that was just reported
    /// gone is permanent instead: [`DomainError::Internal`] at once, logged
    /// at error level, with no second `get` - no concurrent switch explains
    /// it and a retry cannot bring the version back. A plugin `Internal`
    /// error (a version that exists but can never be read) is likewise
    /// surfaced at once. A row found `declared` on the re-read
    /// (suppressed/removed concurrently) is a legitimate miss instead -
    /// `Ok(None)`.
    ///
    /// The re-read re-runs the *same* `resolve_for_get` call (same requesting
    /// tenant/subject/key/chain) rather than looking up the row by id. If it
    /// comes back with a **different** row (a different generation entirely -
    /// the original was deleted and a different secret now resolves), this
    /// fails closed as `NotFound` instead of serving a value the type/PDP
    /// check above never authorized. Since a row's type is immutable for its
    /// lifetime, a matching id means the caller's earlier type resolution and
    /// PDP scope still apply to the re-read.
    #[allow(
        clippy::too_many_arguments,
        reason = "carries the full resolve_for_get key plus the row being retried"
    )]
    async fn fetch_with_retry(
        &self,
        plugin: &Arc<dyn CredStorePluginClientV2>,
        ctx: &SecurityContext,
        req: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        chain: &[Uuid],
        row: &SecretRow,
    ) -> Result<Option<(SecretValue, SecretRow)>, DomainError> {
        let Some(version) = row.value_version.as_ref() else {
            return Ok(None);
        };
        match self
            .plugin_get_timed(plugin, ctx, &row.store_key(), version)
            .await
        {
            Ok(Some(v)) => Ok(Some((v, row.clone()))),
            Err(e) => Err(e),
            Ok(None) => {
                let fresh = self.repo.resolve_for_get(req, subject, key, chain).await?;
                let Some(fresh) = fresh else {
                    // The reference itself is gone (deleted, not merely
                    // rotated) - a genuine miss.
                    return Err(DomainError::NotFound);
                };
                if fresh.id != row.id {
                    // A different generation now resolves; the earlier
                    // type/PDP authorization does not necessarily apply.
                    return Err(DomainError::NotFound);
                }
                let Some(fresh_version) = fresh.value_version.as_ref() else {
                    // The row was suppressed/declared concurrently - a
                    // legitimate miss (ADR-0004), not a store inconsistency.
                    return Ok(None);
                };
                if fresh_version == version {
                    // The pointer did not move, yet its version is gone: no
                    // concurrent switch explains it, and a retry cannot bring
                    // it back - permanent, not a transient 503.
                    return Err(Self::version_lost());
                }
                match self
                    .plugin_get_timed(plugin, ctx, &fresh.store_key(), fresh_version)
                    .await
                {
                    Ok(Some(v)) => {
                        self.metrics.read_retry(ReadRetryOutcome::Recovered);
                        Ok(Some((v, fresh)))
                    }
                    Ok(None) => {
                        self.metrics.read_retry(ReadRetryOutcome::SecondMiss);
                        Err(Self::version_missing())
                    }
                    Err(e) => Err(e),
                }
            }
        }
    }

    /// The permanent outcome for a version that is gone although the row's
    /// pointer did not move: logs the fact (no secret, no reference) at error
    /// level and answers an internal error.
    fn version_lost() -> DomainError {
        tracing::error!(
            "credstore: stored secret version is gone although its pointer did not move"
        );
        DomainError::Internal {
            diagnostic: "stored secret version is gone although its pointer did not move"
                .to_owned(),
            cause: None,
        }
    }

    fn version_missing() -> DomainError {
        DomainError::ServiceUnavailable {
            detail: "value version missing in the store; retry".to_owned(),
            retry_after: None,
            cause: None,
        }
    }

    /// Timed `plugin.get`, recording the dependency metric.
    async fn plugin_get_timed(
        &self,
        plugin: &Arc<dyn CredStorePluginClientV2>,
        ctx: &SecurityContext,
        key: &StoreKey,
        version: &ValueVersion,
    ) -> Result<Option<SecretValue>, DomainError> {
        let t0 = Instant::now();
        let raw = plugin.get(ctx, key, version).await;
        let secs = t0.elapsed().as_secs_f64();
        let outcome = match &raw {
            Ok(Some(_)) => Outcome::Success,
            Ok(None) => Outcome::NotFound,
            Err(e) => plugin_error_outcome(e),
        };
        let result = raw.map_err(DomainError::from);
        self.metrics
            .dependency(Dep::Plugin, DepOp::PluginGet, outcome, secs);
        result
    }

    /// Create or replace the whole credential — record and, unless `value`
    /// is an explicit `None`, its value together (ADR-0007, "PUT: whole replace,
    /// secret tri-state").
    ///
    /// A write targets the row of its own sharing class — `private` →
    /// `(tenant, ref, owner)`, `tenant`/`shared` → `(tenant, ref)` — so a private
    /// and a tenant/shared secret coexist under one reference (per design §4.1);
    /// a write of one class never affects the other.
    ///
    /// `value: Some(_)` follows ADR-0006's single protocol regardless of
    /// precondition: announce the attempt (a write intent, inserted before any
    /// store call; failure answers 503 with nothing written), `plugin.put` the
    /// bytes under the record's key, then ONE transaction retires the intent
    /// and switches the row (`insert_active` on create, `switch_value` on
    /// overwrite, a compare-and-set on the version read first). Every store
    /// cleanup the outcome implies (older versions after a rotation, this
    /// attempt's version after a lost write) is recorded as a debt in that
    /// same transaction and executed by this request after the confirmed
    /// commit. The plugin is resolved (fail-fast) only on this path. A lost
    /// compare-and-set under an `If-Match: *` precondition re-reads the row
    /// and retries once from the start, as a new attempt, before returning
    /// `VersionConflict`. Debts and expired intents the row read reported are
    /// healed on the way (DESIGN section 6.2, "Heal on access").
    /// A create-only `PUT` over an expired own row is a conflict like any
    /// other existing row (the expired record is visible: renew or delete it).
    /// `value: None` never touches the plugin at all on create
    /// (`insert_declared`, a plain `INSERT`) or on replace of an
    /// already-`declared` row (metadata-only, `update_metadata`, itself
    /// skipped when nothing would change — ADR-0007 "No-op rule, and why a secret
    /// write always bumps"); on replace of an `active` row it
    /// removes the value in the same one compare-and-set `PATCH {"secret":
    /// null}` uses (`remove_value`), which records the destroy debts of the
    /// old version in the same transaction (the plugin is resolved only to
    /// learn whether it supports `destroy`).
    ///
    /// `write` is always required; `write_secret` is additionally required
    /// when `value` is `Some(_)`, or when a `None` removes an existing value
    /// (replace of an `active` row) — never when `None` creates or replaces
    /// an already value-less row.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AccessDenied`] if the caller is not permitted the
    /// needed actions on the named type (or, naming none, on any type) —
    /// decided before any row is read, so the same whether or not the record
    /// exists.
    /// Returns [`DomainError::Conflict`] if [`PutPrecondition::CreateOnly`] and
    /// the caller's own tenant already holds a row (of any status, of any
    /// type) under the reference.
    /// Returns [`DomainError::VersionConflict`] if a replace precondition
    /// names no own row, or one of a type the caller may not write
    /// (indistinguishable), or a version/generation mismatch.
    /// Returns [`DomainError::TypeViolation`] on a trait violation, an
    /// unresolvable type, a differing type on replace (`TYPE_IMMUTABLE`), a
    /// missing type on create (`TYPE_REQUIRED`), or a create over a reference
    /// that currently resolves, for the creating caller (its tenant, owner
    /// and ancestor chain), to a record of a different type
    /// (`TYPE_MISMATCH_WITH_INHERITED`): an ancestor's `shared` record — or
    /// over a reference a descendant tenant of the creator already holds, in
    /// any status, as a non-private record with a different type
    /// (`TYPE_MISMATCH_WITH_DESCENDANT`). Both checks are skipped when
    /// creating a `private` record, and private records are never compared.
    pub async fn put(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        write: CredentialWrite,
        precondition: PutPrecondition,
    ) -> Result<PutOutcome, DomainError> {
        let mut audit = None;
        let result = self
            .put_attempts(ctx, key, &write, &precondition, &mut audit)
            .await;
        self.audit_write(ctx, key, audit, &result).await;
        result
    }

    /// The `put` attempt loop; `audit` is set once the write is authorized
    /// and known to create, replace or remove a secret.
    async fn put_attempts(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        write: &CredentialWrite,
        precondition: &PutPrecondition,
        audit: &mut Option<AuditTarget>,
    ) -> Result<PutOutcome, DomainError> {
        // `If-Match: *` is last-writer-wins: a lost pointer switch re-reads
        // the row and retries once from step 1 (the value is re-copied for
        // the second `plugin.put`, as `SecretValue` is not `Clone`).
        let attempts = if matches!(precondition, PutPrecondition::Exists) {
            2
        } else {
            1
        };
        for _ in 0..attempts {
            if let Some(outcome) = self.put_once(ctx, key, write, precondition, audit).await? {
                return Ok(outcome);
            }
        }
        Err(DomainError::VersionConflict)
    }

    /// One pass of steps 1-5 of the write protocol. `Ok(None)` is a definite
    /// loss of the pointer-switch compare-and-set on an existing row.
    #[allow(
        clippy::cognitive_complexity,
        clippy::too_many_lines,
        reason = "write orchestration (validate -> scope -> resolve -> announce -> put -> commit) \
                  is inherently branchy; kept as one function for readability of the flow"
    )]
    async fn put_once(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        write: &CredentialWrite,
        precondition: &PutPrecondition,
        audit: &mut Option<AuditTarget>,
    ) -> Result<Option<PutOutcome>, DomainError> {
        let secret_type = write.secret_type.as_ref();
        let sharing = write.sharing;
        let fallback = Fallback::from(write.fallback);
        let expires_at = write.expires_at;
        let value = match &write.secret {
            SecretWrite::Set(v) => Some(v),
            SecretWrite::Null => None,
        };
        let tenant = TenantId(ctx.subject_tenant_id());
        let owner = OwnerId(ctx.subject_id());

        let create_only = matches!(precondition, PutPrecondition::CreateOnly);

        // Request-only checks first: they depend on the request alone, never
        // on whether a row exists. A create must name its type (ADR-0004 — no
        // default-to-generic); a named type must be registered.
        let named = match secret_type {
            Some(t) => {
                let type_uuid = t.to_uuid();
                Some((type_uuid, self.types.resolve(type_uuid).await?))
            }
            None if create_only => {
                return Err(DomainError::InvalidRequest {
                    field: "type",
                    reason: typing::reasons::TYPE_REQUIRED,
                    detail: "type is required to create a credential".to_owned(),
                });
            }
            None => None,
        };
        if create_only && let Some((_, resolved)) = &named {
            typing::validate_metadata(&resolved.gts_id, &resolved.traits, sharing, expires_at)?;
            if let Some(v) = value {
                typing::validate_value(&resolved.gts_id, &resolved.traits, v)?;
            }
        }

        // Authorize BEFORE any row lookup (anti-enumeration): `write` (plus
        // `write_secret` when a value is named), one PDP evaluation per
        // action regardless of how many credential types exist. A create
        // evaluates the requested concrete type; a replace evaluates the
        // base type and gets the type predicate back in the scope. A caller
        // the PDP refuses is answered 403 here, whether or not the record
        // exists.
        let required: &[&str] = if value.is_some() {
            &[actions::WRITE, actions::WRITE_SECRET]
        } else {
            &[actions::WRITE]
        };
        let mut scope = if create_only {
            let Some((requested_uuid, resolved)) = &named else {
                return Err(DomainError::InvalidRequest {
                    field: "type",
                    reason: typing::reasons::TYPE_REQUIRED,
                    detail: "type is required to create a credential".to_owned(),
                });
            };
            let create_scope = self
                .authorize_on(
                    ctx,
                    tenant,
                    &authz::credential_type_resource(&resolved.gts_id),
                    required,
                )
                .await?;
            // No row exists yet: the PDP's row predicates are evaluated in
            // memory against the would-be row (requested type + reference).
            // Refused exactly like a PDP denial, before any lookup.
            if !authz::row_clamp(&create_scope, tenant.0).admits(*requested_uuid, key.as_ref()) {
                return Err(DomainError::AccessDenied { cause: None });
            }
            create_scope
        } else {
            self.authorize_actions(ctx, tenant, required).await?
        };

        // Look up the caller's own row of this sharing class (own-tenant,
        // keyed by tenant+owner+key+sharing, of either resting status — a
        // `declared` row still "holds" the reference, ADR-0004). A create
        // looks it up unscoped: the unique key does not include the type, so
        // every collision answers the same 409. A replace looks it up WITH
        // the PDP scope, so a row of a type the caller may not write is not
        // found.
        let found = if create_only {
            self.repo
                .find_for_write(&AccessScope::allow_all(), tenant, owner, key, sharing)
                .await?
        } else {
            self.repo
                .find_for_write(&scope, tenant, owner, key, sharing)
                .await?
        };
        // An expired own row still holds the reference: a create-only `PUT`
        // over it is a plain conflict (409) — renew or delete it instead.
        let existing = found;

        if create_only && existing.is_some() {
            // The name is taken: a create cannot succeed whatever the row
            // holds, and the unique key does not include the type, so every
            // collision answers the same 409 — a row of a type the caller may
            // not write is not told apart from one it may.
            if let (Some((_, resolved)), true) = (&named, value.is_some()) {
                *audit = Some(AuditTarget {
                    operation: AuditOperation::Create,
                    secret_type: resolved.gts_id.clone(),
                });
            }
            return Err(DomainError::Conflict);
        }

        if !create_only {
            // Replace: a row that is absent, or of a type the PDP scope
            // excludes, is answered exactly as an absent target.
            let Some(existing) = existing else {
                return Err(DomainError::VersionConflict);
            };
            let resolved = self.resolve_stored(existing.secret_type_uuid).await?;
            // `write_secret` is also needed when a `null` removes a value
            // this row already holds (ADR-0007); never when `null`
            // replaces an already value-less row. The caller already holds
            // `write` on this type, so the row's state is its to learn: a
            // refusal here is a plain 403.
            if value.is_none() && existing.status == SecretStatus::Active {
                let secret_scope = self
                    .authorize_actions(ctx, tenant, &[actions::WRITE_SECRET])
                    .await?;
                if !authz::row_clamp(&secret_scope, tenant.0)
                    .admits(existing.secret_type_uuid, &existing.reference)
                {
                    return Err(DomainError::AccessDenied { cause: None });
                }
                scope = authz::intersect_scopes(&scope, &secret_scope);
            }
            if value.is_some() {
                *audit = Some(AuditTarget {
                    operation: AuditOperation::Replace,
                    secret_type: resolved.gts_id.clone(),
                });
            } else if existing.status == SecretStatus::Active {
                *audit = Some(AuditTarget {
                    operation: AuditOperation::Remove,
                    secret_type: resolved.gts_id.clone(),
                });
            }

            if let Some((requested_uuid, requested)) = &named
                && *requested_uuid != existing.secret_type_uuid
            {
                return Err(DomainError::TypeViolation {
                    field: "type",
                    reason: typing::reasons::TYPE_IMMUTABLE,
                    detail: format!(
                        "credential is of type '{}'; changing it to '{}' is not supported",
                        resolved.gts_id, requested.gts_id
                    ),
                });
            }
            typing::validate_metadata(&resolved.gts_id, &resolved.traits, sharing, expires_at)?;
            if let Some(v) = value {
                typing::validate_value(&resolved.gts_id, &resolved.traits, v)?;
            }
            let expected_version = Self::precheck_put_version(precondition, &existing)?;
            // Heal on access: execute the record's pending debts, best effort.
            if existing.heal.debts {
                self.heal_record_debts(&existing.store_key()).await;
            }

            let validator = match value {
                Some(v) => {
                    let plugin = self.plugins.resolve().await?;
                    let Some(validator) = self
                        .overwrite_existing(
                            ctx, &plugin, &scope, &existing, sharing, fallback, expires_at, v,
                        )
                        .await?
                    else {
                        return Ok(None);
                    };
                    validator
                }
                None if existing.status == SecretStatus::Active => {
                    // Replace of an active row with an explicit `null`:
                    // remove the value in the same one compare-and-set
                    // `PATCH {"secret": null}` uses; the destroy of the
                    // version it left behind is recorded in that same
                    // transaction and executed after the commit.
                    let plugin = self.plugins.resolve().await?;
                    let (row, debts) = self
                        .repo
                        .remove_value(
                            &scope,
                            existing.id,
                            expected_version,
                            sharing,
                            fallback,
                            expires_at,
                            plugin.destroyer().is_some(),
                        )
                        .await?
                        .ok_or(DomainError::VersionConflict)?;
                    self.count_recorded(&debts);
                    self.execute_debts(&debts).await;
                    Validator {
                        id: row.id,
                        version: row.version,
                    }
                }
                None => {
                    // Replace of an already-`declared` row with an explicit
                    // `null`: metadata-only, and a no-op (204, unchanged
                    // ETag, no bump) when nothing about the metadata would
                    // change either — the same rule `PATCH`'s metadata-only
                    // no-op follows (ADR-0007, "No-op rule, and why a secret
                    // write always bumps").
                    if sharing == existing.sharing
                        && fallback == existing.fallback
                        && expires_at == existing.expires_at
                    {
                        Validator {
                            id: existing.id,
                            version: existing.version,
                        }
                    } else {
                        let row = self
                            .repo
                            .update_metadata(
                                &scope,
                                existing.id,
                                expected_version,
                                sharing,
                                fallback,
                                expires_at,
                            )
                            .await?
                            .ok_or(DomainError::VersionConflict)?;
                        Validator {
                            id: row.id,
                            version: row.version,
                        }
                    }
                }
            };
            return Ok(Some(PutOutcome {
                created: false,
                validator,
            }));
        }

        // Create: `named` is the requested type (checked above) and the
        // PDP evaluation on it above allowed it.
        let Some((type_uuid, resolved)) = named else {
            return Err(DomainError::InvalidRequest {
                field: "type",
                reason: typing::reasons::TYPE_REQUIRED,
                detail: "type is required to create a credential".to_owned(),
            });
        };
        if value.is_some() {
            *audit = Some(AuditTarget {
                operation: AuditOperation::Create,
                secret_type: resolved.gts_id.clone(),
            });
        }

        // Type consistency (`fr-override-type-consistency`) binds non-private
        // records only: a private record may carry any type, so it is
        // neither checked nor counted; that is safe because a value is only
        // ever served under a record of its own type (read rule, ADR-0010).
        if sharing != SharingMode::Private {
            // Upward: if the reference currently resolves, among non-private
            // records (the nearest ancestor's `shared` one), to a record of a
            // different type, creating here would silently diverge from what a
            // value read already serves.
            let chain = self.dir.ancestor_chain(ctx, tenant).await?;
            if let Some(inherited) = self.repo.resolve_non_private(tenant, key, &chain).await?
                && inherited.secret_type_uuid != type_uuid
            {
                let inherited_resolved = self.resolve_stored(inherited.secret_type_uuid).await?;
                return Err(DomainError::TypeViolation {
                    field: "type",
                    reason: typing::reasons::TYPE_MISMATCH_WITH_INHERITED,
                    detail: format!(
                        "reference currently resolves to an inherited credential of type '{}'; \
                         '{}' would diverge from it",
                        inherited_resolved.gts_id, resolved.gts_id
                    ),
                });
            }

            // Downward: a descendant of the creator already holding the
            // reference (non-private) with another type would diverge from the
            // new record the same way.
            self.ensure_no_descendant_type_mismatch(ctx, tenant, key, type_uuid)
                .await?;
        }

        let validator = if let Some(v) = value {
            let plugin = self.plugins.resolve().await?;
            self.create_new(
                ctx,
                &plugin,
                NewRecordParams {
                    tenant,
                    owner,
                    key,
                    sharing,
                    fallback,
                    secret_type_uuid: type_uuid,
                    expires_at,
                },
                v,
                &scope,
            )
            .await?
        } else {
            // Explicit `null` on create: insert the row `declared` directly —
            // no `value_version`, no backend call (ADR-0007, "PUT: whole replace, secret tri-state").
            let id = Uuid::new_v4();
            let new = NewDeclaredSecret {
                id,
                tenant_id: tenant,
                reference: key.clone(),
                sharing,
                owner_id: owner,
                secret_type_uuid: type_uuid,
                expires_at,
                fallback,
            };
            self.repo.insert_declared(&scope, &new).await?;
            Validator { id, version: 1 }
        };
        Ok(Some(PutOutcome {
            created: true,
            validator,
        }))
    }

    /// Downward half of the create-time type-consistency check
    /// (`fr-override-type-consistency`): walks, page by page, the distinct
    /// tenants holding `key` with another type outside the creator's tenant
    /// (any status and owner, non-private rows only) and stops at the first one the creator
    /// is an ancestor of, barriers ignored. Cost follows the number of rows of
    /// that reference with another type, usually none, never the subtree size.
    /// The wire detail names neither the tenant nor the type.
    async fn ensure_no_descendant_type_mismatch(
        &self,
        ctx: &SecurityContext,
        tenant: TenantId,
        key: &SecretRef,
        type_uuid: Uuid,
    ) -> Result<(), DomainError> {
        let mut after = None;
        loop {
            let page = self
                .repo
                .list_tenants_with_other_type(key, type_uuid, tenant, after, DESCENDANT_CHECK_PAGE)
                .await?;
            for candidate in &page {
                if self
                    .dir
                    .is_ancestor(ctx, tenant, TenantId(*candidate))
                    .await?
                {
                    return Err(DomainError::TypeViolation {
                        field: "type",
                        reason: typing::reasons::TYPE_MISMATCH_WITH_DESCENDANT,
                        detail: "a descendant tenant already holds this reference with \
                                 another credential type"
                            .to_owned(),
                    });
                }
            }
            match page.last() {
                Some(last) if page.len() as u64 >= DESCENDANT_CHECK_PAGE => after = Some(*last),
                _ => return Ok(()),
            }
        }
    }

    /// Apply a partial change to the record, the value, or both (ADR-0007, "PATCH: merge, never
    /// creates"; RFC 7396 JSON Merge Patch semantics). Never creates.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AccessDenied`] if the caller is not permitted the
    /// needed actions on the named type (or, naming none, on any type).
    /// Returns [`DomainError::NotFound`] if the caller holds no own record
    /// under the reference, or one of a type it may not write.
    /// Returns [`DomainError::InvalidRequest`] (`EMPTY_PATCH`) if the patch
    /// touches nothing at all.
    /// Returns [`DomainError::TypeViolation`] (`TYPE_IMMUTABLE`) if
    /// `secret_type` is present and differs from the stored type, or another
    /// trait violation.
    /// Returns [`DomainError::UnsupportedTransition`] if `sharing` would move
    /// the record between the private and tenant/shared key classes.
    /// Returns [`DomainError::VersionConflict`] on a failed precondition.
    /// Returns [`DomainError::VersionConflict`] on a failed precondition, or
    /// when the pointer switch loses its compare-and-set (under `If-Match: *`
    /// only after one retry from the start).
    pub async fn patch(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        patch: CredentialPatch,
        precondition: WritePrecondition,
    ) -> Result<Validator, DomainError> {
        let mut audit = None;
        let result = self
            .patch_attempts(ctx, key, &patch, &precondition, &mut audit)
            .await;
        self.audit_write(ctx, key, audit, &result).await;
        result
    }

    /// The `patch` attempt loop; `audit` is set once the patch is authorized
    /// and known to set or remove a secret.
    async fn patch_attempts(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        patch: &CredentialPatch,
        precondition: &WritePrecondition,
        audit: &mut Option<AuditTarget>,
    ) -> Result<Validator, DomainError> {
        // `If-Match: *` is last-writer-wins: a lost pointer switch re-reads
        // the row and retries once from step 1.
        let attempts = if matches!(precondition, WritePrecondition::Exists) {
            2
        } else {
            1
        };
        for _ in 0..attempts {
            if let Some(validator) = self
                .patch_once(ctx, key, patch, precondition, audit)
                .await?
            {
                return Ok(validator);
            }
        }
        Err(DomainError::VersionConflict)
    }

    /// One pass of a `PATCH`. `Ok(None)` is a definite loss of the
    /// pointer-switch compare-and-set.
    #[allow(
        clippy::cognitive_complexity,
        clippy::too_many_lines,
        reason = "body-derived authorization and RFC 7396 merge semantics are inherently \
                  branchy; kept as one function for readability of the flow"
    )]
    async fn patch_once(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        patch: &CredentialPatch,
        precondition: &WritePrecondition,
        audit: &mut Option<AuditTarget>,
    ) -> Result<Option<Validator>, DomainError> {
        let tenant = TenantId(ctx.subject_tenant_id());
        let owner = OwnerId(ctx.subject_id());

        // Request-only check: independent of whether a row exists.
        if patch.is_empty() {
            return Err(DomainError::InvalidRequest {
                field: "patch",
                reason: typing::reasons::EMPTY_PATCH,
                detail: "a merge patch must touch at least one field".to_owned(),
            });
        }

        let metadata_present = patch.sharing.is_some()
            || patch.fallback.is_some()
            || !patch.expires_at.is_absent()
            || patch.secret_type.is_some();
        let value_present = !patch.secret.is_absent();

        // Request-only check: a named type must be registered.
        if let Some(t) = patch.secret_type.as_ref() {
            self.types.resolve(t.to_uuid()).await?;
        }

        // Authorize BEFORE any row lookup (anti-enumeration): both required
        // actions must allow - one PDP evaluation per action on the base
        // credential type, whatever the number of types. A caller the PDP
        // refuses is answered 403 here, whether or not the record exists.
        let mut required: Vec<&str> = Vec::with_capacity(2);
        if metadata_present {
            required.push(actions::WRITE);
        }
        if value_present {
            required.push(actions::WRITE_SECRET);
        }
        let scope = self.authorize_actions(ctx, tenant, &required).await?;

        // Own row required, looked up WITH the PDP scope (tenant and type
        // predicates in SQL). A missing row and a row of a type the scope
        // excludes are the same 404.
        let existing = self
            .repo
            .find_own(&scope, tenant, owner, key)
            .await?
            .ok_or(DomainError::NotFound)?;
        let resolved = self.resolve_stored(existing.secret_type_uuid).await?;
        match &patch.secret {
            PatchField::Set(_) => {
                *audit = Some(AuditTarget {
                    operation: AuditOperation::Replace,
                    secret_type: resolved.gts_id.clone(),
                });
            }
            PatchField::Null if existing.status == SecretStatus::Active => {
                *audit = Some(AuditTarget {
                    operation: AuditOperation::Remove,
                    secret_type: resolved.gts_id.clone(),
                });
            }
            _ => {}
        }

        if let Some(requested) = patch.secret_type.as_ref()
            && requested.to_uuid() != existing.secret_type_uuid
        {
            return Err(DomainError::TypeViolation {
                field: "type",
                reason: typing::reasons::TYPE_IMMUTABLE,
                detail: format!("credential type is immutable; cannot change it to '{requested}'"),
            });
        }
        if let Some(new_sharing) = patch.sharing
            && (existing.sharing == SharingMode::Private) != (new_sharing == SharingMode::Private)
        {
            return Err(DomainError::UnsupportedTransition {
                detail: "cannot move between private and tenant/shared".into(),
            });
        }
        let expected_version = Self::precheck_version(Some(precondition), &existing)?;
        // Heal on access: execute the record's pending debts, best effort.
        if existing.heal.debts {
            self.heal_record_debts(&existing.store_key()).await;
        }

        let merged_sharing = patch.sharing.unwrap_or(existing.sharing);
        let merged_fallback = patch.fallback.map_or(existing.fallback, Fallback::from);
        let merged_expires_at = match patch.expires_at {
            PatchField::Absent => existing.expires_at,
            PatchField::Null => None,
            PatchField::Set(at) => Some(at),
        };

        if metadata_present {
            typing::validate_metadata(
                &resolved.gts_id,
                &resolved.traits,
                merged_sharing,
                merged_expires_at,
            )?;
        }

        match &patch.secret {
            PatchField::Absent => {
                if merged_sharing == existing.sharing
                    && merged_fallback == existing.fallback
                    && merged_expires_at == existing.expires_at
                {
                    // No-op: metadata unchanged, no `value` key — return the
                    // current validator without bumping anything.
                    return Ok(Some(Validator {
                        id: existing.id,
                        version: existing.version,
                    }));
                }
                let row = self
                    .repo
                    .update_metadata(
                        &scope,
                        existing.id,
                        expected_version,
                        merged_sharing,
                        merged_fallback,
                        merged_expires_at,
                    )
                    .await?
                    .ok_or(DomainError::VersionConflict)?;
                Ok(Some(Validator {
                    id: row.id,
                    version: row.version,
                }))
            }
            PatchField::Set(new_value) => {
                typing::validate_value(&resolved.gts_id, &resolved.traits, new_value)?;
                self.overwrite_existing(
                    ctx,
                    &self.plugins.resolve().await?,
                    &scope,
                    &existing,
                    merged_sharing,
                    merged_fallback,
                    merged_expires_at,
                    new_value,
                )
                .await
            }
            PatchField::Null => {
                let plugin = self.plugins.resolve().await?;
                let (row, debts) = self
                    .repo
                    .remove_value(
                        &scope,
                        existing.id,
                        expected_version,
                        merged_sharing,
                        merged_fallback,
                        merged_expires_at,
                        plugin.destroyer().is_some(),
                    )
                    .await?
                    .ok_or(DomainError::VersionConflict)?;
                self.count_recorded(&debts);
                self.execute_debts(&debts).await;
                Ok(Some(Validator {
                    id: row.id,
                    version: row.version,
                }))
            }
        }
    }

    /// Evaluate every action in `actions` **once each** on `resource` and
    /// return the intersection of the resulting scopes, after gating each on
    /// the caller's own tenant. The number of PDP calls is the number of
    /// actions - never a function of how many credential types exist or which
    /// ones the tenant holds (ADR-0010).
    ///
    /// # Errors
    ///
    /// `AccessDenied` when the PDP denies any action or a scope excludes the
    /// caller's own tenant (a property of the decision, never of the data);
    /// `ServiceUnavailable` when the PDP cannot be reached.
    async fn authorize_on(
        &self,
        ctx: &SecurityContext,
        tenant: TenantId,
        resource: &ResourceType,
        actions: &[&str],
    ) -> Result<AccessScope, DomainError> {
        let mut combined: Option<AccessScope> = None;
        for action in actions {
            let scope = self.scope_for_timed(ctx, resource, action).await?;
            if !self.repo.scope_includes_tenant(&scope, tenant.0).await? {
                self.metrics.cross_tenant_denied();
                return Err(DomainError::AccessDenied { cause: None });
            }
            combined = Some(match combined {
                None => scope,
                Some(prev) => authz::intersect_scopes(&prev, &scope),
            });
        }
        Ok(combined.unwrap_or_else(AccessScope::deny_all))
    }

    /// [`Self::authorize_on`] for the base credential type: the returned
    /// scope carries the PDP's credential-type predicate, ready to filter
    /// the row lookup in SQL.
    async fn authorize_actions(
        &self,
        ctx: &SecurityContext,
        tenant: TenantId,
        actions: &[&str],
    ) -> Result<AccessScope, DomainError> {
        self.authorize_on(ctx, tenant, &authz::CREDENTIAL_RESOURCE, actions)
            .await
    }

    /// Optimistic-concurrency pre-check before any backend write: returns the
    /// `version = ?` filter to gate on, or `VersionConflict` if the
    /// caller's `If-Match` validator disagrees with the current row.
    fn precheck_version(
        precondition: Option<&WritePrecondition>,
        existing: &SecretRow,
    ) -> Result<Option<i64>, DomainError> {
        match precondition {
            Some(WritePrecondition::Version { id, version }) => {
                if *id != existing.id || *version != existing.version {
                    return Err(DomainError::VersionConflict);
                }
                Ok(Some(*version))
            }
            Some(WritePrecondition::AnyVersion(validators)) => {
                if validators
                    .iter()
                    .any(|(id, version)| *id == existing.id && *version == existing.version)
                {
                    Ok(Some(existing.version))
                } else {
                    Err(DomainError::VersionConflict)
                }
            }
            Some(WritePrecondition::Exists) | None => Ok(None),
        }
    }

    /// Optimistic-concurrency pre-check for `put`'s replace preconditions
    /// (mirrors [`Self::precheck_version`] for [`PutPrecondition`]). Never
    /// called for [`PutPrecondition::CreateOnly`] (handled by the caller
    /// before an `existing` row is even looked at).
    fn precheck_put_version(
        precondition: &PutPrecondition,
        existing: &SecretRow,
    ) -> Result<Option<i64>, DomainError> {
        match precondition {
            // `CreateOnly` can never actually reach here (the caller already
            // returned `Conflict` on an existing row before calling this),
            // but the match stays exhaustive over the full precondition type.
            PutPrecondition::CreateOnly | PutPrecondition::Exists => Ok(None),
            PutPrecondition::Version { id, version } => {
                if *id != existing.id || *version != existing.version {
                    return Err(DomainError::VersionConflict);
                }
                Ok(Some(*version))
            }
            PutPrecondition::AnyVersion(validators) => {
                if validators
                    .iter()
                    .any(|(id, version)| *id == existing.id && *version == existing.version)
                {
                    Ok(Some(existing.version))
                } else {
                    Err(DomainError::VersionConflict)
                }
            }
        }
    }

    /// Create-path write protocol (ADR-0006 section 6.2, `insert_active`
    /// variant): heal the failed creates of the reference, mint the record
    /// id, announce the attempt (write intent), `plugin.put` the value under
    /// the new key, then ONE transaction retires the intent and inserts the
    /// row `active` pointing at the returned version. A create-only
    /// uniqueness conflict is a definite loss: the transaction commits the
    /// intent deletion together with the `purge` debt for the fresh key
    /// (nothing can ever reference it), the debt is executed after the
    /// commit, and the caller answers `Conflict`. An ambiguous failure (the
    /// commit may have happened) is a 503 and leaves the intent for a later
    /// heal; nothing is executed. An intent found already healed is a
    /// 503 and nothing else.
    async fn create_new(
        &self,
        ctx: &SecurityContext,
        plugin: &Arc<dyn CredStorePluginClientV2>,
        params: NewRecordParams<'_>,
        value: &SecretValue,
        scope: &AccessScope,
    ) -> Result<Validator, DomainError> {
        // Heal on access: an earlier create of this reference that failed
        // after announcing itself left an intent and maybe an orphan key.
        self.heal_failed_creates(params.tenant, params.key).await;

        let id = Uuid::new_v4();
        let store_key = StoreKey::new(params.tenant, id);
        let attempt = WriteAttempt {
            attempt_id: Uuid::new_v4(),
            key: store_key.clone(),
            reference: params.key.as_ref().to_owned(),
            destroy_supported: plugin.destroyer().is_some(),
            heal_expired_intents: false,
        };
        self.announce_write(&attempt).await?;
        let value_version = self
            .plugin_put_timed(plugin, ctx, &store_key, value)
            .await?;
        let new = NewSecret {
            id,
            tenant_id: params.tenant,
            reference: params.key.clone(),
            sharing: params.sharing,
            owner_id: params.owner,
            secret_type_uuid: params.secret_type_uuid,
            expires_at: params.expires_at,
            value_version: value_version.clone(),
            fallback: params.fallback,
        };
        match self.repo.insert_active(scope, &new, &attempt).await {
            Ok(IntentCommit::Committed { debts, healed, .. }) => {
                self.after_commit(&debts, healed).await;
                Ok(Validator { id, version: 1 })
            }
            Ok(IntentCommit::Lost { debts }) => {
                self.after_commit(&debts, 0).await;
                Err(DomainError::Conflict)
            }
            Ok(IntentCommit::IntentLost) => Err(Self::write_intent_expired()),
            // Ambiguous: resolve it under a lock before answering.
            Err(_) => self.verify_create(scope, &new, &attempt).await,
        }
    }

    /// Overwrite-path write protocol (ADR-0006 section 6.2, `switch_value`
    /// variant): announce the attempt (write intent), `plugin.put` the value
    /// under the record's key, then ONE transaction retires the intent and
    /// runs one compare-and-set on the row version read in step 1, switching
    /// the pointer. Returns `Ok(None)` on a definite loss (0 rows: the row
    /// changed or vanished concurrently): the intent deletion commits
    /// together with a `destroy(Exactly(own))` debt (row still exists) or a
    /// `purge(key)` debt (row gone). On a win the same transaction records
    /// `destroy(Below(new))` and deletes the record's expired intents when
    /// the row read reported any; the debts are executed after the commit.
    /// Any other repo failure is ambiguous: 503, the intent stays for a later
    /// heal and nothing is executed. An intent found already healed is a
    /// 503 and nothing else. Shared by `put`'s replace leg and
    /// `patch {"secret": ...}` (ADR-0007); accepts a `declared` row too,
    /// switching it back to `active`.
    #[allow(
        clippy::too_many_arguments,
        reason = "carries every field an overwrite's CAS needs: the row read in step 1, sharing, \
                  fallback, expiry, value"
    )]
    async fn overwrite_existing(
        &self,
        ctx: &SecurityContext,
        plugin: &Arc<dyn CredStorePluginClientV2>,
        scope: &AccessScope,
        existing: &SecretRow,
        sharing: SharingMode,
        fallback: Fallback,
        expires_at: Option<OffsetDateTime>,
        value: &SecretValue,
    ) -> Result<Option<Validator>, DomainError> {
        let store_key = existing.store_key();
        let attempt = WriteAttempt {
            attempt_id: Uuid::new_v4(),
            key: store_key.clone(),
            reference: existing.reference.clone(),
            destroy_supported: plugin.destroyer().is_some(),
            heal_expired_intents: existing.heal.expired_intents,
        };
        self.announce_write(&attempt).await?;
        let value_version = self
            .plugin_put_timed(plugin, ctx, &store_key, value)
            .await?;

        // The compare-and-set is always on the version read in step 1,
        // whatever the client precondition: `destroy(Below)` is safe only for
        // a writer whose base is the row it read before its `put`.
        match self
            .repo
            .switch_value(
                scope,
                existing.id,
                existing.version,
                sharing,
                fallback,
                expires_at,
                value_version.clone(),
                &attempt,
            )
            .await
        {
            Ok(IntentCommit::Committed {
                value: row,
                debts,
                healed,
            }) => {
                self.after_commit(&debts, healed).await;
                Ok(Some(Validator {
                    id: row.id,
                    version: row.version,
                }))
            }
            // Definite loss: another writer's committed row stands (or the
            // row is gone); this version's debt was recorded with the intent
            // deletion.
            Ok(IntentCommit::Lost { debts }) => {
                self.after_commit(&debts, 0).await;
                Ok(None)
            }
            Ok(IntentCommit::IntentLost) => Err(Self::write_intent_expired()),
            // Ambiguous: resolve it under a lock before answering.
            Err(_) => Ok(self
                .verify_overwrite(
                    scope,
                    existing,
                    sharing,
                    fallback,
                    expires_at,
                    value_version,
                    &attempt,
                )
                .await?
                .map(|row| Validator {
                    id: row.id,
                    version: row.version,
                })),
        }
    }

    /// Step 2 of a secret write: announce the attempt (tx0). `Err` means the
    /// caller must not `put`: the intent could not be inserted, so nothing
    /// was written to the store.
    async fn announce_write(&self, attempt: &WriteAttempt) -> Result<(), DomainError> {
        if let Err(e) = self
            .repo
            .begin_write_intent(attempt, self.write.intent_lease)
            .await
        {
            tracing::warn!(
                record = %attempt.key.record_id,
                "credstore: could not record the write intent; nothing was written to the store"
            );
            return Err(Self::write_unavailable(
                "the credential store could not record the write",
                e,
            ));
        }
        Ok(())
    }

    /// Tx1 found the writer's own intent gone (a later request
    /// healed it and recorded the cleanup of this version), so nothing was
    /// applied. The answer is a retryable 503; nothing else is done.
    fn write_intent_expired() -> DomainError {
        DomainError::ServiceUnavailable {
            detail: "write intent expired; retry".to_owned(),
            retry_after: None,
            cause: None,
        }
    }

    /// A failure of the commit transaction other than a definite outcome: the
    /// commit may or may not have happened. The verification transaction
    /// itself failed too, so the answer is 503, nothing is executed and the
    /// intent (if it still exists) is left for a later heal.
    fn write_unconfirmed(e: DomainError) -> DomainError {
        Self::write_unavailable("the credential store could not confirm the write", e)
    }

    /// Counts one verification after an ambiguous commit.
    fn verified(&self, op: VerifyOp, outcome: VerifyOutcome) {
        self.metrics.write_commit_verified(op, outcome);
    }

    /// 503 for an attempt the verification found not applied (its version's
    /// cleanup was recorded and executed): a retry by the client is safe.
    fn write_not_applied() -> DomainError {
        DomainError::ServiceUnavailable {
            detail: "the write was not applied; retry".to_owned(),
            retry_after: None,
            cause: None,
        }
    }

    /// Resolves an ambiguous create tx1 with ONE verification transaction
    /// (a locking read of the attempt's own intent, so it waits for the
    /// ambiguous transaction to resolve). Returns the validator of the committed row, or the
    /// definite answer: [`DomainError::Conflict`] when tx1 ran again and lost,
    /// 503 when the attempt did not take effect or the verification failed.
    async fn verify_create(
        &self,
        scope: &AccessScope,
        new: &NewSecret,
        attempt: &WriteAttempt,
    ) -> Result<Validator, DomainError> {
        match self.repo.verify_insert_active(scope, new, attempt).await {
            Ok(WriteVerification::Committed { row }) => {
                self.verified(VerifyOp::Write, VerifyOutcome::Committed);
                // The commit's own debts are pending rows now.
                self.heal_record_debts(&attempt.key).await;
                Ok(Validator {
                    id: row.id,
                    version: row.version,
                })
            }
            Ok(WriteVerification::Retried(commit)) => {
                self.verified(VerifyOp::Write, VerifyOutcome::NotCommitted);
                match commit {
                    IntentCommit::Committed { debts, healed, .. } => {
                        self.after_commit(&debts, healed).await;
                        Ok(Validator {
                            id: new.id,
                            version: 1,
                        })
                    }
                    IntentCommit::Lost { debts } => {
                        self.after_commit(&debts, 0).await;
                        Err(DomainError::Conflict)
                    }
                    IntentCommit::IntentLost => Err(Self::write_intent_expired()),
                }
            }
            Ok(WriteVerification::NotApplied { debts }) => {
                self.verified(VerifyOp::Write, VerifyOutcome::NotApplied);
                self.after_commit(&debts, 0).await;
                Err(Self::write_not_applied())
            }
            Err(e) => {
                self.verified(VerifyOp::Write, VerifyOutcome::Failed);
                Err(Self::write_unconfirmed(e))
            }
        }
    }

    /// Resolves an ambiguous overwrite tx1 like [`Self::verify_create`].
    /// `Ok(None)` is the definite loss (tx1 ran again and its CAS matched no
    /// row).
    #[allow(
        clippy::too_many_arguments,
        reason = "carries the CAS an overwrite may have to run again"
    )]
    async fn verify_overwrite(
        &self,
        scope: &AccessScope,
        existing: &SecretRow,
        sharing: SharingMode,
        fallback: Fallback,
        expires_at: Option<OffsetDateTime>,
        value_version: ValueVersion,
        attempt: &WriteAttempt,
    ) -> Result<Option<SecretRow>, DomainError> {
        match self
            .repo
            .verify_switch_value(
                scope,
                existing.id,
                existing.version,
                sharing,
                fallback,
                expires_at,
                value_version.clone(),
                attempt,
            )
            .await
        {
            Ok(WriteVerification::Committed { row }) => {
                self.verified(VerifyOp::Write, VerifyOutcome::Committed);
                self.heal_record_debts(&attempt.key).await;
                Ok(Some(row))
            }
            Ok(WriteVerification::Retried(commit)) => {
                self.verified(VerifyOp::Write, VerifyOutcome::NotCommitted);
                match commit {
                    IntentCommit::Committed {
                        value: row,
                        debts,
                        healed,
                    } => {
                        self.after_commit(&debts, healed).await;
                        Ok(Some(row))
                    }
                    IntentCommit::Lost { debts } => {
                        self.after_commit(&debts, 0).await;
                        Ok(None)
                    }
                    IntentCommit::IntentLost => Err(Self::write_intent_expired()),
                }
            }
            Ok(WriteVerification::NotApplied { debts }) => {
                self.verified(VerifyOp::Write, VerifyOutcome::NotApplied);
                self.after_commit(&debts, 0).await;
                Err(Self::write_not_applied())
            }
            Err(e) => {
                self.verified(VerifyOp::Write, VerifyOutcome::Failed);
                Err(Self::write_unconfirmed(e))
            }
        }
    }

    /// `e` as a retryable 503 (kept as the cause, never shown on the wire).
    fn write_unavailable(detail: &str, e: DomainError) -> DomainError {
        match e {
            DomainError::ServiceUnavailable { .. } => e,
            other => DomainError::ServiceUnavailable {
                detail: detail.to_owned(),
                retry_after: None,
                cause: Some(Box::new(other)),
            },
        }
    }

    /// Counts the debts a committed transaction recorded.
    fn count_recorded(&self, debts: &[CleanupDebt]) {
        for debt in debts {
            self.metrics.store_cleanup_recorded(debt.task.op());
        }
    }

    /// Step 6, after a CONFIRMED commit of a secret write's commit
    /// transaction: counts what it healed and recorded, then executes the
    /// debts it recorded. Never called after an ambiguous commit.
    async fn after_commit(&self, debts: &[CleanupDebt], healed: u64) {
        if healed > 0 {
            self.metrics.write_intents_healed(healed);
        }
        self.count_recorded(debts);
        self.execute_debts(debts).await;
    }

    /// The service principal that executes cleanup debts: the debts belong to
    /// the store, not to the request that happens to run them.
    fn cleanup_ctx() -> Option<SecurityContext> {
        SecurityContext::builder()
            .subject_id(Uuid::nil())
            .subject_tenant_id(Uuid::nil())
            .build()
            .ok()
    }

    /// Executes `debts` against the plugin, one after the other, deleting
    /// each debt row after its execution succeeded (`purge` ->
    /// `plugin.delete_key`; `destroy` -> `plugin.destroy`, only deleted
    /// without a call for a plugin that does not support it). Best effort by
    /// contract: a failure is logged (the plugin's error text is not curated
    /// for the credstore boundary, so only its kind is) and counted
    /// (`store_cleanup_failed`), the debt row stays for a later request that
    /// touches the record, and the caller's answer never changes. Debts are
    /// idempotent, so a row deleted by another instance in the meantime
    /// matters not.
    async fn execute_debts(&self, debts: &[CleanupDebt]) {
        if debts.is_empty() {
            return;
        }
        let fail_all = |reason: &str, err: Option<&DomainError>| {
            for debt in debts {
                tracing::warn!(
                    op = debt.task.op().as_str(),
                    err = err.map(tracing::field::display),
                    "credstore: store cleanup not executed: {reason}; the debt stays"
                );
                self.metrics.store_cleanup_failed(debt.task.op());
            }
        };
        let Some(ctx) = Self::cleanup_ctx() else {
            fail_all("cannot build the service context", None);
            return;
        };
        let plugin = match self.plugins.resolve().await {
            Ok(p) => p,
            Err(e) => {
                fail_all("no plugin", Some(&e));
                return;
            }
        };
        for debt in debts {
            self.execute_debt(&plugin, &ctx, debt).await;
        }
    }

    /// Executes one debt (see [`Self::execute_debts`]).
    async fn execute_debt(
        &self,
        plugin: &Arc<dyn CredStorePluginClientV2>,
        ctx: &SecurityContext,
        debt: &CleanupDebt,
    ) {
        let op = debt.task.op();
        let t0 = Instant::now();
        let (dep_op, result) = match &debt.task {
            CleanupTask::Purge(key) => (DepOp::PluginDeleteKey, plugin.delete_key(ctx, key).await),
            CleanupTask::Destroy { key, selector } => {
                // Recorded only for destroy-capable plugins, but the plugin
                // can change between recording and execution: a plugin
                // without a destroyer has nothing to destroy.
                let Some(destroyer) = plugin.destroyer() else {
                    self.delete_executed_debt(debt).await;
                    return;
                };
                (
                    DepOp::PluginDestroy,
                    destroyer.destroy(ctx, key, selector.clone()).await,
                )
            }
        };
        self.metrics.dependency(
            Dep::Plugin,
            dep_op,
            result
                .as_ref()
                .map_or_else(plugin_error_outcome, |()| Outcome::Success),
            t0.elapsed().as_secs_f64(),
        );
        match result {
            Ok(()) => self.delete_executed_debt(debt).await,
            Err(e) => {
                tracing::warn!(
                    op = op.as_str(),
                    kind = match e {
                        credstore_sdk::CredStorePluginError::Unavailable { .. } => "unavailable",
                        credstore_sdk::CredStorePluginError::CredentialsRejected { .. } =>
                            "credentials_rejected",
                        _ => "error",
                    },
                    "credstore: store cleanup failed; the debt stays for a later request"
                );
                self.metrics.store_cleanup_failed(op);
            }
        }
    }

    /// Deletes the row of a debt that was executed. A failure is only logged:
    /// the debt is idempotent and a later request executes it again.
    async fn delete_executed_debt(&self, debt: &CleanupDebt) {
        if let Err(e) = self.repo.delete_debt(debt.id).await {
            tracing::warn!(
                op = debt.task.op().as_str(),
                err = %e,
                "credstore: could not delete an executed cleanup debt row; a later request \
                 executes it again"
            );
        }
    }

    /// Heal on access: executes the pending debts of the record `key`, best
    /// effort. A failure to read them is logged and changes nothing.
    async fn heal_record_debts(&self, key: &StoreKey) {
        match self.repo.pending_debts(key).await {
            Ok(debts) => self.execute_debts(&debts).await,
            Err(e) => tracing::warn!(
                record = %key.record_id,
                err = %e,
                "credstore: could not read the pending cleanup debts of the record"
            ),
        }
    }

    /// Heal on access: removes the expired intents of `reference` whose
    /// record has no row (a create that failed after announcing itself) and
    /// executes the `purge` debts it recorded for their keys, best effort.
    async fn heal_failed_creates(&self, tenant: TenantId, reference: &SecretRef) {
        match self.repo.heal_failed_creates(tenant, reference).await {
            Ok(healed) => {
                if healed.intents > 0 {
                    self.metrics.write_intents_healed(healed.intents);
                }
                self.count_recorded(&healed.debts);
                self.execute_debts(&healed.debts).await;
            }
            Err(e) => tracing::warn!(
                reference = reference.as_ref(),
                err = %e,
                "credstore: healing the failed creates of the reference failed; a later request \
                 retries"
            ),
        }
    }

    /// Timed `plugin.put`, mapping the error and recording the dependency
    /// metric. The value is copied for the call (`SecretValue` is not `Clone`
    /// by design) so a retried write can put it again.
    async fn plugin_put_timed(
        &self,
        plugin: &Arc<dyn CredStorePluginClientV2>,
        ctx: &SecurityContext,
        key: &StoreKey,
        value: &SecretValue,
    ) -> Result<ValueVersion, DomainError> {
        let t0 = Instant::now();
        let raw = plugin
            .put(ctx, key, SecretValue::new(value.as_bytes().to_vec()))
            .await;
        let secs = t0.elapsed().as_secs_f64();
        self.metrics.dependency(
            Dep::Plugin,
            DepOp::PluginPut,
            raw.as_ref()
                .map_or_else(plugin_error_outcome, |_| Outcome::Success),
            secs,
        );
        raw.map_err(DomainError::from)
    }

    /// Delete an owned secret (section 6.3).
    ///
    /// One database transaction ([`SecretRepo::delete_by_id`]) deletes the row
    /// and records the `purge` debt of its store key; the reference is free to
    /// reuse the instant that transaction commits (ADR-0006: no name
    /// retention - a successor mints its own record id and key, so it can
    /// never collide with this delete's lagging purge). After the confirmed
    /// commit the same request executes the debt (`plugin.delete_key`) and
    /// deletes its row; a failed execution leaves the debt and does not
    /// change the reply. The purge also removes any orphan version a crashed
    /// or ambiguous write left above the record's pointer.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AccessDenied`] if the caller is permitted `delete`
    /// on no type — decided before any row is read, so the same whether or not
    /// the record exists.
    /// Returns [`DomainError::NotFound`] if no own-tenant row exists for the key
    /// **or** its type is one the caller may not delete (indistinguishable).
    /// Returns [`DomainError::VersionConflict`] if the current version does not
    /// satisfy a version-validator `precondition` (evaluated only after
    /// authorization, on a row the caller may delete). The precondition is
    /// mandatory: [`WritePrecondition::Exists`] is the explicit
    /// delete-whatever-is-there form (REST `If-Match: *`).
    pub async fn delete(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        precondition: WritePrecondition,
    ) -> Result<(), DomainError> {
        let tenant = TenantId(ctx.subject_tenant_id());
        let owner = OwnerId(ctx.subject_id());

        // Authorize BEFORE any row lookup (anti-enumeration, DESIGN 7.1): ONE
        // PDP evaluation of `delete` on the base credential type, whatever
        // the number of types. A caller the PDP refuses is answered 403 here,
        // whether or not the record exists.
        let scope = self
            .authorize_actions(ctx, tenant, &[actions::DELETE])
            .await?;

        // Then the caller's own row, looked up WITH the PDP scope (tenant and
        // type predicates in SQL). A missing row and a row of a type the
        // caller may not delete are the same 404, so a caller without
        // permission on a record cannot tell whether it exists.
        let Some(row) = self.repo.find_own(&scope, tenant, owner, key).await? else {
            return Err(DomainError::NotFound);
        };
        let resolved = self.resolve_stored(row.secret_type_uuid).await?;

        // Only now the optimistic-concurrency pre-check, mirroring the put
        // path: both the row id (a validator minted for a recreated secret's
        // earlier generation must never match, no ABA) and the version.
        let expected_version = Self::precheck_version(Some(&precondition), &row)?;
        // Heal on access: execute the record's pending debts, best effort.
        if row.heal.debts {
            self.heal_record_debts(&row.store_key()).await;
        }

        // One transaction: delete the row, record the key purge. 0 rows
        // (version mismatch, or a concurrent delete/write already moved the
        // row) maps to the existing not-found/conflict semantics.
        let result = match self
            .repo
            .delete_by_id(&scope, &row.store_key(), expected_version)
            .await
        {
            Ok(debts) => {
                self.count_recorded(&debts);
                self.execute_debts(&debts).await;
                Ok(())
            }
            Err(DomainError::NotFound) => Err(Self::delete_not_found(expected_version)),
            // The commit may or may not have happened: resolve it under a
            // lock before answering.
            Err(DomainError::ServiceUnavailable { .. } | DomainError::Internal { .. }) => {
                self.verify_delete(&scope, &row.store_key(), expected_version)
                    .await
            }
            Err(e) => Err(e),
        };
        // Only a record that holds a secret is a secret write.
        let target = row.value_version.is_some().then(|| AuditTarget {
            operation: AuditOperation::Delete,
            secret_type: resolved.gts_id.clone(),
        });
        self.audit_write(ctx, key, target, &result).await;
        result
    }
}

impl Service {
    /// The answer to a delete that matched no row the scope admits: under a
    /// version precondition a lost race (conflict), otherwise not found.
    /// Shared by the delete and its verification, so both answer alike.
    fn delete_not_found(expected_version: Option<i64>) -> DomainError {
        if expected_version.is_some() {
            DomainError::VersionConflict
        } else {
            DomainError::NotFound
        }
    }

    /// Resolves an ambiguous record delete with ONE verification transaction
    /// (a locking read of the record row): no row means the delete committed
    /// (its pending `purge` is executed), a row means it did not (the delete
    /// ran again with the same precondition). A failed verification is 503
    /// with nothing executed.
    async fn verify_delete(
        &self,
        scope: &AccessScope,
        key: &StoreKey,
        expected_version: Option<i64>,
    ) -> Result<(), DomainError> {
        match self.repo.verify_delete(scope, key, expected_version).await {
            Ok(DeleteVerification::Committed) => {
                self.verified(VerifyOp::Delete, VerifyOutcome::Committed);
                self.heal_record_debts(key).await;
                Ok(())
            }
            Ok(DeleteVerification::Retried { debts }) => {
                self.verified(VerifyOp::Delete, VerifyOutcome::NotCommitted);
                self.count_recorded(&debts);
                self.execute_debts(&debts).await;
                Ok(())
            }
            Ok(DeleteVerification::PreconditionFailed) => {
                self.verified(VerifyOp::Delete, VerifyOutcome::NotCommitted);
                Err(DomainError::VersionConflict)
            }
            Ok(DeleteVerification::NotFound) => {
                self.verified(VerifyOp::Delete, VerifyOutcome::NotCommitted);
                Err(Self::delete_not_found(expected_version))
            }
            Err(e) => {
                self.verified(VerifyOp::Delete, VerifyOutcome::Failed);
                Err(Self::write_unavailable(
                    "the credential store could not confirm the delete",
                    e,
                ))
            }
        }
    }
}

/// The fields of a new record that a create carries besides the value and
/// the scope.
#[domain_model]
#[derive(Clone, Copy)]
struct NewRecordParams<'a> {
    tenant: TenantId,
    owner: OwnerId,
    key: &'a SecretRef,
    sharing: SharingMode,
    fallback: Fallback,
    secret_type_uuid: Uuid,
    expires_at: Option<OffsetDateTime>,
}

/// The collection read (`Service::list`, ADR-0005/ADR-0004) — a child module
/// so its `impl Service` block can reach the fields and helper methods
/// above (`repo`, `dir`, `plugins`, `metrics`, `scope_for_timed`,
/// `resolve_stored`, `read_value_for_row`, …) without making any of them
/// crate-visible beyond this file's own module subtree.
#[path = "service/list.rs"]
mod list;

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;

#[cfg(test)]
#[path = "audit_tests.rs"]
mod audit_tests;

#[cfg(test)]
#[path = "write_intent_tests.rs"]
mod write_intent_tests;
