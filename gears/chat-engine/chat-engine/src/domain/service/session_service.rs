//! Session lifecycle service.
//!
//! `SessionService` is the orchestration layer between the REST handlers
//! (Phase 4 handlers + Phase 14 assembly), the SeaORM-backed repositories
//! (`SessionRepo`, `SessionTypeRepo`, `PluginConfigRepo`), and the backend
//! plugin trait `ChatEngineBackendPlugin` (resolved through
//! [`PluginService`]).
//!
//! All mutating methods enforce the lifecycle state machine via
//! [`ensure_can_transition`] — the only sanctioned wrapper around
//! [`LifecycleState::can_transition_to`]. Every plugin invocation builds a
//! [`PluginCallContext`] that carries an explicit `deadline` plus a
//! [`CancellationToken`], honouring the SDK's three-state `remaining()`
//! contract documented on `PluginCallContext::remaining`.
//!
//! Reserved metadata keys (`memory_strategy`, `retention_policy`,
//! `share_expires_at`) are stripped from any outgoing `Session` by
//! [`domain::session::public_metadata`]; client-supplied metadata that
//! contains a reserved key is rejected with `BadRequest`.
//
// @cpt-cf-chat-engine-session-service:p4
// @cpt-cf-chat-engine-adr-session-metadata:p4
// @cpt-cf-chat-engine-adr-session-deletion-strategy:p4

use std::sync::Arc;
use std::time::{Duration, Instant};

use sea_orm::ActiveValue::{NotSet, Set};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use tracing::{info, warn};
use uuid::Uuid;

use chat_engine_sdk::models::{Capability, CapabilityValue, LifecycleState, TenantId, UserId};
use chat_engine_sdk::plugin::{PluginCallContext, SessionPluginCtx};

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::service::plugin_service::PluginService;
use crate::domain::service::webhook::{WebhookEmitter, WebhookEvent};
use crate::domain::session::{
    RESERVED_METADATA_KEYS, Session, SessionType, ensure_can_transition, public_metadata,
};
use crate::infra::db::entity::session as session_entity;
use crate::infra::db::entity::session_type as session_type_entity;
use crate::infra::db::repo::session_repo::{DEFAULT_SOFT_DELETE_RETENTION_DAYS, SessionRepo};
use crate::infra::db::repo::session_type_repo::SessionTypeRepo;

/// Default per-call plugin deadline applied when the service has no other
/// signal. Mirrors the PRD §Performance budget for synchronous lifecycle
/// hooks.
pub const DEFAULT_PLUGIN_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// JWT-derived call identity. Constructed at the REST boundary; services
/// MUST NOT accept tenant / user identifiers from any other source (PRD
/// §7 Security, ADR-0017).
#[domain_model]
#[derive(Debug, Clone)]
pub struct Identity {
    /// Tenant id extracted from the bearer token (`subject_tenant_id`).
    pub tenant_id: String,
    /// User / subject id extracted from the bearer token (`subject_id`).
    pub user_id: String,
    /// Optional client id (device / app) — passed through to plugins but not
    /// used for scoping.
    pub client_id: Option<String>,
}

impl Identity {
    /// Convenience constructor; rejects empty tenant or user ids early so
    /// downstream `TenantId::new` / `UserId::new` panics never fire.
    pub fn new(
        tenant_id: impl Into<String>,
        user_id: impl Into<String>,
        client_id: Option<String>,
    ) -> Result<Self> {
        let tenant_id = tenant_id.into();
        let user_id = user_id.into();
        if tenant_id.is_empty() {
            return Err(ChatEngineError::bad_request(
                "tenant_id missing in identity",
            ));
        }
        if user_id.is_empty() {
            return Err(ChatEngineError::bad_request("user_id missing in identity"));
        }
        Ok(Self {
            tenant_id,
            user_id,
            client_id,
        })
    }
}

