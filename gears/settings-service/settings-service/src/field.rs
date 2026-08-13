// Created: 2026-08-12 by Constructor Tech
//! Field-violation vocabulary for `422` responses.
//!
//! ADR 0005 keeps these constants beside the code that emits them so the wire
//! string and its meaning cannot drift apart. Each is the `code` a consumer sees
//! in the problem document's field-level `errors` array, and each is part of the
//! contract: an administrator's tooling matches on the code, never on `message`.

/// The whole request body, when a violation cannot be pinned to one field.
pub const REQUEST_FIELD: &str = "request";

/// A value failed validation against its declared type.
pub const VALIDATION: &str = "validation";

/// A supplied value is not in the canonical form its type requires.
pub const VALUE_NOT_CANONICAL: &str = "value_not_canonical";

/// A supplied value exceeds the configured size cap.
pub const VALUE_TOO_LARGE: &str = "value_too_large";

/// A mutating request omitted the mandatory `If-Match` header.
pub const IF_MATCH_REQUIRED: &str = "if_match_required";
