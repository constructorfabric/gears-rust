//! Regenerate the `.proto` file for `PaymentApi` from `ContractIr` +
//! schemars-derived schemas. Output is written to:
//!
//! ```text
//! proto/api_contracts/payment/v1/payment.proto
//! ```
//!
//! Run with: `cargo run --example gen_grpc_proto -p cf-api-contracts-sdk`.
//!
//! The generated file is committed to the repo (per project convention);
//! `build.rs` then compiles it via `tonic-prost-build` when the crate is
//! built with the `grpc-client` feature.

use std::fs;
use std::path::PathBuf;

use anyhow::Context as _;
use cf_api_contracts_sdk::contract::payment_api_ir;
use cf_api_contracts_sdk::grpc::payment_api_grpc_binding;
use cf_api_contracts_sdk::models::{
    ChargeRequest, ChargeResponse, Invoice, ListPaymentsFilter, PaymentStatus, PaymentSummary,
};
use schemars::schema_for;

fn main() -> anyhow::Result<()> {
    let contract = payment_api_ir();
    let binding = payment_api_grpc_binding();

    let schemas: Vec<(&str, schemars::Schema)> = vec![
        ("ChargeRequest", schema_for!(ChargeRequest)),
        ("ChargeResponse", schema_for!(ChargeResponse)),
        ("Invoice", schema_for!(Invoice)),
        ("ListPaymentsFilter", schema_for!(ListPaymentsFilter)),
        ("PaymentSummary", schema_for!(PaymentSummary)),
        ("PaymentStatus", schema_for!(PaymentStatus)),
    ];

    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_owned());
    let manifest_dir = PathBuf::from(manifest);
    let out_path = manifest_dir.join("proto/api_contracts/payment/v1/payment.proto");
    let lock_path = manifest_dir.join("proto.lock.toml");

    // Load historic field-number assignments. Missing file → empty lock
    // (first run after lockfile introduction). Existing entries are
    // preserved verbatim; new fields receive next-free numbers.
    let mut lock = toolkit_contract_protogen::ProtoLockfile::load(&lock_path)
        .with_context(|| format!("load {}", lock_path.display()))?;

    let proto =
        toolkit_contract_protogen::generate_proto_file(&contract, &binding, &schemas, &mut lock)
            .context("generating .proto from ContractIr + schemas")?;

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    fs::write(&out_path, &proto).with_context(|| format!("write {}", out_path.display()))?;
    lock.save(&lock_path)
        .with_context(|| format!("save {}", lock_path.display()))?;

    eprintln!(
        "wrote {} ({} bytes); updated {}",
        out_path.display(),
        proto.len(),
        lock_path.display()
    );
    Ok(())
}
