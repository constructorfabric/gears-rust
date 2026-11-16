#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! gRPC consumer gear for the `api-contracts` example.
//!
//! Declares `#[toolkit::consumes(contract = PaymentApi, from = "api-contracts",
//! transport = grpc)]`: the runtime wires a `PaymentApiGrpcResolvingClient`
//! that finds the provider's `api_contracts.payment.v1.PaymentApi` service in
//! the directory on each call, and gates `/readyz` until the service is
//! published. The gear's own code only resolves `Arc<dyn PaymentApi>` from the
//! `ClientHub`, exactly as the REST consumer (`cf-api-contracts-consumer`) does.
//!
//! It is a separate gear rather than a feature of the REST consumer: the
//! transport is fixed at compile time, `inventory` collects every `#[consumes]`
//! linked into a binary, and the gear name is the edge route prefix, so one
//! gear name with two transports would put both behind the same route.

pub mod domain;
pub mod gear;
pub mod rest;

pub use domain::PaymentProxyService;
pub use gear::ApiContractsGrpcConsumer;
