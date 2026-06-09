//! Wire-shape tests for the Account Management REST DTOs.
//!
//! Four endpoint families:
//!
//! * tenant hierarchy — `TenantDto`, `TenantCreateRequestDto`,
//!   `TenantUpdateRequestDto`.
//! * tenant-metadata — `TenantMetadataEntryDto`,
//!   `ResolvedTenantMetadataDto`, `PutTenantMetadataDto`.
//! * `IdP` user-ops — `UserCreateRequestDto`, `UserDto`.
//! * identity — `MeDto` (subject id, type, home tenant).
//!
//! Focus: serde round-trips that the `OpenAPI` yaml relies on,
//! plus the SDK ↔ DTO conversion helpers (`from_idp_user`,
//! `into_idp_new_user`, `from_sdk_tenant`, `into_sdk_create_request`,
//! `into_sdk_tenant_update`) so a future SDK projection change
//! cannot silently drift the wire envelope.

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use uuid::Uuid;

use account_management_sdk::{IdpUser, MetadataEntry, Tenant, TenantId, TenantStatus};
use gts::GtsTypeId;

use toolkit_security::SecurityContext;

use super::{
    MeDto, NewUserPasswordDto, PutTenantMetadataDto, ResolvedTenantMetadataDto,
    TenantCreateRequestDto, TenantDto, TenantMetadataEntryDto, TenantUpdateRequestDto,
    UserCreateRequestDto, UserDto,
};

fn sample_tenant() -> Uuid {
    Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
}

fn sample_schema() -> &'static str {
    "gts.cf.core.am.tenant_metadata.v1~vendor.app.metadata.theme.v1~"
}

fn sample_updated() -> OffsetDateTime {
    datetime!(2026-05-16 12:00:00 UTC)
}

#[test]
fn entry_dto_round_trip_carries_path_tenant_and_sdk_projection() {
    let entry = MetadataEntry::new(
        GtsTypeId::new(sample_schema()),
        json!({"primary": "blue"}),
        sample_updated(),
        1,
    );
    let dto = TenantMetadataEntryDto::from_entry(sample_tenant(), entry);

    let json: Value = serde_json::to_value(&dto).unwrap();
    assert_eq!(
        json,
        json!({
            "tenant_id": "11111111-1111-1111-1111-111111111111",
            "type_id": sample_schema(),
            "value": {"primary": "blue"},
            "updated_at": "2026-05-16T12:00:00Z",
        }),
        "wire shape must mirror OpenAPI TenantMetadataEntry minus the documented created_at omission",
    );
}

#[test]
fn entry_dto_carries_arbitrary_json_payload() {
    let entry = MetadataEntry::new(
        GtsTypeId::new(sample_schema()),
        json!([1, 2, 3]),
        sample_updated(),
        1,
    );
    let dto = TenantMetadataEntryDto::from_entry(sample_tenant(), entry);
    let json: Value = serde_json::to_value(&dto).unwrap();
    assert_eq!(json["value"], json!([1, 2, 3]));
}

#[test]
fn put_dto_accepts_null_body_so_service_layer_owns_rejection() {
    // `#[serde(transparent)]` over `serde_json::Value` accepts JSON
    // `null` and surfaces it as `Value::Null`. The rejection lives in
    // `MetadataService::upsert_metadata` (see `service.rs:586`) so the
    // boundary error envelope can be a proper `DomainError::Validation`
    // with field/code metadata, not a generic deserialization failure
    // before authorization. This test pins the DTO half of that
    // contract — change-detector for the next reader wondering whether
    // null is rejected at the wire layer.
    let dto: PutTenantMetadataDto = serde_json::from_str("null").unwrap();
    assert_eq!(dto.value, Value::Null);
}

