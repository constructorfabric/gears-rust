//! Shared operator policy lifecycle. Both transports use this admitted boundary.
//! Compilation precedes persistence; immutable artifacts publish only after commit.

pub mod schemas;

use super::admission::{Admission, OperatorAdmission};
use super::engines::{EngineRegistry, PolicyArtifactCache};
use super::error::DomainError;
use super::pep::actions;
use super::ports::metrics::{PolicyTransition, QeMetrics};
use async_trait::async_trait;
use quota_enforcement_sdk::{
    EngineValidationInput, EnvironmentInputs, EventId, NotificationEvent, NotificationEventKind,
    NotificationScope, PageRequest, PageResult, PolicyDraft, PolicyId, PolicyPatch,
    PolicySchemaSnapshot, PolicyScope, PolicySpec, PolicyUpdate, PolicyVersion, PolicyVersionMeta,
    QuotaEnforcementStoragePluginV1, StorageError, TransitionOutcome, ValidatedConfig,
};
use serde_json::json;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

/// Server-owned schema resolution and activation compatibility.
#[async_trait]
pub trait PolicySchemas: Send + Sync {
    /// Resolve the policy's admitted schema set, bounded, keeping only the
    /// contracts behind `inputs`. Compile against `ALL`; persist what the
    /// artifact reports it reads.
    /// # Errors
    /// Invalid scope/schema or registry unavailability.
    async fn snapshot(
        &self,
        scope: &PolicyScope,
        engine: &str,
        inputs: EnvironmentInputs,
    ) -> Result<PolicySchemaSnapshot, DomainError>;
    /// Check a retained version against the currently loaded catalogue without rewriting it.
    /// # Errors
    /// A snapshot no longer compatible with the active catalogue.
    fn check_activation(&self, version: &PolicyVersion) -> Result<(), DomainError>;
}

/// Validated operational bounds of policy authoring.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct PolicyLimits {
    /// Maximum serialized engine configuration size.
    pub config_bytes: usize,
    /// Maximum UTF-8 bytes in a comment or description.
    pub comment_bytes: usize,
    /// Maximum history page size.
    pub list_limit: u32,
}

pub use quota_enforcement_sdk::engine::EvaluationLimits;

/// Every bound the policy surface runs with, converted once from the
/// validated `[quota-enforcement.policies]` section. The numbers live in the
/// configuration and nowhere else.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct PolicyRuntimeLimits {
    /// Per-evaluation clamp and cost.
    pub evaluation: EvaluationLimits,
    /// Authoring input bounds.
    pub authoring: PolicyLimits,
    /// Persisted schema snapshot bounds.
    pub snapshot: schemas::SnapshotLimits,
    /// Compiled artifacts held in process.
    pub artifact_cache_entries: NonZeroUsize,
    /// Attempts to prepare a missing artifact before giving up.
    pub preparation_max_attempts: NonZeroU32,
    /// Artifact compilations allowed to run at once.
    pub preparation_max_concurrency: NonZeroUsize,
}

/// References shared by a policy operation; neither transport can bypass admission.
pub struct PolicyManagement<'a> {
    /// Operator admission boundary.
    pub admission: &'a Admission,
    /// Transactional storage.
    pub storage: &'a dyn QuotaEnforcementStoragePluginV1,
    /// Immutable registry.
    pub engines: &'a EngineRegistry,
    /// Post-commit artifacts.
    pub cache: &'a PolicyArtifactCache,
    /// Server-owned schema provider.
    pub schemas: &'a dyn PolicySchemas,
    /// Bounded-label instrumentation.
    pub metrics: &'a dyn QeMetrics,
    /// Validated configured limits.
    pub limits: PolicyLimits,
}

