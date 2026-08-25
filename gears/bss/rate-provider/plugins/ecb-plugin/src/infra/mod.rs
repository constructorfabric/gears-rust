//! Infrastructure layer — the outbound ECB feed adapter (HTTP fetch + XML parse
//! + `rate_micro` conversion) behind the ledger's `RateProviderV1` contract.

pub mod source;
