//! Regression tests for the single `From<DomainError> for CanonicalError`
//! ladder in [`super::sdk_error_mapping`].
//!
//! Per ADR 0005 there is one classification ladder; these tests pin the
//! exact `CanonicalError` envelope it produces — AIP-193 category, HTTP
//! status, resource type, and key context fields (`field_violations`,
//! `violations`, `reason`) — for every `DomainError` variant. They are
//! the unit-level wire-invariant guard; the HTTP-level sweep lives in
//! `tests/api_status_mapping_test.rs`.

use std::time::Duration;
use toolkit_gts::gts_id;

use toolkit_canonical_errors::{CanonicalError, InvalidArgument};

use crate::domain::error::{DomainError, UnsupportedResource};

/// Run a `DomainError` through the single canonical ladder.
fn round_trip(d: DomainError) -> CanonicalError {
    CanonicalError::from(d)
}

// ---------------------------------------------------------------------------
// InvalidArgument (HTTP 400)
// ---------------------------------------------------------------------------

#[test]
fn invalid_tenant_type_maps_to_invalid_argument() {
    let canonical = round_trip(DomainError::InvalidTenantType {
        detail: "bad type".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

#[test]
fn validation_maps_to_invalid_argument_with_tenant_resource() {
    let canonical = round_trip(DomainError::Validation {
        detail: "bad name".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

#[test]
fn metadata_validation_maps_to_invalid_argument_with_metadata_resource() {
    // Pins the split introduced for the REST surface: metadata-content
    // failures (malformed `type_id`, null body, GTS body validation
    // failure) MUST carry the metadata GTS resource type on the
    // canonical envelope. The tenant-state guards keep `Validation`
    // (and `TenantResource`) — see `validation_maps_to_invalid_argument_with_tenant_resource`
    // above for the sibling pin.
    let canonical = round_trip(DomainError::MetadataValidation {
        detail: "metadata value must not be null".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_METADATA_RESOURCE_TYPE)
    );
}

#[test]
fn root_tenant_cannot_delete_maps_to_400() {
    let canonical = round_trip(DomainError::RootTenantCannotDelete);
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

#[test]
fn root_tenant_cannot_convert_maps_to_400() {
    let canonical = round_trip(DomainError::RootTenantCannotConvert);
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

/// Symmetric coverage with `_delete` / `_convert` above: the new
/// `RootTenantCannotChangeStatus` variant (added to close the
/// suspend/unsuspend protection gap discovered via e2e probing) must
/// map to 400 `invalid_argument` with the tenant resource type, so
/// the wire envelope is indistinguishable from the existing
/// root-protection rejections.
#[test]
fn root_tenant_cannot_change_status_maps_to_400() {
    let canonical = round_trip(DomainError::RootTenantCannotChangeStatus);
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

/// Pin the full wire shape for the `IdP` plugin's permanent
/// shape-rejection: 400 `invalid_argument` on `TenantResource` with
/// the dotted-path `field` carried as the canonical
/// `field_violations[0].field` (not squashed into the description).
/// `reason = "IDP_INVALID_INPUT"` is the discriminator clients use
/// to tell this rejection from generic `VALIDATION` (`InvalidRequest`)
/// without parsing `detail`.
#[test]
fn idp_invalid_input_with_field_carries_dotted_path_on_canonical() {
    let canonical = round_trip(DomainError::IdpInvalidInput {
        detail: "realm_name must be non-empty".to_owned(),
        field: Some("provisioning_metadata.realm_name".to_owned()),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::InvalidArgument { ctx, .. } = canonical else {
        panic!("expected CanonicalError::InvalidArgument");
    };
    let InvalidArgument::FieldViolations { field_violations } = ctx else {
        panic!("expected InvalidArgument::FieldViolations ctx");
    };
    assert_eq!(field_violations.len(), 1);
    assert_eq!(
        field_violations[0].field, "provisioning_metadata.realm_name",
        "dotted-path field MUST survive to the canonical envelope, not be squashed into detail"
    );
    assert_eq!(
        field_violations[0].description,
        "realm_name must be non-empty"
    );
    assert_eq!(field_violations[0].reason, "IDP_INVALID_INPUT");
}

/// Companion to the `Some(...)` pin above: when the plugin can't
/// localise the violation to a sub-key (`field = None`), the
/// canonical envelope falls back to the shared
/// `"provisioning_metadata"` field key — every `IdP` plugin shares
/// this surface — so callers always see a structured attribution
/// rather than a missing field.
#[test]
fn idp_invalid_input_without_field_falls_back_to_provisioning_metadata() {
    let canonical = round_trip(DomainError::IdpInvalidInput {
        detail: "metadata body rejected".to_owned(),
        field: None,
    });
    assert_eq!(canonical.status_code(), 400);
    let CanonicalError::InvalidArgument { ctx, .. } = canonical else {
        panic!("expected CanonicalError::InvalidArgument");
    };
    let InvalidArgument::FieldViolations { field_violations } = ctx else {
        panic!("expected InvalidArgument::FieldViolations ctx");
    };
    assert_eq!(field_violations[0].field, "provisioning_metadata");
    assert_eq!(field_violations[0].reason, "IDP_INVALID_INPUT");
}

// ---------------------------------------------------------------------------
// NotFound (HTTP 404)
// ---------------------------------------------------------------------------

#[test]
fn not_found_carries_resource_name_and_tenant_type() {
    let canonical = round_trip(DomainError::NotFound {
        detail: "tenant 7 not found".to_owned(),
        resource: "7".to_owned(),
    });
    assert_eq!(canonical.status_code(), 404);
    assert_eq!(canonical.resource_name(), Some("7"));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

#[test]
fn user_not_found_maps_to_not_found_404_with_user_resource() {
    // Pins the per-resource NotFound for `delete_user` / `list_users`
    // user-id lookups: 404 + `USER_RESOURCE_TYPE` + the supplied id
    // surfaces as `resource_name`. Without this, a future drift in
    // the mapper (e.g. routing through `TenantResource::not_found`
    // because the variant lives under tenant scope) would silently
    // change the resource type on the wire.
    let user_id = "00000000-0000-0000-0000-000000000077";
    let canonical = round_trip(DomainError::UserNotFound {
        detail: format!("user {user_id} not found"),
        resource: user_id.to_owned(),
    });
    assert_eq!(canonical.status_code(), 404);
    assert_eq!(canonical.resource_name(), Some(user_id));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::USER_RESOURCE_TYPE)
    );
    assert!(
        matches!(canonical, CanonicalError::NotFound { .. }),
        "UserNotFound MUST surface as the NotFound variant"
    );
}

#[test]
fn service_account_not_found_maps_to_not_found_404_with_its_own_resource_type() {
    // A machine identity must not surface under the *user* resource
    // type: a client keys "can I grant this?" off `resource_type`, and
    // collapsing the two would advertise credential minting as user
    // management. The `client_id` rides `resource_name`.
    let client_id = "svc-abc";
    let canonical = round_trip(DomainError::ServiceAccountNotFound {
        detail: "service account not found in tenant".to_owned(),
        resource: client_id.to_owned(),
    });
    assert_eq!(canonical.status_code(), 404);
    assert_eq!(canonical.resource_name(), Some(client_id));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::SERVICE_ACCOUNT_RESOURCE_TYPE)
    );
    assert!(
        matches!(canonical, CanonicalError::NotFound { .. }),
        "ServiceAccountNotFound MUST surface as the NotFound variant"
    );
}

#[test]
fn service_account_invalid_input_maps_to_400_attributed_to_the_request() {
    // The adapter's own field attribution is untrusted text and was
    // dropped at the domain boundary, so the violation names `request`
    // as a whole rather than a field the provider chose.
    let canonical = round_trip(DomainError::ServiceAccountInvalidInput {
        detail: crate::domain::idp::SA_INVALID_INPUT_MESSAGE.to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::SERVICE_ACCOUNT_RESOURCE_TYPE)
    );
    let CanonicalError::InvalidArgument { ctx, .. } = canonical else {
        panic!("expected CanonicalError::InvalidArgument");
    };
    let InvalidArgument::FieldViolations { field_violations } = ctx else {
        panic!("expected InvalidArgument::FieldViolations ctx");
    };
    assert_eq!(field_violations.len(), 1);
    assert_eq!(
        field_violations[0].field,
        account_management_sdk::field::REQUEST_FIELD
    );
    assert_eq!(
        field_violations[0].reason,
        account_management_sdk::field::IDP_INVALID_INPUT
    );
}

/// The ambiguous outcome is 409 with its own reason, not the 503 a clean
/// failure gets: retrying the same request would collide with the
/// account the uncertain call may have created, so the envelope must
/// say "reconcile", not "retry".
#[test]
fn service_account_ambiguous_maps_to_409_aborted_with_ambiguous_reason() {
    let canonical = round_trip(DomainError::ServiceAccountAmbiguous {
        detail: crate::domain::idp::SA_AMBIGUOUS_MESSAGE.to_owned(),
    });
    assert_eq!(canonical.status_code(), 409);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::SERVICE_ACCOUNT_RESOURCE_TYPE)
    );
    assert!(
        matches!(canonical, CanonicalError::Aborted { .. }),
        "ServiceAccountAmbiguous MUST surface as Aborted, not ServiceUnavailable"
    );
    let rendered = format!("{canonical:?}");
    assert!(
        rendered.contains(account_management_sdk::reason::aborted::AMBIGUOUS_OUTCOME),
        "expected the AMBIGUOUS_OUTCOME reason token: {rendered}"
    );
    assert!(
        rendered.contains("reconcile"),
        "the detail must steer the caller to reconcile: {rendered}"
    );
}

#[test]
fn conversion_request_not_found_maps_to_not_found_404() {
    // Pins the wire shape for conversion-request lookups that miss
    // their target (`cancel` / `reject` / `approve` / `get`).
    // Distinct from `PendingExists` (covered separately at line 250):
    // 404 instead of 409, NotFound variant instead of AlreadyExists,
    // request-id carried as `resource_name` so the caller can show
    // the missing id without parsing `detail`.
    let req_id = "11111111-2222-3333-4444-555555555555";
    let canonical = round_trip(DomainError::ConversionRequestNotFound {
        detail: format!("conversion request {req_id} not found"),
        resource: req_id.to_owned(),
    });
    assert_eq!(canonical.status_code(), 404);
    assert_eq!(canonical.resource_name(), Some(req_id));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::CONVERSION_REQUEST_RESOURCE_TYPE)
    );
    assert!(
        matches!(canonical, CanonicalError::NotFound { .. }),
        "ConversionRequestNotFound MUST surface as the NotFound variant"
    );
}

#[test]
fn metadata_entry_not_found_uses_metadata_resource_type_with_chained_type_id_as_name() {
    // Unified metadata 404: both "schema unknown to registry" and
    // "entry missing for tenant" collapse to
    // `MetadataEntryNotFound` and surface as
    // `TENANT_METADATA_RESOURCE_TYPE` (`gts.cf.core.am.tenant_metadata.v1~`)
    // with the chained `type_id` the caller supplied as
    // `resource_name`.
    let chain = gts_id!("cf.core.am.tenant_metadata.v1~cf.core.billing.usage.v1~");
    let canonical = round_trip(DomainError::MetadataEntryNotFound {
        detail: "entry missing".to_owned(),
        entry: chain.to_owned(),
    });
    assert_eq!(canonical.status_code(), 404);
    assert_eq!(canonical.resource_name(), Some(chain));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_METADATA_RESOURCE_TYPE)
    );
}

// ---------------------------------------------------------------------------
// AlreadyExists (HTTP 409)
// ---------------------------------------------------------------------------

#[test]
fn already_exists_maps_to_409() {
    let canonical = round_trip(DomainError::AlreadyExists {
        detail: "tenant exists".to_owned(),
    });
    assert_eq!(canonical.status_code(), 409);
    assert_eq!(canonical.resource_name(), Some("tenant"));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

/// An `IdP`-reported user uniqueness collision surfaces as
/// 409 `already_exists` on the USER resource type, with the stable
/// colliding-field token as `resource_name` and a detail derived from
/// the typed field — never the caller-supplied value and never the raw
/// provider text.
#[test]
fn user_already_exists_maps_to_409_with_user_resource() {
    for (field, token, phrase) in [
        (
            account_management_sdk::IdpUserDuplicateField::Username,
            "username",
            "a user with this username already exists",
        ),
        (
            account_management_sdk::IdpUserDuplicateField::Email,
            "email",
            "a user with this email already exists",
        ),
        (
            account_management_sdk::IdpUserDuplicateField::UsernameOrEmail,
            "username_or_email",
            "a user with this username or email already exists",
        ),
    ] {
        let canonical = round_trip(DomainError::UserAlreadyExists { field });
        assert_eq!(canonical.status_code(), 409);
        assert_eq!(canonical.resource_name(), Some(token));
        assert_eq!(
            canonical.resource_type(),
            Some(account_management_sdk::gts::USER_RESOURCE_TYPE)
        );
        assert!(
            matches!(canonical, CanonicalError::AlreadyExists { .. }),
            "UserAlreadyExists MUST surface as the AlreadyExists variant"
        );
        assert_eq!(canonical.detail(), phrase);
    }
}

/// An `IdP` password-policy reject carries the structured
/// `password` / `PASSWORD_POLICY` field-violation tokens (not the
/// generic `request` / `VALIDATION` pair) so clients can attribute
/// the 400 to the password input without parsing `detail`.
#[test]
fn idp_password_policy_maps_to_400_with_password_field_violation() {
    let canonical = round_trip(DomainError::IdpPasswordPolicy {
        detail: "the supplied password does not meet the identity provider's password policy"
            .to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::USER_RESOURCE_TYPE)
    );
    let CanonicalError::InvalidArgument { ctx, .. } = canonical else {
        panic!("expected CanonicalError::InvalidArgument");
    };
    let InvalidArgument::FieldViolations { field_violations } = ctx else {
        panic!("expected InvalidArgument::FieldViolations ctx");
    };
    assert_eq!(field_violations.len(), 1);
    assert_eq!(
        field_violations[0].field,
        account_management_sdk::field::PASSWORD_FIELD
    );
    assert_eq!(
        field_violations[0].reason,
        account_management_sdk::field::PASSWORD_POLICY
    );
}

/// An `IdP`-managed attribute reject lands on 400 `invalid_argument`
/// carrying the *patched property's own name* as
/// `field_violations[].field` plus the `IDP_MANAGED_FIELD` reason, so a
/// client can disable exactly that form input. Every
/// [`account_management_sdk::IdpUserAttribute`] is exercised: the field
/// token MUST equal the `UserUpdateRequest` JSON property name, because
/// clients key form-field attribution off that string.
///
/// The `description` is pinned to the exact curated sentence, not merely
/// checked non-empty: the typed-attribute design exists so the public
/// wording lives in one place and cannot drift, which only holds if the
/// wording is asserted somewhere.
#[test]
fn idp_field_not_writable_maps_to_400_with_the_patched_field_violation() {
    for (attribute, expected_field, expected_description) in [
        (
            account_management_sdk::IdpUserAttribute::Username,
            "username",
            "the username is managed by the identity provider and cannot be changed through this API",
        ),
        (
            account_management_sdk::IdpUserAttribute::Email,
            "email",
            "the email address is managed by the identity provider and cannot be changed through this API",
        ),
        (
            account_management_sdk::IdpUserAttribute::DisplayName,
            "display_name",
            "the display name is managed by the identity provider and cannot be changed through this API",
        ),
        (
            account_management_sdk::IdpUserAttribute::FirstName,
            "first_name",
            "the first name is managed by the identity provider and cannot be changed through this API",
        ),
        (
            account_management_sdk::IdpUserAttribute::LastName,
            "last_name",
            "the last name is managed by the identity provider and cannot be changed through this API",
        ),
    ] {
        let canonical = round_trip(DomainError::IdpFieldNotWritable {
            fields: vec![attribute],
        });
        assert_eq!(
            canonical.status_code(),
            400,
            "{expected_field}: writability is a field capability, not an authz decision -- \
             it MUST NOT surface as 403"
        );
        assert_eq!(
            canonical.resource_type(),
            Some(account_management_sdk::gts::USER_RESOURCE_TYPE)
        );
        let CanonicalError::InvalidArgument { ctx, .. } = canonical else {
            panic!("{expected_field}: expected CanonicalError::InvalidArgument");
        };
        let InvalidArgument::FieldViolations { field_violations } = ctx else {
            panic!("{expected_field}: expected InvalidArgument::FieldViolations ctx");
        };
        assert_eq!(field_violations.len(), 1);
        assert_eq!(
            field_violations[0].field, expected_field,
            "field token MUST match the UserUpdateRequest property name"
        );
        assert_eq!(
            field_violations[0].reason,
            account_management_sdk::field::IDP_MANAGED_FIELD
        );
        assert_eq!(
            field_violations[0].description, expected_description,
            "{expected_field}: the curated public wording is a contract -- it lives in exactly \
             one place and MUST NOT drift"
        );
    }
}

/// A patch touching several locked attributes yields one violation per
/// refused attribute in a single envelope, so the client learns the whole
/// refused set from one round-trip instead of discovering it field by
/// field. Repeats a provider may send collapse to one violation each.
#[test]
fn idp_field_not_writable_emits_one_violation_per_refused_attribute() {
    let canonical = round_trip(DomainError::IdpFieldNotWritable {
        fields: vec![
            account_management_sdk::IdpUserAttribute::Email,
            account_management_sdk::IdpUserAttribute::FirstName,
            account_management_sdk::IdpUserAttribute::LastName,
            account_management_sdk::IdpUserAttribute::Email,
        ],
    });
    assert_eq!(canonical.status_code(), 400);
    let CanonicalError::InvalidArgument { ctx, .. } = canonical else {
        panic!("expected CanonicalError::InvalidArgument");
    };
    let InvalidArgument::FieldViolations { field_violations } = ctx else {
        panic!("expected InvalidArgument::FieldViolations ctx");
    };
    let fields: Vec<&str> = field_violations.iter().map(|v| v.field.as_str()).collect();
    assert_eq!(
        fields,
        ["email", "first_name", "last_name"],
        "every refused attribute MUST get its own violation, de-duplicated"
    );
    assert!(
        field_violations
            .iter()
            .all(|v| v.reason == account_management_sdk::field::IDP_MANAGED_FIELD),
        "every violation in the set carries the IDP_MANAGED_FIELD reason"
    );
}

/// An empty refused set is a provider contract violation. A 400 whose
/// `field_violations[]` is empty attributes the refusal to nothing, so
/// the mapping degrades to the generic `request` / `VALIDATION` pair
/// rather than emitting an unattributable `IDP_MANAGED_FIELD`.
#[test]
fn idp_field_not_writable_with_no_attributes_degrades_to_the_generic_violation() {
    let canonical = round_trip(DomainError::IdpFieldNotWritable { fields: Vec::new() });
    assert_eq!(canonical.status_code(), 400);
    let CanonicalError::InvalidArgument { ctx, .. } = canonical else {
        panic!("expected CanonicalError::InvalidArgument");
    };
    let InvalidArgument::FieldViolations { field_violations } = ctx else {
        panic!("expected InvalidArgument::FieldViolations ctx");
    };
    assert_eq!(field_violations.len(), 1);
    assert_eq!(
        field_violations[0].field,
        account_management_sdk::field::REQUEST_FIELD
    );
    assert_eq!(
        field_violations[0].reason,
        account_management_sdk::field::VALIDATION
    );
}

// ---------------------------------------------------------------------------
// Aborted (HTTP 409 with reason)
// ---------------------------------------------------------------------------

#[test]
fn aborted_maps_to_409_with_reason() {
    let canonical = round_trip(DomainError::Aborted {
        reason: "SERIALIZATION_CONFLICT".to_owned(),
        detail: "serialization conflict; retry budget exhausted".to_owned(),
    });
    assert_eq!(canonical.status_code(), 409);
    let CanonicalError::Aborted { ctx, .. } = canonical else {
        panic!("expected Aborted variant");
    };
    assert_eq!(ctx.reason, "SERIALIZATION_CONFLICT");
}

#[test]
fn metadata_version_mismatch_maps_to_aborted_409_with_reason() {
    // `upsert_metadata` with `expected_version` not matching the stored
    // row surfaces as the Aborted variant (HTTP 409) tagged with the
    // `METADATA_VERSION_MISMATCH` reason. The reason token is the
    // contract callers branch on to distinguish a stale-version
    // conflict from a generic 409, and a future mapper drift that
    // dropped `with_reason` would change the wire envelope in a way
    // unit-tested ONLY here.
    let chain = gts_id!("cf.core.am.tenant_metadata.v1~cf.core.billing.usage.v1~");
    let canonical = round_trip(DomainError::MetadataVersionMismatch {
        entry: chain.to_owned(),
        expected: 4,
        current: 7,
    });
    assert_eq!(canonical.status_code(), 409);
    assert_eq!(canonical.resource_name(), Some(chain));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_METADATA_RESOURCE_TYPE)
    );
    let CanonicalError::Aborted { ctx, .. } = canonical else {
        panic!("MetadataVersionMismatch MUST surface as the Aborted variant");
    };
    assert_eq!(
        ctx.reason, "METADATA_VERSION_MISMATCH",
        "envelope MUST pin the `METADATA_VERSION_MISMATCH` reason token"
    );
}

// ---------------------------------------------------------------------------
// FailedPrecondition (HTTP 400)
// ---------------------------------------------------------------------------

#[test]
fn type_not_allowed_maps_to_failed_precondition() {
    let canonical = round_trip(DomainError::TypeNotAllowed {
        detail: "child of leaf".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations.len(), 1);
    assert_eq!(ctx.violations[0].subject, "tenant_type");
    assert_eq!(ctx.violations[0].type_, "TYPE_NOT_ALLOWED");
}

#[test]
fn tenant_depth_exceeded_maps_to_failed_precondition() {
    let canonical = round_trip(DomainError::TenantDepthExceeded {
        detail: "depth 7 > 6".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "depth");
    assert_eq!(ctx.violations[0].type_, "TENANT_DEPTH_EXCEEDED");
}

#[test]
fn tenant_has_children_maps_to_failed_precondition() {
    let canonical = round_trip(DomainError::TenantHasChildren);
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "tenant");
    assert_eq!(ctx.violations[0].type_, "TENANT_HAS_CHILDREN");
}

#[test]
fn tenant_has_resources_maps_to_failed_precondition() {
    let canonical = round_trip(DomainError::TenantHasResources);
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "tenant");
    assert_eq!(ctx.violations[0].type_, "TENANT_HAS_RESOURCES");
}

#[test]
fn pending_exists_maps_to_already_exists_409_on_conversion_request() {
    // Duplicate-on-create per AIP-193: the at-most-one-pending invariant
    // surfaces as `code=pending_exists` (HTTP 409). The OpenAPI spec
    // (`docs/account-management-v1.yaml`) documents the 409, so this
    // test pins both the wire status and the resource_name carrying
    // the existing `request_id`.
    let canonical = round_trip(DomainError::PendingExists {
        request_id: "req-1".to_owned(),
    });
    assert_eq!(canonical.status_code(), 409);
    assert_eq!(canonical.resource_name(), Some("req-1"));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::CONVERSION_REQUEST_RESOURCE_TYPE)
    );
    assert!(
        matches!(canonical, CanonicalError::AlreadyExists { .. }),
        "expected AlreadyExists variant for pending_exists; the duplicate-on-create \
         contract is HTTP 409, not 400 failed_precondition",
    );
}

