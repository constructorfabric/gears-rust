use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use settings_service_sdk::error::not_found_resource;
use settings_service_sdk::gts::Resource;
use settings_service_sdk::models::GetEffectiveRequest;
use settings_service_sdk::{SettingKey, SettingKeyError, SettingsError, SettingsReaderClient};
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::{DomainError, is_retryable};
use crate::domain::subject_settings::TenantDefaults;

/// The setting that holds a tenant's personalization default for new subjects:
/// `gts.cf.core.settings.setting_type.v1~cf.construct.personalization.default_enabled.v1~`.
fn personalization_default_key() -> Result<SettingKey, SettingKeyError> {
    SettingKey::contributed(
        "cf",
        "construct",
        "personalization",
        "default_enabled",
        NonZeroU32::MIN,
    )
}

/// How long one read of the tenant default may take before it counts as the
/// settings service being unavailable.
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Why the configured fallback stood in for the tenant setting.
#[derive(Clone, Copy)]
enum Fallback {
    NoClient,
    Undeclared,
    Retired,
}

impl Fallback {
    const ALL: usize = 3;

    fn index(self) -> usize {
        match self {
            Self::NoClient => 0,
            Self::Undeclared => 1,
            Self::Retired => 2,
        }
    }

    fn reason(self) -> &'static str {
        match self {
            Self::NoClient => "no settings client is registered in ClientHub",
            Self::Undeclared => "the setting is not declared in the settings service",
            Self::Retired => "the setting was retired in the settings service",
        }
    }
}

/// [`TenantDefaults`] read from the settings service through `ClientHub`.
///
/// Nothing declares Construct's settings in the settings service yet, and a
/// host may run without it. Until then the configured fallback stands in for
/// the tenant setting, in three cases only: no settings client is registered,
/// the setting is not declared, or it was retired. The first time each case
/// happens it is logged at warn level, later at debug level, so a host that
/// ignores the tenant setting shows in the log without flooding it. Any other
/// failure is an error, not a reason to guess: personalization is a privacy
/// setting.
///
/// @cpt-dod:cpt-cf-construct-dod-subject-settings-tenant-default:p1
pub struct SettingsServiceDefaults {
    hub: Arc<ClientHub>,
    key: SettingKey,
    fallback: bool,
    read_timeout: Duration,
    warned: [AtomicBool; Fallback::ALL],
}

impl SettingsServiceDefaults {
    /// # Errors
    ///
    /// When the setting key does not parse, which is a defect in this gear.
    pub fn new(hub: Arc<ClientHub>, fallback: bool) -> Result<Self, SettingKeyError> {
        Ok(Self {
            hub,
            key: personalization_default_key()?,
            fallback,
            read_timeout: DEFAULT_READ_TIMEOUT,
            warned: Default::default(),
        })
    }

    /// Replace [`DEFAULT_READ_TIMEOUT`].
    #[must_use]
    pub fn with_read_timeout(mut self, read_timeout: Duration) -> Self {
        self.read_timeout = read_timeout;
        self
    }

    /// Answer with the configured fallback, and say why.
    fn fall_back(&self, tenant_id: Uuid, why: Fallback) -> bool {
        let first = !self.warned[why.index()].swap(true, Ordering::Relaxed);
        if first {
            tracing::warn!(
                %tenant_id,
                key = %self.key,
                reason = why.reason(),
                fallback = self.fallback,
                "personalization default: using the configured fallback, not the tenant setting \
                 (logged at warn once per reason)"
            );
        } else {
            tracing::debug!(
                %tenant_id,
                key = %self.key,
                reason = why.reason(),
                "personalization default: using the configured fallback"
            );
        }
        self.fallback
    }

    fn unavailable(&self, tenant_id: Uuid, detail: &str) -> DomainError {
        tracing::warn!(%tenant_id, key = %self.key, error = %detail, "settings service unavailable");
        DomainError::Unavailable(format!("settings service unavailable: {detail}"))
    }

    /// Decide what a failed read means.
    fn on_failure(&self, tenant_id: Uuid, err: CanonicalError) -> Result<bool, DomainError> {
        // Read these before the projection consumes the error: a `NotFound` is
        // a missing declaration only when the declaration resource is named.
        let undeclared = not_found_resource(&err) == Some(Resource::Declaration);
        let retryable = is_retryable(&err);

        match SettingsError::from(err) {
            // @cpt-begin:cpt-cf-construct-algo-subject-settings-tenant-default:p1:inst-default-undeclared
            SettingsError::NotFound { .. } if undeclared => {
                Ok(self.fall_back(tenant_id, Fallback::Undeclared))
            }
            SettingsError::Retired { .. } => Ok(self.fall_back(tenant_id, Fallback::Retired)),
            // @cpt-end:cpt-cf-construct-algo-subject-settings-tenant-default:p1:inst-default-undeclared
            // @cpt-begin:cpt-cf-construct-algo-subject-settings-tenant-default:p1:inst-default-unavailable
            // `DomainError::Unavailable` carries no retry delay, as on the
            // policy-service path, so the hint is not passed on.
            SettingsError::Unavailable {
                detail,
                retry_after_secs: _,
            } => Err(self.unavailable(tenant_id, &detail)),
            SettingsError::Other { canonical } if retryable => {
                Err(self.unavailable(tenant_id, &canonical.to_string()))
            }
            // @cpt-end:cpt-cf-construct-algo-subject-settings-tenant-default:p1:inst-default-unavailable
            // @cpt-begin:cpt-cf-construct-algo-subject-settings-tenant-default:p1:inst-default-failed
            other => {
                tracing::error!(
                    %tenant_id,
                    key = %self.key,
                    error = %other,
                    "personalization default read failed"
                );
                Err(DomainError::internal(format!(
                    "personalization default read failed: {other}"
                )))
            } // @cpt-end:cpt-cf-construct-algo-subject-settings-tenant-default:p1:inst-default-failed
        }
    }
}

#[async_trait]
impl TenantDefaults for SettingsServiceDefaults {
    async fn personalization_default(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<bool, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-subject-settings-tenant-default:p1:inst-default-client
        let Some(reader) = self.hub.try_get::<dyn SettingsReaderClient>() else {
            return Ok(self.fall_back(tenant_id, Fallback::NoClient));
        };
        // @cpt-end:cpt-cf-construct-algo-subject-settings-tenant-default:p1:inst-default-client

        // @cpt-begin:cpt-cf-construct-algo-subject-settings-tenant-default:p1:inst-default-read
        let request = GetEffectiveRequest {
            key: self.key.clone(),
            scope: format!("/tenants/{tenant_id}"),
        };
        let Ok(resolved) =
            tokio::time::timeout(self.read_timeout, reader.get_effective(ctx, request)).await
        else {
            return Err(self.unavailable(
                tenant_id,
                &format!("no answer within {:?}", self.read_timeout),
            ));
        };
        // @cpt-end:cpt-cf-construct-algo-subject-settings-tenant-default:p1:inst-default-read

        match resolved {
            // @cpt-begin:cpt-cf-construct-algo-subject-settings-tenant-default:p1:inst-default-value
            Ok(effective) => effective.value.as_bool().ok_or_else(|| {
                DomainError::internal(format!("setting {} is not a boolean", self.key))
            }),
            // @cpt-end:cpt-cf-construct-algo-subject-settings-tenant-default:p1:inst-default-value
            Err(err) => self.on_failure(tenant_id, err),
        }
    }
}