#[test]
fn put_dto_is_transparent_over_the_value_payload() {
    // PUT body is `TenantMetadataValue` directly per OpenAPI -- not a
    // wrapped object. `#[serde(transparent)]` on `PutTenantMetadataDto`
    // achieves the same wire shape as the SDK's `serde_json::Value`.
    // The DTO is request-only (no Serialize), so we assert deserialise
    // semantics only -- a wrapper object would surface as `null` here.
    //
    // The yaml `TenantMetadataValue` is intentionally any-JSON (no
    // `type:` constraint) so the DTO can accept every root shape and
    // delegate per-schema validation to the service. Pin object, array,
    // string, number, and bool here; null is covered separately by
    // `put_dto_accepts_null_body_so_service_layer_owns_rejection`.
    let raw = r#"{"theme":"dark","contrast":12}"#;
    let dto: PutTenantMetadataDto = serde_json::from_str(raw).unwrap();
    assert_eq!(dto.value, json!({"theme": "dark", "contrast": 12}));

    let array_body: PutTenantMetadataDto = serde_json::from_str("[1,2,3]").unwrap();
    assert_eq!(array_body.value, json!([1, 2, 3]));

    let string_body: PutTenantMetadataDto = serde_json::from_str(r#""hello""#).unwrap();
    assert_eq!(string_body.value, json!("hello"));

    let number_body: PutTenantMetadataDto = serde_json::from_str("42").unwrap();
    assert_eq!(number_body.value, json!(42));

    let bool_body: PutTenantMetadataDto = serde_json::from_str("true").unwrap();
    assert_eq!(bool_body.value, json!(true));
}

#[test]
fn resolved_dto_some_carries_value_and_resolved_true() {
    let entry = MetadataEntry::new(
        GtsTypeId::new(sample_schema()),
        json!({"foo": "bar"}),
        sample_updated(),
        1,
    );
    let dto = ResolvedTenantMetadataDto::from_resolution(
        sample_tenant(),
        sample_schema().to_owned(),
        Some(entry),
    );
    let json: Value = serde_json::to_value(&dto).unwrap();
    assert_eq!(
        json,
        json!({
            "tenant_id": "11111111-1111-1111-1111-111111111111",
            "type_id": sample_schema(),
            "resolved": true,
            "value": {"foo": "bar"},
        }),
        "resolved=true surfaces only tenant_id, type_id, resolved, value",
    );
}

#[test]
fn resolved_dto_none_omits_value_and_carries_resolved_false() {
    let dto = ResolvedTenantMetadataDto::from_resolution(
        sample_tenant(),
        sample_schema().to_owned(),
        None,
    );
    let json: Value = serde_json::to_value(&dto).unwrap();
    assert_eq!(
        json,
        json!({
            "tenant_id": "11111111-1111-1111-1111-111111111111",
            "type_id": sample_schema(),
            "resolved": false,
        }),
        "empty walk-up serialises with resolved=false and no `value` key",
    );
}

// ---- IdP user-ops DTOs ------------------------------------------

fn sample_user_id() -> Uuid {
    Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
}

#[test]
fn user_create_dto_required_username_only_deserialises() {
    // `email` and `display_name` are optional per `UserCreateRequest`
    // — omitting them MUST resolve to `None` so the lowered
    // `IdpNewUser` does not falsely advertise vendor-side profile
    // fields to the IdP plugin.
    let dto: UserCreateRequestDto = serde_json::from_str(r#"{"username":"alice"}"#).unwrap();
    assert_eq!(dto.username, "alice");
    assert!(dto.email.is_none());
    assert!(dto.display_name.is_none());
}

#[test]
fn user_create_dto_full_payload_round_trips_into_idp_new_user() {
    let raw = r#"{"username":"alice","email":"alice@example.test","display_name":"Alice A"}"#;
    let dto: UserCreateRequestDto = serde_json::from_str(raw).unwrap();
    let payload = dto.into_idp_new_user();
    assert_eq!(payload.username, "alice");
    assert_eq!(payload.email.as_deref(), Some("alice@example.test"));
    assert_eq!(payload.display_name.as_deref(), Some("Alice A"));
}

#[test]
fn user_create_dto_lowers_first_last_name_and_password_into_idp_new_user() {
    // Wire→SDK lowering MUST propagate every new optional verbatim.
    // Plugins such as the static / Keycloak IdPs read these fields
    // off `IdpNewUser` directly — a regression where the DTO drops
    // first_name / last_name / password would silently degrade
    // "create user with credentials" into a credential-less create.
    let raw = r#"{
        "username": "alice",
        "email": "alice@example.test",
        "first_name": "Alice",
        "last_name": "Anderson",
        "password": {"value": "s3cret!", "temporary": true}
    }"#;
    let dto: UserCreateRequestDto = serde_json::from_str(raw).unwrap();
    let payload = dto.into_idp_new_user();
    assert_eq!(payload.username, "alice");
    assert_eq!(payload.email.as_deref(), Some("alice@example.test"));
    assert_eq!(payload.first_name.as_deref(), Some("Alice"));
    assert_eq!(payload.last_name.as_deref(), Some("Anderson"));
    let pw = payload.password.as_ref().expect("password lowered");
    assert_eq!(pw.value, "s3cret!");
    assert!(pw.temporary);
}

#[test]
fn user_create_dto_password_temporary_defaults_to_false() {
    // The published REST contract lets callers omit `temporary` for
    // the permanent-credential case; the DTO mirrors the SDK default
    // (false). Without `#[serde(default)]` on the DTO we would
    // require `temporary` to be present in every payload — a breaking
    // change vs. the SDK shape.
    let raw = r#"{
        "username": "bob",
        "password": {"value": "hunter2"}
    }"#;
    let dto: UserCreateRequestDto = serde_json::from_str(raw).unwrap();
    let pw = dto.password.as_ref().expect("password parsed");
    assert!(
        !pw.temporary,
        "missing `temporary` MUST default to false on the wire"
    );
    let payload = dto.into_idp_new_user();
    let lowered = payload.password.as_ref().expect("password lowered");
    assert!(!lowered.temporary, "default MUST survive the lowering");
}

#[test]
fn new_user_password_dto_debug_redacts_value() {
    // `NewUserPasswordDto` carries plaintext only for the duration of
    // the create call. Any `tracing::debug!(?body)` on the wrapping
    // request DTO MUST NOT spill the password into structured logs.
    let dto = NewUserPasswordDto {
        value: "super-secret-password".into(),
        temporary: true,
    };
    let rendered = format!("{dto:?}");
    assert!(
        !rendered.contains("super-secret-password"),
        "plaintext password leaked into Debug: `{rendered}`"
    );
    assert!(
        rendered.contains("<redacted>"),
        "Debug must mark the redaction explicitly: `{rendered}`"
    );
    assert!(
        rendered.contains("temporary: true"),
        "non-sensitive temporary flag must remain visible: `{rendered}`"
    );
}

