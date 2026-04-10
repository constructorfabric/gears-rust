//! gRPC binding IR — transport-specific projection of a contract for gRPC.
//!
//! Mirrors [`crate::ir::binding::HttpBindingIr`] but encodes gRPC-specific
//! metadata: package, service name, RPC names per method, streaming flags,
//! `idempotency_level` proto3 method option.
//!
//! Lives in the SDK crate (provider-side) — like `HttpBindingIr`.

use serde::{Deserialize, Serialize};

use super::contract::ContractIr;
use super::validation::ValidationError;

/// gRPC binding projection for a contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrpcBindingIr {
    /// Proto package, e.g. `service_hub_demo.payment.v1`.
    pub package: String,
    /// gRPC service name, e.g. `PaymentApi` (`PascalCase`).
    pub service: String,
    /// Per-method gRPC bindings.
    pub methods: Vec<GrpcMethodBindingIr>,
}

impl GrpcBindingIr {
    /// Find the binding for a specific contract method by name.
    #[must_use]
    pub fn find_method(&self, method_name: &str) -> Option<&GrpcMethodBindingIr> {
        self.methods.iter().find(|m| m.method_name == method_name)
    }
}

/// gRPC binding for a single method.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Each bool maps to an independent proto3 method facet (streaming directions, retry policy, optional contract); collapsing them into an enum would conflate orthogonal axes and force serde renames across every consumer of this IR."
)]
pub struct GrpcMethodBindingIr {
    /// Method name from the trait — matches `MethodIr.name` (`snake_case`).
    pub method_name: String,
    /// gRPC RPC name (`PascalCase`, e.g. `Charge`).
    pub rpc_name: String,
    /// `true` when the client streams a sequence of messages.
    #[serde(default)]
    pub client_streaming: bool,
    /// `true` when the server streams a sequence of messages.
    #[serde(default)]
    pub server_streaming: bool,
    /// proto3 `idempotency_level` method option.
    pub idempotency_level: GrpcIdempotency,
    /// Whether the client may auto-retry transient failures.
    #[serde(default)]
    pub retryable: bool,
    /// Whether the underlying contract method has a default body
    /// (peers MAY omit this RPC).
    #[serde(default)]
    pub optional: bool,
}

/// proto3 `idempotency_level` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrpcIdempotency {
    /// `NO_SIDE_EFFECTS` — safe read.
    NoSideEffects,
    /// `IDEMPOTENT` — repeated calls produce the same result.
    Idempotent,
    /// `IDEMPOTENCY_UNKNOWN` (proto3 default) — non-idempotent write.
    NotIdempotent,
}

impl GrpcIdempotency {
    /// proto3 enum-variant identifier (used in generated `.proto` files).
    #[must_use]
    pub const fn proto_variant(self) -> &'static str {
        match self {
            GrpcIdempotency::NoSideEffects => "NO_SIDE_EFFECTS",
            GrpcIdempotency::Idempotent => "IDEMPOTENT",
            GrpcIdempotency::NotIdempotent => "IDEMPOTENCY_UNKNOWN",
        }
    }
}

