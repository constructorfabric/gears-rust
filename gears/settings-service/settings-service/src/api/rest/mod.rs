// Created: 2026-08-13 by Virtuozzo International GmbH
//! REST surface.

/// The `If-Match` tag a mutation presents, read the same way on every
/// mutation handler of this service.
///
/// Only the header's HTTP framing comes off — surrounding whitespace and one
/// pair of double quotes, which an RFC 7232 client puts around the validator
/// it was given — and nothing else: a weak validator (`W/"…"`) is not
/// stripped and so never matches, as `If-Match`'s strong comparison asks.
/// What is left goes to the domain's precondition evaluator, which compares
/// it verbatim. Absent is `None`, and only the evaluator decides what an
/// absent tag means for the operation at hand.
pub(crate) fn if_match(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().trim_matches('"'))
}

pub mod access_dto;
pub mod access_handlers;
pub mod access_routes;
pub mod declaration_dto;
pub mod declaration_handlers;
pub mod declaration_routes;
pub mod dto;
pub mod handlers;
pub mod routes;
pub mod search_dto;
pub mod search_handlers;
pub mod search_routes;
pub mod setting_dto;
pub mod setting_handlers;
pub mod setting_routes;
pub mod value_dto;
pub mod value_handlers;
pub mod value_routes;

#[cfg(test)]
#[path = "if_match_tests.rs"]
mod if_match_tests;

#[cfg(test)]
#[path = "read_surface_tests.rs"]
mod read_surface_tests;

#[cfg(test)]
#[path = "category_surface_tests.rs"]
mod category_surface_tests;

#[cfg(test)]
#[path = "access_surface_tests.rs"]
mod access_surface_tests;

#[cfg(test)]
#[path = "declaration_surface_tests.rs"]
mod declaration_surface_tests;

#[cfg(test)]
#[path = "value_surface_tests.rs"]
mod value_surface_tests;