#[test]
fn user_create_dto_debug_does_not_leak_password() {
    // `UserCreateRequestDto` keeps `#[derive(Debug)]`; the nested
    // `NewUserPasswordDto` custom Debug is what protects us. This test
    // pins that interaction so a future field rename / type swap
    // cannot regress redaction at the request level.
    let dto = UserCreateRequestDto {
        username: "alice".into(),
        email: Some("alice@example.test".into()),
        display_name: None,
        first_name: Some("Alice".into()),
        last_name: Some("Anderson".into()),
        password: Some(NewUserPasswordDto {
            value: "super-secret-password".into(),
            temporary: false,
        }),
    };
    let rendered = format!("{dto:?}");
    assert!(
        !rendered.contains("super-secret-password"),
        "plaintext password leaked from UserCreateRequestDto Debug: `{rendered}`"
    );
    assert!(
        rendered.contains("<redacted>"),
        "request Debug must mark the password redaction: `{rendered}`"
    );
    assert!(
        rendered.contains("alice@example.test"),
        "non-sensitive email must remain visible: `{rendered}`"
    );
}

#[test]
fn user_create_dto_rejects_missing_username() {
    // `UserCreateRequest.required = [username]` — the schema
    // contract is "username MUST be present"; serde reflects that as
    // a deserialisation failure on the wire.
    let err = serde_json::from_str::<UserCreateRequestDto>("{}")
        .expect_err("username is required per OpenAPI UserCreateRequest");
    let msg = err.to_string();
    assert!(
        msg.contains("username"),
        "error mentions the missing field: got `{msg}`"
    );
}

#[test]
fn user_dto_required_fields_only_serialises_without_optionals() {
    // Omitting `email` and `display_name` MUST drop the keys
    // entirely on the wire — the tenant-minimal projection per
    // `adr-idp-user-identity-source-of-truth` cannot leak empty /
    // null vendor placeholders.
    let user = IdpUser::new(sample_user_id(), "alice");
    let dto = UserDto::from_idp_user(user);
    let json: Value = serde_json::to_value(&dto).unwrap();
    assert_eq!(
        json,
        json!({
            "id": "22222222-2222-2222-2222-222222222222",
            "username": "alice",
        }),
        "omitted optional fields must not surface on the wire",
    );
}

#[test]
fn user_dto_full_payload_round_trips_from_idp_user() {
    let user = IdpUser::new(sample_user_id(), "alice")
        .with_email("alice@example.test")
        .with_display_name("Alice A");
    let dto = UserDto::from_idp_user(user);
    let json: Value = serde_json::to_value(&dto).unwrap();
    assert_eq!(
        json,
        json!({
            "id": "22222222-2222-2222-2222-222222222222",
            "username": "alice",
            "email": "alice@example.test",
            "display_name": "Alice A",
        }),
        "wire shape must mirror OpenAPI User",
    );
}

#[test]
fn user_dto_carries_first_last_name_when_present() {
    let user = IdpUser::new(sample_user_id(), "alice")
        .with_first_name("Alice")
        .with_last_name("Anderson");
    let dto = UserDto::from_idp_user(user);
    let json: Value = serde_json::to_value(&dto).unwrap();
    assert_eq!(json["first_name"], "Alice");
    assert_eq!(json["last_name"], "Anderson");
}

#[test]
fn user_dto_omits_first_last_name_when_absent() {
    let user = IdpUser::new(sample_user_id(), "alice");
    let dto = UserDto::from_idp_user(user);
    let json: Value = serde_json::to_value(&dto).unwrap();
    let map = json.as_object().unwrap();
    assert!(!map.contains_key("first_name"));
    assert!(!map.contains_key("last_name"));
}

// ---- Tenant hierarchy DTOs --------------------------------------

fn sample_tenant_id() -> TenantId {
    TenantId(Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap())
}

fn sample_parent_id() -> TenantId {
    TenantId(Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap())
}

fn sample_created() -> OffsetDateTime {
    datetime!(2026-05-01 09:30:00 UTC)
}

fn sample_updated_tenant() -> OffsetDateTime {
    datetime!(2026-05-16 12:00:00 UTC)
}

fn sample_active_tenant() -> Tenant {
    Tenant {
        id: sample_tenant_id(),
        name: "acme corp".into(),
        status: TenantStatus::Active,
        tenant_type: Some("gts.cf.core.am.tenant_type.v1~vendor.app.customer.v1~".into()),
        parent_id: Some(sample_parent_id()),
        self_managed: false,
        depth: 2,
        created_at: sample_created(),
        updated_at: sample_updated_tenant(),
        deleted_at: None,
    }
}

