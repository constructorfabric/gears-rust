//! Service-account facade — confidential `client_credentials` clients
//! in the platform realm: create / rotate secret / revoke.
//!
//! Claims ride the realm-default client scope (usermodel-attribute
//! mappers provisioned by realm bootstrap): the facade sets
//! `tenant_id` + `user_type` attributes on the client's service-account
//! user instead of installing per-client protocol mappers.

use std::sync::Arc;

use account_management_sdk::idp_service_account::{
    IdpProvisionServiceAccountRequest, IdpServiceAccountCredentials,
};
use secrecy::SecretString;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::{SUBJECT_SERVICE_TYPE, ServiceAccountConfig};
use crate::domain::error::{AmbiguousStage, KcStatusKind, PluginError, redact_secrets};
use crate::domain::kc::client::KcAdminClient;
use crate::domain::kc::factory::KeycloakAdminClientFactory;
use crate::domain::kc::transport::truncate_2kb;
use crate::domain::kc_provisioning::{
    PROVISIONING_TENANT_ID_ATTR, ambiguous_from_kc_error, encode_component,
};
use crate::domain::ports::metrics::{
    FailureMetricsPort, FailureVariant, PluginOp, SaOp, SaOpMetricsPort,
};
use crate::domain::ports::purge::TenantAccountPurgeHook;

pub const SP_CLIENT_ID_PREFIX: &str = "svc";
const NAME_MAX_LEN: usize = 40;
/// Page size for the owned-client walk in [`ServiceAccountFacade::list_owned_reps`].
///
/// Sized well above any realistic `per_tenant_quota` so the common case is a
/// single round trip, while still bounding one response body. It is NOT a
/// result cap — the walk pages to exhaustion.
const SA_LIST_PAGE_SIZE: usize = 100;
const SA_TENANT_ID_ATTR: &str = "tenant_id";
const SA_USER_TYPE_ATTR: &str = "user_type";

/// `svc-<tenant_id>-<name>` — server-built, never caller-supplied.
#[must_use]
pub fn build_client_id(tenant_id: Uuid, name: &str) -> String {
    format!("{SP_CLIENT_ID_PREFIX}-{tenant_id}-{name}")
}

/// Lowercase alnum + '-', 1..=40 chars.
pub fn validate_name(name: &str) -> Result<(), PluginError> {
    let ok = !name.is_empty()
        && name.len() <= NAME_MAX_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(PluginError::SaInvalidInput {
            detail: format!("name must be 1..={NAME_MAX_LEN} chars of [a-z0-9-], got '{name}'"),
            field: Some("name".into()),
        })
    }
}

/// Decode a Keycloak JSON response body, mapping a non-2xx status →
/// [`PluginError::KcRest`] (truncated + redacted body) and a decode failure →
/// [`PluginError::KcRest`] carrying the serde message. Shared by the read
/// helpers (`resolve_scopes` / `list_owned_reps` / `lookup_client` / `sa_user`)
/// so the non-2xx + decode shape cannot drift between them.
fn kc_json<T: DeserializeOwned>(
    method: &'static str,
    path_template: &'static str,
    status: u16,
    body: &str,
) -> Result<T, PluginError> {
    if !(200..=299).contains(&status) {
        return Err(PluginError::KcRest {
            method,
            path_template: path_template.into(),
            status: KcStatusKind::Http(status),
            body_first_2kb: redact_secrets(&truncate_2kb(body)),
        });
    }
    serde_json::from_str(body).map_err(|e| PluginError::KcRest {
        method,
        path_template: path_template.into(),
        status: KcStatusKind::Http(status),
        body_first_2kb: format!("json decode: {e}"),
    })
}

// Field subset of Keycloak's `ClientRepresentation`, decoded by `kc_json` above.
#[allow(unknown_lints, de0101_no_serde_in_contract)]
#[domain_model]
#[derive(Deserialize)]
struct SaClientRep {
    id: Uuid,
    #[serde(rename = "clientId", default)]
    client_id: String,
    #[serde(default)]
    enabled: bool,
    #[serde(rename = "defaultClientScopes", default)]
    default_client_scopes: Vec<String>,
    #[serde(default)]
    attributes: Option<Value>,
}

// Field subset of Keycloak's `CredentialRepresentation`; same as `SaClientRep`.
#[allow(unknown_lints, de0101_no_serde_in_contract)]
#[domain_model]
#[derive(Deserialize)]
struct SecretRep {
    value: SecretString,
}

// Field subset of Keycloak's `ClientScopeRepresentation`; same as `SaClientRep`.
#[allow(unknown_lints, de0101_no_serde_in_contract)]
#[domain_model]
#[derive(Deserialize)]
struct ScopeRep {
    id: Uuid,
    name: String,
}

/// Return true when a KC attribute is exactly the expected scalar value.
/// Keycloak may encode attributes either as `"value"` or `["value"]`; arrays
/// with multiple values are rejected as corrupt managed state.
fn attr_value_matches(attr: Option<&Value>, expected: &str) -> bool {
    match attr {
        Some(Value::String(s)) => s.as_str() == expected,
        Some(Value::Array(items)) => {
            items.len() == 1 && items.first().and_then(Value::as_str) == Some(expected)
        }
        _ => false,
    }
}

