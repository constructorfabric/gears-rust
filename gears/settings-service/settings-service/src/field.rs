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

/// A category key falls outside the 1..128 character bound.
pub const CATEGORY_KEY_LENGTH: &str = "category_key_length";

/// A category name falls outside the 1..256 character bound.
pub const CATEGORY_NAME_LENGTH: &str = "category_name_length";

/// A category description exceeds the 4096 character bound.
pub const CATEGORY_DESCRIPTION_LENGTH: &str = "category_description_length";

/// A caller tried to change a category `key`, which is immutable.
pub const CATEGORY_KEY_IMMUTABLE: &str = "category_key_immutable";

/// A category key contains the reserved `/` separator.
pub const CATEGORY_KEY_RESERVED_SEPARATOR: &str = "category_key_reserved_separator";

/// An `OData` expression referenced an unmapped field, used an unsupported
/// operator, or carried a cursor that no longer decodes.
pub const ODATA_QUERY: &str = "odata_query";

/// A request used an `OData` option this resource does not implement.
pub const ODATA_UNSUPPORTED_OPTION: &str = "odata_unsupported_option";
