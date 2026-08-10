use super::*;
use crate::domain::error::{AmbiguousStage, KcStatusKind, PluginError};
use crate::domain::metadata_codec::DecodeError;

// ----- provision_failure translations -----

#[test]
fn translate_provision_kc_404_is_clean_failure() {
    let e = PluginError::KcRest {
        method: "GET",
        path_template: "/admin/realms/{realm}".into(),
        status: KcStatusKind::Http(404),
        body_first_2kb: "missing".into(),
    };
    assert!(matches!(
        translate_provision_failure(e),
        IdpProvisionFailure::CleanFailure { .. }
    ));
}

#[test]
fn translate_provision_kc_500_is_ambiguous() {
    let e = PluginError::KcRest {
        method: "POST",
        path_template: "/admin/realms".into(),
        status: KcStatusKind::Http(500),
        body_first_2kb: "boom".into(),
    };
    assert!(matches!(
        translate_provision_failure(e),
        IdpProvisionFailure::Ambiguous { .. }
    ));
}

#[test]
fn translate_provision_kc_transport_is_ambiguous() {
    let e = PluginError::KcRest {
        method: "POST",
        path_template: "/admin/realms".into(),
        status: KcStatusKind::Transport,
        body_first_2kb: "connection refused".into(),
    };
    assert!(matches!(
        translate_provision_failure(e),
        IdpProvisionFailure::Ambiguous { .. }
    ));
}

#[test]
fn translate_provision_kc_timeout_is_ambiguous() {
    let e = PluginError::KcRest {
        method: "GET",
        path_template: "/admin/realms/{realm}".into(),
        status: KcStatusKind::Timeout,
        body_first_2kb: "deadline exceeded".into(),
    };
    assert!(matches!(
        translate_provision_failure(e),
        IdpProvisionFailure::Ambiguous { .. }
    ));
}

#[test]
fn translate_provision_bootstrap_perms_missing_is_unsupported() {
    let e = PluginError::BootstrapPermsMissing {
        realm: "partner-acme".into(),
    };
    let f = translate_provision_failure(e);
    assert!(matches!(
        f,
        IdpProvisionFailure::UnsupportedOperation { .. }
    ));
    assert!(
        f.detail()
            .contains("bootstrap admin lacks view-realm/query-groups"),
        "expected runbook-friendly detail, got: {}",
        f.detail()
    );
}

#[test]
fn translate_provision_ambiguous_created_is_ambiguous_with_prefix() {
    let e = PluginError::AmbiguousCreated {
        stage: AmbiguousStage::KcRealmCreate,
        detail: "kc:POST /admin/realms -> 500: boom".into(),
    };
    let f = translate_provision_failure(e);
    assert!(matches!(f, IdpProvisionFailure::Ambiguous { .. }));
    assert!(
        f.detail().starts_with("ambig:kc_realm_create:"),
        "expected ambig:kc_realm_create: prefix, got: {}",
        f.detail()
    );
}

#[test]
fn translate_provision_created_realm_exists_is_clean_failure() {
    let e = PluginError::CreatedRealmExists {
        realm: "partner-acme".into(),
    };
    let f = translate_provision_failure(e);
    assert!(matches!(f, IdpProvisionFailure::CleanFailure { .. }));
    assert!(f.detail().contains("partner-acme"));
    assert!(f.detail().contains("already exists"));
}

/// Malformed wire shape of `provisioning_metadata` (serde decode
/// failure, missing required fields, unknown variants) is a permanent
/// client error — the plugin proved no KC call was made because the
/// input failed shape-validation up front. Translate to
/// [`IdpProvisionFailure::InvalidInput`] so AM surfaces
/// `CanonicalError::InvalidArgument` (HTTP 400) rather than the
/// `CleanFailure` 503 "retry later" shape, which would mislead callers
/// into retrying a permanent error.
#[test]
fn translate_provision_metadata_decode_is_invalid_input() {
    let e = PluginError::MetadataDecode(DecodeError::MissingVersion);
    let f = translate_provision_failure(e);
    assert!(
        matches!(f, IdpProvisionFailure::InvalidInput { .. }),
        "expected InvalidInput, got {f:?}"
    );
    // The `field` must point at the offending input key so the AM
    // canonical envelope can populate `field_violations` with a
    // useful pointer.
    let IdpProvisionFailure::InvalidInput { field, .. } = f else {
        unreachable!()
    };
    assert_eq!(field.as_deref(), Some("provisioning_metadata"));
}