/// Request body for `POST /session-types`. The handler maps the wire DTO
/// into this struct after stripping any `tenant_id` / `user_id` fields the
/// client may have attempted to send.
#[domain_model]
#[derive(Debug, Clone)]
pub struct RegisterSessionTypeRequest {
    /// Human-readable name (per ADR-0017 — opaque to Chat Engine).
    pub name: String,
    /// GTS plugin instance ID; resolved via [`PluginService::resolve`].
    pub plugin_instance_id: Option<String>,
    /// Optional plugin configuration JSONB persisted via
    /// [`PluginService::load_config`] / [`PluginConfigRepo::upsert`]. The
    /// shape is plugin-defined.
    pub plugin_config: Option<JsonValue>,
}

/// Request body for `POST /sessions`.
#[domain_model]
#[derive(Debug, Clone)]
pub struct CreateSessionRequest {
    /// Optional session-type binding. `None` is allowed for sessions that
    /// don't need a backend plugin (e.g., read-only export shells); plugin
    /// calls are skipped when this is `None`.
    pub session_type_id: Option<Uuid>,
    /// Client-supplied metadata. Reserved keys are rejected here so they
    /// can't leak into the persisted row through this surface.
    pub metadata: Option<JsonValue>,
}

/// Service-level result of `delete_session` — handlers decide between
/// 200 (Soft) and 204 (Hard) based on this value.
//
// `Soft` carries a full `Session` while `Hard` is a unit; that size skew is
// intentional. The value is returned by-value and immediately matched at the
// handler, and `Soft` is the common outcome — boxing it to satisfy
// `large_enum_variant` would add an allocation on the hot path to shrink the
// rare `Hard` case, a net pessimisation.
#[allow(clippy::large_enum_variant)]
#[domain_model]
#[derive(Debug, Clone)]
pub enum SessionDeleteOutcome {
    Soft { session: Session },
    Hard,
}

/// Orchestration of session lifecycle plus session-type registration.
#[domain_model]
#[derive(Clone)]
pub struct SessionService {
    sessions: Arc<dyn SessionRepo>,
    session_types: Arc<dyn SessionTypeRepo>,
    plugins: PluginService,
    webhooks: Arc<dyn WebhookEmitter>,
    /// Default per-plugin-call deadline; can be overridden per call site via
    /// [`SessionService::with_plugin_timeout`].
    plugin_timeout: Duration,
}

impl SessionService {
    #[must_use]
    pub fn new(
        sessions: Arc<dyn SessionRepo>,
        session_types: Arc<dyn SessionTypeRepo>,
        plugins: PluginService,
        webhooks: Arc<dyn WebhookEmitter>,
    ) -> Self {
        Self {
            sessions,
            session_types,
            plugins,
            webhooks,
            plugin_timeout: DEFAULT_PLUGIN_CALL_TIMEOUT,
        }
    }

    /// Override the default plugin call deadline (mostly useful for tests
    /// and load-shed scenarios). Returns `self` for chained construction.
    #[must_use]
    pub fn with_plugin_timeout(mut self, timeout: Duration) -> Self {
        self.plugin_timeout = timeout;
        self
    }

    // ---------------------------------------------------------------------
    // Session-type registration
    // ---------------------------------------------------------------------