#[test]
fn tenant_dto_active_wire_shape_mirrors_openapi() {
    let dto = TenantDto::from_sdk_tenant(sample_active_tenant());
    let json: Value = serde_json::to_value(&dto).unwrap();
    assert_eq!(
        json,
        json!({
            "id": "33333333-3333-3333-3333-333333333333",
            "name": "acme corp",
            "status": "active",
            "tenant_type": "gts.cf.core.am.tenant_type.v1~vendor.app.customer.v1~",
            "parent_id": "44444444-4444-4444-4444-444444444444",
            "self_managed": false,
            "depth": 2,
            "created_at": "2026-05-01T09:30:00Z",
            "updated_at": "2026-05-16T12:00:00Z",
        }),
        "active tenant omits deleted_at and surfaces snake_case status",
    );
    assert!(
        json.as_object()
            .is_some_and(|o| !o.contains_key("deleted_at")),
        "active tenant must omit `deleted_at`",
    );
}

#[test]
fn tenant_dto_deleted_carries_deleted_at() {
    // Soft-deleted tenants stay SDK-visible until the reaper hard-
    // deletes them. The tombstone must surface so admin UIs can
    // render the "will be removed" hint. Wire name is `deleted_at`,
    // mirroring the SDK / storage column.
    let mut tenant = sample_active_tenant();
    tenant.status = TenantStatus::Deleted;
    tenant.deleted_at = Some(datetime!(2026-06-15 12:00:00 UTC));
    let dto = TenantDto::from_sdk_tenant(tenant);
    let json: Value = serde_json::to_value(&dto).unwrap();
    assert_eq!(json["status"], json!("deleted"));
    assert_eq!(json["deleted_at"], json!("2026-06-15T12:00:00Z"));
}

#[test]
fn tenant_dto_root_serialises_parent_id_as_null() {
    // `parent_id` is required by the OpenAPI Tenant schema with
    // `type: [string, 'null']` — root tenants surface `null`, not an
    // omitted key. Pins the serde policy (no `skip_serializing_if`).
    let mut tenant = sample_active_tenant();
    tenant.parent_id = None;
    tenant.depth = 0;
    let dto = TenantDto::from_sdk_tenant(tenant);
    let json: Value = serde_json::to_value(&dto).unwrap();
    assert_eq!(json["parent_id"], Value::Null);
    assert_eq!(json["depth"], json!(0));
}

#[test]
fn tenant_dto_omits_tenant_type_when_registry_unreachable() {
    // `tenant_type` is `Option<String>` per the SDK comment — `None`
    // when the types registry was unreachable at lowering. The wire
    // shape must drop the key entirely (not surface `null`) so consumers
    // can distinguish "registry blip" from "explicit null".
    let mut tenant = sample_active_tenant();
    tenant.tenant_type = None;
    let dto = TenantDto::from_sdk_tenant(tenant);
    let json: Value = serde_json::to_value(&dto).unwrap();
    assert!(
        json.get("tenant_type").is_none(),
        "tenant_type key absent on the wire when None: got {json}",
    );
}

#[test]
fn tenant_create_request_required_fields_only_deserialise() {
    // `name`, `parent_id`, `tenant_type` are the required wire fields;
    // `self_managed` defaults to `false`, `provisioning_metadata` to
    // `None`. The lowering generates a fresh UUIDv4 for the child id.
    let raw = r#"{
        "name": "acme corp",
        "parent_id": "44444444-4444-4444-4444-444444444444",
        "tenant_type": "gts.cf.core.am.tenant_type.v1~vendor.app.customer.v1~"
    }"#;
    let dto: TenantCreateRequestDto = serde_json::from_str(raw).unwrap();
    assert_eq!(dto.name, "acme corp");
    assert_eq!(dto.parent_id, sample_parent_id().0);
    assert_eq!(
        dto.tenant_type,
        "gts.cf.core.am.tenant_type.v1~vendor.app.customer.v1~"
    );
    assert!(!dto.self_managed);
    assert!(dto.provisioning_metadata.is_none());

    let request = dto.into_sdk_create_request();
    assert_eq!(request.parent_id, sample_parent_id().0);
    assert_eq!(request.name, "acme corp");
    assert!(!request.self_managed);
    assert!(request.provisioning_metadata.is_none());
    // Child id is server-allocated (`Uuid::new_v4`); pin that it is
    // not nil and not the parent id so a future refactor cannot
    // accidentally echo a wire-side identifier.
    assert!(!request.child_id.is_nil());
    assert_ne!(request.child_id, request.parent_id);
}

#[test]
fn tenant_create_request_full_payload_round_trips_into_sdk() {
    let raw = r#"{
        "name": "acme child",
        "parent_id": "44444444-4444-4444-4444-444444444444",
        "tenant_type": "gts.cf.core.am.tenant_type.v1~vendor.app.customer.v1~",
        "self_managed": true,
        "provisioning_metadata": {"vendor": "okta", "domain": "acme.example"}
    }"#;
    let dto: TenantCreateRequestDto = serde_json::from_str(raw).unwrap();
    let request = dto.into_sdk_create_request();
    assert!(request.self_managed);
    assert_eq!(
        request.provisioning_metadata,
        Some(json!({"vendor": "okta", "domain": "acme.example"})),
    );
}