/// VHP-1836: `ProvisionInputRejected` is the dedicated pre-KC caller-input
/// rejection (created+explicit-realm, shared-with-no-parent-metadata, bad
/// mode/realm shape). Unlike `Config`, it is emitted only by `parse_input`
/// before any KC call, so it maps cleanly to
/// [`IdpProvisionFailure::InvalidInput`] (HTTP 400) with the
/// `provisioning_metadata` field pointer.
#[test]
fn provision_input_rejected_maps_to_invalid_input() {
    let e = PluginError::ProvisionInputRejected {
        detail: "created + explicit realm_name".into(),
    };
    let f = translate_provision_failure(e);
    assert!(
        matches!(f, IdpProvisionFailure::InvalidInput { .. }),
        "expected InvalidInput, got {f:?}"
    );
    let IdpProvisionFailure::InvalidInput { field, .. } = f else {
        unreachable!()
    };
    assert_eq!(field.as_deref(), Some("provisioning_metadata"));
}

/// `PluginError::Config` maps to `CleanFailure` (503), NOT `InvalidInput`
/// (400). Since VHP-1836 the pre-KC caller-input rejections moved to
/// `ProvisionInputRejected` (→ 400); what still emits `Config` is the
/// post-KC-creation paths (`SecretRef::new(...)` in
/// `TenantFacade::provision_created` / `provision_shared`, AFTER realm +
/// admin-client exist in KC). Tagging THOSE as `InvalidInput` would tell AM
/// to compensate the row and return 400 to the caller, leaking real KC state
/// with no reaper run — so `Config` stays in the conservative `CleanFailure`
/// bucket. This pins that boundary mapping.
#[test]
fn translate_provision_config_is_clean_failure() {
    let e = PluginError::Config {
        detail: "secret_ref invalid for realm-admin client secret".into(),
    };
    let f = translate_provision_failure(e);
    assert!(
        matches!(f, IdpProvisionFailure::CleanFailure { .. }),
        "expected CleanFailure (Config is too broad to be InvalidInput; see translate_provision_failure docs), got {f:?}"
    );
    assert_eq!(
        f.detail(),
        "secret_ref invalid for realm-admin client secret"
    );
}

/// The pre-saga KC health-probe failure also surfaces via `PluginError::Config`
/// (`provision_tenant_saga` re-tags the probe's transport error as `Config`
/// because nothing has been mutated yet). Same translation rule as above —
/// `Config` collapses to `CleanFailure` (503 retry-later), which is exactly
/// right for a transient KC-unreachable probe.
#[test]
fn translate_provision_config_from_health_probe_is_clean_failure() {
    let e = PluginError::Config {
        detail: "pre-saga KC health probe failed: connection refused".into(),
    };
    let f = translate_provision_failure(e);
    assert!(
        matches!(f, IdpProvisionFailure::CleanFailure { .. }),
        "expected CleanFailure, got {f:?}"
    );
    assert!(f.detail().contains("health probe failed"));
}

// ----- deprovision_failure translations -----

#[test]
fn translate_deprovision_not_found_is_not_found() {
    let e = PluginError::DeprovisionNotFound {
        realm_name: "partner-acme".into(),
    };
    assert!(matches!(
        translate_deprovision_failure(e),
        IdpDeprovisionFailure::NotFound { .. }
    ));
}

#[test]
fn translate_deprovision_retryable_is_retryable() {
    let e = PluginError::DeprovisionRetryable {
        detail: "kc:DELETE /admin/realms/{realm}/groups/{tenant_group_id} -> 503: boom".into(),
    };
    assert!(matches!(
        translate_deprovision_failure(e),
        IdpDeprovisionFailure::Retryable { .. }
    ));
}