impl PolicyManagement<'_> {
    /// Author a version after operator authorization and engine validation.
    /// # Errors
    /// Admission, bounded input, engine/schema validation, or atomic storage failure.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-policy-write:p1
    pub async fn create(
        &self,
        ctx: &SecurityContext,
        spec: PolicySpec,
    ) -> Result<PolicyVersion, DomainError> {
        self.validate(
            &spec.engine_config,
            spec.timeout_ms,
            spec.comment.as_deref(),
        )?;
        self.comment(spec.description.as_deref())?;
        let admitted = self.admission.admit_operator(ctx, actions::CREATE).await?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-engine-lookup
        self.require_engine(&spec.engine_id)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-engine-lookup
        // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-snapshot
        let full = self
            .schemas
            .snapshot(&spec.scope, &spec.engine_id, EnvironmentInputs::ALL)
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-snapshot
        // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-validate
        let compiled = self.compile(&admitted, &spec.engine_id, &spec.engine_config, &full)?;
        // Persist only the contracts the artifact reads: a change to an input
        // it never touches must not invalidate a later activation.
        let snapshot = self
            .schemas
            .snapshot(&spec.scope, &spec.engine_id, compiled.inputs())
            .await?;
        let artifact = self.rebuild(&admitted, &spec.engine_id, &spec.engine_config, &snapshot)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-validate
        let events = [event(None, "created")];
        let draft = PolicyDraft {
            scope: spec.scope,
            engine_id: spec.engine_id,
            engine_config: spec.engine_config,
            timeout_ms: spec.timeout_ms,
            description: spec.description,
            comment: spec.comment,
            created_by: ctx.subject_id().to_string(),
            schema_snapshot: snapshot,
        };
        // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-persist
        let version = self
            .storage
            .create_policy(ctx, draft, &events)
            .await
            .map_err(|e| self.storage_error(e))?;
        self.publish(&version, artifact);
        // @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-persist
        // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-return
        self.metrics
            .record_policy_transition(PolicyTransition::Create);
        Ok(version)
        // @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-return
    }

    /// Validate the complete merged configuration, then conditionally create a version.
    /// # Errors
    /// Admission, missing/deleted policy, validation, version conflict, or backend failure.
    pub async fn update(
        &self,
        ctx: &SecurityContext,
        id: PolicyId,
        patch: PolicyPatch,
    ) -> Result<PolicyVersion, DomainError> {
        self.comment(patch.comment.as_deref())?;
        if let Some(config) = &patch.engine_config {
            self.validate(config, patch.timeout_ms, None)?;
        }
        if patch.timeout_ms == Some(0) || patch.if_match_version == 0 {
            return Err(invalid(
                "if_match_version/timeout_ms",
                "INVALID_POLICY",
                "version and timeout must be positive",
            ));
        }
        let admitted = self.admission.admit_operator(ctx, actions::UPDATE).await?;
        let current = self.active(&id).await?;
        if current.version != patch.if_match_version {
            return Err(self.storage_error(StorageError::VersionConflict {
                expected: patch.if_match_version,
                actual: current.version,
            }));
        }
        let engine_id = patch.engine_id.unwrap_or(current.engine_id);
        let config = patch.engine_config.unwrap_or(current.engine_config);
        let timeout_ms = patch.timeout_ms.or(current.timeout_ms);
        self.validate(&config, timeout_ms, patch.comment.as_deref())?;
        self.require_engine(&engine_id)?;
        let full = self
            .schemas
            .snapshot(&current.scope, &engine_id, EnvironmentInputs::ALL)
            .await?;
        let compiled = self.compile(&admitted, &engine_id, &config, &full)?;
        let snapshot = self
            .schemas
            .snapshot(&current.scope, &engine_id, compiled.inputs())
            .await?;
        let artifact = self.rebuild(&admitted, &engine_id, &config, &snapshot)?;
        let update = PolicyUpdate {
            if_match_version: patch.if_match_version,
            engine_id: Some(engine_id),
            engine_config: Some(config),
            timeout_ms,
            comment: patch.comment,
            created_by: ctx.subject_id().to_string(),
            schema_snapshot: Some(snapshot),
        };
        let events = [event(Some(id.clone()), "updated")];
        let version = self
            .storage
            .update_policy(ctx, id, update, &events)
            .await
            .map_err(|e| self.storage_error(e))?;
        self.publish(&version, artifact);
        self.metrics
            .record_policy_transition(PolicyTransition::Update);
        Ok(version)
    }

    /// Reactivate a compatible retained version. Replays emit no transition metric.
    /// # Errors
    /// Admission, incompatible/terminal/unknown version, or transactional failure.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-policy-rollback:p1
    pub async fn rollback(
        &self,
        ctx: &SecurityContext,
        id: PolicyId,
        target: u32,
        comment: Option<String>,
    ) -> Result<PolicyVersion, DomainError> {
        self.comment(comment.as_deref())?;
        let admitted = self
            .admission
            .admit_operator(ctx, actions::POLICY_ROLLBACK)
            .await?;
        let version = self
            .storage
            .read_policy_version(&id, target)
            .await?
            .ok_or_else(|| {
                DomainError::from(StorageError::UnknownPolicyVersion {
                    policy_id: id.clone(),
                    version: target,
                })
            })?;
        self.schemas.check_activation(&version)?;
        let artifact = self.compile(
            &admitted,
            &version.engine_id,
            &version.engine_config,
            &version.schema_snapshot,
        )?;
        // Rollback is a latest-pointer move, so the notification says `updated`;
        // `rolled_back` is the state the displaced version lands in, not a
        // change kind (feature flow `inst-prd-rollback-apply`).
        let events = [event(Some(id.clone()), "updated")];
        let outcome = self
            .storage
            .rollback_policy(ctx, id, target, comment, &events)
            .await
            .map_err(|e| self.storage_error(e))?;
        let version = match outcome {
            // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-rollback-return
            TransitionOutcome::Applied(version) => {
                self.metrics
                    .record_policy_transition(PolicyTransition::Rollback);
                version
            }
            TransitionOutcome::NoOp(version) => version,
            // @cpt-end:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-rollback-return
        };
        self.publish(&version, artifact);
        Ok(version)
    }

    /// Soft-delete a metric policy, preserving history. Repeated deletion succeeds.
    /// # Errors
    /// Admission, protected global/unknown ID, or storage failure.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-policy-delete:p1
    pub async fn delete(
        &self,
        ctx: &SecurityContext,
        id: PolicyId,
        comment: Option<String>,
    ) -> Result<(), DomainError> {
        self.comment(comment.as_deref())?;
        let _admitted = self.admission.admit_operator(ctx, actions::DELETE).await?;
        let events = [event(Some(id.clone()), "deleted")];
        // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-delete-return
        if matches!(
            self.storage
                .delete_policy(ctx, id, comment, &events)
                .await?,
            TransitionOutcome::Applied(())
        ) {
            self.metrics
                .record_policy_transition(PolicyTransition::Delete);
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-delete-return
        Ok(())
    }

    /// Read the active or a specific retained version after operator admission.
    /// # Errors
    /// Admission, unknown/deleted ID or missing version, or storage failure.
    pub async fn read(
        &self,
        ctx: &SecurityContext,
        id: &PolicyId,
        version: Option<u32>,
    ) -> Result<PolicyVersion, DomainError> {
        let _admitted = self.admission.admit_operator(ctx, actions::GET).await?;
        match version {
            None => self.active(id).await,
            Some(version) => self
                .storage
                .read_policy_version(id, version)
                .await?
                .ok_or_else(|| {
                    StorageError::UnknownPolicyVersion {
                        policy_id: id.clone(),
                        version,
                    }
                    .into()
                }),
        }
    }

    /// Read immutable history with the storage-issued opaque cursor.
    /// # Errors
    /// Admission, invalid pagination, missing policy, or backend failure.
    pub async fn list(
        &self,
        ctx: &SecurityContext,
        id: &PolicyId,
        page: PageRequest,
    ) -> Result<PageResult<PolicyVersionMeta>, DomainError> {
        if page.limit == 0
            || page.limit > self.limits.list_limit
            || page.cursor.as_ref().is_some_and(|s| s.len() > 1024)
        {
            return Err(invalid(
                "page",
                "INVALID_PAGE",
                "page exceeds configured bounds",
            ));
        }
        let _admitted = self.admission.admit_operator(ctx, actions::LIST).await?;
        Ok(self.storage.list_policy_versions(id, page).await?)
    }

    async fn active(&self, id: &PolicyId) -> Result<PolicyVersion, DomainError> {
        if let Some(version) = self.storage.read_active_policy_by_id(id).await? {
            return Ok(version);
        }
        if self.storage.read_policy_version(id, 1).await?.is_some() {
            return Err(StorageError::PolicyDeleted {
                policy_id: id.clone(),
            }
            .into());
        }
        Err(StorageError::PolicyNotFound {
            policy_id: id.clone(),
        }
        .into())
    }

    /// `UNKNOWN_ENGINE` naming the engines this deployment registered, so the
    /// operator can correct the id without reading the binary's manifest.
    // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-unknown
    fn unknown_engine(&self, id: &str) -> DomainError {
        let registered: Vec<&str> = self.engines.ids().collect();
        invalid(
            "engine_id",
            "UNKNOWN_ENGINE",
            &format!(
                "engine `{id}` is not registered in this deployment; registered engines: {}",
                registered.join(", ")
            ),
        )
    }
    // @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-unknown

    // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-unknown-if
    fn require_engine(&self, id: &str) -> Result<(), DomainError> {
        if self.engines.get(id).is_none() {
            return Err(self.unknown_engine(id));
        }
        Ok(())
    }
    // @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-unknown-if

    // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-invalid
    // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-invalid-if
    fn compile(
        &self,
        _admitted: &OperatorAdmission,
        engine_id: &str,
        config: &serde_json::Value,
        snapshot: &PolicySchemaSnapshot,
    ) -> Result<Arc<dyn ValidatedConfig>, DomainError> {
        let engine = self
            .engines
            .get(engine_id)
            .ok_or_else(|| self.unknown_engine(engine_id))?;
        engine
            .validate_config(EngineValidationInput {
                raw: config,
                schemas: snapshot,
            })
            .map_err(|e| DomainError::InvalidPolicy {
                field: "engine_config",
                reason: "INVALID_ENGINE_CONFIG",
                detail: e.to_string(),
            })
    }
    // @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-invalid-if
    // @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-invalid

    /// Compile a second time from the narrowed closure that will be persisted.
    /// A version storage accepts must be one a restart can rebuild, so an
    /// engine that under-reports the inputs it reads fails here, at the write,
    /// instead of stranding the policy at its next activation.
    fn rebuild(
        &self,
        admitted: &OperatorAdmission,
        engine_id: &str,
        config: &serde_json::Value,
        snapshot: &PolicySchemaSnapshot,
    ) -> Result<Arc<dyn ValidatedConfig>, DomainError> {
        self.compile(admitted, engine_id, config, snapshot)
            .map_err(|error| {
                DomainError::Internal(format!(
                    "engine `{engine_id}` reads inputs its persisted schemas cannot rebuild: {error}"
                ))
            })
    }

    fn validate(
        &self,
        config: &serde_json::Value,
        timeout: Option<u64>,
        comment: Option<&str>,
    ) -> Result<(), DomainError> {
        // The transport bounds the raw request; this also covers in-process callers.
        let bytes = serde_json::to_vec(config).map_err(|_| {
            invalid(
                "engine_config",
                "INVALID_ENGINE_CONFIG",
                "configuration does not serialize",
            )
        })?;
        if bytes.len() > self.limits.config_bytes {
            return Err(invalid(
                "engine_config",
                "POLICY_CONFIG_TOO_LARGE",
                "configuration exceeds configured bounds",
            ));
        }
        if timeout == Some(0) {
            return Err(invalid(
                "timeout_ms",
                "INVALID_TIMEOUT",
                "timeout must be positive",
            ));
        }
        self.comment(comment)
    }

    fn comment(&self, comment: Option<&str>) -> Result<(), DomainError> {
        if comment.is_some_and(|s| s.len() > self.limits.comment_bytes) {
            return Err(invalid(
                "comment",
                "COMMENT_TOO_LONG",
                "text exceeds configured bounds",
            ));
        }
        Ok(())
    }

    fn publish(&self, version: &PolicyVersion, artifact: Arc<dyn ValidatedConfig>) {
        self.cache
            .publish(version.policy_id.clone(), version.version, artifact);
    }

    // @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-conflict
    fn storage_error(&self, error: StorageError) -> DomainError {
        if matches!(error, StorageError::VersionConflict { .. }) {
            self.metrics.record_policy_conflict();
        }
        error.into()
    }
    // @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-conflict
}

fn invalid(field: &'static str, reason: &'static str, detail: &str) -> DomainError {
    DomainError::InvalidPolicy {
        field,
        reason,
        detail: detail.into(),
    }
}

fn event(policy_id: Option<PolicyId>, change: &str) -> NotificationEvent {
    NotificationEvent {
        event_id: EventId::generate(),
        kind: NotificationEventKind::PolicyChanged,
        scope: NotificationScope::Platform,
        quota_id: None,
        policy_id,
        subject: None,
        payload: json!({"change_kind": change}),
        emitted_at: OffsetDateTime::now_utc(),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "policies_tests.rs"]
mod policies_tests;
