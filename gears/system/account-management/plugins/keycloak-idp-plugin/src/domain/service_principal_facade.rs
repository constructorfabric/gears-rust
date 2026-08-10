//! Service-principal facade — confidential `client_credentials` clients
//! in the platform realm: create / rotate secret / revoke.
//!
//! Claims ride the realm-default client scope (usermodel-attribute
//! mappers provisioned by realm bootstrap): the facade sets
//! `tenant_id` + `user_type` attributes on the client's service-account
//! user instead of installing per-client protocol mappers.

use std::sync::Arc;

use secrecy::SecretString;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use service_principal_sdk::{CreateServicePrincipalRequest, ServicePrincipalCredentials};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::ServicePrincipalConfig;
use crate::domain::error::{AmbiguousStage, KcStatusKind, PluginError, redact_secrets};
use crate::domain::kc::client::KcAdminClient;
use crate::domain::kc::factory::KeycloakAdminClientFactory;
use crate::domain::kc::transport::truncate_2kb;
use crate::domain::ports::metrics::{
    FailureMetricsPort, FailureVariant, PluginOp, SpOp, SpOpMetricsPort,
};
use crate::domain::tenant_facade::{PROVISIONING_TENANT_ID_ATTR, ambiguous_from_kc_error};

pub(crate) const SP_CLIENT_ID_PREFIX: &str = "svc";
const NAME_MAX_LEN: usize = 40;
const SA_TENANT_ID_ATTR: &str = "tenant_id";
const SA_USER_TYPE_ATTR: &str = "user_type";

/// `svc-<tenant_id>-<name>` — server-built, never caller-supplied.
pub(crate) fn build_client_id(tenant_id: Uuid, name: &str) -> String {
    format!("{SP_CLIENT_ID_PREFIX}-{tenant_id}-{name}")
}

/// Lowercase alnum + '-', 1..=40 chars.
pub(crate) fn validate_name(name: &str) -> Result<(), PluginError> {
    let ok = !name.is_empty()
        && name.len() <= NAME_MAX_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(PluginError::SpInvalidInput {
            detail: format!("name must be 1..={NAME_MAX_LEN} chars of [a-z0-9-], got '{name}'"),
            field: Some("name".into()),
        })
    }
}