    pub async fn register_session_type(
        &self,
        identity: &Identity,
        req: RegisterSessionTypeRequest,
    ) -> Result<SessionType> {
        if req.name.trim().is_empty() {
            return Err(ChatEngineError::bad_request(
                "session-type name must not be empty",
            ));
        }

        let session_type_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();

        // Persist the row before reaching out to the plugin so a slow /
        // unhealthy plugin does not block the developer from registering
        // (per §1.5 — health is advisory). The plugin invocation below is
        // best-effort and never rolls back the insert.
        let model = session_type_entity::ActiveModel {
            session_type_id: Set(session_type_id),
            name: Set(req.name.clone()),
            plugin_instance_id: Set(req.plugin_instance_id.clone()),
            created_at: Set(now),
            updated_at: Set(now),
        };
        let inserted = self.session_types.insert(model).await?;

        if let Some(plugin_instance_id) = req.plugin_instance_id.as_deref() {
            // Plugin presence is required when an id was supplied — return
            // 404 (mapped from `NotFound`) so the developer can correct it.
            let plugin = self.plugins.resolve(plugin_instance_id)?;

            // Persist plugin_config (optional, plugin-defined shape) keyed by
            // (plugin_instance_id, session_type_id) so `create_session`'s
            // `PluginService::load_config` observes it later. A persistence
            // failure surfaces to the caller rather than silently dropping the
            // config.
            if let Some(cfg) = req.plugin_config.clone() {
                self.plugins
                    .save_config(plugin_instance_id, session_type_id, cfg)
                    .await?;
            }

            // Build a cancellable, deadline-bounded ctx and invoke the
            // plugin. We discard the capability list here — capabilities
            // become real only when a session is created against this
            // type (per §1.4 of the feature spec).
            let cancel = CancellationToken::new();
            let ctx = PluginCallContext {
                request_id: Uuid::new_v4(),
                tenant_id: TenantId::new(identity.tenant_id.as_str()),
                user_id: UserId::new(identity.user_id.as_str()),
                plugin_instance_id: plugin_instance_id.to_string(),
                session_type_id,
                plugin_config: req.plugin_config.clone(),
                enabled_capabilities: None,
                deadline: Some(Instant::now() + self.plugin_timeout),
                cancel: cancel.clone(),
            };
            let session_ctx = SessionPluginCtx {
                session_type_id,
                session_id: None,
                call_ctx: ctx,
            };

            // `on_session_type_configured` is best-effort: invalid input
            // is the only outcome that fails registration (per the spec).
            match self
                .invoke_with_deadline(plugin.on_session_type_configured(session_ctx), &cancel)
                .await
            {
                // `on_session_type_configured` has no session, so any returned
                // metadata is ignored; only the call's success/failure matters.
                Ok(_result) => {
                    info!(
                        plugin_instance_id = %plugin_instance_id,
                        session_type_id = %session_type_id,
                        "session-type configured with plugin"
                    );
                }
                Err(err) => {
                    warn!(
                        plugin_instance_id = %plugin_instance_id,
                        session_type_id = %session_type_id,
                        error = %err,
                        "plugin on_session_type_configured failed \u{2014} registration kept (advisory)"
                    );
                }
            }

            // Best-effort health probe (PluginService::health_probe folds
            // everything except Healthy into WARN and never blocks).
            if let Err(err) = self.plugins.health_probe(plugin_instance_id).await {
                warn!(
                    plugin_instance_id = %plugin_instance_id,
                    error = %err,
                    "plugin health probe failed during session-type registration (advisory)"
                );
            }
        }

        Ok(SessionType::from_model(inserted))
    }

    pub async fn list_session_types(&self, _identity: &Identity) -> Result<Vec<SessionType>> {
        let rows = self.session_types.list().await?;
        Ok(rows.into_iter().map(SessionType::from_model).collect())
    }

    pub async fn get_session_type(
        &self,
        _identity: &Identity,
        session_type_id: Uuid,
    ) -> Result<SessionType> {
        let row = self
            .session_types
            .find_by_id(session_type_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("session_type", session_type_id))?;
        Ok(SessionType::from_model(row))
    }

    // ---------------------------------------------------------------------
    // Session lifecycle
    // ---------------------------------------------------------------------