#[test]
fn tenant_create_request_rejects_missing_required_fields() {
    // `TenantCreateRequest.required = [name, parent_id, tenant_type]`
    // per OpenAPI. Each missing field surfaces as a deserialise error
    // at the wire boundary before the service ever runs — pin all
    // three so a future serde-default slip-up cannot silently relax
    // the contract.
    let err_name = serde_json::from_str::<TenantCreateRequestDto>(
        r#"{"parent_id":"00000000-0000-0000-0000-000000000001","tenant_type":"x"}"#,
    )
    .expect_err("name is required per OpenAPI TenantCreateRequest");
    assert!(
        err_name.to_string().contains("name"),
        "missing-name error mentions the field: got `{err_name}`",
    );

    let err_parent =
        serde_json::from_str::<TenantCreateRequestDto>(r#"{"name":"acme","tenant_type":"x"}"#)
            .expect_err("parent_id is required per OpenAPI TenantCreateRequest");
    assert!(
        err_parent.to_string().contains("parent_id"),
        "missing-parent_id error mentions the field: got `{err_parent}`",
    );

    let err_type = serde_json::from_str::<TenantCreateRequestDto>(
        r#"{"name":"acme","parent_id":"00000000-0000-0000-0000-000000000001"}"#,
    )
    .expect_err("tenant_type is required per OpenAPI TenantCreateRequest");
    assert!(
        err_type.to_string().contains("tenant_type"),
        "missing-tenant_type error mentions the field: got `{err_type}`",
    );
}

#[test]
fn tenant_create_request_rejects_non_object_provisioning_metadata() {
    // yaml `TenantCreateRequest.provisioning_metadata` is
    // `type: [object, 'null']`; the DTO types it as
    // `Option<Map<String, Value>>` so serde rejects arrays / scalars
    // at the wire boundary before the request ever touches the IdP
    // plugin (which has no input validation in this layer). The
    // happy path with a JSON object is covered by
    // `tenant_create_request_full_payload_round_trips_into_sdk`.
    let raw_array = r#"{
        "name": "acme corp",
        "parent_id": "44444444-4444-4444-4444-444444444444",
        "tenant_type": "gts.cf.core.am.tenant_type.v1~vendor.app.customer.v1~",
        "provisioning_metadata": [1, 2, 3]
    }"#;
    let err = serde_json::from_str::<TenantCreateRequestDto>(raw_array)
        .expect_err("provisioning_metadata must be object | null per yaml");
    // serde reports the typed deserialise mismatch by shape ("expected
    // a map") rather than by field name. Either signal is fine as long
    // as the payload is rejected at the wire layer.
    let msg = err.to_string();
    assert!(
        msg.contains("provisioning_metadata") || msg.contains("expected a map"),
        "error pinpoints the type mismatch: got `{err}`",
    );

    let raw_string = r#"{
        "name": "acme corp",
        "parent_id": "44444444-4444-4444-4444-444444444444",
        "tenant_type": "gts.cf.core.am.tenant_type.v1~vendor.app.customer.v1~",
        "provisioning_metadata": "literal"
    }"#;
    assert!(
        serde_json::from_str::<TenantCreateRequestDto>(raw_string).is_err(),
        "string provisioning_metadata must also be rejected",
    );

    // `null` explicitly is admissible per yaml `type: [object, 'null']`.
    let raw_null = r#"{
        "name": "acme corp",
        "parent_id": "44444444-4444-4444-4444-444444444444",
        "tenant_type": "gts.cf.core.am.tenant_type.v1~vendor.app.customer.v1~",
        "provisioning_metadata": null
    }"#;
    let dto: TenantCreateRequestDto = serde_json::from_str(raw_null).unwrap();
    assert!(dto.provisioning_metadata.is_none());
}

#[test]
fn tenant_update_request_empty_object_deserialises_but_lowers_to_empty_patch() {
    // The yaml `minProperties: 1` rule is enforced at the service
    // layer (`TenantService::update_tenant` rejects empty patches as
    // `code=validation`), not by serde — `TenantUpdateRequest`
    // accepts `{}` so the rejection carries the canonical error
    // envelope with field metadata. Pin the DTO-side half of that
    // contract.
    let dto: TenantUpdateRequestDto = serde_json::from_str("{}").unwrap();
    let patch = dto.into_sdk_tenant_update();
    assert!(patch.is_empty(), "{{}} lowers to an empty SDK patch");
}

#[test]
fn tenant_update_request_name_round_trip() {
    // `status` was removed from the PATCH wire shape — lifecycle
    // transitions go through the dedicated `/suspend`, `/unsuspend`
    // (AIP-136 sub-resource fallback) and `DELETE` endpoints. This
    // pins the surviving `name` lift end-to-end.
    let raw = r#"{"name":"renamed"}"#;
    let dto: TenantUpdateRequestDto = serde_json::from_str(raw).unwrap();
    let patch = dto.into_sdk_tenant_update();
    assert_eq!(patch.name.as_deref(), Some("renamed"));
}

