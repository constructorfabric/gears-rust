//! Optimistic concurrency: the tag a head's `internal_revision` renders as, and
//! the tag an `If-Match` header parses back to.
//!
//! `docs/features/foundation.md`'s save-an-edit flow states the contract this
//! type exists to keep in one place: **"A `GET` on a head returns an `ETag`
//! that a subsequent `PATCH` accepts as `If-Match`"**, and **"A save without
//! `If-Match` is refused `VALIDATION`; a save with a stale `If-Match` is
//! refused `STALE_REVISION`"** (Acceptance Criteria, §"A save writes..."). The
//! same section is explicit that the absent case is `VALIDATION` rather than
//! some other bare-400 shape: *"the request parsed, so the bare 400 this gear
//! reserves for a malformed request does not apply"* — this gear mints no
//! second 400 class, so a missing header and an unreadable one both ride
//! `VALIDATION`, distinguished only by the message.
//!
//! # `internal_revision`, never `published_version`
//!
//! The head row carries two counters, and only one of them is this tag's
//! operand. `internal_revision` "moves on every admitted write" (the entity
//! doc comment, `infra::storage::entity::product::Model` and
//! `entity::sku::Model`) — every save, every transition, every publish bumps
//! it, so it is the one counter that is current exactly when the caller's last
//! read is. `published_version` "moves only on publish": a caller who read a
//! draft between two publishes would see the same `published_version` both
//! times and believe nothing had changed, when a concurrent editor's save had
//! already landed. Pinning `If-Match` to `published_version` would make the
//! precondition blind to exactly the edits `fr-concurrent-edit`-shaped races
//! are about; pinning it to `internal_revision`, as this module does, is what
//! makes every admitted write visible to the next one.
//!
//! # No comparison here
//!
//! This module renders and parses a tag. It does not decide whether a
//! submitted one is current — that comparison reads the row under the write
//! and is the door's and the repository's to make, inside the same statement
//! that would otherwise race it. A second, weaker comparison here, ahead of
//! that one, would buy nothing and could disagree with it.
//!
//! # No increment here
//!
//! For the reason the donor's own module gives (`gears/bss/pricing`'s
//! `domain::concurrency`): the successor is computed by the database, in the
//! same `UPDATE` that matches on the revision the caller read, and never by a
//! caller or by this type. A `next()` here would let two writers holding the
//! same current revision compute the same successor and both write it.

use core::fmt;

use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::validation::ValidationReport;

/// The `internal_revision` of a Product or SKU head, as an optimistic-
/// concurrency tag.
///
/// Carried as `i64` because that is the storage column's own type
/// (`internal_revision: i64` on both `entity::product::Model` and
/// `entity::sku::Model`) — there is no wire width to reconcile the way
/// pricing's `u64`-over-`bigint` `RowVersion` has to, so this type holds the
/// column's representation directly rather than through a checked cast.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InternalRevision(i64);

impl InternalRevision {
    /// Wrap a revision read off, or about to be written to, the head row.
    #[must_use]
    pub const fn new(revision: i64) -> Self {
        Self(revision)
    }

    /// The underlying revision number.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    /// The RFC 9110 **strong** entity tag for this revision: the decimal
    /// revision in double quotes, e.g. `"3"`.
    ///
    /// Strong, not weak: a write guard needs a statement about identity, and a
    /// weak validator only asserts that two representations are semantically
    /// equivalent.
    #[must_use]
    pub fn to_etag(self) -> String {
        format!("\"{}\"", self.0)
    }

    /// Parse one `If-Match` header value naming an internal revision.
    ///
    /// Exactly one strong entity tag is accepted: a double quote, one or more
    /// ASCII digits, a closing double quote, with optional surrounding
    /// whitespace. Three of the refusals are refusals of *meaning* rather than
    /// of syntax:
    ///
    /// - a **weak** validator (`W/"3"`) — RFC 9110 §13.1.1 forbids a weak
    ///   validator on `If-Match`, because a weak comparison cannot decide
    ///   whether a write is safe;
    /// - the **wildcard** `*` — it means "if the resource exists at all", i.e.
    ///   overwrite whichever revision is current. Products takes the same
    ///   position pricing's `RowVersion::from_etag` does, and deliberately the
    ///   opposite of `gears/file-storage`'s write path (`domain/service/
    ///   write.rs`), which accepts `*` and treats `If-Match` as optional on
    ///   several routes. That gear's silent-overwrite tolerance is not a
    ///   precedent here: a later reader must not "fix" this refusal to match
    ///   it — the wildcard is exactly the unconditional write a revision-pinned
    ///   `If-Match` exists to make unreachable;
    /// - a comma-separated **list** — a mutating verb targets one known
    ///   revision, so accepting a list would mean guessing which member the
    ///   caller actually read.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] naming the `If-Match` subject, for every
    /// shape that is not one strong entity tag: the wildcard, a weak
    /// validator, a list, an unquoted or empty body, a non-digit, and a
    /// revision past `i64`. This is the same `VALIDATION` code an absent
    /// header rides (`api::rest::preconditions::if_match`) — the design set
    /// draws no second bare-400 class, so both are refused the same way and
    /// differ only in their message.
    pub fn from_etag(raw: &str) -> Result<Self, DomainError> {
        let digits = strong_tag_body(raw)?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(refuse_tag(
                raw,
                "the tag must quote one or more ASCII digits naming the internal revision",
            ));
        }
        digits
            .parse::<i64>()
            .map(Self)
            .map_err(|_| refuse_tag(raw, "the revision is past the representable range"))
    }
}

