//! Public REST API for the token-issuer (JWKS + discovery + gated OBO re-mint).

pub mod dto;
pub mod handlers;
pub mod routes;

pub use routes::register_routes;