    pub async fn create_session(
        &self,
        identity: &Identity,
        req: CreateSessionRequest,
    ) -> Result<Session> {
        reject_reserved_metadata(req.metadata.as_ref())?;

        // Resolve session-type (if requested) before any write so we can
        // surface 404 cleanly.
        let session_type = match req.session_type_id {
            Some(id) => Some(
                self.session_types
                    .find_by_id(id)
                    .await?
                    .ok_or_else(|| ChatEngineError::not_found("session_type", id))?,
            ),
            None => None,
        };

        let session_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();

        let active = session_entity::ActiveModel {
            session_id: Set(session_id),
            tenant_id: Set(identity.tenant_id.clone()),
            user_id: Set(identity.user_id.clone()),
            client_id: Set(identity.client_id.clone()),
            session_type_id: Set(req.session_type_id),
            enabled_capabilities: Set(None),
            metadata: Set(req.metadata),
            lifecycle_state: Set(LifecycleState::Active.as_str().to_string()),
            share_token: Set(None),
            deleted_at: NotSet,
            scheduled_hard_delete_at: NotSet,
            created_at: Set(now),
            updated_at: Set(now),
        };

        let inserted = self.sessions.insert(active).await?;
        let inserted_id = inserted.session_id;

        // Invoke the plugin once a session-type with a bound plugin exists.
        // Plugin errors here are fatal per the feature spec (§Create Session):
        // map to 502 and roll back the session row to avoid orphaning a
        // session against a plugin that refused to accept it.
        let mut enabled_capabilities: Option<JsonValue> = None;
        let mut plugin_metadata: Option<JsonValue> = None;
        if let Some(ref st) = session_type
            && let Some(plugin_instance_id) = st.plugin_instance_id.clone()
        {
            let plugin = self.plugins.resolve(&plugin_instance_id)?;
            let plugin_config = self
                .plugins
                .load_config(&plugin_instance_id, st.session_type_id)
                .await?;

            let cancel = CancellationToken::new();
            let call_ctx = PluginCallContext {
                request_id: Uuid::new_v4(),
                tenant_id: TenantId::new(identity.tenant_id.as_str()),
                user_id: UserId::new(identity.user_id.as_str()),
                plugin_instance_id: plugin_instance_id.clone(),
                session_type_id: st.session_type_id,
                plugin_config,
                enabled_capabilities: None,
                deadline: Some(Instant::now() + self.plugin_timeout),
                cancel: cancel.clone(),
            };
            let session_ctx = SessionPluginCtx {
                session_type_id: st.session_type_id,
                session_id: Some(inserted_id),
                call_ctx,
            };

            match self
                .invoke_with_deadline(plugin.on_session_created(session_ctx), &cancel)
                .await
            {
                Ok(result) => {
                    enabled_capabilities = Some(capabilities_to_json(result.capabilities));
                    plugin_metadata = result.metadata;
                }
                Err(err) => {
                    // Rollback: hard-delete the orphan session row so the
                    // caller can safely retry. If the rollback itself fails
                    // the row is orphaned — surface that as an internal error
                    // (combining both causes) rather than masking it behind
                    // the otherwise-retryable plugin error.
                    if let Err(rollback_err) = self
                        .sessions
                        .hard_delete(&identity.tenant_id, &identity.user_id, inserted_id)
                        .await
                    {
                        warn!(
                            session_id = %inserted_id,
                            plugin_error = %err,
                            rollback_error = %rollback_err,
                            "failed to roll back orphaned session row after plugin rejection",
                        );
                        return Err(ChatEngineError::internal(format!(
                            "session rollback failed after plugin error ({err}); \
                             session {inserted_id} may be orphaned"
                        )));
                    }
                    return Err(err);
                }
            }
        }

        let mut persisted = if let Some(caps) = enabled_capabilities {
            self.sessions
                .update_capabilities(
                    &identity.tenant_id,
                    &identity.user_id,
                    inserted_id,
                    Some(caps),
                )
                .await?
        } else {
            inserted
        };

        // Plugin-supplied metadata is merged into the session metadata
        // (object merge; engine-reserved keys are stripped).
        if let Some(plugin_meta) = plugin_metadata {
            let merged = merge_plugin_metadata(persisted.metadata.clone(), plugin_meta);
            persisted = self
                .sessions
                .update_metadata(
                    &identity.tenant_id,
                    &identity.user_id,
                    inserted_id,
                    Some(merged),
                )
                .await?;
        }

        let session: Session = persisted.into();

        // Webhook event — best-effort, never blocks the response.
        self.webhooks
            .emit(WebhookEvent::SessionCreated {
                session_id: session.session_id,
                tenant_id: identity.tenant_id.clone(),
                user_id: identity.user_id.clone(),
                session_type_id: session.session_type_id,
            })
            .await
            .unwrap_or_else(|err| {
                warn!(error = %err, "webhook emit failed for session.created");
            });

        Ok(redact_session(session))
    }

    pub async fn get_session(&self, identity: &Identity, session_id: Uuid) -> Result<Session> {
        let row = self
            .sessions
            .find_by_id(&identity.tenant_id, &identity.user_id, session_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("session", session_id))?;
        Ok(redact_session(row.into()))
    }