#[test]
fn tenant_update_request_rejects_status_payload_at_the_wire_boundary() {
    // Regression guard for the silent half-apply pre-fix: the DTO
    // used to carry `status: Option<TenantPatchStatusDto>` and lower
    // it to a no-op via `let _ = self.status;` inside
    // `into_sdk_tenant_update`, so a mixed `{"name":_, "status":_}`
    // PATCH would 200 with the rename applied and the status
    // silently dropped. The fix removed the field entirely; the
    // DTO's `#[serde(deny_unknown_fields)]` now turns ANY `status`
    // payload (`active`, `suspended`, `deleted`, garbage) into a
    // wire-layer rejection so the caller sees the half-apply fail
    // loudly. The soft-delete transition is `DELETE
    // /tenants/{tenant_id}`'s responsibility per FEATURE §3 and
    // active/suspended go through `/suspend` and `/unsuspend`.
    for raw in [
        r#"{"status":"active"}"#,
        r#"{"status":"suspended"}"#,
        r#"{"status":"deleted"}"#,
        r#"{"name":"renamed","status":"suspended"}"#,
    ] {
        let err = serde_json::from_str::<TenantUpdateRequestDto>(raw)
            .expect_err("status payload MUST be rejected at the wire");
        assert!(
            err.to_string().contains("status") || err.to_string().contains("unknown"),
            "error pinpoints the offending field for payload `{raw}`: got `{err}`",
        );
    }
}

#[test]
fn tenant_create_request_rejects_unknown_fields() {
    // yaml `TenantCreateRequest.additionalProperties: false` is
    // enforced by `#[serde(deny_unknown_fields)]` on the DTO. The
    // primary footgun this prevents: a caller that reuses the
    // SDK's `CreateTenantRequest` JSON (which has a `child_id`
    // field) would otherwise receive `201 Created` for a
    // server-allocated UUID DIFFERENT from the one they sent —
    // believing it created a specific tenant id when it did not.
    let raw = r#"{
        "name": "acme corp",
        "parent_id": "44444444-4444-4444-4444-444444444444",
        "tenant_type": "gts.cf.core.am.tenant_type.v1~vendor.app.customer.v1~",
        "child_id": "55555555-5555-5555-5555-555555555555"
    }"#;
    let err = serde_json::from_str::<TenantCreateRequestDto>(raw)
        .expect_err("child_id is not part of the wire contract");
    assert!(
        err.to_string().contains("child_id") || err.to_string().contains("unknown"),
        "error pinpoints the unknown field: got `{err}`",
    );
}

#[test]
fn tenant_update_request_rejects_unknown_fields() {
    // yaml `TenantUpdateRequest.additionalProperties: false`. The
    // primary footgun: a client PATCHing a full edited tenant
    // object (`{"name":..., "parent_id":...}`) would otherwise see
    // `200 OK` with the immutable `parent_id` silently dropped —
    // believing the reparent succeeded when it did not.
    let raw = r#"{"name":"renamed","parent_id":"44444444-4444-4444-4444-444444444444"}"#;
    let err = serde_json::from_str::<TenantUpdateRequestDto>(raw)
        .expect_err("parent_id is immutable and not part of the patch contract");
    assert!(
        err.to_string().contains("parent_id") || err.to_string().contains("unknown"),
        "error pinpoints the unknown field: got `{err}`",
    );
}

// `TenantPatchStatusDto` and the `status` field on
// `TenantUpdateRequestDto` were removed entirely. The PATCH DTO now
// carries only `name`; lifecycle transitions go through dedicated
// endpoints (`/suspend`, `/unsuspend`, `DELETE`). Any leftover
// `status` payload (in any combination) is rejected at the wire by
// `#[serde(deny_unknown_fields)]` — pinned by
// `tenant_update_request_rejects_status_payload_at_the_wire_boundary`
// above.

// ====================================================================
// Conversion-request DTO wire-shape tests.
// ====================================================================

mod conversions {
    use super::*;
    use crate::api::rest::dto::{
        ChildConversionRequestDto, ConversionPatchDto, ConversionPatchStatusDto, ConversionSideDto,
        ConversionStatusDto, OwnConversionRequestDto, RequestChildConversionDto,
        RequestOwnConversionDto, TargetModeDto,
    };
    use crate::domain::conversion::model::{
        ConversionRequest, ConversionSide, ConversionStatus, TargetMode,
    };
    use crate::domain::conversion::service::{ConversionCaller, ConversionRequestParentProjection};

    fn fixed_now() -> OffsetDateTime {
        datetime!(2026-05-16 12:00:00 UTC)
    }

    fn sample_request() -> ConversionRequest {
        ConversionRequest {
            id: Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap(),
            tenant_id: Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap(),
            parent_id: Some(Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap()),
            child_tenant_name: "c-1".to_owned(),
            initiator_side: ConversionSide::Child,
            target_mode: TargetMode::SelfManaged,
            status: ConversionStatus::Pending,
            requested_by: Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap(),
            approved_by: None,
            cancelled_by: None,
            rejected_by: None,
            requested_at: fixed_now(),
            resolved_at: None,
            expires_at: fixed_now() + time::Duration::days(7),
            deleted_at: None,
            requested_comment: Some("audit rationale".to_owned()),
            approved_comment: None,
            cancelled_comment: None,
            rejected_comment: None,
        }
    }

    fn sample_parent_projection() -> ConversionRequestParentProjection {
        ConversionRequestParentProjection {
            request_id: Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap(),
            tenant_id: Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap(),
            child_tenant_name: "live-c".to_owned(),
            initiator_side: ConversionSide::Child,
            target_mode: TargetMode::SelfManaged,
            status: ConversionStatus::Pending,
            requested_by: Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap(),
            approved_by: None,
            cancelled_by: None,
            rejected_by: None,
            created_at: fixed_now(),
            expires_at: fixed_now() + time::Duration::days(7),
            resolved_at: None,
            requested_comment: Some("audit rationale".to_owned()),
            approved_comment: None,
            cancelled_comment: None,
            rejected_comment: None,
        }
    }

