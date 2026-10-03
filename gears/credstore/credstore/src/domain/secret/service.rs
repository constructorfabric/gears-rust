//! Credential-store domain service (ADR-0006: immutable value versions).
//!
//! Orchestrates authorization, typed-secret validation, hierarchy resolution
//! and the immutable-value write/read/delete protocols. Every side effect on
//! the value store is either announced in the database before it happens
//! (a write intent, inserted before `plugin.put`) or executed by an outbox
//! task enqueued in the same transaction that learned it is needed (older
//! versions after a rotation, an unreferenced version after a lost write, a
//! whole key after a delete). No best-effort store call is ever relied on for
//! cleanup: this service never calls `destroy` or `delete_key` itself.

use std::sync::Arc;
use std::time::{Duration, Instant};

use authz_resolver_sdk::PolicyEnforcer;
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV2, Credential, CredentialPatch, CredentialStatus,
    CredentialWrite, Fallback as SdkFallback, InheritanceStatus, OwnerId, PatchField, PutOutcome,
    Secret, SecretRef, SecretValue, SharingMode, StoreKey, TenantId, Validator, ValueVersion,
};
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use authz_resolver_sdk::pep::ResourceType;

use crate::domain::authz::{self, actions, scope_for};
use crate::domain::error::DomainError;
use crate::domain::ports::audit::{AuditEvent, AuditOperation, AuditOutcome, AuditSink, NoopAudit};
use crate::domain::ports::clock::{MonotonicClock, SystemClock};
use crate::domain::ports::metrics::{
    CleanupOp, CredStoreMetricsPort, Dep, DepOp, Outcome, ReadOutcome, ReadRetryOutcome,
};
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::resolver::TenantDirectory;
use time::OffsetDateTime;

use crate::domain::secret::list_filter;
use crate::domain::secret::model::{
    CleanupTask, Fallback, IntentCommit, NewDeclaredSecret, NewSecret, PutPrecondition, SecretRow,
    SecretStatus, WriteAttempt, WritePrecondition,
};
use crate::domain::secret::repo::SecretRepo;
use crate::domain::secret::type_resolver::{ResolvedSecretType, SecretTypeResolver};
use crate::domain::secret::typing;

/// Maps a [`CredStoreError`] from the plugin layer to a [`DomainError`].
#[must_use]
pub fn map_plugin_err(e: CredStoreError) -> DomainError {
    match e {
        CredStoreError::NotFound => DomainError::NotFound,
        CredStoreError::AccessDenied => DomainError::AccessDenied { cause: None },
        CredStoreError::ServiceUnavailable {
            detail,
            retry_after,
        } => {
            // The plugin's own detail may embed backend/infra specifics (a
            // future vault-backed plugin's error text is not curated for the
            // credstore boundary the way a CF-internal sibling's is, and could
            // in principle carry sensitive material — e.g. echo a token being
            // written). The wire already redacts it; for the same reason we do
            // NOT log the raw detail either (a credential store must not risk
            // secret material in its logs). We record only its length so
            // operators can still tell an empty detail from a populated one
            // when correlating an outage.
            tracing::warn!(
                detail_len = detail.len(),
                "credstore: storage plugin reported unavailable (detail redacted)"
            );
            DomainError::ServiceUnavailable {
                detail: "storage backend unavailable".to_owned(),
                retry_after,
                cause: None,
            }
        }
        // Operator misconfiguration, not a transient outage: keep a stable,
        // distinguishable detail and no `retry_after` so callers don't retry.
        CredStoreError::NoPluginAvailable => DomainError::ServiceUnavailable {
            detail: "no storage plugin registered".into(),
            retry_after: None,
            cause: None,
        },
        CredStoreError::Conflict => DomainError::Conflict,
        // Expiry is the gear's own read-time verdict, never a plugin's: a
        // plugin only stores versioned bytes. Treat it as a contract
        // violation.
        // The one permanent read failure a plugin may report: the version is
        // held but can never be returned. Counted where it is produced.
        CredStoreError::SecretUnreadable => DomainError::SecretUnreadable,
        CredStoreError::SecretExpired => DomainError::Internal {
            diagnostic: "plugin returned SecretExpired".to_owned(),
            cause: None,
        },
        // These are plugin contract violations (a plugin should never return
        // them for get/put/delete on this SPI). Their free-text payloads
        // originate in the plugin, and — like the `ServiceUnavailable` detail
        // above — a future non-CF backend's error text is not curated for the
        // credstore boundary and could carry secret material. A credential
        // store must not risk that in its own logs, so we drop the raw text
        // and keep only its length as an internal diagnostic.
        CredStoreError::InvalidSecretRef { reason } => DomainError::Internal {
            diagnostic: format!(
                "plugin returned InvalidSecretRef (detail redacted, {} bytes)",
                reason.len()
            ),
            cause: None,
        },
        CredStoreError::UnsupportedTransition { detail } => DomainError::Internal {
            diagnostic: format!(
                "plugin returned UnsupportedTransition (detail redacted, {} bytes)",
                detail.len()
            ),
            cause: None,
        },
        CredStoreError::TypeViolation { reason, detail } => DomainError::Internal {
            diagnostic: format!(
                "plugin returned TypeViolation (detail redacted, {} bytes)",
                reason.len() + detail.len()
            ),
            cause: None,
        },
        CredStoreError::InvalidRequest { reason, detail } => DomainError::Internal {
            diagnostic: format!(
                "plugin returned InvalidRequest (detail redacted, {} bytes)",
                reason.len() + detail.len()
            ),
            cause: None,
        },
        CredStoreError::Internal(s) => DomainError::Internal {
            diagnostic: format!(
                "plugin returned Internal error (detail redacted, {} bytes)",
                s.len()
            ),
            cause: None,
        },
    }
}