#[test]
fn translate_deprovision_terminal_is_terminal() {
    let e = PluginError::DeprovisionTerminal {
        detail: "metadata corrupt".into(),
    };
    assert!(matches!(
        translate_deprovision_failure(e),
        IdpDeprovisionFailure::Terminal { .. }
    ));
}

#[test]
fn translate_deprovision_bootstrap_perms_missing_is_terminal() {
    let e = PluginError::BootstrapPermsMissing {
        realm: "partner-acme".into(),
    };
    assert!(matches!(
        translate_deprovision_failure(e),
        IdpDeprovisionFailure::Terminal { .. }
    ));
}

#[test]
fn translate_deprovision_metadata_decode_is_terminal() {
    let e = PluginError::MetadataDecode(DecodeError::UnsupportedVersion {
        observed: "v2".into(),
    });
    assert!(matches!(
        translate_deprovision_failure(e),
        IdpDeprovisionFailure::Terminal { .. }
    ));
}

// ----- user-op failure translations -----

#[test]
fn translate_user_op_rejected_is_rejected() {
    let e = PluginError::UserOpRejected {
        detail: "user already exists".into(),
    };
    let f = translate_user_op_failure(e);
    assert!(matches!(f, IdpUserOperationFailure::Rejected { .. }));
    assert_eq!(f.detail(), "user already exists");
}

// VHP-2158: classified rejections must ride their typed SDK variants —
// a fallback to `Rejected`/`Unavailable` here would silently revert the
// duplicate/password classification to the redacted generic envelope.
#[test]
fn translate_user_op_duplicate_is_duplicate_user() {
    let e = PluginError::UserOpDuplicate {
        field: account_management_sdk::IdpUserDuplicateField::Username,
        detail: "kc 409".into(),
    };
    let f = translate_user_op_failure(e);
    assert!(matches!(
        f,
        IdpUserOperationFailure::DuplicateUser {
            field: account_management_sdk::IdpUserDuplicateField::Username,
            ..
        }
    ));
}

#[test]
fn translate_user_op_password_policy_is_password_policy() {
    let e = PluginError::UserOpPasswordPolicy {
        detail: "kc 400 policy".into(),
    };
    let f = translate_user_op_failure(e);
    assert!(matches!(f, IdpUserOperationFailure::PasswordPolicy { .. }));
}

#[test]
fn translate_user_op_unavailable_is_unavailable() {
    let e = PluginError::UserOpUnavailable {
        detail: "kc:POST /admin/realms/{realm}/users -> 503: boom".into(),
    };
    let f = translate_user_op_failure(e);
    assert!(matches!(f, IdpUserOperationFailure::Unavailable { .. }));
    assert!(f.detail().contains("503"));
}

#[test]
fn translate_user_op_unsupported_is_unsupported() {
    let e = PluginError::UserOpUnsupported {
        detail: "provider is read-only".into(),
    };
    let f = translate_user_op_failure(e);
    assert!(matches!(
        f,
        IdpUserOperationFailure::UnsupportedOperation { .. }
    ));
    assert_eq!(f.detail(), "provider is read-only");
}

#[test]
fn translate_user_op_leaked_kc_rest_is_unavailable() {
    // Defensive: if a `KcRest` ever leaks past the user-side translator
    // in the facade, the boundary still maps it to Unavailable so AM
    // surfaces the right public envelope.
    let e = PluginError::KcRest {
        method: "GET",
        path_template: "/admin/realms/{realm}/users".into(),
        status: KcStatusKind::Http(500),
        body_first_2kb: "boom".into(),
    };
    assert!(matches!(
        translate_user_op_failure(e),
        IdpUserOperationFailure::Unavailable { .. }
    ));
}

// ----- deprovision_tenant service-principal cascade -----

mod deprovision_cascade {
    use std::sync::Arc;

