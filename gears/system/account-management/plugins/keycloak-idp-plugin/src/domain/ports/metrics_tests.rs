use std::sync::Arc;

use super::*;
use crate::domain::error::{PluginError, failure_variant_label};
use crate::domain::metadata_codec::DecodeError;
use crate::domain::test_support::NoopMetrics;

// ---- SpOp label pins ----

#[test]
fn sp_op_as_str_stable() {
    assert_eq!(SpOp::Create.as_str(), "sp_create");
    assert_eq!(SpOp::RotateSecret.as_str(), "sp_rotate_secret");
    assert_eq!(SpOp::Revoke.as_str(), "sp_revoke");
    assert_eq!(SpOp::List.as_str(), "sp_list");
    assert_eq!(SpOp::Purge.as_str(), "sp_purge");
}

// ---- closed-set label enums round-trip the strings they advertise ----

#[test]
fn user_op_as_str_stable() {
    assert_eq!(UserOp::ProvisionUser.as_str(), "provision_user");
    assert_eq!(UserOp::DeprovisionUser.as_str(), "deprovision_user");
    assert_eq!(UserOp::ListUsers.as_str(), "list_users");
}

#[test]
fn token_tier_as_str_stable() {
    assert_eq!(TokenTier::StaticEnv.as_str(), "static_env");
    assert_eq!(TokenTier::OpenBao.as_str(), "openbao");
}

#[test]
fn plugin_op_as_str_stable() {
    assert_eq!(PluginOp::ProvisionTenant.as_str(), "provision_tenant");
    assert_eq!(PluginOp::DeprovisionTenant.as_str(), "deprovision_tenant");
    assert_eq!(PluginOp::ListUsers.as_str(), "list_users");
    assert_eq!(PluginOp::SpCreate.as_str(), "sp_create");
    assert_eq!(PluginOp::SpRotateSecret.as_str(), "sp_rotate_secret");
    assert_eq!(PluginOp::SpRevoke.as_str(), "sp_revoke");
    assert_eq!(PluginOp::SpList.as_str(), "sp_list");
    assert_eq!(PluginOp::SpPurge.as_str(), "sp_purge");
}

#[test]
fn credstore_op_outcome_as_str_stable() {
    assert_eq!(CredstoreOp::Put.as_str(), "put");
    assert_eq!(CredstoreOp::Delete.as_str(), "delete");
    assert_eq!(CredstoreOutcome::Ok.as_str(), "ok");
    assert_eq!(CredstoreOutcome::Error.as_str(), "error");
}

// ---- sealed newtypes mirror failure_variant_label exactly ----

#[test]
fn failure_variant_constants_match_failure_variant_label() {
    // Every PluginError variant must produce the same string via both
    // FailureVariant::from(&err) and failure_variant_label(&err) —
    // pins the bridge to the source of truth.
    let cases: &[(PluginError, &str)] = &[
        (
            PluginError::Config {
                detail: String::new(),
            },
            "config",
        ),
        (
            PluginError::CredStoreRead {
                detail: String::new(),
            },
            "credstore_read",
        ),
        (
            PluginError::MetadataDecode(DecodeError::MissingVersion),
            "metadata_decode",
        ),
        (
            PluginError::KcRest {
                method: "GET",
                path_template: String::new(),
                status: crate::domain::error::KcStatusKind::Http(500),
                body_first_2kb: String::new(),
            },
            "kc_rest",
        ),
        (
            PluginError::AmbiguousCreated {
                stage: crate::domain::error::AmbiguousStage::KcRealmCreate,
                detail: String::new(),
            },
            "ambiguous_created",
        ),
        (
            PluginError::CreatedRealmExists {
                realm: String::new(),
            },
            "created_realm_exists",
        ),
        (
            PluginError::BootstrapPermsMissing {
                realm: String::new(),
            },
            "bootstrap_perms_missing",
        ),
        (
            PluginError::DeprovisionNotFound {
                realm_name: String::new(),
            },
            "deprovision_not_found",
        ),
        (
            PluginError::DeprovisionRetryable {
                detail: String::new(),
            },
            "deprovision_retryable",
        ),
        (
            PluginError::DeprovisionTerminal {
                detail: String::new(),
            },
            "deprovision_terminal",
        ),
        (
            PluginError::UserOpRejected {
                detail: String::new(),
            },
            "user_op_rejected",
        ),
        (
            PluginError::UserOpUnavailable {
                detail: String::new(),
            },
            "user_op_unavailable",
        ),
        (
            PluginError::UserOpUnsupported {
                detail: String::new(),
            },
            "user_op_unsupported",
        ),
        (
            PluginError::SpInvalidInput {
                detail: String::new(),
                field: None,
            },
            "sp_invalid_input",
        ),
        (
            PluginError::SpNotFound {
                detail: String::new(),
            },
            "sp_not_found",
        ),
    ];
    for (err, expected) in cases {
        assert_eq!(FailureVariant::from(err).as_str(), *expected);
        assert_eq!(failure_variant_label(err), *expected);
    }
}