/// Collection-read settings (`Service::list`, ADR-0005/ADR-0004), from
/// `ListCfg`: the metadata-mode page-size cap (`limit`/`$top`) and the
/// secret-mode (`$select` containing `secret`) match-set cap.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct ListSettings {
    pub max_limit: u64,
    pub secret_mode_cap: u64,
}

/// Secret-write settings (`WriteCfg`): how long a write intent is protected
/// from reclaim, and how many expired intents one reclaim pass handles.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct WriteSettings {
    pub intent_lease: Duration,
    pub reclaim_batch: u64,
}

impl Default for WriteSettings {
    fn default() -> Self {
        Self {
            intent_lease: Duration::from_mins(5),
            reclaim_batch: 16,
        }
    }
}

/// Upper bound on the reclaim passes of one startup run (each handles up to
/// `reclaim_batch` intents): a larger backlog drains through the piggyback
/// reclaim of later writes.
const MAX_STARTUP_RECLAIM_PASSES: u32 = 64;

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
    clock: Arc<dyn MonotonicClock>,
}

/// What a write is about to do to a secret, known once the write is
/// authorized. Carried out of `put_once`/`patch_once` so the one final
/// outcome (after any `If-Match: *` retry) is audited exactly once.
#[domain_model]
struct AuditTarget {
    operation: AuditOperation,
    secret_type: String,
}

