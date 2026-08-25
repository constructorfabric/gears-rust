use super::*;
use crate::domain::system_actor::build_system_ctx;
use async_trait::async_trait;
use credstore_sdk::{GetSecretResponse, SecretValue, SharingMode, TenantId, WritePrecondition};
use parking_lot::Mutex;
use secrecy::ExposeSecret;
use uuid::Uuid;

/// Stub serving one canned response (`GetSecretResponse` is not `Clone`
/// because `SecretValue` isn't; we `take()` on first call).
#[domain_model]
#[derive(Default)]
struct StubClient {
    response: Mutex<Option<GetSecretResponse>>,
}

#[async_trait]
impl CredStoreClientV1 for StubClient {
    async fn create(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
    ) -> Result<(), CredStoreError> {
        // `create` is the one preconditionless write; forward with the
        // delete-whatever-is-there form so the stub stays a no-op.
        self.put(ctx, key, value, sharing, WritePrecondition::Exists)
            .await
    }

    async fn get(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        Ok(self.response.lock().take())
    }

    async fn put(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
        _value: SecretValue,
        _sharing: SharingMode,
        _precondition: WritePrecondition,
    ) -> Result<(), CredStoreError> {
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
        _precondition: WritePrecondition,
    ) -> Result<(), CredStoreError> {
        Ok(())
    }
}

fn make_response(value: &str) -> GetSecretResponse {
    GetSecretResponse {
        value: SecretValue::from(value),
        id: uuid::Uuid::nil(),
        secret_type: String::new(),
        expires_at: None,
        owner_tenant_id: TenantId::nil(),
        sharing: SharingMode::Tenant,
        is_inherited: false,
        version: 1,
    }
}

#[tokio::test]
async fn get_extracts_value_from_response() {
    let stub = Arc::new(StubClient {
        response: Mutex::new(Some(make_response("my-secret"))),
    });
    let ctx = build_system_ctx(Uuid::nil());
    let reader = CredStoreReader::new(stub, ctx);
    let key = SecretRef::new("k").expect("valid SecretRef");
    let got = reader.get(&key).await.expect("read ok");
    assert_eq!(
        got.as_ref().map(ExposeSecret::expose_secret),
        Some("my-secret"),
    );
}

#[tokio::test]
async fn missing_returns_none() {
    let stub = Arc::new(StubClient {
        response: Mutex::new(None),
    });
    let ctx = build_system_ctx(Uuid::nil());
    let reader = CredStoreReader::new(stub, ctx);
    let key = SecretRef::new("k").expect("valid SecretRef");
    assert!(reader.get(&key).await.expect("read ok").is_none());
}

#[tokio::test]
async fn non_utf8_value_surfaces_as_internal_error() {
    // 0xFF is not valid UTF-8 in any position.
    let stub = Arc::new(StubClient {
        response: Mutex::new(Some(GetSecretResponse {
            value: SecretValue::new(vec![0xFF, 0xFE, 0xFD]),
            id: uuid::Uuid::nil(),
            secret_type: String::new(),
            expires_at: None,
            owner_tenant_id: TenantId::nil(),
            sharing: SharingMode::Tenant,
            is_inherited: false,
            version: 1,
        })),
    });
    let ctx = build_system_ctx(Uuid::nil());
    let reader = CredStoreReader::new(stub, ctx);
    let key = SecretRef::new("k").expect("valid SecretRef");
    let err = reader.get(&key).await.expect_err("must reject non-UTF-8");
    assert!(matches!(err, CredStoreError::Internal(_)));
}