#[test]
fn invalid_actor_for_transition_maps_to_failed_precondition_on_conversion_request() {
    let canonical = round_trip(DomainError::InvalidActorForTransition {
        attempted_status: "approved".to_owned(),
        caller_side: "child".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::CONVERSION_REQUEST_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "conversion_request");
    assert_eq!(ctx.violations[0].type_, "INVALID_ACTOR_FOR_TRANSITION");
}

#[test]
fn already_resolved_maps_to_failed_precondition_on_conversion_request() {
    let canonical = round_trip(DomainError::AlreadyResolved);
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::CONVERSION_REQUEST_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "conversion_request");
    assert_eq!(ctx.violations[0].type_, "ALREADY_RESOLVED");
}

#[test]
fn conflict_maps_to_failed_precondition_with_request_subject() {
    let canonical = round_trip(DomainError::Conflict {
        detail: "tenant deleted".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "request");
    assert_eq!(ctx.violations[0].type_, "PRECONDITION_FAILED");
}

#[test]
fn feature_disabled_maps_to_failed_precondition_on_configuration() {
    let canonical = round_trip(DomainError::FeatureDisabled {
        detail: "feature off".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "configuration");
    assert_eq!(ctx.violations[0].type_, "FEATURE_DISABLED");
}

// ---------------------------------------------------------------------------
// PermissionDenied (HTTP 403)
// ---------------------------------------------------------------------------

#[test]
fn cross_tenant_denied_maps_to_403_with_reason() {
    let canonical = round_trip(DomainError::CrossTenantDenied { cause: None });
    assert_eq!(canonical.status_code(), 403);
    let CanonicalError::PermissionDenied { ctx, .. } = canonical else {
        panic!("expected PermissionDenied variant");
    };
    assert_eq!(ctx.reason, "CROSS_TENANT_DENIED");
}

// ---------------------------------------------------------------------------
// ServiceUnavailable (HTTP 503)
// ---------------------------------------------------------------------------

#[test]
fn service_unavailable_maps_to_503_with_retry_after() {
    let canonical = round_trip(DomainError::ServiceUnavailable {
        detail: "idp warming up".to_owned(),
        retry_after: Some(Duration::from_secs(15)),
        cause: None,
    });
    assert_eq!(canonical.status_code(), 503);
    let CanonicalError::ServiceUnavailable { ctx, .. } = canonical else {
        panic!("expected ServiceUnavailable variant");
    };
    assert_eq!(ctx.retry_after_seconds, Some(15));
}

#[test]
fn idp_unavailable_maps_to_503_without_retry_after() {
    let canonical = round_trip(DomainError::IdpUnavailable {
        detail: "vendor SDK error: token expired".to_owned(),
    });
    assert_eq!(canonical.status_code(), 503);
    let CanonicalError::ServiceUnavailable { ctx, .. } = canonical else {
        panic!("expected ServiceUnavailable variant");
    };
    assert!(ctx.retry_after_seconds.is_none());
}

// ---------------------------------------------------------------------------
// Unimplemented (HTTP 501)
// ---------------------------------------------------------------------------

/// A 501 must name the surface the caller actually addressed.
///
/// All three halves of `IdpPluginClient` collapse into one `DomainError`
/// variant, so the boundary picks `resource_type` from the variant's
/// `resource` discriminator. Asserting only the status code — which is
/// identical for all three — is what previously allowed every 501 to claim
/// `resource_type = tenant`, including on the machine-identity surface.
#[test]
fn unsupported_operation_maps_to_501_naming_the_addressed_resource() {
    for (resource, expected) in [
        (
            UnsupportedResource::Tenant,
            account_management_sdk::gts::TENANT_RESOURCE_TYPE,
        ),
        (
            UnsupportedResource::User,
            account_management_sdk::gts::USER_RESOURCE_TYPE,
        ),
        (
            UnsupportedResource::ServiceAccount,
            account_management_sdk::gts::SERVICE_ACCOUNT_RESOURCE_TYPE,
        ),
    ] {
        let canonical = round_trip(DomainError::UnsupportedOperation {
            detail: "vendor x lacks profile-edit".to_owned(),
            resource,
        });
        assert_eq!(canonical.status_code(), 501, "{resource:?} must be 501");
        assert_eq!(
            canonical.resource_type(),
            Some(expected),
            "{resource:?} must carry its own resource_type, not another surface's"
        );
    }
}

#[test]
fn service_account_unsupported_uses_the_fixed_am_owned_detail() {
    let canonical = round_trip(DomainError::UnsupportedOperation {
        detail: "provider detail must not reach the wire".to_owned(),
        resource: UnsupportedResource::ServiceAccount,
    });

    assert_eq!(
        canonical.detail(),
        crate::domain::idp::SA_UNSUPPORTED_MESSAGE
    );
    assert!(!canonical.detail().contains("provider detail"));
}

// ---------------------------------------------------------------------------
// ResourceExhausted (HTTP 429)
// ---------------------------------------------------------------------------

#[test]
fn integrity_check_in_progress_maps_to_429_with_quota_violation() {
    let canonical = round_trip(DomainError::IntegrityCheckInProgress);
    assert_eq!(canonical.status_code(), 429);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::ResourceExhausted { ctx, .. } = canonical else {
        panic!("expected ResourceExhausted variant");
    };
    assert_eq!(ctx.violations.len(), 1);
    assert_eq!(ctx.violations[0].subject, "integrity_check");
}

// ---------------------------------------------------------------------------
// Internal (HTTP 500)
// ---------------------------------------------------------------------------

#[test]
fn internal_maps_to_500() {
    let canonical = round_trip(DomainError::Internal {
        diagnostic: "unclassified".to_owned(),
        cause: None,
    });
    assert_eq!(canonical.status_code(), 500);
}