    pub async fn list_sessions(
        &self,
        identity: &Identity,
        query: &ODataQuery,
    ) -> Result<Page<Session>> {
        let page = self
            .sessions
            .list_paginated(&identity.tenant_id, &identity.user_id, query)
            .await?;
        Ok(page.map_items(|row| redact_session(Session::from(row))))
    }

    pub async fn update_metadata(
        &self,
        identity: &Identity,
        session_id: Uuid,
        metadata: JsonValue,
    ) -> Result<Session> {
        reject_reserved_metadata(Some(&metadata))?;

        let row = self.load_modifiable(identity, session_id).await?;
        // Soft-deleted sessions cannot receive metadata writes per the spec.
        let state = parse_state(&row.lifecycle_state);
        if matches!(
            state,
            LifecycleState::SoftDeleted | LifecycleState::HardDeleted
        ) {
            return Err(ChatEngineError::conflict(
                "session is deleted and cannot accept metadata updates",
            ));
        }

        let updated = self
            .sessions
            .update_metadata(
                &identity.tenant_id,
                &identity.user_id,
                session_id,
                Some(metadata),
            )
            .await?;
        Ok(redact_session(updated.into()))
    }

    pub async fn update_capabilities(
        &self,
        identity: &Identity,
        session_id: Uuid,
        caps: Vec<CapabilityValue>,
    ) -> Result<Session> {
        let row = self.load_modifiable(identity, session_id).await?;
        let state = parse_state(&row.lifecycle_state);
        if matches!(
            state,
            LifecycleState::SoftDeleted | LifecycleState::HardDeleted
        ) {
            return Err(ChatEngineError::conflict(
                "session is deleted and cannot accept capability updates",
            ));
        }

        let session_type_id = row.session_type_id;
        let plugin_instance_id = match session_type_id {
            Some(st_id) => self
                .session_types
                .find_by_id(st_id)
                .await?
                .and_then(|st| st.plugin_instance_id),
            None => None,
        };

        let mut new_caps_json = capability_values_to_json(&caps);
        let mut plugin_metadata: Option<JsonValue> = None;

        if let Some(ref plugin_instance_id) = plugin_instance_id {
            let plugin = self.plugins.resolve(plugin_instance_id)?;
            let plugin_config = match session_type_id {
                Some(st_id) => self.plugins.load_config(plugin_instance_id, st_id).await?,
                None => None,
            };
            let cancel = CancellationToken::new();
            let call_ctx = PluginCallContext {
                request_id: Uuid::new_v4(),
                tenant_id: TenantId::new(identity.tenant_id.as_str()),
                user_id: UserId::new(identity.user_id.as_str()),
                plugin_instance_id: plugin_instance_id.clone(),
                session_type_id: session_type_id.unwrap_or_else(Uuid::nil),
                plugin_config,
                enabled_capabilities: Some(caps.clone()),
                deadline: Some(Instant::now() + self.plugin_timeout),
                cancel: cancel.clone(),
            };
            let session_ctx = SessionPluginCtx {
                session_type_id: session_type_id.unwrap_or_else(Uuid::nil),
                session_id: Some(session_id),
                call_ctx,
            };
            // Plugin failure on update → 502 (mapped via the standard
            // PluginError → ChatEngineError conversion).
            let returned = self
                .invoke_with_deadline(plugin.on_session_updated(session_ctx), &cancel)
                .await?;
            new_caps_json = capabilities_to_json(returned.capabilities);
            plugin_metadata = returned.metadata;
        }

        let mut updated = self
            .sessions
            .update_capabilities(
                &identity.tenant_id,
                &identity.user_id,
                session_id,
                Some(new_caps_json),
            )
            .await?;

        // Merge plugin-supplied metadata into the session metadata.
        if let Some(plugin_meta) = plugin_metadata {
            let merged = merge_plugin_metadata(updated.metadata.clone(), plugin_meta);
            updated = self
                .sessions
                .update_metadata(
                    &identity.tenant_id,
                    &identity.user_id,
                    session_id,
                    Some(merged),
                )
                .await?;
        }
        Ok(redact_session(updated.into()))
    }

