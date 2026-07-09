//! REST surface for usage-collector. Layout mirrors `resource-group`:
//! `dto` (wire shapes), `handlers/` (axum, one file per resource family),
//! `routes/` (`OperationBuilder`, one file per resource family). Endpoint
//! families are enumerated in `docs/usage-collector-v1.yaml`.

pub mod dto;
pub mod handlers;
pub mod routes;
