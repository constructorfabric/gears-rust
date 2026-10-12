//! Content-hash modes (ADR-0006): `HashMode`, `ManifestEntry`, `Manifest`.
//!
//! Both modes are SHA-256: `whole-sha256` is `sha256(object bytes)` with no manifest;
//! `multipart-composite-sha256` is `root = sha256(manifest)`, where `manifest` is a canonical
//! text encoding of every part's byte offset and `sha256(part_bytes)`, in ascending order.
//!
//! This module is the only place the manifest wire format is produced or parsed, so every
//! backend and verifier derives the same `root`.
//!
//! Grammar (normative):
//! ```text
//! manifest    = version "," part *("," part)
//! version     = "v1"
//! part        = offset ":" digest
//! offset      = "0" / (nonzero-digit *digit)      ; decimal, no leading zeros
//! digest      = 64(hex-lower)                     ; sha256(part_bytes), lowercase
//! ```

use crate::domain::error::DomainError;
use crate::infra::content::hash;

/// One of the two hash modes, carried from the multipart plan to the stored version row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashMode {
    WholeSha256,
    MultipartCompositeSha256,
}

impl HashMode {
    /// Wire/DB spelling of [`Self::WholeSha256`].
    pub const WHOLE_SHA256: &'static str = "whole-sha256";
    /// Wire/DB spelling of [`Self::MultipartCompositeSha256`].
    pub const MULTIPART_COMPOSITE_SHA256: &'static str = "multipart-composite-sha256";

    /// The wire/DB spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WholeSha256 => Self::WHOLE_SHA256,
            Self::MultipartCompositeSha256 => Self::MULTIPART_COMPOSITE_SHA256,
        }
    }

    /// Parse from the DB/wire spelling; `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            Self::WHOLE_SHA256 => Some(Self::WholeSha256),
            Self::MULTIPART_COMPOSITE_SHA256 => Some(Self::MultipartCompositeSha256),
            _ => None,
        }
    }
}

/// One manifest entry: a part's start offset in the assembled object and its SHA-256 digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestEntry {
    pub offset: u64,
    pub digest: [u8; 32],
}

/// An ordered manifest. Entries are in strictly ascending offset order starting at `0`,
/// enforced by every constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest(Vec<ManifestEntry>);

/// The manifest format-version token; an incompatible grammar change would use a new one.
const VERSION_PREFIX: &str = "v1";

impl Manifest {
    /// Build a manifest from ordered entries.
    ///
    /// # Errors
    /// Validation error if `entries` is empty, does not start at offset `0`, or is not
    /// strictly ascending.
    pub fn new(entries: Vec<ManifestEntry>) -> Result<Self, DomainError> {
        if entries.is_empty() {
            return Err(DomainError::validation(
                "manifest",
                "must have at least one part",
            ));
        }
        if entries[0].offset != 0 {
            return Err(DomainError::validation(
                "manifest",
                "first part must start at offset 0",
            ));
        }
        for pair in entries.windows(2) {
            if pair[1].offset <= pair[0].offset {
                return Err(DomainError::validation(
                    "manifest",
                    "part offsets must be strictly ascending",
                ));
            }
        }
        Ok(Self(entries))
    }

    /// The ordered manifest entries.
    #[must_use]
    pub fn entries(&self) -> &[ManifestEntry] {
        &self.0
    }

    /// Number of parts recorded in this manifest.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Always `false` (a manifest is non-empty by construction); satisfies clippy.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Serialize to `v1,{offset_0}:{hex(digest_0)},...`: no trailing delimiter or whitespace,
    /// lowercase hex, no leading zeros on offsets.
    #[must_use]
    pub fn to_wire_string(&self) -> String {
        // "v1" + per-entry ",{offset}:{64 hex chars}".
        let mut s = String::with_capacity(VERSION_PREFIX.len() + self.0.len() * 90);
        s.push_str(VERSION_PREFIX);
        for entry in &self.0 {
            s.push(',');
            s.push_str(itoa_u64(entry.offset).as_str());
            s.push(':');
            s.push_str(&hex::encode(entry.digest));
        }
        s
    }

    /// Parse a manifest string, rejecting any deviation from the grammar (unknown version,
    /// bad delimiters, non-canonical offsets, digests not 64 lowercase hex, non-ascending
    /// offsets, empty part list).
    ///
    /// # Errors
    /// Validation error describing the first rule violated.
    pub fn from_wire_string(s: &str) -> Result<Self, DomainError> {
        let err = |msg: &'static str| DomainError::validation("manifest", msg);

        let mut segments = s.split(',');
        let prefix = segments.next().ok_or_else(|| err("empty manifest"))?;
        if prefix != VERSION_PREFIX {
            return Err(err("unrecognized manifest version prefix"));
        }

        let mut entries = Vec::new();
        for segment in segments {
            let (offset_str, digest_str) = segment
                .split_once(':')
                .ok_or_else(|| err("malformed part (missing ':' delimiter)"))?;

            if offset_str.is_empty() || !offset_str.bytes().all(|b| b.is_ascii_digit()) {
                return Err(err("offset must be a decimal integer"));
            }
            if offset_str.len() > 1 && offset_str.starts_with('0') {
                return Err(err("offset must not have leading zeros"));
            }
            let offset: u64 = offset_str
                .parse()
                .map_err(|_| err("offset is out of u64 range"))?;

            if digest_str.len() != 64
                || !digest_str
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(err(
                    "digest must be exactly 64 lowercase hex characters (0-9, a-f)",
                ));
            }
            let digest_bytes = hex::decode(digest_str).map_err(|_| err("invalid hex digest"))?;
            let digest: [u8; 32] = digest_bytes
                .try_into()
                .map_err(|_| err("digest must decode to exactly 32 bytes"))?;

            entries.push(ManifestEntry { offset, digest });
        }

        Self::new(entries)
    }

    /// `root = sha256(to_wire_string())`, stored as the version's `hash_value`.
    #[must_use]
    pub fn root(&self) -> [u8; 32] {
        hash::digest_to_array(hash::sha256(self.to_wire_string().as_bytes()))
    }
}

/// Plain decimal `String` without leading zeros.
fn itoa_u64(value: u64) -> String {
    value.to_string()
}

#[cfg(test)]
#[path = "hash_mode_tests.rs"]
mod hash_mode_tests;