#[test]
fn failure_variant_constants_match_their_literal() {
    // Pin every `pub const` literal so a typo in one constant fails the
    // test before it lands in the wire contract.
    assert_eq!(FailureVariant::CONFIG.as_str(), "config");
    assert_eq!(FailureVariant::CREDSTORE_READ.as_str(), "credstore_read");
    assert_eq!(FailureVariant::METADATA_DECODE.as_str(), "metadata_decode");
    assert_eq!(FailureVariant::KC_REST.as_str(), "kc_rest");
    assert_eq!(
        FailureVariant::AMBIGUOUS_CREATED.as_str(),
        "ambiguous_created"
    );
    assert_eq!(
        FailureVariant::CREATED_REALM_EXISTS.as_str(),
        "created_realm_exists"
    );
    assert_eq!(
        FailureVariant::BOOTSTRAP_PERMS_MISSING.as_str(),
        "bootstrap_perms_missing"
    );
    assert_eq!(
        FailureVariant::DEPROVISION_NOT_FOUND.as_str(),
        "deprovision_not_found"
    );
    assert_eq!(
        FailureVariant::DEPROVISION_RETRYABLE.as_str(),
        "deprovision_retryable"
    );
    assert_eq!(
        FailureVariant::DEPROVISION_TERMINAL.as_str(),
        "deprovision_terminal"
    );
    assert_eq!(
        FailureVariant::USER_OP_REJECTED.as_str(),
        "user_op_rejected"
    );
    assert_eq!(
        FailureVariant::USER_OP_UNAVAILABLE.as_str(),
        "user_op_unavailable"
    );
    assert_eq!(
        FailureVariant::USER_OP_UNSUPPORTED.as_str(),
        "user_op_unsupported"
    );
    assert_eq!(
        FailureVariant::SP_INVALID_INPUT.as_str(),
        "sp_invalid_input"
    );
    assert_eq!(FailureVariant::SP_NOT_FOUND.as_str(), "sp_not_found");
}

#[test]
fn version_observed_handles_all_decode_error_variants() {
    assert_eq!(
        VersionObserved::from(&DecodeError::MissingVersion).as_str(),
        "missing",
    );
    assert_eq!(
        VersionObserved::from(&DecodeError::Malformed { detail: "x".into() }).as_str(),
        "malformed",
    );
    assert_eq!(
        VersionObserved::from(&DecodeError::UnsupportedVersion {
            observed: "v9".into()
        })
        .as_str(),
        "v9",
    );
}

#[test]
fn version_observed_constants_match_their_literal() {
    assert_eq!(VersionObserved::MISSING.as_str(), "missing");
    assert_eq!(VersionObserved::MALFORMED.as_str(), "malformed");
}

#[test]
fn endpoint_class_unknown_constant_is_constructable() {
    assert_eq!(EndpointClass::UNKNOWN.as_str(), "unknown");
}

// ---- NoopMetrics implements every port ----

#[test]
fn noop_metrics_implements_every_port() {
    let noop: Arc<NoopMetrics> = Arc::new(NoopMetrics);

    let tenant: Arc<dyn TenantLifecycleMetricsPort> = Arc::clone(&noop) as _;
    let user: Arc<dyn UserOpMetricsPort> = Arc::clone(&noop) as _;
    let kc: Arc<dyn KcAdminMetricsPort> = Arc::clone(&noop) as _;
    let credstore: Arc<dyn CredstoreMetricsPort> = Arc::clone(&noop) as _;
    let metadata: Arc<dyn MetadataCodecMetricsPort> = Arc::clone(&noop) as _;
    let failure: Arc<dyn FailureMetricsPort> = Arc::clone(&noop) as _;
    let sp: Arc<dyn SpOpMetricsPort> = Arc::clone(&noop) as _;

    // Smoke-call every method to confirm signatures match.
    tenant.provision_tenant_duration(RealmBinding::Shared, 0.0);
    tenant.realm_bound(RealmBinding::Shared, "r");
    tenant.realm_unbound(RealmBinding::Shared, "r");
    tenant.deprovision_missing_metadata();
    user.user_op_duration(UserOp::ProvisionUser, 0.0);
    kc.kc_admin_request_duration(EndpointClass::UNKNOWN, 0.0);
    kc.kc_admin_token_refresh(TokenRefreshOutcome::Success, TokenTier::StaticEnv, "r");
    kc.credential_refresh(TokenRefreshOutcome::Success, TokenTier::OpenBao, "r");
    credstore.credstore_write(CredstoreOp::Put, CredstoreOutcome::Ok);
    metadata.metadata_decode_failure(VersionObserved::MISSING);
    failure.failure(PluginOp::ProvisionTenant, FailureVariant::CONFIG);
    sp.sp_op_duration(SpOp::Create, 0.0);
}