    use account_management_sdk::idp::IdpDeprovisionTenantRequest;
    use account_management_sdk::idp_user::IdpTenantContext;
    use gts::GtsTypeId;
    use serde_json::json;
    use uuid::Uuid;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::config::{ServicePrincipalConfig, TenantFacadeConfig, UserFacadeConfig};
    use crate::domain::credstore::{CredStoreReader, CredStoreWriter};
    use crate::domain::kc::factory::KeycloakAdminClientFactory;
    use crate::domain::kc::transport::KcTransport;
    use crate::domain::metadata_codec::RealmBinding;
    use crate::domain::service_principal_facade::ServicePrincipalFacade;
    use crate::domain::system_actor::build_system_ctx;
    use crate::domain::tenant_facade::TenantFacade;
    use crate::domain::test_support::{
        StubCredStore, keycloak_cfg, make_metadata, noop_credstore_metrics, noop_failure_metrics,
        noop_metadata_metrics, noop_sp_metrics, noop_tenant_metrics, noop_user_metrics,
    };
    use crate::domain::user_facade::UserFacade;
    use crate::infra::kc_http::ReqwestKcTransport;

    fn build_plugin(server: &MockServer) -> KeycloakIdpPlugin {
        let kc_cfg = keycloak_cfg(&server.uri());
        let realm_admin_cfg = kc_cfg.realm_admin.clone();
        let cs: Arc<dyn credstore_sdk::CredStoreClientV1> = Arc::new(StubCredStore);
        let cs_reader = CredStoreReader::new(Arc::clone(&cs), build_system_ctx(Uuid::nil()));
        let cs_writer = CredStoreWriter::new(cs, build_system_ctx(Uuid::nil()));
        let transport: Arc<dyn KcTransport> =
            Arc::new(ReqwestKcTransport::new(reqwest::Client::new()));
        let factory = Arc::new(KeycloakAdminClientFactory::new(
            kc_cfg,
            transport,
            cs_reader,
            Arc::new(crate::domain::test_support::NoopMetrics),
        ));
        let codec = Arc::new(MetadataCodec);
        let tenant_facade = Arc::new(TenantFacade::new(
            TenantFacadeConfig {
                realm_defaults: serde_json::Value::Null,
                tenant_group_root: "/tenants".into(),
                provision_timeout_ms: 30_000,
            },
            realm_admin_cfg.clone(),
            Arc::clone(&factory),
            Arc::clone(&codec),
            cs_writer,
            noop_tenant_metrics(),
            noop_credstore_metrics(),
            noop_metadata_metrics(),
            noop_failure_metrics(),
        ));
        let user_facade = Arc::new(UserFacade::new(
            UserFacadeConfig {
                default_user_required_actions: vec![],
                list_users_page_limit_default: 50,
                list_users_page_limit_max: 200,
                session_revoke_on_deprovision: true,
            },
            realm_admin_cfg,
            Arc::clone(&factory),
            Arc::clone(&codec),
            noop_user_metrics(),
            noop_metadata_metrics(),
            noop_failure_metrics(),
        ));
        let sp_facade = Arc::new(ServicePrincipalFacade::new(
            ServicePrincipalConfig::default(),
            factory,
            noop_sp_metrics(),
            noop_failure_metrics(),
        ));
        tenant_facade.set_service_principal_purge_facade(Arc::clone(&sp_facade));
        KeycloakIdpPlugin {
            tenant_facade,
            user_facade,
            codec,
            sp_facade,
        }
    }

