//! SHA-256 digests in lowercase hex over canonical JSON (`aws-lc-rs`, as pricing's `digest.rs`):
//! the inbox payload digest and the request digest of operation keys and audit rows. They are not
//! part of the FIPS boundary, which covers the TLS data plane only (`docs/security/fips/PRD.md`).

use std::fmt;

use aws_lc_rs::digest::{SHA256, digest};
use serde_json::Value;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

/// A SHA-256 digest as 64 lowercase hex characters.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sha256Hex(String);

impl Sha256Hex {
    /// The digest of `bytes`.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let hash = digest(&SHA256, bytes);
        let mut hex = String::with_capacity(64);
        for byte in hash.as_ref() {
            hex.push(char::from(HEX[usize::from(byte >> 4)]));
            hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        Self(hex)
    }

    /// The digest of the canonical JSON text of `value`: object members sorted by key at every
    /// depth, arrays in order, no insignificant whitespace. Two payloads that differ only in member
    /// order have one digest.
    #[must_use]
    pub fn of_json(value: &Value) -> Self {
        let mut text = String::new();
        canonical_json(value, &mut text);
        Self::of_bytes(text.as_bytes())
    }

    /// Reads a digest back from storage.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] unless `hex` is 64 lowercase hex characters.
    pub fn parse(hex: &str) -> Result<Self, DomainError> {
        if hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(Self(hex.to_owned()))
        } else {
            Err(DomainError::InvalidArgument {
                field: "payload_digest",
                detail: "a digest is 64 lowercase hex characters".to_owned(),
                reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
            })
        }
    }

    /// The 64 hex characters.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha256Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// The canonical JSON text [`Sha256Hex::of_json`] digests: object members sorted by key at every
/// depth, arrays in order, no insignificant whitespace. The name a usage source's id is derived
/// from (feature 04 §3.3).
#[must_use]
pub fn canonical_json_text(value: &Value) -> String {
    let mut text = String::new();
    canonical_json(value, &mut text);
    text
}

fn canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut members: Vec<(&String, &Value)> = map.iter().collect();
            members.sort_by_key(|(key, _)| *key);
            out.push('{');
            for (index, (key, member)) in members.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_json_string(key, out);
                out.push(':');
                canonical_json(member, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        Value::String(text) => push_json_string(text, out),
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// Writes `text` as a JSON string literal, escaped exactly as `serde_json` escapes it, without
/// copying it into a `Value` first: `"` and `\` escaped, `\b \t \n \f \r` in their short form,
/// every other byte below U+0020 as `\u00xx` (lowercase hex), and everything else, non-ASCII
/// included, as it is. A property test holds it to `serde_json`'s output.
fn push_json_string(text: &str, out: &mut String) {
    out.push('"');
    let mut copied = 0;
    for (index, byte) in text.bytes().enumerate() {
        let short = match byte {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            0x08 => "\\b",
            b'\t' => "\\t",
            b'\n' => "\\n",
            0x0c => "\\f",
            b'\r' => "\\r",
            0x20.. => continue,
            _ => "",
        };
        // Only ASCII bytes are escaped, so `index` is a character boundary.
        out.push_str(&text[copied..index]);
        if short.is_empty() {
            out.push_str("\\u00");
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        } else {
            out.push_str(short);
        }
        copied = index + 1;
    }
    out.push_str(&text[copied..]);
    out.push('"');
}

#[cfg(test)]
#[path = "digest_tests.rs"]
mod tests;