/// Return `true` when `attr` carries exactly one non-blank value, whatever
/// that value is.
///
/// Mirrors [`attr_value_matches`]'s tolerance of the scalar and
/// singleton-array KC encodings and its rejection of multi-valued markers —
/// it differs only in not caring what the value *is*. Used for markers whose
/// presence proves a provisioning step ran but whose content is a
/// deployment-wide constant rather than per-account data.
fn attr_value_present(attr: Option<&Value>) -> bool {
    match attr {
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(items)) => {
            items.len() == 1
                && items
                    .first()
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty())
        }
        _ => false,
    }
}

/// Return `true` when `rep.attributes` carries the
/// [`PROVISIONING_TENANT_ID_ATTR`] value matching `tenant_id`. Tolerates both
/// scalar and singleton-array KC encodings, but rejects multi-valued markers.
fn owner_matches(rep: &SaClientRep, tenant_id: Uuid) -> bool {
    let Some(Value::Object(attrs)) = rep.attributes.as_ref() else {
        return false;
    };
    let target = tenant_id.to_string();
    attr_value_matches(attrs.get(PROVISIONING_TENANT_ID_ATTR), &target)
}

#[domain_model]
pub struct ServiceAccountFacade {
    cfg: ServiceAccountConfig,
    kc_factory: Arc<KeycloakAdminClientFactory>,
    sa_metrics: Arc<dyn SaOpMetricsPort>,
    failure_metrics: Arc<dyn FailureMetricsPort>,
}

impl ServiceAccountFacade {
    /// Construct the facade over a Keycloak admin-client factory and the
    /// segregated metric ports. Holds no per-call state; clone-cheap via the
    /// `Arc`-shared factory and metric sinks.
    pub fn new(
        cfg: ServiceAccountConfig,
        kc_factory: Arc<KeycloakAdminClientFactory>,
        sa_metrics: Arc<dyn SaOpMetricsPort>,
        failure_metrics: Arc<dyn FailureMetricsPort>,
    ) -> Self {
        Self {
            cfg,
            kc_factory,
            sa_metrics,
            failure_metrics,
        }
    }

    /// Emit op latency (success AND failure, mirroring `user_op_duration`)
    /// plus the failure counter on errors.
    fn emit_outcome<T>(
        &self,
        op: SaOp,
        plugin_op: PluginOp,
        elapsed: f64,
        result: &Result<T, PluginError>,
    ) {
        self.sa_metrics.sa_op_duration(op, elapsed);
        if let Err(e) = result {
            self.failure_metrics
                .failure(plugin_op, FailureVariant::from(e));
        }
    }

    fn admin_base(&self) -> String {
        format!(
            "{}admin/realms/{}",
            self.kc_factory.base_url_str(),
            encode_component(&self.cfg.realm),
        )
    }

    fn token_url(&self) -> String {
        format!(
            "{}realms/{}/protocol/openid-connect/token",
            self.kc_factory.base_url_str(),
            encode_component(&self.cfg.realm),
        )
    }

    fn validate_scopes(&self, scopes: &[String]) -> Result<(), PluginError> {
        for s in scopes {
            if !self.cfg.scope_allowlist.iter().any(|a| a == s) {
                return Err(PluginError::SaInvalidInput {
                    detail: format!("scope '{s}' is not in the configured allowlist"),
                    field: Some("scopes".into()),
                });
            }
        }
        Ok(())
    }

    /// Create a confidential `client_credentials` client owned by
    /// `req.tenant_context.tenant_id`, returning its live credentials (secret included).
    /// Times the operation and emits the op-latency + failure metrics around
    /// [`Self::create_impl`]. See the SDK [`create`] contract for the
    /// rejection / ambiguity semantics.
    ///
    /// [`create`]: account_management_sdk::IdpPluginClient::provision_service_account
    pub async fn create_inner(
        &self,
        ctx: &SecurityContext,
        req: &IdpProvisionServiceAccountRequest,
    ) -> Result<IdpServiceAccountCredentials, PluginError> {
        let started = std::time::Instant::now();
        let result = self.create_impl(ctx, req).await;
        let elapsed = started.elapsed().as_secs_f64();
        self.emit_outcome(SaOp::Create, PluginOp::SaCreate, elapsed, &result);
        result
    }