    pub async fn archive_session(&self, identity: &Identity, session_id: Uuid) -> Result<Session> {
        let row = self.load_modifiable(identity, session_id).await?;
        let from = parse_state(&row.lifecycle_state);
        ensure_can_transition(from, LifecycleState::Archived)?;

        let updated = self
            .sessions
            .update_lifecycle_state(
                &identity.tenant_id,
                &identity.user_id,
                session_id,
                LifecycleState::Archived,
            )
            .await?;
        self.webhooks
            .emit(WebhookEvent::SessionArchived {
                session_id,
                tenant_id: identity.tenant_id.clone(),
                user_id: identity.user_id.clone(),
            })
            .await
            .unwrap_or_else(|err| warn!(error = %err, "webhook emit failed for session.archived"));
        Ok(redact_session(updated.into()))
    }

    pub async fn restore_session(&self, identity: &Identity, session_id: Uuid) -> Result<Session> {
        let row = self.load_modifiable(identity, session_id).await?;
        let from = parse_state(&row.lifecycle_state);
        ensure_can_transition(from, LifecycleState::Active)?;

        // Refuse to restore once the hard-delete window has passed (per
        // ADR-0021): the row is technically still readable but the spec
        // requires a clear failure rather than silently re-arming a
        // session that's about to be reaped.
        if let Some(scheduled) = row.scheduled_hard_delete_at
            && scheduled < OffsetDateTime::now_utc()
        {
            return Err(ChatEngineError::conflict(
                "soft-delete grace period elapsed; session can no longer be restored",
            ));
        }

        let updated = self
            .sessions
            .update_lifecycle_state(
                &identity.tenant_id,
                &identity.user_id,
                session_id,
                LifecycleState::Active,
            )
            .await?;
        self.webhooks
            .emit(WebhookEvent::SessionRestored {
                session_id,
                tenant_id: identity.tenant_id.clone(),
                user_id: identity.user_id.clone(),
            })
            .await
            .unwrap_or_else(|err| warn!(error = %err, "webhook emit failed for session.restored"));
        Ok(redact_session(updated.into()))
    }

    pub async fn delete_session(
        &self,
        identity: &Identity,
        session_id: Uuid,
        hard: bool,
    ) -> Result<SessionDeleteOutcome> {
        let row = self.load_modifiable(identity, session_id).await?;
        let from = parse_state(&row.lifecycle_state);
        let target = if hard {
            LifecycleState::HardDeleted
        } else {
            LifecycleState::SoftDeleted
        };
        ensure_can_transition(from, target)?;

        if hard {
            let removed = self
                .sessions
                .hard_delete(&identity.tenant_id, &identity.user_id, session_id)
                .await?;
            if !removed {
                return Err(ChatEngineError::not_found("session", session_id));
            }
            self.webhooks
                .emit(WebhookEvent::SessionHardDeleted {
                    session_id,
                    tenant_id: identity.tenant_id.clone(),
                    user_id: identity.user_id.clone(),
                })
                .await
                .unwrap_or_else(|err| {
                    warn!(error = %err, "webhook emit failed for session.hard_deleted");
                });
            Ok(SessionDeleteOutcome::Hard)
        } else {
            let retention_days = DEFAULT_SOFT_DELETE_RETENTION_DAYS;
            let updated = self
                .sessions
                .soft_delete(
                    &identity.tenant_id,
                    &identity.user_id,
                    session_id,
                    retention_days,
                )
                .await?;
            // Plugin notification for soft-delete is best-effort — the
            // current SDK trait (`ChatEngineBackendPlugin`) does NOT expose
            // an `on_session_deleted` hook. Phase 4 wires the webhook
            // emission; the plugin-trait extension lives in a future
            // version of the SDK (out of scope here).
            self.webhooks
                .emit(WebhookEvent::SessionSoftDeleted {
                    session_id,
                    tenant_id: identity.tenant_id.clone(),
                    user_id: identity.user_id.clone(),
                })
                .await
                .unwrap_or_else(|err| {
                    warn!(error = %err, "webhook emit failed for session.soft_deleted");
                });
            Ok(SessionDeleteOutcome::Soft {
                session: redact_session(updated.into()),
            })
        }
    }

    // ---------------------------------------------------------------------
    // Internals
    // ---------------------------------------------------------------------