    #[test]
    fn own_conversion_dto_full_payload_wire_shape() {
        let row = sample_request();
        let dto = OwnConversionRequestDto::from_conversion(row);
        let json: Value = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["id"], "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        assert_eq!(json["tenant_id"], "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb");
        assert_eq!(json["parent_id"], "cccccccc-cccc-cccc-cccc-cccccccccccc");
        assert_eq!(json["child_tenant_name"], "c-1");
        assert_eq!(json["target_mode"], "self_managed");
        assert_eq!(json["initiator_side"], "child");
        assert_eq!(json["status"], "pending");
        assert_eq!(json["created_at"], "2026-05-16T12:00:00Z");
        assert_eq!(json["requested_comment"], "audit rationale");
        // Unset audit comments must NOT appear on the wire.
        assert!(json.get("approved_comment").is_none());
        assert!(json.get("cancelled_comment").is_none());
        assert!(json.get("rejected_comment").is_none());
    }

    #[test]
    fn own_conversion_dto_omits_resolved_at_when_pending() {
        let dto = OwnConversionRequestDto::from_conversion(sample_request());
        let json: Value = serde_json::to_value(&dto).unwrap();
        assert!(
            json.get("resolved_at").is_none(),
            "pending rows MUST omit resolved_at on the wire (sentinel for terminal status)"
        );
    }