/// A strong validator over a resource's **content** rather than a stored
/// counter: the first eight bytes of the SHA-256 of its canonical rendering,
/// as a decimal (P-D-205).
///
/// For a resource with no revision column of its own — the tenant's approval
/// policy is a set of `(kind, quorum)` rows, and a write to one kind moves no
/// counter another kind carries. Pricing's policy tag has the same shape
/// (`policy_tag`, a strong decimal over the whole policy), so a client that
/// speaks one speaks both.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContentTag(u64);

impl ContentTag {
    /// The tag of a canonical-rendering digest: its first eight bytes, big-endian.
    #[must_use]
    pub const fn of_digest(digest: &[u8; 32]) -> Self {
        let [b0, b1, b2, b3, b4, b5, b6, b7, ..] = *digest;
        Self(u64::from_be_bytes([b0, b1, b2, b3, b4, b5, b6, b7]))
    }

    /// The RFC 9110 strong entity tag: the decimal in double quotes.
    #[must_use]
    pub fn to_etag(self) -> String {
        format!("\"{}\"", self.0)
    }

    /// Parse one `If-Match` header value naming a content tag.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] naming the `If-Match` subject for every
    /// shape [`strong_tag_body`] refuses, an empty or non-digit body, and a
    /// value past `u64` — the same `VALIDATION` code as an absent header.
    pub fn from_etag(raw: &str) -> Result<Self, DomainError> {
        let digits = strong_tag_body(raw)?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(refuse_tag(
                raw,
                "the tag must quote one or more ASCII digits naming the content read",
            ));
        }
        digits
            .parse::<u64>()
            .map(Self)
            .map_err(|_| refuse_tag(raw, "the tag is past the representable range"))
    }
}

/// The body of one **strong** entity tag, with every shape that is not one
/// refused — the syntax half of `If-Match`, shared by every tagged subject.
///
/// # Why this is a function and not a rule each parser repeats
///
/// [`InternalRevision::from_etag`] asserts four refusals through it — the
/// wildcard, a weak validator, a list, an unquoted body. A later tag parser
/// reuses this rather than writing the refusals a second time, so the two
/// cannot drift; what differs between tag bodies is only what the quoted body
/// has to look like, which stays with each caller.
///
/// # Errors
///
/// [`DomainError::Validation`] naming the `If-Match` subject for the
/// wildcard, a weak validator, a comma-separated list, and a body not wrapped
/// in double quotes.
pub fn strong_tag_body(raw: &str) -> Result<&str, DomainError> {
    let tag = raw.trim();
    if tag == "*" {
        return Err(refuse_tag(
            raw,
            "the wildcard matches whichever revision is current and would let this write \
             overwrite a concurrent editor's; pin the `ETag` a `GET` on this head returned",
        ));
    }
    if tag.starts_with("W/") {
        return Err(refuse_tag(
            raw,
            "a weak validator (`W/\"...\"`) cannot decide whether this write is safe; a strong \
             entity tag is required",
        ));
    }
    if tag.contains(',') {
        return Err(refuse_tag(
            raw,
            "one entity tag is expected; a comma-separated list does not say which revision was \
             read",
        ));
    }
    tag.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .ok_or_else(|| refuse_tag(raw, "a strong entity tag is wrapped in double quotes"))
}

/// Build the [`DomainError::Validation`] a malformed `If-Match` body is
/// refused with, naming both the raw header value and why it was refused.
fn refuse_tag(raw: &str, why: &str) -> DomainError {
    let mut report = ValidationReport::new();
    report.violate("VALIDATION", "If-Match", format!("If-Match {raw}: {why}"));
    DomainError::Validation(report)
}

impl fmt::Display for InternalRevision {
    /// The bare integer — the storage spelling and the body of the entity
    /// tag, so a log line and a wire value are read the same way.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
