// Created: 2026-08-12 by Constructor Tech
//! Field-violation vocabulary for validation rejections — canonical invalid-argument, rendered as `400`.
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

/// The named value type is not registered in the types registry.
pub const VALUE_TYPE_UNKNOWN: &str = "value_type_unknown";

/// The value violates its type's JSON Schema at the named path.
pub const VALUE_SCHEMA: &str = "value_schema";

/// The value violates a `format` keyword its type declares.
pub const VALUE_FORMAT: &str = "value_format";

/// A regex-bearing value does not compile.
pub const VALUE_REGEX_INVALID: &str = "value_regex_invalid";

/// An entity-reference value does not resolve to a registered instance.
pub const VALUE_REFERENCE_UNRESOLVED: &str = "value_reference_unresolved";

/// A scope path that is neither `/` nor `/tenants/{id}`.
pub const SCOPE_PATH: &str = "scope_path";

/// A `tenant` query parameter that is not a UUID.
pub const TENANT_PARAM: &str = "tenant_param";

/// A clone of a secret-classified setting, which would couple the target to
/// the source credential's lifecycle.
pub const SECRET_NOT_CLONEABLE: &str = "secret_not_cloneable";

/// A secret handle that does not decode; the token itself is never echoed.
pub const SECRET_HANDLE_MALFORMED: &str = "secret_handle_malformed";

/// A secret handle naming a setting that is not secret-classified.
pub const NOT_A_SECRET: &str = "not_a_secret";

/// A composed setting key segment outside the GTS grammar.
pub const SETTING_KEY_SEGMENT: &str = "setting_key_segment";

/// A declaration without a valid scope class.
pub const SCOPE_CLASS_INVALID: &str = "scope_class_invalid";

/// A declaration whose author-supplied classification contradicts the trait.
pub const CLASSIFICATION_CONFLICT: &str = "classification_conflict";

/// A non-empty Schema Default on a secret-trait declaration.
pub const SECRET_DEFAULT_NOT_EMPTY: &str = "secret_default_not_empty";

/// `anonymous_exposable` on a setting that is not `public`.
pub const EXPOSABLE_NOT_SENSITIVE: &str = "exposable_not_sensitive";

/// A `PATCH` field that is immutable or unknown.
pub const DECLARATION_FIELD_IMMUTABLE: &str = "declaration_field_immutable";

/// `default_value` omitted from a declaration.
pub const DEFAULT_REQUIRED: &str = "default_required";

/// A cron expression that does not parse under its declared dialect.
pub const VALUE_CRON_INVALID: &str = "value_cron_invalid";

/// A cron dialect this gear cannot check.
pub const VALUE_CRON_DIALECT_UNKNOWN: &str = "value_cron_dialect_unknown";

/// A value outside the membership of its declared dynamic enumeration.
pub const VALUE_NOT_IN_ENUM: &str = "value_not_in_enum";

/// A dynamic enumeration source that does not resolve.
pub const VALUE_ENUM_SOURCE_UNKNOWN: &str = "value_enum_source_unknown";