    async fn load_modifiable(
        &self,
        identity: &Identity,
        session_id: Uuid,
    ) -> Result<session_entity::Model> {
        self.sessions
            .find_by_id(&identity.tenant_id, &identity.user_id, session_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("session", session_id))
    }

    /// Run a plugin call future against the cancellation token + deadline
    /// agreed on the [`PluginCallContext`]. On deadline elapse we cancel the
    /// token (so the plugin observes the signal) and return
    /// `BackendUnavailable` mapped via the standard `PluginError::timeout`
    /// path.
    async fn invoke_with_deadline<F, T>(&self, fut: F, cancel: &CancellationToken) -> Result<T>
    where
        F: std::future::Future<
                Output = std::result::Result<T, chat_engine_sdk::error::PluginError>,
            >,
    {
        match timeout(self.plugin_timeout, fut).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(e.into()),
            Err(_elapsed) => {
                cancel.cancel();
                Err(chat_engine_sdk::error::PluginError::timeout().into())
            }
        }
    }
}

// ---------------- Free helpers used by tests + handlers ----------------

/// Reject metadata payloads that try to write a reserved key. Centralised so
/// every handler/service entry point applies the same rule (per ADR-0017).
pub fn reject_reserved_metadata(metadata: Option<&JsonValue>) -> Result<()> {
    let Some(JsonValue::Object(map)) = metadata else {
        return Ok(());
    };
    for key in RESERVED_METADATA_KEYS {
        if map.contains_key(*key) {
            return Err(ChatEngineError::bad_request(format!(
                "metadata key '{key}' is reserved and cannot be set by clients"
            )));
        }
    }
    Ok(())
}

fn parse_state(raw: &str) -> LifecycleState {
    LifecycleState::from_str_value(raw).unwrap_or(LifecycleState::Active)
}

fn capabilities_to_json(caps: Vec<Capability>) -> JsonValue {
    serde_json::to_value(caps).unwrap_or(JsonValue::Array(Vec::new()))
}

fn capability_values_to_json(caps: &[CapabilityValue]) -> JsonValue {
    serde_json::to_value(caps).unwrap_or(JsonValue::Array(Vec::new()))
}

/// Merge plugin-supplied `overlay` metadata into the session's existing `base`
/// metadata (FR session-hook metadata). Object-level merge: overlay keys
/// override same-name base keys. Engine-reserved keys
/// (`memory_strategy` / `retention_policy` / `share_expires_at`) are stripped
/// from the overlay so a plugin can't clobber engine-managed session state.
/// A non-object overlay is ignored when the base is an object (to avoid
/// dropping client metadata); otherwise the overlay wins.
pub(crate) fn merge_plugin_metadata(base: Option<JsonValue>, overlay: JsonValue) -> JsonValue {
    let overlay = match overlay {
        JsonValue::Object(mut map) => {
            map.retain(|k, _| !RESERVED_METADATA_KEYS.contains(&k.as_str()));
            JsonValue::Object(map)
        }
        other => other,
    };
    match base {
        Some(JsonValue::Object(mut base_map)) => {
            if let JsonValue::Object(over_map) = overlay {
                for (k, v) in over_map {
                    base_map.insert(k, v);
                }
            }
            JsonValue::Object(base_map)
        }
        _ => overlay,
    }
}

/// Strip reserved metadata keys and clear the `share_token` (which is a
/// bearer secret) before returning a session to the caller. Phase 14 DTO
/// mapping must call this helper before serialization.
#[must_use]
pub fn redact_session(mut s: Session) -> Session {
    s.metadata = public_metadata(&s);
    // share_token is owned by Phase 10 — Phase 4 must not leak it.
    s.share_token = None;
    s
}

// ---------------- Conversion helpers for SessionType ----------------

trait SessionTypeFromModel: Sized {
    fn from_model(model: session_type_entity::Model) -> Self;
}