/// Selection-aware answer to the point read (`Service::get_item`, ADR-0004
/// Amendment A): the resolved [`Credential`], its decrypted value when the
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
            clock: Arc::new(SystemClock),
        }
    }

    /// Route secret read/write audit events to `audit` (default: discard).
    #[must_use]
    pub fn with_audit(mut self, audit: Arc<dyn AuditSink>) -> Self {
        self.audit = audit;
        self
    }

    /// Use `write` for the write-intent lease and the reclaim batch (default:
    /// [`WriteSettings::default`]).
    #[must_use]
    pub fn with_write_settings(mut self, write: WriteSettings) -> Self {
        self.write = write;
        self
    }

    /// Use `clock` for the lease guard (default: the system monotonic clock).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn MonotonicClock>) -> Self {
        self.clock = clock;
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
    /// Returns [`DomainError::AccessDenied`] if the caller is out of scope.
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
    /// `read` alone, exactly as [`Self::get`]).
    ///
    /// Returns the assembled [`Credential`] together with the winning row's
    /// `(id, version)` whenever the caller holds no own row — the REST
    /// layer's weak-`ETag` source (ADR-0004, D4): `Credential::validator`
    /// stays `None` in that case (the caller has no row of its own to hold a
    /// real validator for), so the weak hash has to come from here instead.
    /// Not part of the SDK contract, hence not [`Self::get`] itself.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AccessDenied`] if the caller is out of scope.
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
    /// ADR-0004 Amendment A — mirror of [`Self::authorize_write_actions`] for
    /// reads): resolves the candidates and reduces them exactly as
    /// [`Self::resolve_credential`] and the collection read do, evaluates
    /// the action(s) `fields` requires once each on the effective concrete
    /// type — `read` when `fields` is `None` or names any administrative
    /// record field (`sharing`/`status`/`fallback`/`inheritance`/`version`/
    /// `updated_at`/`owner_id`), `read_secret` when `fields` names `secret`,
    /// both when both — and reads the value via
    /// [`Self::read_value_for_row`] only when `secret` is selected and the
    /// winner has one. A denial on either required action is the canonical
    /// 404 (`Ok(None)`), before either representation is assembled. A
    /// `secret`-only projection (no administrative field alongside it — the
    /// shape [`Self::get_secret`] uses) against a winner with no value to
    /// serve is the canonical miss too, exactly as the withdrawn
    /// `GET …/secret` was; the same value-less winner, projected together
    /// with an administrative field, is returned as a record with no
    /// `secret` instead.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AccessDenied`] if the caller is out of scope.
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
        if let Some(f) = fields {
            list_filter::validate_select(f)?;
        }

        // Amendment A's projection-to-action rule: `read` is required unless
        // this is a pure value request (`secret` named, no administrative
        // field alongside it) — the shape `get_secret` uses; `read_secret`
        // is required whenever `secret` is named. At least one is always
        // `true`.
        let need_value = list_filter::is_secret_mode(fields);
        let need_admin = list_filter::admin_field_selected(fields);
        let need_read = !need_value || need_admin;
        let need_read_secret = need_value;

        let req = TenantId(ctx.subject_tenant_id());
        let subject = OwnerId(ctx.subject_id());
        let chain = self.dir.ancestor_chain(ctx, req).await?;

        let candidates = self
            .repo
            .resolve_candidates(req, subject, key, &chain)
            .await?;
        if candidates.is_empty() {
            if need_read_secret {
                self.metrics.read_outcome(ReadOutcome::Miss);
            }
            return Ok(None);
        }

        // The caller's own row: two-phase priority, private beats non-private
        // at the same (own) tenant — mirrors `find_own`/`resolve_for_get`.
        let own = candidates
            .iter()
            .filter(|r| r.tenant_id == req)
            .min_by_key(|r| i32::from(r.sharing != SharingMode::Private));

        // The winner: nearest row that actually resolves — `active` (expired
        // or not: expiry applies to the secret, so an expired record stays
        // the decisive one), or `declared` with `fallback: none` (a
        // suppressing row that competes and blocks). A `declared`/`inherit`
        // row never competes (ADR-0004, Suppression; ADR-0005, "Reducing a
        // reference to one item").
        let now = OffsetDateTime::now_utc();
        let resolvable = |r: &&SecretRow| match r.status {
            SecretStatus::Active => true,
            SecretStatus::Declared => r.fallback == Fallback::None,
        };
        let pos = |t: TenantId| chain.iter().position(|c| *c == t.0).unwrap_or(usize::MAX);
        let winner = candidates.iter().filter(resolvable).min_by(|a, b| {
            pos(a.tenant_id)
                .cmp(&pos(b.tenant_id))
                .then((a.sharing != SharingMode::Private).cmp(&(b.sharing != SharingMode::Private)))
        });

        let Some(effective) = own.or(winner) else {
            // Nothing resolves and the caller holds no row at all.
            if need_read_secret {
                self.metrics.read_outcome(ReadOutcome::Miss);
            }
            return Ok(None);
        };

        let inheritance = if let Some(w) = winner {
            if w.status == SecretStatus::Declared {
                // A declared/none winner blocks the walk — the caller's own
                // row or an ancestor's, either way the outcome is the same
                // name (ADR-0004, Suppression).
                InheritanceStatus::Suppressed
            } else if own.is_some_and(|o| o.id == w.id) {
                let ancestor_candidate_exists = candidates.iter().any(|r| r.tenant_id != req);
                if ancestor_candidate_exists {
                    InheritanceStatus::Overridden
                } else {
                    InheritanceStatus::Own
                }
            } else {
                InheritanceStatus::Inherited
            }
        } else {
            // Nothing resolves; the caller has an own `declared`/`inherit`
            // row with nothing behind it — reported as `Own` (ADR-0004:
            // "choose Own and document").
            InheritanceStatus::Own
        };

        // Evaluate whichever of `read`/`read_secret` the projection needs -
        // ONE PDP evaluation per action on the base credential type, never
        // one per type - gated on the caller's own tenant, before either
        // representation is assembled (mirrors the write paths). The PDP's
        // type constraint must admit the effective record's type. A PDP
        // denial, an out-of-scope tenant or an unadmitted type is
        // indistinguishable from a missing record (anti-enumeration 404); a
        // PDP or registry *outage* propagates.
        let mut required: Vec<&str> = Vec::with_capacity(2);
        if need_read {
            required.push(actions::READ);
        }
        if need_read_secret {
            required.push(actions::READ_SECRET);
        }
        let scope = match self.authorize_actions(ctx, req, &required).await {
            Ok(scope) => scope,
            Err(DomainError::AccessDenied { .. }) => {
                if need_read_secret {
                    self.metrics.read_outcome(ReadOutcome::Miss);
                }
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        if !authz::row_clamp(&scope, req.0).admits(effective.secret_type_uuid, &effective.reference)
        {
            if need_read_secret {
                self.metrics.read_outcome(ReadOutcome::Miss);
            }
            return Ok(None);
        }
        let resolved = self.resolve_stored(effective.secret_type_uuid).await?;

        let (status, fallback, version, updated_at, owner_id, validator) = match own {
            Some(o) => (
                if o.is_expired(now) {
                    CredentialStatus::Expired
                } else if o.status == SecretStatus::Active {
                    CredentialStatus::Active
                } else {
                    CredentialStatus::Declared
                },
                Some(SdkFallback::from(o.fallback)),
                Some(o.version),
                Some(o.updated_at),
                Some(o.owner_id),
                Some(Validator {
                    id: o.id,
                    version: o.version,
                }),
            ),
            None => (CredentialStatus::None, None, None, None, None, None),
        };

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
            fallback,
            status,
            inheritance,
            version,
            updated_at,
            owner_id,
            expires_at: effective.expires_at,
            validator,
        };

        // Read the value only when it was asked for and the winner actually
        // has one. This re-resolves the winning row through
        // `resolve_for_get` — the same single targeted query
        // `get_secret`'s classic implementation issued — rather than reusing
        // the in-memory `winner` above: it is what carries this read's
        // retry-once protocol (ADR-0006 §6.2), and
        // its `Secret::validator` is the row the value actually came from,
        // which a concurrent switch can make momentarily different from the
        // `winner` snapshot taken above.
        let secret = if need_read_secret {
            self.read_secret_audited(ctx, req, subject, key, &chain, &credential.secret_type)
                .await?
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
    /// another record - 404) discloses nothing and is not audited.
    async fn read_secret_audited(
        &self,
        ctx: &SecurityContext,
        req: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        chain: &[Uuid],
        secret_type: &str,
    ) -> Result<Option<Secret>, DomainError> {
        let read = async {
            let value_row = self.repo.resolve_for_get(req, subject, key, chain).await?;
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
    /// sends over the in-process trait (ADR-0004, "One item, one shape").
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AccessDenied`] if the caller is out of scope.
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
    /// and, per reduced winner, the collection read's secret mode (ADR-0005
    /// "Bulk secret read").
    ///
    /// Callers MUST have already checked `row.value_version.is_some()` (a
    /// `declared`/suppressed row has nothing to read, and never causes a store
    /// call) and authorized `resolved_gts_id` - this method performs neither
    /// the resolution nor the PDP gate, only the read.
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
    /// gone, or a plugin reporting the version unreadable, is permanent
    /// instead: [`DomainError::SecretUnreadable`] (and the
    /// `secret_unreadable` metric). A row found `declared` on the re-read (suppressed/removed
    /// concurrently) is a legitimate miss instead - `Ok(None)`.
    ///
    /// The re-read re-runs the *same* `resolve_for_get` call (same requesting
    /// tenant/subject/key/chain) rather than looking up the row by id. If it
    /// comes back with a **different** row (a different generation entirely -
    /// the original was deleted and a different secret now resolves), this
    /// fails closed as `NotFound` instead of serving a value the type/PDP
    /// check above never authorized. Since a row's type is immutable for its
    /// lifetime, a matching id means the caller's earlier type resolution and
    /// PDP scope still apply to the re-read.
    ///
    /// Returns `Ok(None)` when the *final* attempt reports `AccessDenied`,
    /// folded into the anti-enumeration miss like a PDP denial.
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
            Err(DomainError::AccessDenied { .. }) => Ok(None),
            Err(DomainError::SecretUnreadable) => Err(Self::log_unreadable(row, "plugin")),
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
                    self.metrics.secret_unreadable();
                    return Err(Self::log_unreadable(row, "pointer_unchanged"));
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
                    Err(DomainError::AccessDenied { .. }) => Ok(None),
                    Err(DomainError::SecretUnreadable) => {
                        Err(Self::log_unreadable(&fresh, "plugin"))
                    }
                    Err(e) => Err(e),
                }
            }
        }
    }

    /// The permanent "unreadable" outcome for `row`: logs the identifiers
    /// (never the value) and returns the error. The metric is counted where
    /// the outcome arises (`plugin_get_timed`, or the unchanged-pointer miss).
    fn log_unreadable(row: &SecretRow, cause: &'static str) -> DomainError {
        tracing::warn!(
            tenant_id = %row.tenant_id.0,
            record_id = %row.id,
            reference = %row.reference,
            cause,
            "credstore: stored secret version is unreadable (permanent)"
        );
        DomainError::SecretUnreadable
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
        let result = plugin.get(ctx, key, version).await.map_err(map_plugin_err);
        let secs = t0.elapsed().as_secs_f64();
        let outcome = match &result {
            Ok(Some(_)) => Outcome::Success,
            Ok(None) => Outcome::NotFound,
            Err(_) => Outcome::Error,
        };
        if matches!(result, Err(DomainError::SecretUnreadable)) {
            self.metrics.secret_unreadable();
        }
        self.metrics
            .dependency(Dep::Plugin, DepOp::PluginGet, outcome, secs);
        result
    }

    /// Create or replace the whole credential — record and, unless `value`
    /// is an explicit `None`, its value together (ADR-0004, "Two write verbs
    /// on one resource"; Amendment B, "The value-less record: reached only
    /// on purpose").
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
    /// attempt's version after a lost write) is enqueued in that same
    /// transaction and executed by the outbox, never inline. The plugin is
    /// resolved (fail-fast) only on this path. A lost compare-and-set under an
    /// `If-Match: *` precondition re-reads the row and retries once from the
    /// start, as a new attempt, before returning `VersionConflict`. After
    /// every attempt a bounded reclaim of expired intents runs best effort.
    /// A create-only `PUT` over an expired own row is a conflict like any
    /// other existing row (the expired record is visible: renew or delete it).
    /// `value: None` never touches the plugin at all on create
    /// (`insert_declared`, a plain `INSERT`) or on replace of an
    /// already-`declared` row (metadata-only, `update_metadata`, itself
    /// skipped when nothing would change — ADR-0004 "A metadata-only write
    /// that changes nothing bumps nothing"); on replace of an `active` row it
    /// removes the value in the same one compare-and-set `PATCH {"secret":
    /// null}` uses (`remove_value`), which enqueues the destroy of the old
    /// version in the same transaction (the plugin is resolved only to learn
    /// whether it supports `destroy`).
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
    /// (`TYPE_MISMATCH_WITH_INHERITED`): an ancestor's `shared` record or,
    /// when creating a private record, the tenant's own non-private one.
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
        let value = write.secret.as_ref();
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
            // this row already holds (Amendment B); never when `null`
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
                    // version it left behind is enqueued in that same
                    // transaction.
                    let plugin = self.plugins.resolve().await?;
                    let (row, enqueued) = self
                        .repo
                        .remove_value(
                            &scope,
                            existing.id,
                            expected_version,
                            sharing,
                            fallback,
                            expires_at,
                            plugin.supports_destroy(),
                        )
                        .await?
                        .ok_or(DomainError::VersionConflict)?;
                    self.count_enqueued(&enqueued);
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
                    // no-op follows (ADR-0004, "A metadata-only write that
                    // changes nothing bumps nothing").
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

        // If the reference currently resolves (for this caller: its tenant,
        // owner and ancestor chain) to a record of a different type, creating
        // here would silently diverge from what a value read already serves
        // (`fr-override-type-consistency`).
        let chain = self.dir.ancestor_chain(ctx, tenant).await?;
        if let Some(inherited) = self
            .repo
            .resolve_for_get(tenant, owner, key, &chain)
            .await?
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
            // no `value_version`, no backend call (ADR-0004 Amendment B).
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

    /// Apply a partial change to the record, the value, or both (ADR-0004,
    /// RFC 7396 JSON Merge Patch semantics). Never creates.
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
                let (row, enqueued) = self
                    .repo
                    .remove_value(
                        &scope,
                        existing.id,
                        expected_version,
                        merged_sharing,
                        merged_fallback,
                        merged_expires_at,
                        plugin.supports_destroy(),
                    )
                    .await?
                    .ok_or(DomainError::VersionConflict)?;
                self.count_enqueued(&enqueued);
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
    /// variant): mint the record id, announce the attempt (write intent),
    /// `plugin.put` the value under the new key, then ONE transaction retires
    /// the intent and inserts the row `active` pointing at the returned
    /// version. A create-only uniqueness conflict is a definite loss: the
    /// transaction commits the intent deletion together with the purge of the
    /// fresh key (nothing can ever reference it) and the caller answers
    /// `Conflict`. An ambiguous failure (the commit may have happened) is a
    /// 503 and leaves the intent for the reclaim; an intent found already
    /// reclaimed is settled by the writer itself (503). A bounded reclaim of
    /// expired intents runs after every attempt.
    async fn create_new(
        &self,
        ctx: &SecurityContext,
        plugin: &Arc<dyn CredStorePluginClientV2>,
        params: NewRecordParams<'_>,
        value: &SecretValue,
        scope: &AccessScope,
    ) -> Result<Validator, DomainError> {
        let id = Uuid::new_v4();
        let store_key = StoreKey::new(params.tenant, id);
        let attempt = WriteAttempt {
            attempt_id: Uuid::new_v4(),
            key: store_key.clone(),
            destroy_supported: plugin.supports_destroy(),
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
        let result = match self.repo.insert_active(scope, &new, &attempt).await {
            Ok(IntentCommit::Committed { enqueued, .. }) => {
                self.count_enqueued(&enqueued);
                Ok(Validator { id, version: 1 })
            }
            Ok(IntentCommit::Lost { enqueued }) => {
                self.count_enqueued(&enqueued);
                Err(DomainError::Conflict)
            }
            Ok(IntentCommit::IntentLost) => {
                Err(self.settle_lost_intent(&attempt, &value_version).await)
            }
            Err(e) => Err(Self::write_unconfirmed(e)),
        };
        self.piggyback_reclaim().await;
        result
    }

    /// Overwrite-path write protocol (ADR-0006 section 6.2, `switch_value`
    /// variant): announce the attempt (write intent), `plugin.put` the value
    /// under the record's key, then ONE transaction retires the intent and
    /// runs one compare-and-set on the row version read in step 1, switching
    /// the pointer. Returns `Ok(None)` on a definite loss (0 rows: the row
    /// changed or vanished concurrently): the intent deletion commits
    /// together with `destroy(Exactly(own))` (row still exists) or
    /// `purge(key)` (row gone). On a win the same transaction enqueues
    /// `destroy(Below(new))`. Any other repo failure is ambiguous: 503, the
    /// intent stays for the reclaim. An intent found already reclaimed is
    /// settled by the writer itself (503). A bounded reclaim of expired
    /// intents runs after every attempt. Shared by `put`'s replace leg and
    /// `patch {"secret": ...}` (ADR-0004); accepts a `declared` row too,
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
            destroy_supported: plugin.supports_destroy(),
        };
        self.announce_write(&attempt).await?;
        let value_version = self
            .plugin_put_timed(plugin, ctx, &store_key, value)
            .await?;

        // The compare-and-set is always on the version read in step 1,
        // whatever the client precondition: `destroy(Below)` is safe only for
        // a writer whose base is the row it read before its `put`.
        let result = match self
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
                enqueued,
            }) => {
                self.count_enqueued(&enqueued);
                Ok(Some(Validator {
                    id: row.id,
                    version: row.version,
                }))
            }
            // Definite loss: another writer's committed row stands (or the
            // row is gone); this version's cleanup was enqueued with the
            // intent deletion.
            Ok(IntentCommit::Lost { enqueued }) => {
                self.count_enqueued(&enqueued);
                Ok(None)
            }
            Ok(IntentCommit::IntentLost) => {
                Err(self.settle_lost_intent(&attempt, &value_version).await)
            }
            Err(e) => Err(Self::write_unconfirmed(e)),
        };
        self.piggyback_reclaim().await;
        result
    }

    /// Steps 2-3 of a secret write: announce the attempt (tx0), then the
    /// lease guard. `Err` means the caller must not `put`, and nothing is
    /// left behind: either the intent could not be inserted, or it was
    /// abandoned.
    ///
    /// The lease guard bounds the one residual the intent protocol has:
    /// reclaim never claims an intent before `lease_until`, so a writer that
    /// finds more than half the lease spent between announcing and its `put`
    /// (a stalled instance, a slow database) would risk landing a version
    /// after its intent was reclaimed, and abandons the write instead. The
    /// monotonic start instant is taken BEFORE tx0 is issued, so the
    /// database-clock lease can never start earlier than the writer thinks.
    async fn announce_write(&self, attempt: &WriteAttempt) -> Result<(), DomainError> {
        let started = self.clock.now();
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
        if self.clock.now().saturating_duration_since(started) > self.write.intent_lease / 2 {
            if let Err(e) = self.repo.drop_write_intent(attempt.attempt_id).await {
                // Best effort: the intent expires and is reclaimed anyway.
                tracing::warn!(
                    record = %attempt.key.record_id,
                    err = %e,
                    "credstore: could not delete the abandoned write intent; it will be reclaimed"
                );
            }
            return Err(DomainError::ServiceUnavailable {
                detail: "the write took too long to start; retry".to_owned(),
                retry_after: None,
                cause: None,
            });
        }
        Ok(())
    }

    /// Step 5c: the commit found the writer's own intent reclaimed. The
    /// writer is alive and knows its version, so it enqueues the cleanup
    /// nobody else will (`destroy(Exactly(version))` if the record has a row,
    /// else `purge(key)`) and answers 503. If that transaction fails too, the
    /// version stays until the record's next secret write or delete (the
    /// documented lease-window residual): logged and counted.
    async fn settle_lost_intent(
        &self,
        attempt: &WriteAttempt,
        version: &ValueVersion,
    ) -> DomainError {
        self.metrics.write_intent_lost();
        match self
            .repo
            .settle_lost_intent(&attempt.key, version, attempt.destroy_supported)
            .await
        {
            Ok(tasks) => self.count_enqueued(&tasks),
            Err(e) => {
                self.metrics.write_intent_reclaim_failed();
                tracing::error!(
                    record = %attempt.key.record_id,
                    err = %e,
                    "credstore: write intent was reclaimed and its version could not be settled; \
                     it stays until the record's next secret write or delete"
                );
            }
        }
        DomainError::ServiceUnavailable {
            detail: "the write outlived its lease and was not applied; retry".to_owned(),
            retry_after: None,
            cause: None,
        }
    }

    /// A failure of the commit transaction other than a definite outcome: the
    /// commit may or may not have happened, so the answer is 503 and the
    /// intent (if it still exists) is left for the reclaim.
    fn write_unconfirmed(e: DomainError) -> DomainError {
        Self::write_unavailable("the credential store could not confirm the write", e)
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

    /// Counts the cleanup tasks a committed transaction enqueued.
    fn count_enqueued(&self, tasks: &[CleanupTask]) {
        for task in tasks {
            self.metrics.store_cleanup_enqueued(task.op());
        }
    }

    /// Step 6: reclaim expired write intents after a write, best effort in
    /// its own transaction. A failure is logged and counted by
    /// [`Self::reclaim_expired`] and never changes the write's reply.
    async fn piggyback_reclaim(&self) {
        drop(self.reclaim_expired().await);
    }

    /// One reclaim pass: deletes up to `reclaim_batch` intents whose lease
    /// expired and enqueues the purge of every key whose record has no row.
    /// Returns how many intents it reclaimed. A failure is logged and counted
    /// (`write_intent_reclaim_failed`); the intents stay for the next pass.
    ///
    /// # Errors
    ///
    /// Returns the repository error of a failed pass.
    pub async fn reclaim_expired(&self) -> Result<u64, DomainError> {
        match self.repo.reclaim_expired(self.write.reclaim_batch).await {
            Ok(reclaimed) => {
                if reclaimed.intents > 0 {
                    self.metrics.write_intents_reclaimed(reclaimed.intents);
                }
                self.count_enqueued(&reclaimed.enqueued);
                Ok(reclaimed.intents)
            }
            Err(e) => {
                self.metrics.write_intent_reclaim_failed();
                tracing::warn!(err = %e, "credstore: reclaiming expired write intents failed");
                Err(e)
            }
        }
    }

    /// Startup reclaim, run by `serve` before the gear reports ready: passes
    /// repeat until one reclaims fewer intents than `reclaim_batch` (at most
    /// [`MAX_STARTUP_RECLAIM_PASSES`]; a larger backlog drains through the
    /// piggyback reclaim of later writes). Returns the total reclaimed.
    ///
    /// # Errors
    ///
    /// Returns the error of the first failed pass (already logged and
    /// counted); the caller starts regardless.
    pub async fn reclaim_at_startup(&self) -> Result<u64, DomainError> {
        let mut total = 0;
        for _ in 0..MAX_STARTUP_RECLAIM_PASSES {
            let reclaimed = self.reclaim_expired().await?;
            total += reclaimed;
            if reclaimed < self.write.reclaim_batch {
                break;
            }
        }
        Ok(total)
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
        let result = plugin
            .put(ctx, key, SecretValue::new(value.as_bytes().to_vec()))
            .await
            .map_err(map_plugin_err);
        let secs = t0.elapsed().as_secs_f64();
        self.metrics.dependency(
            Dep::Plugin,
            DepOp::PluginPut,
            if result.is_ok() {
                Outcome::Success
            } else {
                Outcome::Error
            },
            secs,
        );
        result
    }

    /// Delete an owned secret (section 6.3).
    ///
    /// One database transaction ([`SecretRepo::delete_by_id`]) deletes the row
    /// and enqueues the purge of its store key in the platform transactional
    /// outbox; the reference is free to reuse the instant that transaction
    /// commits (ADR-0006: no name retention - a successor mints its own record
    /// id and key, so it can never collide with this delete's lagging purge).
    /// The outbox handler calls `plugin.delete_key` and retries until it
    /// succeeds. No plugin call happens on this path. The purge also removes
    /// any orphan version a crashed or ambiguous write left above the
    /// record's pointer.
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

        // One transaction: delete the row, enqueue the key purge. 0 rows
        // (version mismatch, or a concurrent delete/write already moved the
        // row) maps to the existing not-found/conflict semantics.
        let result = match self
            .repo
            .delete_by_id(&scope, &row.store_key(), expected_version)
            .await
        {
            Ok(()) => {
                self.metrics.store_cleanup_enqueued(CleanupOp::Purge);
                Ok(())
            }
            Err(DomainError::NotFound) if expected_version.is_some() => {
                Err(DomainError::VersionConflict)
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