/// Percent-encode a single path/query component. Uses `url::form_urlencoded`
/// (workspace-preferred over the standalone `urlencoding` crate). It encodes
/// space as `+` rather than `%20`; harmless here since every input is a realm
/// name or `svc-<uuid>-<name>` client id drawn from `[a-z0-9-]`.
fn encode_component(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
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
struct SpClientRep {
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

// Field subset of Keycloak's `CredentialRepresentation`; same as `SpClientRep`.
#[allow(unknown_lints, de0101_no_serde_in_contract)]
#[domain_model]
#[derive(Deserialize)]
struct SecretRep {
    value: String,
}

// Field subset of Keycloak's `ClientScopeRepresentation`; same as `SpClientRep`.
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

/// Return `true` when `rep.attributes` carries the
/// [`PROVISIONING_TENANT_ID_ATTR`] value matching `tenant_id`. Tolerates both
/// scalar and singleton-array KC encodings, but rejects multi-valued markers.
fn owner_matches(rep: &SpClientRep, tenant_id: Uuid) -> bool {
    let Some(Value::Object(attrs)) = rep.attributes.as_ref() else {
        return false;
    };
    let target = tenant_id.to_string();
    attr_value_matches(attrs.get(PROVISIONING_TENANT_ID_ATTR), &target)
}

#[domain_model]
pub struct ServicePrincipalFacade {
    cfg: ServicePrincipalConfig,
    kc_factory: Arc<KeycloakAdminClientFactory>,
    sp_metrics: Arc<dyn SpOpMetricsPort>,
    failure_metrics: Arc<dyn FailureMetricsPort>,
}

impl ServicePrincipalFacade {
    /// Construct the facade over a Keycloak admin-client factory and the
    /// segregated metric ports. Holds no per-call state; clone-cheap via the
    /// `Arc`-shared factory and metric sinks.
    pub fn new(
        cfg: ServicePrincipalConfig,
        kc_factory: Arc<KeycloakAdminClientFactory>,
        sp_metrics: Arc<dyn SpOpMetricsPort>,
        failure_metrics: Arc<dyn FailureMetricsPort>,
    ) -> Self {
        Self {
            cfg,
            kc_factory,
            sp_metrics,
            failure_metrics,
        }
    }

    /// Emit op latency (success AND failure, mirroring `user_op_duration`)
    /// plus the failure counter on errors.
    fn emit_outcome<T>(
        &self,
        op: SpOp,
        plugin_op: PluginOp,
        elapsed: f64,
        result: &Result<T, PluginError>,
    ) {
        self.sp_metrics.sp_op_duration(op, elapsed);
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
                return Err(PluginError::SpInvalidInput {
                    detail: format!("scope '{s}' is not in the configured allowlist"),
                    field: Some("scopes".into()),
                });
            }
        }
        Ok(())
    }

    /// Create a confidential `client_credentials` client owned by
    /// `req.tenant_id`, returning its live credentials (secret included).
    /// Times the operation and emits the op-latency + failure metrics around
    /// [`Self::create_impl`]. See the SDK [`create`] contract for the
    /// rejection / ambiguity semantics.
    ///
    /// [`create`]: service_principal_sdk::ServicePrincipalClientV1::create
    pub async fn create_inner(
        &self,
        ctx: &SecurityContext,
        req: &CreateServicePrincipalRequest,
    ) -> Result<ServicePrincipalCredentials, PluginError> {
        let started = std::time::Instant::now();
        let result = self.create_impl(ctx, req).await;
        let elapsed = started.elapsed().as_secs_f64();
        self.emit_outcome(SpOp::Create, PluginOp::SpCreate, elapsed, &result);
        result
    }

    async fn create_impl(
        &self,
        _ctx: &SecurityContext,
        req: &CreateServicePrincipalRequest,
    ) -> Result<ServicePrincipalCredentials, PluginError> {
        validate_name(&req.name)?;
        self.validate_scopes(&req.scopes)?;
        let tenant_id = req.tenant_id.0;

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

        Ok(ServicePrincipalCredentials {
            client_id,
            client_secret: SecretString::from(secret),
            token_url: self.token_url(),
            subject_id,
        })
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
    async fn list_owned_reps(&self, tenant_id: Uuid) -> Result<Vec<SpClientRep>, PluginError> {
        let prefix = format!("{SP_CLIENT_ID_PREFIX}-{tenant_id}-");
        // `search=true` is KC's INFIX match, not exact-prefix — it widens the
        // result set rather than narrowing it. Ownership is enforced
        // client-side below via `starts_with(prefix)` + `owner_matches`.
        let url = format!(
            "{}/clients?clientId={}&search=true",
            self.admin_base(),
            encode_component(&prefix),
        );
        let reps: Vec<SpClientRep> = self
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
        Ok(reps
            .into_iter()
            .filter(|r| r.client_id.starts_with(&prefix) && owner_matches(r, tenant_id))
            .collect())
    }

    /// Count `svc-<tenant>-` clients; reject at the configured quota.
    /// Best-effort cap: no cross-replica serialization, concurrent
    /// creates may briefly overshoot.
    async fn enforce_quota(&self, tenant_id: Uuid) -> Result<(), PluginError> {
        let count = self.list_owned_reps(tenant_id).await?.len();
        if count >= self.cfg.per_tenant_quota as usize {
            return Err(PluginError::SpInvalidInput {
                detail: format!(
                    "tenant {tenant_id} already has {count} service principals (quota {})",
                    self.cfg.per_tenant_quota,
                ),
                field: Some("tenant_id".into()),
            });
        }
        Ok(())
    }

    /// POST the confidential client_credentials-only client.
    ///
    /// 409 → `SpInvalidInput` (strict reject, regardless of owner): a
    /// name collision must not resume another consumer's principal —
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
            409 => Err(PluginError::SpInvalidInput {
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
    ) -> Result<Option<SpClientRep>, PluginError> {
        let url = format!(
            "{}/clients?clientId={}",
            self.admin_base(),
            encode_component(client_id),
        );
        let (status, body) = bootstrap
            .raw_request("GET", &url, None, "/admin/realms/{realm}/clients")
            .await?;
        let mut reps: Vec<SpClientRep> =
            kc_json("GET", "/admin/realms/{realm}/clients", status, &body)?;
        Ok(if reps.is_empty() {
            None
        } else {
            Some(reps.remove(0))
        })
    }

    /// GET-modify-PUT the SA user's attributes: `tenant_id` + `user_type`.
    /// Returns the SA user's UUID — the principal's subject id.
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
            .entry("attributes".to_string())
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

        attrs_obj.insert(
            SA_TENANT_ID_ATTR.to_string(),
            json!([tenant_id.to_string()]),
        );
        attrs_obj.insert(
            SA_USER_TYPE_ATTR.to_string(),
            json!([self.cfg.subject_type]),
        );

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

    /// `GET` or `POST` `.../clients/{uuid}/client-secret` → plaintext secret value.
    ///
    /// Pass `method = "GET"` for initial secret read, `method = "POST"` for rotation.
    /// 404/410 on POST map to `SpNotFound` (lookup→rotate deletion race).
    /// Non-2xx on GET maps to `AmbiguousCreated` (post-mutation read).
    async fn read_secret(
        &self,
        bootstrap: &KcAdminClient,
        client_id: &str,
        client_uuid: Uuid,
        method: &'static str,
        stage: AmbiguousStage,
    ) -> Result<String, PluginError> {
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
            return Err(PluginError::SpNotFound {
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

    /// Exact ownership-checked lookup. Absent or foreign → `SpNotFound`.
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
            _ => Err(PluginError::SpNotFound {
                detail: format!("service principal '{client_id}' not found for tenant {tenant_id}"),
            }),
        }
    }

    /// `GET .../clients/{uuid}/service-account-user` → (SA user UUID,
    /// managed-attrs-bound flag). 404/410 → `SpNotFound` (lookup→read
    /// deletion race — mirrors the secret-POST / revoke mapping).
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
            return Err(PluginError::SpNotFound {
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
                attr_value_matches(attrs.get(SA_TENANT_ID_ATTR), &tenant_id)
                    && attr_value_matches(
                        attrs.get(SA_USER_TYPE_ATTR),
                        self.cfg.subject_type.as_str(),
                    )
            });
        Ok((user_id, attrs_bound))
    }

    /// Regenerate the principal's secret (the old one stops working) and
    /// return fresh credentials. Times the operation and emits op-latency +
    /// failure metrics around [`Self::rotate_secret_impl`]. An incomplete /
    /// corrupt principal is rejected with `SpInvalidInput` (recover via
    /// revoke + create) rather than handing back a secret for a half-repaired
    /// client.
    pub async fn rotate_secret_inner(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        client_id: &str,
    ) -> Result<ServicePrincipalCredentials, PluginError> {
        let started = std::time::Instant::now();
        let result = self.rotate_secret_impl(ctx, tenant_id, client_id).await;
        let elapsed = started.elapsed().as_secs_f64();
        self.emit_outcome(
            SpOp::RotateSecret,
            PluginOp::SpRotateSecret,
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
    ) -> Result<ServicePrincipalCredentials, PluginError> {
        let client_uuid = self.lookup_owned_client(tenant_id, client_id).await?;

        // Fetch subject_id BEFORE the secret POST — pure read, pre-mutation.
        let (subject_id, attrs_bound) = self
            .kc_factory
            .with_reactive_401_bootstrap(|bootstrap| async move {
                self.sa_user(&bootstrap, client_uuid, tenant_id).await
            })
            .await?;

        // Half-created or corrupt principal. Rotate cannot reconstruct the
        // originally requested client scopes, so returning a fresh secret would
        // make the caller persist credentials for a partially repaired client.
        // Keep the documented recovery path explicit: revoke, then create.
        if !attrs_bound {
            return Err(PluginError::SpInvalidInput {
                detail: format!(
                    "service principal '{client_id}' is incomplete or has corrupt managed attributes; recover with revoke + create"
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

        Ok(ServicePrincipalCredentials {
            client_id: client_id.to_owned(),
            client_secret: SecretString::from(secret),
            token_url: self.token_url(),
            subject_id,
        })
    }

    /// Delete the principal's client. Times the operation and emits the
    /// op-latency + failure metrics around [`Self::revoke_impl`]. A repeat
    /// revoke yields `SpNotFound`, which the SDK boundary treats as
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
        self.emit_outcome(SpOp::Revoke, PluginOp::SpRevoke, elapsed, &result);
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

    /// DELETE `.../clients/{uuid}`; 404/410 → `SpNotFound`.
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
                        404 | 410 => Err(PluginError::SpNotFound {
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

    /// List the tenant's owned service principals (no secrets). Times the
    /// operation and emits op-latency + failure metrics around
    /// [`Self::list_impl`].
    pub async fn list_inner(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Vec<service_principal_sdk::ServicePrincipalSummary>, PluginError> {
        let started = std::time::Instant::now();
        let result = self.list_impl(ctx, tenant_id).await;
        let elapsed = started.elapsed().as_secs_f64();
        self.emit_outcome(SpOp::List, PluginOp::SpList, elapsed, &result);
        result
    }

    async fn list_impl(
        &self,
        _ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Vec<service_principal_sdk::ServicePrincipalSummary>, PluginError> {
        Ok(self
            .list_owned_reps(tenant_id)
            .await?
            .into_iter()
            .map(|r| service_principal_sdk::ServicePrincipalSummary {
                client_id: r.client_id,
                enabled: r.enabled,
                scopes: r.default_client_scopes,
            })
            .collect())
    }

    /// Tenant-deprovision cascade: delete every owned SP client so no
    /// live `client_credentials` credential survives its tenant.
    /// Concurrent-delete 404s are tolerated; other failures propagate so
    /// the deprovision caller retries.
    pub async fn purge_tenant_principals(&self, tenant_id: Uuid) -> Result<usize, PluginError> {
        let started = std::time::Instant::now();
        let result = self.purge_tenant_principals_impl(tenant_id).await;
        let elapsed = started.elapsed().as_secs_f64();
        self.emit_outcome(SpOp::Purge, PluginOp::SpPurge, elapsed, &result);
        result
    }

    async fn purge_tenant_principals_impl(&self, tenant_id: Uuid) -> Result<usize, PluginError> {
        let mut deleted_count = 0usize;
        for rep in self.list_owned_reps(tenant_id).await? {
            match self.delete_client_by_uuid(&rep.client_id, rep.id).await {
                Ok(()) => deleted_count += 1,
                Err(PluginError::SpNotFound { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(deleted_count)
    }
}

#[cfg(test)]
#[path = "service_principal_facade_tests.rs"]
mod service_principal_facade_tests;