    async fn mount_tokens_and_probe(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/realms/master"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/realms/master/protocol/openid-connect/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "t-master",
                "expires_in": 600
            })))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/realms/platform/protocol/openid-connect/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "t-realm",
                "expires_in": 600
            })))
            .mount(server)
            .await;
    }

    fn deprovision_req(tenant_id: Uuid, group_uuid: Uuid) -> IdpDeprovisionTenantRequest {
        IdpDeprovisionTenantRequest::new(IdpTenantContext::new(
            tenant_id,
            "stub-tenant",
            GtsTypeId::new("gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~"),
            Some(make_metadata(
                "platform",
                RealmBinding::Shared,
                group_uuid,
                None,
            )),
        ))
    }

    fn deprovision_req_without_metadata(tenant_id: Uuid) -> IdpDeprovisionTenantRequest {
        IdpDeprovisionTenantRequest::new(IdpTenantContext::new(
            tenant_id,
            "stub-tenant",
            GtsTypeId::new("gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~"),
            None,
        ))
    }

    /// Missing metadata is a local deprovision fast-path. The SP purge hook
    /// must not force a live KC/SP realm before the saga classifies it as
    /// success-equivalent `NotFound`.
    #[tokio::test]
    async fn deprovision_tenant_missing_metadata_does_not_purge_service_principals() {
        let server = MockServer::start().await;
        let tenant = Uuid::new_v4();

        Mock::given(method("GET"))
            .and(path("/admin/realms/platform/clients"))
            .and(query_param("search", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(0)
            .mount(&server)
            .await;

        let plugin = build_plugin(&server);
        let ctx = build_system_ctx(Uuid::nil());
        let err = plugin
            .deprovision_tenant(&ctx, &deprovision_req_without_metadata(tenant))
            .await
            .expect_err("missing metadata maps to NotFound");
        assert!(
            matches!(err, IdpDeprovisionFailure::NotFound { .. }),
            "expected NotFound, got {err:?}"
        );
    }

    /// Tenant teardown deletes the tenant's service principals before the
    /// group — no orphaned `client_credentials` clients survive.
    #[tokio::test]
    async fn deprovision_tenant_purges_service_principals() {
        let server = MockServer::start().await;
        mount_tokens_and_probe(&server).await;
        let tenant = Uuid::new_v4();
        let group_uuid = Uuid::new_v4();
        let sp_uuid = "55555555-5555-5555-5555-555555555555";

        Mock::given(method("GET"))
            .and(path("/admin/realms/platform/clients"))
            .and(query_param("search", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id": sp_uuid,
                "clientId": format!("svc-{tenant}-metrics"),
                "attributes": { "vhp.provisioning.tenant_id": [tenant.to_string()] },
            }])))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("/admin/realms/platform/clients/{sp_uuid}")))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("/admin/realms/platform/groups/{group_uuid}")))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let plugin = build_plugin(&server);
        let ctx = build_system_ctx(Uuid::nil());
        plugin
            .deprovision_tenant(&ctx, &deprovision_req(tenant, group_uuid))
            .await
            .expect("deprovision with SP cascade must succeed");
    }

    /// A failed purge blocks teardown and surfaces Retryable so AM's
    /// retention loop retries the whole deprovision.
    #[tokio::test]
    async fn deprovision_tenant_purge_failure_is_retryable_and_blocks_teardown() {
        let server = MockServer::start().await;
        mount_tokens_and_probe(&server).await;
        let tenant = Uuid::new_v4();
        let group_uuid = Uuid::new_v4();

        Mock::given(method("GET"))
            .and(path("/admin/realms/platform/clients"))
            .and(query_param("search", "true"))
            .respond_with(ResponseTemplate::new(503).set_body_string("kc down"))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("/admin/realms/platform/groups/{group_uuid}")))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;

        let plugin = build_plugin(&server);
        let ctx = build_system_ctx(Uuid::nil());
        let err = plugin
            .deprovision_tenant(&ctx, &deprovision_req(tenant, group_uuid))
            .await
            .expect_err("purge failure must fail deprovision");
        assert!(
            matches!(err, IdpDeprovisionFailure::Retryable { .. }),
            "expected Retryable, got {err:?}"
        );
    }

    /// Permanent SP-realm failures park the AM retention row instead of
    /// retrying forever. A 403 means the bootstrap admin lacks SP-realm
    /// permissions, so operator intervention is required.
    #[tokio::test]
    async fn deprovision_tenant_purge_403_is_terminal_and_blocks_teardown() {
        let server = MockServer::start().await;
        mount_tokens_and_probe(&server).await;
        let tenant = Uuid::new_v4();
        let group_uuid = Uuid::new_v4();

        Mock::given(method("GET"))
            .and(path("/admin/realms/platform/clients"))
            .and(query_param("search", "true"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("/admin/realms/platform/groups/{group_uuid}")))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;

        let plugin = build_plugin(&server);
        let ctx = build_system_ctx(Uuid::nil());
        let err = plugin
            .deprovision_tenant(&ctx, &deprovision_req(tenant, group_uuid))
            .await
            .expect_err("permanent purge failure must fail deprovision");
        assert!(
            matches!(err, IdpDeprovisionFailure::Terminal { .. }),
            "expected Terminal, got {err:?}"
        );
    }
}
