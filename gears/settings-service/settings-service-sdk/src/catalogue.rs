// Created: 2026-09-06 by Constructor Tech
//! The starter catalogue of value types a declaration's `value_type_id` names.
//!
//! A setting's *key* is a type of its own (ADR-002) and says nothing about the
//! shape of its value; the shape comes from a curated **value type** under
//! `gts.cf.toolkit.settings.type_*~`, named by the declaration's
//! `value_type_id` and validated against by the Type Validator. DESIGN.md §4.7
//! gives that catalogue to the toolkit. Nothing in the workspace declares it
//! yet, so this module carries a starter set as the interim owner — recorded in
//! DECOMPOSITION §1 — declared the way every other platform base type is:
//! submitted to the link-time schema inventory that the types registry drains
//! when it initializes.
//!
//! # Why hand-written JSON rather than `#[gts_type_schema]`
//!
//! The macro derives an **object** schema from a struct's fields. A value type
//! is mostly a scalar — a boolean flag, a string, a port — and its trait
//! annotations (`x-gts-traits`) are what tell the Type Validator to compile a
//! regex or the reader to mask a secret. Both are stated directly here, and a
//! test pins every entry's shape.
//!
//! # Compatibility
//!
//! Value types evolve under the registry's **backward** rule (§4.7): a revision
//! it accepts as compatible is a minor of the same `v1`, and a breaking change
//! is a new value type that a setting adopts through a new major of its own.
//! That is a registry-side check on the schema diff, not a keyword here.

use serde_json::{Value, json};
use toolkit_gts::InventoryTypeSchema;

/// The prefix every catalogue value type shares.
pub const VALUE_TYPE_PREFIX: &str = "gts.cf.toolkit.settings.type_";

/// One catalogue entry: the type id and the schema it registers.
#[derive(Debug, Clone, Copy)]
pub struct ValueType {
    /// The GTS type id a declaration names in `value_type_id`.
    pub id: &'static str,
    /// The JSON Schema document registered under that id.
    pub schema: fn() -> Value,
}

/// The trait vocabulary the Type Validator and the reader interpret, stated as
/// the `x-gts-traits-schema` of every catalogue type that carries traits.
fn traits_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "secret": {
                "type": "boolean",
                "description": "The value is a credential: stored by reference, masked on every administrative path, plaintext machine-only."
            },
            "multiline": {
                "type": "boolean",
                "description": "Rendered as multi-line text."
            },
            "regex": {
                "type": "boolean",
                "description": "The value is a regular expression and must compile."
            },
            "cron_dialect": {
                "type": "string",
                "description": "The value is a cron expression in this dialect."
            }
        }
    })
}

/// A schema document with the envelope every entry shares.
fn value_type(id: &str, description: &str, mut body: serde_json::Map<String, Value>) -> Value {
    let mut doc = serde_json::Map::new();
    doc.insert("$id".to_owned(), json!(format!("gts://{id}")));
    doc.insert(
        "$schema".to_owned(),
        json!("http://json-schema.org/draft-07/schema#"),
    );
    doc.insert("description".to_owned(), json!(description));
    if body.contains_key("x-gts-traits") {
        doc.insert("x-gts-traits-schema".to_owned(), traits_schema());
    }
    doc.append(&mut body);
    Value::Object(doc)
}