    async fn create_impl(
        &self,
        _ctx: &SecurityContext,
        req: &IdpProvisionServiceAccountRequest,
    ) -> Result<IdpServiceAccountCredentials, PluginError> {
        validate_name(&req.name)?;
        self.validate_scopes(&req.scopes)?;
        let tenant_id = req.tenant_context.tenant_id;

        // Pre-flight: resolve all scope names → uuids before any POST.
        let resolved_scopes: Vec<(String, Uuid)> = if req.scopes.is_empty() {
            vec![]
        } else {
            self.kc_factory
                .with_reactive_401_bootstrap(|bootstrap| async move {
                    self.resolve_scopes(&bootstrap, &req.scopes).await
                })
                .await?
        };

        self.enforce_quota(tenant_id).await?;
        let client_id = build_client_id(tenant_id, &req.name);

        self.kc_factory
            .with_reactive_401_bootstrap(|bootstrap| {
                let client_id = client_id.clone();
                async move { self.post_client(&bootstrap, &client_id, tenant_id).await }
            })
            .await?;

        // Mutation already happened — any escape below is Ambiguous. The
        // wrap sits OUTSIDE the wrapper so a KcRest 401 from the lookup
        // still reaches the reactive re-mint instead of being swallowed.
        let client_uuid = self
            .kc_factory
            .with_reactive_401_bootstrap(|bootstrap| {
                let client_id = client_id.clone();
                async move { self.lookup_client(&bootstrap, &client_id).await }
            })
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::KcClientCreate, e))?
            .ok_or_else(|| PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcClientCreate,
                detail: format!(
                    "kc:GET /admin/realms/{{realm}}/clients?clientId={client_id} -> 200: not found after create"
                ),
            })?
            .id;

        let subject_id = self
            .kc_factory
            .with_reactive_401_bootstrap(|bootstrap| {
                let client_id = client_id.clone();
                async move {
                    self.bind_sa_attributes(&bootstrap, &client_id, client_uuid, tenant_id)
                        .await
                }
            })
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::KcSaAttrSet, e))?;

        for (scope_name, scope_uuid) in &resolved_scopes {
            let scope_name = scope_name.clone();
            let scope_uuid = *scope_uuid;
            self.kc_factory
                .with_reactive_401_bootstrap(|bootstrap| {
                    let client_id = client_id.clone();
                    let scope_name = scope_name.clone();
                    async move {
                        self.attach_scope_by_uuid(
                            &bootstrap,
                            &client_id,
                            client_uuid,
                            scope_uuid,
                            &scope_name,
                        )
                        .await
                    }
                })
                .await
                .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::KcClientScopeAttach, e))?;
        }

        let secret = self
            .kc_factory
            .with_reactive_401_bootstrap(|bootstrap| {
                let client_id = client_id.clone();
                async move {
                    self.read_secret(
                        &bootstrap,
                        &client_id,
                        client_uuid,
                        "GET",
                        AmbiguousStage::KcClientSecretRead,
                    )
                    .await
                }
            })
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::KcClientSecretRead, e))?;

        Ok(IdpServiceAccountCredentials::new(
            client_id,
            secret,
            self.token_url(),
            subject_id,
        ))
    }

    /// Resolve requested scope names → (name, uuid) pairs in one list fetch.
    /// Absent scope → `PluginError::Config` (pre-flight, no POST has happened).
    async fn resolve_scopes(
        &self,
        bootstrap: &KcAdminClient,
        scope_names: &[String],
    ) -> Result<Vec<(String, Uuid)>, PluginError> {
        let path_template = "/admin/realms/{realm}/client-scopes";
        let list_url = format!("{}/client-scopes", self.admin_base());
        let (status, body) = bootstrap
            .raw_request("GET", &list_url, None, path_template)
            .await?;
        let scopes: Vec<ScopeRep> = kc_json("GET", path_template, status, &body)?;
        let mut resolved = Vec::with_capacity(scope_names.len());
        for name in scope_names {
            let uuid = scopes
                .iter()
                .find(|s| &s.name == name)
                .map(|s| s.id)
                .ok_or_else(|| PluginError::Config {
                    detail: format!(
                        "client scope '{name}' is allowlisted but absent in realm \
                         '{}' — provision it via realm bootstrap",
                        self.cfg.realm,
                    ),
                })?;
            resolved.push((name.clone(), uuid));
        }
        Ok(resolved)
    }

    /// Prefix query + ownership filter — the tenant's owned SP clients.
    /// Shared by quota / list / purge so the filter cannot drift.
    ///
    /// Paginated explicitly. Without `first`/`max` Keycloak applies its own
    /// server-side default (100 in current releases, and operator-tunable),
    /// which would silently truncate all three callers: the quota gate would
    /// under-count and let a tenant exceed its cap, `list` would hide
    /// accounts, and — worst — the tenant-deprovision purge would leave live
    /// `client_credentials` credentials behind a torn-down tenant with no
    /// error raised. Walking to exhaustion is what makes "every owned client"
    /// literally true.
    async fn list_owned_reps(&self, tenant_id: Uuid) -> Result<Vec<SaClientRep>, PluginError> {
        let prefix = format!("{SP_CLIENT_ID_PREFIX}-{tenant_id}-");
        let mut owned = Vec::new();
        let mut first = 0usize;
        loop {
            // `search=true` is KC's INFIX match, not exact-prefix — it widens
            // the result set rather than narrowing it. Ownership is enforced
            // client-side below via `starts_with(prefix)` + `owner_matches`.
            //
            // Paging therefore counts SERVER-side rows (pre-filter), which is
            // why the loop termination below keys on the raw page length and
            // not on how many survived the filter.
            let url = format!(
                "{}/clients?clientId={}&search=true&first={first}&max={SA_LIST_PAGE_SIZE}",
                self.admin_base(),
                encode_component(&prefix),
            );
            let page: Vec<SaClientRep> = self
                .kc_factory
                .with_reactive_401_bootstrap(|bootstrap| {
                    let url = url.clone();
                    async move {
                        let (status, body) = bootstrap
                            .raw_request("GET", &url, None, "/admin/realms/{realm}/clients")
                            .await?;
                        kc_json("GET", "/admin/realms/{realm}/clients", status, &body)
                    }
                })
                .await?;
            let page_len = page.len();
            owned.extend(
                page.into_iter()
                    .filter(|r| r.client_id.starts_with(&prefix) && owner_matches(r, tenant_id)),
            );
            // A short page means the server had nothing more to give. A full
            // page might be the last one, in which case the next request
            // returns empty and ends the loop — one extra round trip, versus
            // silently dropping accounts.
            if page_len < SA_LIST_PAGE_SIZE {
                break;
            }
            first += page_len;
        }
        Ok(owned)
    }

    /// Count `svc-<tenant>-` clients; reject at the configured quota.
    /// Best-effort cap: no cross-replica serialization, concurrent
    /// creates may briefly overshoot.
    async fn enforce_quota(&self, tenant_id: Uuid) -> Result<(), PluginError> {
        let count = self.list_owned_reps(tenant_id).await?.len();
        if count >= self.cfg.per_tenant_quota as usize {
            return Err(PluginError::SaInvalidInput {
                detail: format!(
                    "tenant {tenant_id} already has {count} service accounts (quota {})",
                    self.cfg.per_tenant_quota,
                ),
                field: Some("tenant_id".into()),
            });
        }
        Ok(())
    }

    /// POST the confidential client_credentials-only client.
    ///
    /// 409 → `SaInvalidInput` (strict reject, regardless of owner): a
    /// name collision must not resume another consumer's account —
    /// that would hand out its live secret and graft scopes onto it.
    /// Recovery from an Ambiguous half-create is revoke → create.
    ///
    /// Other non-2xx after a sent request → `AmbiguousCreated`.
    async fn post_client(
        &self,
        bootstrap: &KcAdminClient,
        client_id: &str,
        tenant_id: Uuid,
    ) -> Result<(), PluginError> {
        let url = format!("{}/clients", self.admin_base());
        let payload = json!({
            "clientId": client_id,
            "protocol": "openid-connect",
            "publicClient": false,
            "serviceAccountsEnabled": true,
            "standardFlowEnabled": false,
            "directAccessGrantsEnabled": false,
            "implicitFlowEnabled": false,
            "authorizationServicesEnabled": false,
            "clientAuthenticatorType": "client-secret",
            "attributes": { PROVISIONING_TENANT_ID_ATTR: tenant_id.to_string() },
        });
        let (status, body) = bootstrap
            .raw_request(
                "POST",
                &url,
                Some(&payload),
                "/admin/realms/{realm}/clients",
            )
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::KcClientCreate, e))?;
        match status {
            200..=299 => Ok(()),
            // Without this carve-out the wrapper would never see a 401 —
            // the POST has not mutated KC state when auth is rejected.
            401 => Err(PluginError::KcRest {
                method: "POST",
                path_template: "/admin/realms/{realm}/clients".into(),
                status: KcStatusKind::Http(401),
                body_first_2kb: redact_secrets(&truncate_2kb(&body)),
            }),
            // 409 retained no new state either — the id was taken before
            // this request. Permanent reject per the SDK contract.
            409 => Err(PluginError::SaInvalidInput {
                detail: format!("client '{client_id}' already exists"),
                field: Some("name".into()),
            }),
            _ => Err(PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcClientCreate,
                detail: format!(
                    "kc:POST /admin/realms/{{realm}}/clients -> {status}: {}",
                    redact_secrets(&truncate_2kb(&body))
                ),
            }),
        }
    }

    /// `GET /clients?clientId=` exact lookup → `Some(rep)` | `None`.
    ///
    /// Errors are `KcRest`-shaped (transport errors pass through unchanged;
    /// non-2xx and decode failures become `KcRest`). Pre-mutation callers
    /// can leave errors unwrapped; post-mutation callers wrap with
    /// `ambiguous_from_kc_error` at the call site.
    async fn lookup_client(
        &self,
        bootstrap: &KcAdminClient,
        client_id: &str,
    ) -> Result<Option<SaClientRep>, PluginError> {
        let url = format!(
            "{}/clients?clientId={}",
            self.admin_base(),
            encode_component(client_id),
        );
        let (status, body) = bootstrap
            .raw_request("GET", &url, None, "/admin/realms/{realm}/clients")
            .await?;
        let reps: Vec<SaClientRep> =
            kc_json("GET", "/admin/realms/{realm}/clients", status, &body)?;
        Ok(reps.into_iter().next())
    }

    /// GET-modify-PUT the SA user's attributes: `tenant_id` + `user_type`.
    /// Returns the SA user's UUID — the account's subject id.
    async fn bind_sa_attributes(
        &self,
        bootstrap: &KcAdminClient,
        client_id: &str,
        client_uuid: Uuid,
        tenant_id: Uuid,
    ) -> Result<Uuid, PluginError> {
        let sa_url = format!(
            "{}/clients/{client_uuid}/service-account-user",
            self.admin_base()
        );
        let (status, body) = bootstrap
            .raw_request(
                "GET",
                &sa_url,
                None,
                "/admin/realms/{realm}/clients/{uuid}/service-account-user",
            )
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::KcSaAttrSet, e))?;
        // Without this carve-out the wrapper would never see a 401.
        if status == 401 {
            return Err(PluginError::KcRest {
                method: "GET",
                path_template: "/admin/realms/{realm}/clients/{uuid}/service-account-user".into(),
                status: KcStatusKind::Http(401),
                body_first_2kb: redact_secrets(&truncate_2kb(&body)),
            });
        }
        if !(200..=299).contains(&status) {
            return Err(PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcSaAttrSet,
                detail: format!(
                    "client '{client_id}': kc:GET /admin/realms/{{realm}}/clients/{{uuid}}/service-account-user -> {status}: {}",
                    redact_secrets(&truncate_2kb(&body))
                ),
            });
        }
        let mut user: Value =
            serde_json::from_str(&body).map_err(|e| PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcSaAttrSet,
                detail: format!(
                    "client '{client_id}': kc:GET /admin/realms/{{realm}}/clients/{{uuid}}/service-account-user json decode: {e}"
                ),
            })?;
        let user_id: Uuid = user["id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcSaAttrSet,
                detail: format!("client '{client_id}': service-account-user without parsable id"),
            })?;

        // Defensive attributes mutation — mirror bind_admin_user_to_tenant.
        let user_obj = user
            .as_object_mut()
            .ok_or_else(|| PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcSaAttrSet,
                detail: format!("client '{client_id}': user representation is not a JSON object"),
            })?;
        let attrs = user_obj
            .entry("attributes".to_owned())
            .or_insert_with(|| json!({}));
        let attrs_obj = attrs
            .as_object_mut()
            .ok_or_else(|| PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcSaAttrSet,
                detail: format!("client '{client_id}': user.attributes is not a JSON object"),
            })?;
        // Hijack guard — mirror bind_admin_user_to_tenant: refuse to
        // overwrite a foreign or multi-valued tenant_id (hand-edited or
        // inconsistent KC state). Idempotent re-bind of this tenant passes.
        if let Some(existing) = attrs_obj.get(SA_TENANT_ID_ATTR) {
            let target = tenant_id.to_string();
            let idempotent = attr_value_matches(Some(existing), &target);
            if !idempotent {
                return Err(PluginError::AmbiguousCreated {
                    stage: AmbiguousStage::KcSaAttrSet,
                    detail: format!(
                        "client '{client_id}': SA user {user_id} already carries attributes.tenant_id = {existing}; refusing to overwrite"
                    ),
                });
            }
        }

        attrs_obj.insert(SA_TENANT_ID_ATTR.to_owned(), json!([tenant_id.to_string()]));
        attrs_obj.insert(SA_USER_TYPE_ATTR.to_owned(), json!([SUBJECT_SERVICE_TYPE]));

        let put_url = format!("{}/users/{user_id}", self.admin_base());
        let (status, body) = bootstrap
            .raw_request(
                "PUT",
                &put_url,
                Some(&user),
                "/admin/realms/{realm}/users/{id}",
            )
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::KcSaAttrSet, e))?;
        // Without this carve-out the wrapper would never see a 401.
        if status == 401 {
            return Err(PluginError::KcRest {
                method: "PUT",
                path_template: "/admin/realms/{realm}/users/{id}".into(),
                status: KcStatusKind::Http(401),
                body_first_2kb: redact_secrets(&truncate_2kb(&body)),
            });
        }
        if !(200..=299).contains(&status) {
            return Err(PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcSaAttrSet,
                detail: format!(
                    "client '{client_id}': kc:PUT /admin/realms/{{realm}}/users/{{id}} -> {status}: {}",
                    redact_secrets(&truncate_2kb(&body))
                ),
            });
        }
        Ok(user_id)
    }

    /// Attach a pre-resolved scope uuid as a client default scope.
    async fn attach_scope_by_uuid(
        &self,
        bootstrap: &KcAdminClient,
        client_id: &str,
        client_uuid: Uuid,
        scope_uuid: Uuid,
        scope_name: &str,
    ) -> Result<(), PluginError> {
        let put_url = format!(
            "{}/clients/{client_uuid}/default-client-scopes/{scope_uuid}",
            self.admin_base(),
        );
        let (status, body) = bootstrap
            .raw_request(
                "PUT",
                &put_url,
                None,
                "/admin/realms/{realm}/clients/{uuid}/default-client-scopes/{scope}",
            )
            .await
            .map_err(|e| ambiguous_from_kc_error(AmbiguousStage::KcClientScopeAttach, e))?;
        // Without this carve-out the wrapper would never see a 401.
        if status == 401 {
            return Err(PluginError::KcRest {
                method: "PUT",
                path_template: "/admin/realms/{realm}/clients/{uuid}/default-client-scopes/{scope}"
                    .into(),
                status: KcStatusKind::Http(401),
                body_first_2kb: redact_secrets(&truncate_2kb(&body)),
            });
        }
        if !(200..=299).contains(&status) {
            return Err(PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcClientScopeAttach,
                detail: format!(
                    "client '{client_id}': kc:PUT /admin/realms/{{realm}}/clients/{{uuid}}/default-client-scopes/{scope_name} -> {status}: {}",
                    redact_secrets(&truncate_2kb(&body))
                ),
            });
        }
        Ok(())
    }

    /// `GET` or `POST` `.../clients/{uuid}/client-secret` → wrapped secret value.
    ///
    /// Pass `method = "GET"` for initial secret read, `method = "POST"` for rotation.
    /// 404/410 on POST map to `SaNotFound` (lookup→rotate deletion race).
    /// Non-2xx on GET maps to `AmbiguousCreated` (post-mutation read).
    async fn read_secret(
        &self,
        bootstrap: &KcAdminClient,
        client_id: &str,
        client_uuid: Uuid,
        method: &'static str,
        stage: AmbiguousStage,
    ) -> Result<SecretString, PluginError> {
        let path_template = "/admin/realms/{realm}/clients/{uuid}/client-secret";
        let url = format!("{}/clients/{client_uuid}/client-secret", self.admin_base());
        let (status, body) = bootstrap
            .raw_request(method, &url, None, path_template)
            .await
            .map_err(|e| ambiguous_from_kc_error(stage, e))?;
        // Without this carve-out the wrapper would never see a 401.
        if status == 401 {
            return Err(PluginError::KcRest {
                method,
                path_template: path_template.into(),
                status: KcStatusKind::Http(401),
                body_first_2kb: redact_secrets(&truncate_2kb(&body)),
            });
        }
        if method == "POST" && matches!(status, 404 | 410) {
            return Err(PluginError::SaNotFound {
                detail: format!("client '{client_id}' already absent (kc {status})"),
            });
        }
        if !(200..=299).contains(&status) {
            return Err(PluginError::AmbiguousCreated {
                stage,
                detail: format!(
                    "client '{client_id}': kc:{method} /admin/realms/{{realm}}/clients/{{uuid}}/client-secret -> {status}: {}",
                    redact_secrets(&truncate_2kb(&body))
                ),
            });
        }
        let rep: SecretRep =
            serde_json::from_str(&body).map_err(|e| PluginError::AmbiguousCreated {
                stage,
                detail: format!(
                    "client '{client_id}': kc:{method} /admin/realms/{{realm}}/clients/{{uuid}}/client-secret json decode: {e}"
                ),
            })?;
        Ok(rep.value)
    }

    /// Exact ownership-checked lookup. Absent or foreign → `SaNotFound`.
    async fn lookup_owned_client(
        &self,
        tenant_id: Uuid,
        client_id: &str,
    ) -> Result<Uuid, PluginError> {
        let rep = self
            .kc_factory
            .with_reactive_401_bootstrap(|bootstrap| {
                let client_id = client_id.to_owned();
                async move { self.lookup_client(&bootstrap, &client_id).await }
            })
            .await?;
        let expected_prefix = format!("{SP_CLIENT_ID_PREFIX}-{tenant_id}-");
        match rep {
            Some(r)
                if r.client_id.starts_with(&expected_prefix) && owner_matches(&r, tenant_id) =>
            {
                Ok(r.id)
            }
            _ => Err(PluginError::SaNotFound {
                detail: format!("service account '{client_id}' not found for tenant {tenant_id}"),
            }),
        }
    }

    /// `GET .../clients/{uuid}/service-account-user` → (SA user UUID,
    /// managed-attrs-bound flag). 404/410 → `SaNotFound` (lookup→read
    /// deletion race — mirrors the secret-POST / revoke mapping).
    ///
    /// # What the bound flag means
    ///
    /// It answers **"did provisioning finish?"**, not "was this account
    /// provisioned under the current configuration". The two markers are
    /// therefore checked differently:
    ///
    /// * [`SA_TENANT_ID_ATTR`] must **equal** `tenant_id` — per-account data,
    ///   and the ownership fact `rotate_secret` relies on.
    /// * [`SA_USER_TYPE_ATTR`] is checked for **presence** only. Its value is
    ///   a deployment-wide constant, so any non-blank value proves the
    ///   attribute-PUT step ran, which is the whole invariant. The client was
    ///   already proven ours by `lookup_owned_client` (client-id prefix plus
    ///   the [`PROVISIONING_TENANT_ID_ATTR`] marker), so this marker adds no
    ///   ownership signal to re-verify.
    ///
    /// Comparing it against the current [`SUBJECT_SERVICE_TYPE`] would key
    /// account health on a versioned classification value: changing that
    /// constant would retroactively mark every already-provisioned account
    /// corrupt, and because only [`Self::rotate_secret_impl`] consults this
    /// flag the failure would be silent and lopsided — `rotate_secret` breaks
    /// fleet-wide while `list`
    /// and `revoke` keep working, with the documented recovery being
    /// re-issuance of every live credential. A classification constant is not
    /// worth that blast radius, and the value is already pinned at config
    /// load by `ServiceAccountConfig::validate`.
    async fn sa_user(
        &self,
        bootstrap: &KcAdminClient,
        client_uuid: Uuid,
        tenant_id: Uuid,
    ) -> Result<(Uuid, bool), PluginError> {
        let path_template = "/admin/realms/{realm}/clients/{uuid}/service-account-user";
        let url = format!(
            "{}/clients/{client_uuid}/service-account-user",
            self.admin_base()
        );
        let (status, body) = bootstrap
            .raw_request("GET", &url, None, path_template)
            .await?;
        if matches!(status, 404 | 410) {
            return Err(PluginError::SaNotFound {
                detail: format!(
                    "service-account user for client {client_uuid} absent (kc {status})"
                ),
            });
        }
        let user: Value = kc_json("GET", path_template, status, &body)?;
        let user_id = user["id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| PluginError::KcRest {
                method: "GET",
                path_template: path_template.into(),
                status: KcStatusKind::Http(status),
                body_first_2kb: "service-account-user missing parsable id".into(),
            })?;
        let attrs_bound = user
            .get("attributes")
            .and_then(Value::as_object)
            .is_some_and(|attrs| {
                let tenant_id = tenant_id.to_string();
                // `tenant_id` must MATCH: it is per-account data and the
                // ownership fact rotate depends on.
                attr_value_matches(attrs.get(SA_TENANT_ID_ATTR), &tenant_id)
                    // `user_type` is checked for PRESENCE only, deliberately
                    // NOT against the current constant — see the fn docs.
                    && attr_value_present(attrs.get(SA_USER_TYPE_ATTR))
            });
        Ok((user_id, attrs_bound))
    }

    /// Regenerate the account's secret (the old one stops working) and
    /// return fresh credentials. Times the operation and emits op-latency +
    /// failure metrics around [`Self::rotate_secret_impl`]. An incomplete /
    /// corrupt account is rejected with `SaInvalidInput` (recover via
    /// revoke + create) rather than handing back a secret for a half-repaired
    /// client.
    pub async fn rotate_secret_inner(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        client_id: &str,
    ) -> Result<IdpServiceAccountCredentials, PluginError> {
        let started = std::time::Instant::now();
        let result = self.rotate_secret_impl(ctx, tenant_id, client_id).await;
        let elapsed = started.elapsed().as_secs_f64();
        self.emit_outcome(
            SaOp::RotateSecret,
            PluginOp::SaRotateSecret,
            elapsed,
            &result,
        );
        result
    }

    async fn rotate_secret_impl(
        &self,
        _ctx: &SecurityContext,
        tenant_id: Uuid,
        client_id: &str,
    ) -> Result<IdpServiceAccountCredentials, PluginError> {
        let client_uuid = self.lookup_owned_client(tenant_id, client_id).await?;

        // Fetch subject_id BEFORE the secret POST — pure read, pre-mutation.
        let (subject_id, attrs_bound) = self
            .kc_factory
            .with_reactive_401_bootstrap(|bootstrap| async move {
                self.sa_user(&bootstrap, client_uuid, tenant_id).await
            })
            .await?;

        // Half-created or corrupt account. Rotate cannot reconstruct the
        // originally requested client scopes, so returning a fresh secret would
        // make the caller persist credentials for a partially repaired client.
        // Keep the documented recovery path explicit: revoke, then create.
        if !attrs_bound {
            return Err(PluginError::SaInvalidInput {
                detail: format!(
                    "service account '{client_id}' is incomplete or has corrupt managed attributes; recover with revoke + create"
                ),
                field: Some("client_id".into()),
            });
        }

        let secret = self
            .kc_factory
            .with_reactive_401_bootstrap(|bootstrap| {
                let client_id = client_id.to_owned();
                async move {
                    self.read_secret(
                        &bootstrap,
                        &client_id,
                        client_uuid,
                        "POST",
                        AmbiguousStage::KcClientSecretRotate,
                    )
                    .await
                }
            })
            .await?;

        Ok(IdpServiceAccountCredentials::new(
            client_id.to_owned(),
            secret,
            self.token_url(),
            subject_id,
        ))
    }

    /// Delete the account's client. Times the operation and emits the
    /// op-latency + failure metrics around [`Self::revoke_impl`]. A repeat
    /// revoke yields `SaNotFound`, which the SDK boundary treats as
    /// success-equivalent.
    pub async fn revoke_inner(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        client_id: &str,
    ) -> Result<(), PluginError> {
        let started = std::time::Instant::now();
        let result = self.revoke_impl(ctx, tenant_id, client_id).await;
        let elapsed = started.elapsed().as_secs_f64();
        self.emit_outcome(SaOp::Revoke, PluginOp::SaRevoke, elapsed, &result);
        result
    }

    async fn revoke_impl(
        &self,
        _ctx: &SecurityContext,
        tenant_id: Uuid,
        client_id: &str,
    ) -> Result<(), PluginError> {
        let client_uuid = self.lookup_owned_client(tenant_id, client_id).await?;
        self.delete_client_by_uuid(client_id, client_uuid).await
    }

    /// DELETE `.../clients/{uuid}`; 404/410 → `SaNotFound`.
    async fn delete_client_by_uuid(
        &self,
        client_id: &str,
        client_uuid: Uuid,
    ) -> Result<(), PluginError> {
        self.kc_factory
            .with_reactive_401_bootstrap(|bootstrap| {
                let client_id = client_id.to_owned();
                async move {
                    let url = format!("{}/clients/{client_uuid}", self.admin_base());
                    let (status, body) = bootstrap
                        .raw_request(
                            "DELETE",
                            &url,
                            None,
                            "/admin/realms/{realm}/clients/{uuid}",
                        )
                        .await
                        .map_err(|e| {
                            ambiguous_from_kc_error(AmbiguousStage::KcClientDelete, e)
                        })?;
                    // Without this carve-out the wrapper would never see a 401.
                    if status == 401 {
                        return Err(PluginError::KcRest {
                            method: "DELETE",
                            path_template: "/admin/realms/{realm}/clients/{uuid}".into(),
                            status: KcStatusKind::Http(401),
                            body_first_2kb: redact_secrets(&truncate_2kb(&body)),
                        });
                    }
                    match status {
                        200..=299 => Ok(()),
                        404 | 410 => Err(PluginError::SaNotFound {
                            detail: format!(
                                "client '{client_id}' already absent (kc {status})"
                            ),
                        }),
                        400..=499 if status != 429 => Err(PluginError::KcRest {
                            method: "DELETE",
                            path_template: "/admin/realms/{realm}/clients/{uuid}".into(),
                            status: KcStatusKind::Http(status),
                            body_first_2kb: redact_secrets(&truncate_2kb(&body)),
                        }),
                        _ => Err(PluginError::AmbiguousCreated {
                            stage: AmbiguousStage::KcClientDelete,
                            detail: format!(
                                "client '{client_id}': kc:DELETE /admin/realms/{{realm}}/clients/{{uuid}} -> {status}: {}",
                                redact_secrets(&truncate_2kb(&body))
                            ),
                        }),
                    }
                }
            })
            .await
    }

    /// List the tenant's owned service accounts (no secrets). Times the
    /// operation and emits op-latency + failure metrics around
    /// [`Self::list_impl`].
    pub async fn list_inner(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<
        Vec<account_management_sdk::idp_service_account::IdpServiceAccountSummary>,
        PluginError,
    > {
        let started = std::time::Instant::now();
        let result = self.list_impl(ctx, tenant_id).await;
        let elapsed = started.elapsed().as_secs_f64();
        self.emit_outcome(SaOp::List, PluginOp::SaList, elapsed, &result);
        result
    }

    async fn list_impl(
        &self,
        _ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<
        Vec<account_management_sdk::idp_service_account::IdpServiceAccountSummary>,
        PluginError,
    > {
        // Same derivation `list_owned_reps` filters on, reused here to recover
        // the caller-supplied name from each owned client id.
        let prefix = format!("{SP_CLIENT_ID_PREFIX}-{tenant_id}-");
        Ok(self
            .list_owned_reps(tenant_id)
            .await?
            .into_iter()
            .map(|r| {
                // The contract requires the caller-supplied `name` verbatim on
                // every entry: it is the only bridge from a name a caller
                // submitted back to an adapter-assigned client id, and the
                // whole ambiguous-create reconciliation path depends on it.
                // This adapter derives the client id as
                // `svc-<tenant_id>-<name>` (see `build_client_id`), so it can
                // invert its own construction — callers must not, which is why
                // the contract makes the adapter report it.
                let name = r
                    .client_id
                    .strip_prefix(&prefix)
                    .unwrap_or(&r.client_id)
                    .to_owned();
                account_management_sdk::idp_service_account::IdpServiceAccountSummary::new(
                    r.client_id,
                    name,
                    r.enabled,
                    r.default_client_scopes,
                )
            })
            .collect())
    }

    async fn purge_tenant_accounts_impl(&self, tenant_id: Uuid) -> Result<usize, PluginError> {
        let mut deleted_count = 0usize;
        for rep in self.list_owned_reps(tenant_id).await? {
            match self.delete_client_by_uuid(&rep.client_id, rep.id).await {
                Ok(()) => deleted_count += 1,
                Err(PluginError::SaNotFound { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(deleted_count)
    }
}

/// Tenant-deprovision cascade: delete every owned SA client so no live
/// `client_credentials` credential survives its tenant.
///
/// Implemented as a port rather than an inherent method so `TenantFacade` can
/// hold the cascade without depending on this module — see
/// [`crate::domain::ports::purge`].
#[async_trait::async_trait]
impl TenantAccountPurgeHook for ServiceAccountFacade {
    /// Concurrent-delete 404s are tolerated; other failures propagate so the
    /// deprovision caller retries.
    async fn purge_tenant_accounts(&self, tenant_id: Uuid) -> Result<usize, PluginError> {
        let started = std::time::Instant::now();
        let result = self.purge_tenant_accounts_impl(tenant_id).await;
        let elapsed = started.elapsed().as_secs_f64();
        self.emit_outcome(SaOp::Purge, PluginOp::SaPurge, elapsed, &result);
        result
    }
}

#[cfg(test)]
#[path = "service_account_facade_tests.rs"]
mod service_account_facade_tests;
