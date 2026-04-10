//! Behavior tests for `#[toolkit::rest_contract]`.
//!
//! These tests use a self-contained base contract so they do not depend on
//! the demo SDK.

#![allow(clippy::unwrap_used)]

use async_trait::async_trait;
use toolkit_contract::{
    HttpFieldBinding, HttpMethod, contract, rest_contract, validate_contract, validate_http_binding,
};

mod fakes {
    #[derive(Debug, Clone)]
    pub struct FakeSecurityContext;

    impl FakeSecurityContext {
        /// Mirrors the shape of `toolkit_security::SecurityContext::bearer_token`
        /// so the `rest_contract` codegen has a method to call.
        #[allow(
            clippy::unused_self,
            reason = "Method signature must mirror toolkit_security::SecurityContext::bearer_token so the rest_contract codegen calls it via the same `ctx.bearer_token()` shape; making it an associated function would break the call site."
        )]
        pub fn bearer_token(&self) -> Option<&::secrecy::SecretString> {
            None
        }
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct ChargeRequest {
        pub amount: i64,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct ChargeResponse {
        pub id: String,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct Invoice {
        pub id: String,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FakeError {
    #[error("boom")]
    Boom,
    #[cfg(feature = "rest-client")]
    #[error("transport: {0}")]
    Transport(#[from] toolkit_contract::runtime::transport_error::TransportError),
}

// SecurityContext alias-by-name — the macro skips parameters whose type
// path's last segment is `SecurityContext`.
type SecurityContext = fakes::FakeSecurityContext;

#[contract(gear = "billing", version = "v1")]
pub trait BillingApi: Send + Sync {
    #[idempotency(NonIdempotentWrite)]
    async fn charge(
        &self,
        ctx: SecurityContext,
        req: fakes::ChargeRequest,
    ) -> Result<fakes::ChargeResponse, FakeError>;

    #[idempotency(SafeRead)]
    async fn get_invoice(
        &self,
        ctx: SecurityContext,
        invoice_id: String,
    ) -> Result<fakes::Invoice, FakeError>;
}

#[rest_contract(base_path = "/api/billing/v1")]
pub trait BillingApiRest: BillingApi {
    #[post("/charge")]
    async fn charge(
        &self,
        ctx: SecurityContext,
        req: fakes::ChargeRequest,
    ) -> Result<fakes::ChargeResponse, FakeError>;

    #[get("/invoices/{invoice_id}")]
    #[retryable]
    async fn get_invoice(
        &self,
        ctx: SecurityContext,
        invoice_id: String,
    ) -> Result<fakes::Invoice, FakeError>;
}

#[test]
fn binding_function_is_named_in_snake_case() {
    let binding = billing_api_rest_http_binding();
    assert_eq!(binding.base_path, "/api/billing/v1");
    assert_eq!(binding.methods.len(), 2);
}

#[test]
fn post_method_emits_body_binding() {
    let binding = billing_api_rest_http_binding();
    let charge = binding.find_method("charge").expect("method present");
    assert_eq!(charge.http_method, HttpMethod::Post);
    assert_eq!(charge.path_template, "/charge");
    assert!(matches!(
        charge.field_bindings.as_slice(),
        [HttpFieldBinding::Body]
    ));
    assert!(!charge.retryable);
    assert!(!charge.streaming);
}

#[test]
fn get_with_path_param_emits_path_binding() {
    let binding = billing_api_rest_http_binding();
    let get_invoice = binding.find_method("get_invoice").expect("method present");
    assert_eq!(get_invoice.http_method, HttpMethod::Get);
    assert_eq!(get_invoice.path_template, "/invoices/{invoice_id}");
    assert!(get_invoice.retryable);

    let path_binding = get_invoice
        .field_bindings
        .iter()
        .find_map(|fb| match fb {
            HttpFieldBinding::Path { field, param } => Some((field.clone(), param.clone())),
            _ => None,
        })
        .expect("Path binding present");
    assert_eq!(path_binding.0, "invoice_id");
    assert_eq!(path_binding.1, "invoice_id");
}

#[test]
fn security_context_is_skipped_from_field_bindings() {
    let binding = billing_api_rest_http_binding();
    let charge = binding.find_method("charge").expect("present");
    // No Header / Path / Query binding for `ctx`.
    let has_ctx_binding = charge.field_bindings.iter().any(|fb| match fb {
        HttpFieldBinding::Path { field, .. }
        | HttpFieldBinding::Query { field, .. }
        | HttpFieldBinding::Header { field, .. } => field == "ctx",
        HttpFieldBinding::Body => false,
    });
    assert!(!has_ctx_binding);
}

#[test]
fn generated_binding_passes_validation_against_contract_ir() {
    let contract_ir = billing_api_ir();
    let binding = billing_api_rest_http_binding();
    validate_contract(&contract_ir).expect("contract valid");
    validate_http_binding(&contract_ir, &binding).expect("binding valid against contract");
}

// Streaming projection — exercises the `#[streaming]` attribute path even
// though the base trait wraps the return type in a Stream.

#[contract(gear = "stream-svc", version = "v1")]
pub trait StreamSvcBackend: Send + Sync {
    #[idempotency(SafeRead)]
    #[streaming]
    fn ticks(&self, ctx: SecurityContext) -> Result<u64, FakeError>;
}

#[rest_contract(base_path = "/api/stream/v1")]
pub trait StreamSvcBackendRest: StreamSvcBackend {
    #[get("/ticks")]
    #[streaming]
    fn ticks(&self, ctx: SecurityContext) -> Result<u64, FakeError>;
}

#[test]
fn streaming_method_marks_streaming_flag() {
    let binding = stream_svc_backend_rest_http_binding();
    let ticks = binding.find_method("ticks").expect("present");
    assert!(ticks.streaming);
    assert_eq!(ticks.http_method, HttpMethod::Get);
}

// Make sure the projection trait is implementable — i.e. it survives the
// `async_trait` rewrite and accepts a custom impl.
struct DummyClient;

#[async_trait]
impl BillingApi for DummyClient {
    async fn charge(
        &self,
        _ctx: SecurityContext,
        _req: fakes::ChargeRequest,
    ) -> Result<fakes::ChargeResponse, FakeError> {
        Ok(fakes::ChargeResponse {
            id: "charged".to_owned(),
        })
    }

    async fn get_invoice(
        &self,
        _ctx: SecurityContext,
        _invoice_id: String,
    ) -> Result<fakes::Invoice, FakeError> {
        Ok(fakes::Invoice {
            id: "inv-1".to_owned(),
        })
    }
}

#[async_trait]
impl BillingApiRest for DummyClient {
    async fn charge(
        &self,
        ctx: SecurityContext,
        req: fakes::ChargeRequest,
    ) -> Result<fakes::ChargeResponse, FakeError> {
        BillingApi::charge(self, ctx, req).await
    }

    async fn get_invoice(
        &self,
        ctx: SecurityContext,
        invoice_id: String,
    ) -> Result<fakes::Invoice, FakeError> {
        BillingApi::get_invoice(self, ctx, invoice_id).await
    }
}

#[tokio::test]
async fn projection_trait_is_implementable() {
    let c = DummyClient;
    let resp = BillingApiRest::charge(
        &c,
        fakes::FakeSecurityContext,
        fakes::ChargeRequest { amount: 100 },
    )
    .await
    .unwrap();
    assert_eq!(resp.id, "charged");
}