/// Declare one catalogue type: its id constant, its schema function, and its
/// inventory submission.
macro_rules! catalogue_type {
    ($(#[$doc:meta])* $const_name:ident, $type_name:literal, $fn_name:ident, $description:literal, $body:tt) => {
        $(#[$doc])*
        pub const $const_name: &str = concat!("gts.cf.toolkit.settings.type_", $type_name, ".v1~");

        fn $fn_name() -> Value {
            let Value::Object(body) = json!($body) else {
                unreachable!("catalogue bodies are objects")
            };
            value_type($const_name, $description, body)
        }

        toolkit_gts::inventory::submit! {
            InventoryTypeSchema {
                type_id: $const_name,
                schema_fn: || $fn_name().to_string(),
            }
        }
    };
}

catalogue_type! {
    /// A yes/no switch.
    BOOL_FLAG, "bool_flag", bool_flag_schema,
    "A boolean flag.",
    { "type": "boolean" }
}

catalogue_type! {
    /// A single line of text.
    STRING, "string", string_schema,
    "A single-line string of at most 4096 characters.",
    { "type": "string", "maxLength": 4096 }
}

catalogue_type! {
    /// Multi-line text, rendered as such.
    TEXT, "text", text_schema,
    "Multi-line text of at most 32768 characters.",
    { "type": "string", "maxLength": 32768, "x-gts-traits": { "multiline": true } }
}

catalogue_type! {
    /// A credential: stored by reference, masked everywhere, plaintext machine-only.
    SECRET_STRING, "secret_string", secret_string_schema,
    "A secret string. Stored in the Credential Store by reference, masked on every administrative path; plaintext resolves only through the machine-only reader.",
    { "type": "string", "maxLength": 4096, "x-gts-traits": { "secret": true } }
}

catalogue_type! {
    /// A whole number within the range a double carries exactly.
    INTEGER, "integer", integer_schema,
    "An integer. The value guard bounds it to the range IEEE-754 binary64 represents exactly.",
    { "type": "integer" }
}

catalogue_type! {
    /// A number, integer or decimal.
    NUMBER, "number", number_schema,
    "A number, integer or decimal, canonical under IEEE-754 binary64.",
    { "type": "number" }
}

catalogue_type! {
    /// A TCP or UDP port.
    PORT, "port", port_schema,
    "A TCP or UDP port number.",
    { "type": "integer", "minimum": 1, "maximum": 65535 }
}

catalogue_type! {
    /// A non-negative duration in seconds.
    DURATION_SECONDS, "duration_seconds", duration_seconds_schema,
    "A duration in whole seconds, zero or more.",
    { "type": "integer", "minimum": 0 }
}

catalogue_type! {
    /// A URL.
    URL, "url", url_schema,
    "An absolute URL.",
    { "type": "string", "format": "uri", "maxLength": 2048 }
}

catalogue_type! {
    /// A DNS host name.
    HOSTNAME, "hostname", hostname_schema,
    "A DNS host name.",
    { "type": "string", "format": "hostname", "maxLength": 253 }
}

catalogue_type! {
    /// An IPv4 address.
    IPV4, "ipv4", ipv4_schema,
    "An IPv4 address in dotted-quad form.",
    { "type": "string", "format": "ipv4" }
}

catalogue_type! {
    /// An e-mail address.
    EMAIL, "email", email_schema,
    "An e-mail address.",
    { "type": "string", "format": "email", "maxLength": 320 }
}

catalogue_type! {
    /// A cron expression in the standard five-field dialect.
    CRON, "cron", cron_schema,
    "A cron expression in the standard five-field dialect.",
    { "type": "string", "maxLength": 256, "x-gts-traits": { "cron_dialect": "standard" } }
}

catalogue_type! {
    /// A regular expression that must compile.
    REGEX, "regex", regex_schema,
    "A regular expression; the value is refused unless it compiles.",
    { "type": "string", "maxLength": 1024, "x-gts-traits": { "regex": true } }
}

catalogue_type! {
    /// An arbitrary JSON object.
    JSON, "json", json_schema,
    "An arbitrary JSON object, bounded only by the value size cap.",
    { "type": "object" }
}

/// Every entry of the starter catalogue, in a fixed order.
pub const CATALOGUE: &[ValueType] = &[
    ValueType {
        id: BOOL_FLAG,
        schema: bool_flag_schema,
    },
    ValueType {
        id: STRING,
        schema: string_schema,
    },
    ValueType {
        id: TEXT,
        schema: text_schema,
    },
    ValueType {
        id: SECRET_STRING,
        schema: secret_string_schema,
    },
    ValueType {
        id: INTEGER,
        schema: integer_schema,
    },
    ValueType {
        id: NUMBER,
        schema: number_schema,
    },
    ValueType {
        id: PORT,
        schema: port_schema,
    },
    ValueType {
        id: DURATION_SECONDS,
        schema: duration_seconds_schema,
    },
    ValueType {
        id: URL,
        schema: url_schema,
    },
    ValueType {
        id: HOSTNAME,
        schema: hostname_schema,
    },
    ValueType {
        id: IPV4,
        schema: ipv4_schema,
    },
    ValueType {
        id: EMAIL,
        schema: email_schema,
    },
    ValueType {
        id: CRON,
        schema: cron_schema,
    },
    ValueType {
        id: REGEX,
        schema: regex_schema,
    },
    ValueType {
        id: JSON,
        schema: json_schema,
    },
];

#[cfg(test)]
#[path = "catalogue_tests.rs"]
mod catalogue_tests;