/// Validate a gRPC binding IR against its corresponding contract IR.
///
/// Checks:
/// - Package and service name must not be empty.
/// - Every contract method must have a corresponding binding.
/// - No extra bindings for methods not in the contract.
/// - No duplicate `rpc_name` values.
/// - `server_streaming` must agree with `MethodIr.kind == ServerStreaming`.
///
/// # Errors
///
/// Returns a vector of [`ValidationError`] when one or more checks fail.
pub fn validate_grpc_binding(
    contract: &ContractIr,
    binding: &GrpcBindingIr,
) -> Result<(), Vec<ValidationError>> {
    use std::collections::HashSet;

    let mut errors = Vec::new();

    if binding.package.is_empty() {
        errors.push(ValidationError {
            location: "GrpcBindingIr".to_owned(),
            message: "package must not be empty".to_owned(),
        });
    }
    if binding.service.is_empty() {
        errors.push(ValidationError {
            location: "GrpcBindingIr".to_owned(),
            message: "service must not be empty".to_owned(),
        });
    }

    let contract_methods: HashSet<&str> =
        contract.methods.iter().map(|m| m.name.as_str()).collect();
    let mut binding_methods: HashSet<&str> = HashSet::new();
    let mut binding_rpc_names: HashSet<&str> = HashSet::new();

    for method_binding in &binding.methods {
        let method_name = method_binding.method_name.as_str();
        if !binding_methods.insert(method_name) {
            errors.push(ValidationError {
                location: format!("GrpcBindingIr.methods[{method_name}]"),
                message: format!("duplicate binding for contract method: {method_name}"),
            });
        }
        let rpc_name = method_binding.rpc_name.as_str();
        if !binding_rpc_names.insert(rpc_name) {
            errors.push(ValidationError {
                location: format!("GrpcBindingIr.methods[{method_name}]"),
                message: format!("duplicate rpc_name: {rpc_name}"),
            });
        }

        if !contract_methods.contains(method_name) {
            errors.push(ValidationError {
                location: format!("GrpcBindingIr.methods[{method_name}]"),
                message: format!("binding for unknown method not in contract: {method_name}"),
            });
            continue;
        }

        // server_streaming flag must agree with MethodIr.kind.
        let Some(contract_method) = contract.methods.iter().find(|m| m.name == method_name) else {
            // Unreachable: the `contract_methods.contains` check above continues for
            // any binding whose method is not in the contract. Skip rather than panic.
            continue;
        };
        let kind_streaming = matches!(
            contract_method.kind,
            super::contract::MethodKind::ServerStreaming
        );
        if kind_streaming != method_binding.server_streaming {
            errors.push(ValidationError {
                location: format!("GrpcBindingIr.methods[{method_name}]"),
                message: format!(
                    "server_streaming flag ({}) does not match contract MethodKind ({:?})",
                    method_binding.server_streaming, contract_method.kind,
                ),
            });
        }
    }

    for name in &contract_methods {
        if !binding_methods.contains(name) {
            errors.push(ValidationError {
                location: format!("GrpcBindingIr.methods[{name}]"),
                message: format!("missing binding for contract method: {name}"),
            });
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::ir::contract::{
        FieldIr, Idempotency, InputShape, MethodIr, MethodKind, PrimitiveType, ServiceIr, TypeRef,
    };

    fn sample_contract() -> ContractIr {
        ServiceIr {
            name: "PaymentApi".into(),
            gear: "service-hub-demo".into(),
            version: "v1".into(),
            methods: vec![
                MethodIr {
                    name: "charge".into(),
                    kind: MethodKind::Unary,
                    input: InputShape {
                        fields: vec![FieldIr {
                            name: "req".into(),
                            ty: TypeRef::Named("ChargeRequest".into()),
                            optional: false,
                            role: crate::ir::contract::FieldRole::Wire,
                        }],
                    },
                    output: TypeRef::Named("ChargeResponse".into()),
                    error: Some(TypeRef::Named("PaymentError".into())),
                    idempotency: Idempotency::NonIdempotentWrite,
                    optional: false,
                },
                MethodIr {
                    name: "list_payments".into(),
                    kind: MethodKind::ServerStreaming,
                    input: InputShape {
                        fields: vec![FieldIr {
                            name: "filter".into(),
                            ty: TypeRef::Primitive(PrimitiveType::String),
                            optional: false,
                            role: crate::ir::contract::FieldRole::Wire,
                        }],
                    },
                    output: TypeRef::Named("PaymentSummary".into()),
                    error: Some(TypeRef::Named("PaymentError".into())),
                    idempotency: Idempotency::SafeRead,
                    optional: false,
                },
            ],
        }
    }

    fn sample_binding() -> GrpcBindingIr {
        GrpcBindingIr {
            package: "service_hub_demo.payment.v1".into(),
            service: "PaymentApi".into(),
            methods: vec![
                GrpcMethodBindingIr {
                    method_name: "charge".into(),
                    rpc_name: "Charge".into(),
                    client_streaming: false,
                    server_streaming: false,
                    idempotency_level: GrpcIdempotency::NotIdempotent,
                    retryable: false,
                    optional: false,
                },
                GrpcMethodBindingIr {
                    method_name: "list_payments".into(),
                    rpc_name: "ListPayments".into(),
                    client_streaming: false,
                    server_streaming: true,
                    idempotency_level: GrpcIdempotency::NoSideEffects,
                    retryable: false,
                    optional: false,
                },
            ],
        }
    }

    #[test]
    fn validates_complete_binding() {
        validate_grpc_binding(&sample_contract(), &sample_binding()).expect("valid");
    }

    #[test]
    fn rejects_missing_method_binding() {
        let mut binding = sample_binding();
        binding.methods.pop();
        let errs = validate_grpc_binding(&sample_contract(), &binding).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("missing binding for contract method"))
        );
    }

    #[test]
    fn rejects_duplicate_rpc_name() {
        let mut binding = sample_binding();
        binding.methods[1].rpc_name = "Charge".into();
        let errs = validate_grpc_binding(&sample_contract(), &binding).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("duplicate rpc_name"))
        );
    }

    #[test]
    fn rejects_streaming_flag_mismatch() {
        let mut binding = sample_binding();
        binding.methods[0].server_streaming = true; // charge is unary
        let errs = validate_grpc_binding(&sample_contract(), &binding).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("server_streaming flag"))
        );
    }

    #[test]
    fn rejects_extra_binding() {
        let mut binding = sample_binding();
        binding.methods.push(GrpcMethodBindingIr {
            method_name: "ghost".into(),
            rpc_name: "Ghost".into(),
            client_streaming: false,
            server_streaming: false,
            idempotency_level: GrpcIdempotency::NotIdempotent,
            retryable: false,
            optional: false,
        });
        let errs = validate_grpc_binding(&sample_contract(), &binding).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("binding for unknown method"))
        );
    }

    #[test]
    fn empty_package_or_service_rejected() {
        let mut binding = sample_binding();
        binding.package = String::new();
        let errs = validate_grpc_binding(&sample_contract(), &binding).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("package must not be empty"))
        );
    }

    #[test]
    fn proto_variant_mapping() {
        assert_eq!(
            GrpcIdempotency::NoSideEffects.proto_variant(),
            "NO_SIDE_EFFECTS"
        );
        assert_eq!(GrpcIdempotency::Idempotent.proto_variant(), "IDEMPOTENT");
        assert_eq!(
            GrpcIdempotency::NotIdempotent.proto_variant(),
            "IDEMPOTENCY_UNKNOWN"
        );
    }
}