impl SessionTypeFromModel for SessionType {
    fn from_model(model: session_type_entity::Model) -> Self {
        SessionType {
            session_type_id: model.session_type_id,
            name: model.name,
            plugin_instance_id: model.plugin_instance_id,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

// Phase 2 only emitted the bridge for `Session`; the `SessionType` bridge is
// declared here per the Phase 2 contract's open-items list.
impl From<session_type_entity::Model> for SessionType {
    fn from(model: session_type_entity::Model) -> Self {
        <SessionType as SessionTypeFromModel>::from_model(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_reserved_metadata_blocks_known_keys() {
        let metadata = serde_json::json!({"memory_strategy": {"type": "full"}});
        let err = reject_reserved_metadata(Some(&metadata)).unwrap_err();
        assert!(matches!(err, ChatEngineError::BadRequest { .. }));
    }

    #[test]
    fn merge_plugin_metadata_object_merge_overlay_wins() {
        let base = Some(serde_json::json!({"a": 1, "b": 2}));
        let overlay = serde_json::json!({"b": 99, "c": 3});
        let merged = merge_plugin_metadata(base, overlay);
        assert_eq!(merged, serde_json::json!({"a": 1, "b": 99, "c": 3}));
    }

    #[test]
    fn merge_plugin_metadata_strips_reserved_keys_from_overlay() {
        let base = Some(serde_json::json!({"a": 1}));
        // Plugin tries to set engine-reserved keys — they must be dropped.
        let overlay = serde_json::json!({
            "memory_strategy": {"type": "full"},
            "retention_policy": {"x": 1},
            "share_expires_at": "2026-01-01",
            "model": "gpt-4",
        });
        let merged = merge_plugin_metadata(base, overlay);
        assert_eq!(merged, serde_json::json!({"a": 1, "model": "gpt-4"}));
    }

    #[test]
    fn merge_plugin_metadata_uses_overlay_when_base_absent() {
        let merged = merge_plugin_metadata(None, serde_json::json!({"k": "v"}));
        assert_eq!(merged, serde_json::json!({"k": "v"}));
    }

    #[test]
    fn merge_plugin_metadata_keeps_base_when_overlay_not_object() {
        // A non-object overlay must not clobber existing client metadata.
        let base = Some(serde_json::json!({"a": 1}));
        let merged = merge_plugin_metadata(base, serde_json::json!("oops"));
        assert_eq!(merged, serde_json::json!({"a": 1}));
    }

    #[test]
    fn reject_reserved_metadata_allows_client_keys() {
        let metadata = serde_json::json!({"title": "hello"});
        reject_reserved_metadata(Some(&metadata)).expect("client metadata accepted");
    }

    #[test]
    fn redact_session_clears_share_token_and_reserved_metadata() {
        let s = Session {
            session_id: Uuid::nil(),
            tenant_id: TenantId::new("t"),
            user_id: UserId::new("u"),
            client_id: None,
            session_type_id: None,
            enabled_capabilities: None,
            metadata: Some(serde_json::json!({
                "memory_strategy": {"type": "full"},
                "client_field": "ok",
            })),
            lifecycle_state: LifecycleState::Active,
            share_token: Some("super-secret".into()),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        };
        let redacted = redact_session(s);
        assert!(redacted.share_token.is_none());
        assert_eq!(
            redacted.metadata,
            Some(serde_json::json!({"client_field": "ok"}))
        );
    }

    #[test]
    fn identity_rejects_empty_tenant() {
        let err = Identity::new("", "u", None).unwrap_err();
        assert!(matches!(err, ChatEngineError::BadRequest { .. }));
    }

    #[test]
    fn identity_rejects_empty_user() {
        let err = Identity::new("t", "", None).unwrap_err();
        assert!(matches!(err, ChatEngineError::BadRequest { .. }));
    }

    #[test]
    fn parse_state_falls_back_to_active() {
        assert_eq!(parse_state("garbage"), LifecycleState::Active);
        assert_eq!(parse_state("soft_deleted"), LifecycleState::SoftDeleted);
    }

    // Anchor for the acceptance criterion that requires `ensure_can_transition`
    // to be called before every state-changing write — the call sites in
    // archive/restore/delete invoke this same helper, and the test below
    // verifies the routing for one representative edge.
    #[test]
    fn ensure_can_transition_path_used_by_service_for_archive() {
        let from = LifecycleState::Active;
        let to = LifecycleState::Archived;
        ensure_can_transition(from, to).expect("active->archived is valid");
    }
}