    #[test]
    fn child_conversion_dto_minimal_projection_no_subtree_leakage() {
        let dto = ChildConversionRequestDto::from_parent_projection(sample_parent_projection());
        let json: Value = serde_json::to_value(&dto).unwrap();
        // Field set per `dod-managed-self-managed-modes-parent-side-minimal-surface`.
        let allowed = [
            "request_id",
            "tenant_id",
            "child_tenant_name",
            "target_mode",
            "initiator_side",
            "status",
            "requested_by",
            "created_at",
            "expires_at",
            "requested_comment",
        ];
        let object = json.as_object().expect("dto serializes to object");
        for key in object.keys() {
            assert!(
                allowed.contains(&key.as_str()),
                "child projection MUST NOT carry `{key}` -- cross-barrier subtree-leakage risk",
            );
        }
        assert_eq!(json["request_id"], "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        assert_eq!(json["child_tenant_name"], "live-c");
    }

    #[test]
    fn request_own_conversion_dto_target_mode_required() {
        // `target_mode` is required on the wire — a missing field
        // MUST fail at deserialise time so the rejection surfaces as
        // a clean 400 envelope before the service sees the body.
        let raw = r#"{"comment": "ok"}"#;
        let err = serde_json::from_str::<RequestOwnConversionDto>(raw).expect_err("missing field");
        assert!(
            err.to_string().contains("target_mode"),
            "deserialisation error must reference the missing `target_mode` field: {err}"
        );
    }

    #[test]
    fn request_child_conversion_dto_rejects_missing_target_mode() {
        let raw = r#"{"child_tenant_id":"00000000-0000-0000-0000-000000000001"}"#;
        let err =
            serde_json::from_str::<RequestChildConversionDto>(raw).expect_err("missing field");
        assert!(err.to_string().contains("target_mode"));
    }

    #[test]
    fn request_own_conversion_dto_rejects_unknown_fields() {
        // `deny_unknown_fields` keeps callers from passing legacy
        // `target_mode_override` or other stale fields silently.
        let raw = r#"{"target_mode":"self_managed","stale_field":true}"#;
        let err = serde_json::from_str::<RequestOwnConversionDto>(raw).expect_err("unknown field");
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn request_own_conversion_dto_accepts_optional_comment() {
        let raw = r#"{"target_mode":"managed","comment":"rationale"}"#;
        let dto: RequestOwnConversionDto = serde_json::from_str(raw).unwrap();
        assert_eq!(dto.target_mode, TargetModeDto::Managed);
        assert_eq!(dto.comment.as_deref(), Some("rationale"));
    }

    #[test]
    fn request_own_conversion_dto_omits_comment_when_absent() {
        let raw = r#"{"target_mode":"managed"}"#;
        let dto: RequestOwnConversionDto = serde_json::from_str(raw).unwrap();
        assert!(
            dto.comment.is_none(),
            "absent comment must stay None at the DTO layer"
        );
    }

    #[test]
    fn conversion_patch_dto_rejects_pending_status_at_wire() {
        // `pending` is excluded from `ConversionPatchStatusDto` so the
        // wire layer rejects it at deserialise time, leaving callers
        // with a clean 400 envelope instead of a service-layer
        // `code=already_resolved` confusion.
        let raw = r#"{"status":"pending"}"#;
        let err = serde_json::from_str::<ConversionPatchDto>(raw).expect_err("status=pending");
        assert!(err.to_string().to_lowercase().contains("variant"));
    }

    #[test]
    fn conversion_patch_dto_rejects_expired_status_at_wire() {
        // `expired` is system-driven; a caller PATCHing to expired
        // would lie about the `actor_kind` on the audit envelope.
        let raw = r#"{"status":"expired"}"#;
        let err = serde_json::from_str::<ConversionPatchDto>(raw).expect_err("status=expired");
        assert!(err.to_string().to_lowercase().contains("variant"));
    }

    #[test]
    fn conversion_patch_dto_accepts_each_admissible_status() {
        for variant in [
            ConversionPatchStatusDto::Approved,
            ConversionPatchStatusDto::Cancelled,
            ConversionPatchStatusDto::Rejected,
        ] {
            let raw = match variant {
                ConversionPatchStatusDto::Approved => r#"{"status":"approved"}"#,
                ConversionPatchStatusDto::Cancelled => r#"{"status":"cancelled"}"#,
                ConversionPatchStatusDto::Rejected => r#"{"status":"rejected"}"#,
            };
            let dto: ConversionPatchDto = serde_json::from_str(raw).unwrap();
            assert_eq!(dto.status, variant);
            assert!(dto.comment.is_none());
        }
    }

    #[test]
    fn conversion_patch_dto_rejects_unknown_fields() {
        let raw = r#"{"status":"approved","stale_field":"bar"}"#;
        let err = serde_json::from_str::<ConversionPatchDto>(raw).expect_err("unknown field");
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn target_mode_dto_round_trips_both_variants() {
        for (variant, wire) in [
            (TargetModeDto::Managed, "\"managed\""),
            (TargetModeDto::SelfManaged, "\"self_managed\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, wire);
            let parsed: TargetModeDto = serde_json::from_str(wire).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn conversion_status_dto_round_trips_all_five_variants() {
        for (variant, wire) in [
            (ConversionStatusDto::Pending, "\"pending\""),
            (ConversionStatusDto::Approved, "\"approved\""),
            (ConversionStatusDto::Cancelled, "\"cancelled\""),
            (ConversionStatusDto::Rejected, "\"rejected\""),
            (ConversionStatusDto::Expired, "\"expired\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, wire);
            let parsed: ConversionStatusDto = serde_json::from_str(wire).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn conversion_side_dto_serialises_both_variants() {
        // Response-only enum, so we only assert the serialise side
        // (the field exists for codegen clients on the wire shape).
        assert_eq!(
            serde_json::to_string(&ConversionSideDto::Child).unwrap(),
            "\"child\""
        );
        assert_eq!(
            serde_json::to_string(&ConversionSideDto::Parent).unwrap(),
            "\"parent\""
        );
    }

    #[test]
    fn request_own_conversion_lowering_sets_caller_and_tenant_id() {
        let raw = r#"{"target_mode":"self_managed","comment":"audit"}"#;
        let dto: RequestOwnConversionDto = serde_json::from_str(raw).unwrap();
        let tenant_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let input = dto.into_service_input(ConversionCaller::child(tenant_id));
        assert_eq!(input.tenant_id, tenant_id);
        assert_eq!(input.target_mode, TargetMode::SelfManaged);
        assert_eq!(input.comment.as_deref(), Some("audit"));
    }

    #[test]
    fn request_child_conversion_lowering_carries_body_child_tenant_id() {
        let raw = r#"{
            "child_tenant_id":"00000000-0000-0000-0000-000000000005",
            "target_mode":"managed"
        }"#;
        let dto: RequestChildConversionDto = serde_json::from_str(raw).unwrap();
        let parent_id = Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap();
        let input = dto.into_service_input(ConversionCaller::parent(parent_id));
        assert_eq!(
            input.tenant_id,
            Uuid::parse_str("00000000-0000-0000-0000-000000000005").unwrap()
        );
        assert_eq!(input.target_mode, TargetMode::Managed);
        assert!(input.comment.is_none());
    }
}

// ---- /me identity DTO -------------------------------------------

fn sample_subject_id() -> Uuid {
    Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap()
}

fn sample_home_tenant() -> Uuid {
    Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap()
}

#[test]
fn me_dto_carries_subject_id_type_and_home_tenant() {
    let ctx = SecurityContext::builder()
        .subject_id(sample_subject_id())
        .subject_type("user")
        .subject_tenant_id(sample_home_tenant())
        .build()
        .expect("ctx");

    let dto = MeDto::from_security_context(&ctx);
    let json: Value = serde_json::to_value(&dto).unwrap();

    assert_eq!(
        json,
        json!({
            "subject_id": "33333333-3333-3333-3333-333333333333",
            "subject_type": "user",
            "subject_tenant_id": "44444444-4444-4444-4444-444444444444",
        }),
        "wire shape mirrors SecurityContext: subject id, type, home tenant",
    );
}

#[test]
fn me_dto_omits_subject_type_when_absent() {
    // `subject_type` is the optional IdP claim — when the context carries
    // no type, the field MUST be omitted from the wire (not `null`), so
    // codegen clients see an absent optional rather than an explicit null.
    let ctx = SecurityContext::builder()
        .subject_id(sample_subject_id())
        .subject_tenant_id(sample_home_tenant())
        .build()
        .expect("ctx");

    let dto = MeDto::from_security_context(&ctx);
    let json: Value = serde_json::to_value(&dto).unwrap();

    assert_eq!(
        json,
        json!({
            "subject_id": "33333333-3333-3333-3333-333333333333",
            "subject_tenant_id": "44444444-4444-4444-4444-444444444444",
        }),
        "absent subject_type serialises with no `subject_type` key",
    );
}
