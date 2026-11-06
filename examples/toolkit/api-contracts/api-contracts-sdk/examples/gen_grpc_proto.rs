//! Regenerate the `.proto` file for `PaymentApi` from its contract. Output:
//!
//! ```text
//! proto/api_contracts/payment/v1/payment.proto
//! ```
//!
//! Run with `cargo run --example gen_grpc_proto -p cf-api-contracts-sdk
//! --features grpc-client`, or `cargo xtask proto-regen` for every SDK. The
//! schemas come from the `payment_api_grpc_schemas` visitor that
//! `#[toolkit::grpc_contract]` generates, so no model type is listed here; the
//! `the_committed_proto_is_up_to_date` test fails when this file is stale.
//!
//! The generated file is committed to the repo (per project convention);
//! `build.rs` then compiles it via `tonic-prost-build` when the crate is
//! built with the `grpc-client` feature.

use std::path::PathBuf;

use cf_api_contracts_sdk::contract::payment_api_ir;
use cf_api_contracts_sdk::grpc::{payment_api_grpc_binding, payment_api_grpc_schemas};
use toolkit_contract_protogen::{ProtoSpec, SchemaCollector, write_proto};

fn main() -> Result<(), String> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut schemas = SchemaCollector::default();
    payment_api_grpc_schemas(&mut schemas);
    let spec = ProtoSpec {
        contract: payment_api_ir(),
        binding: payment_api_grpc_binding(),
        schemas,
        proto: manifest_dir.join("proto/api_contracts/payment/v1/payment.proto"),
        lock: manifest_dir.join("proto.lock.toml"),
    };
    write_proto(&spec)?;
    eprintln!(
        "wrote {}; updated {}",
        spec.proto.display(),
        spec.lock.display()
    );
    Ok(())
}
