//! Consumer gear for the `api-contracts` example.
//!
//! Demonstrates the **consumer half** of the contract-binding model: a gear that
//! depends only on `api-contracts-sdk`, declares
//! `#[toolkit::consumes(contract = PaymentApi, from = "api-contracts")]`, and
//! fulfils its own work by resolving `Arc<dyn PaymentApi>` from the `ClientHub`
//! — never knowing whether that impl is a co-located in-process provider
//! (local-wins) or a generated directory-resolving REST client.
//!
//! Pairs with the provider gear `cf-api-contracts` (`ApiContracts`, which
//! `#[toolkit::provides]` the same `PaymentApi`). See
//! `tests/provider_consumer.rs` for the two-gear boot.

pub mod domain;
pub mod gear;
pub mod rest;

pub use domain::ChargeProxyService;
pub use gear::ApiContractsConsumer;
