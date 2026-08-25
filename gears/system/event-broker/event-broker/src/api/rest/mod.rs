//! REST transport layer (`DESIGN.md` §1.3 Architecture Layers, §3.3 API
//! Contracts). Handler bodies and DTO wire shapes land with #4346; this
//! crate only holds the shells `module.rs` will eventually gate per mode.
//!
//! #4346 adds three more modules here, left undeclared until they hold code
//! because `cargo shear --deny-warnings` rejects comment-only files:
//!
//! - `dto` - serde + utoipa shapes, finalized against `docs/openapi.yaml`
//! - `error` - `DomainError` → RFC 9457 `Problem` (§3.3 Error Response Format)
//! - `extractors` - custom Axum extractors

pub mod handlers;
pub mod routes;
