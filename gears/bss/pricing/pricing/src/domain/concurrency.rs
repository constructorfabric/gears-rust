//! Optimistic concurrency: the row version, and the entity tag a caller submits
//! it back as.
//!
//! `cpt-cf-bss-pricing-fr-concurrent-edit` (`design/01-foundation.md` §1.2)
//! forbids one outcome above every other: a bulk import and an interactive edit
//! both writing the same draft, the loser's change gone and nobody told. The
//! guard is a version the caller must have **read** before it may write — a
//! submit carrying a version that is no longer current is refused as
//! `STALE_VERSION` (409, §3.3) instead of applied.
//!
//! The type lives in the **domain** even though `ETag` and `If-Match` are HTTP
//! spellings. Optimistic concurrency is a Foundation rule rather than a
//! transport convenience: the repositories and the REST layer have to agree, to
//! the byte, on what tag denotes what version. Were the rendering left to the
//! REST layer it would be re-derived there — a second definition of a valid tag,
//! free to drift from the one that parses tags back, and a drift between those
//! two is a submit that looks fresh and is not.
//!
//! # Two tags, because two resources have different state
//!
//! [`RowVersion`] is the tag of a **mutable row**, and it is what every `If-Match`
//! on the authoring plane names. [`PolicyTag`] is the tag of the tenant's
//! approval-threshold policy, which has no mutable row to version: the store is
//! append-only, so the thing a caller read is a *representation* composed of two
//! facts rather than a column. Both live here for the reason the paragraph above
//! gives — one rendering, shared by whoever emits a tag and whoever parses one
//! back — and both share [`strong_tag_body`], so neither reader can end up laxer
//! than the other on the wildcard, the weak validator and the list.
//!
//! There is deliberately **no increment** here. The bump belongs in SQL —
//! `row_version = row_version + 1` in the same atomic UPDATE that matches on
//! the version the caller read — so the successor is computed by the database
//! under the row lock and never by a caller. A `next()` on this type would let
//! two writers holding the same current version compute the same successor and
//! both write it: the silent overwrite the whole construct exists to prevent,
//! reintroduced by the helper meant to serve it.

use std::fmt;

use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

/// The optimistic-concurrency version of an authoring row.
///
/// Carried as `u64` because a version is a count that only ever goes up, while
/// the storage column is `bigint`. Both directions of that mismatch are
/// **checked** rather than cast ([`RowVersion::from_stored`],
/// [`RowVersion::to_stored`]): a cast would turn a corrupt column into a
/// plausible version number, and a plausible version number is what the whole
/// comparison trusts.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowVersion(u64);

impl RowVersion {
    /// Wrap a version number.
    #[must_use]
    pub const fn new(version: u64) -> Self {
        Self(version)
    }

    /// The underlying version number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Rehydrate a version from its `bigint` storage column.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] — **not** [`DomainError::InvalidRequest`] — on
    /// a negative value, naming the column and the value. The column is
    /// `NOT NULL DEFAULT 0` and is only ever incremented, so a negative reading
    /// is a broken invariant: no caller could have caused it and no caller can
    /// reshape a request to avoid it, which is exactly the line between a bad
    /// request and an internal fault.
    pub fn from_stored(stored: i64) -> Result<Self, DomainError> {
        u64::try_from(stored).map(Self).map_err(|_| {
            DomainError::Internal(format!(
                "row_version column holds a negative value: {stored}"
            ))
        })
    }

    /// Render a version for its `bigint` storage column.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] past [`i64::MAX`], naming the column and the
    /// value — the column cannot hold it, and as with
    /// [`RowVersion::from_stored`] there is no request the caller could reshape
    /// to make it fit.
    pub fn to_stored(self) -> Result<i64, DomainError> {
        i64::try_from(self.0).map_err(|_| {
            DomainError::Internal(format!(
                "row_version {} exceeds the bigint column range",
                self.0
            ))
        })
    }

    /// The RFC 9110 **strong** entity tag for this version: the decimal version
    /// in double quotes, e.g. `"12"`.
    ///
    /// Strong on purpose. A weak tag asserts only that two representations are
    /// semantically equivalent, which is a statement about rendering; a write
    /// guard needs a statement about identity.
    #[must_use]
    pub fn to_etag(self) -> String {
        format!("\"{}\"", self.0)
    }

    /// Parse one `If-Match` header value.
    ///
    /// Exactly one strong entity tag is accepted: optional surrounding
    /// whitespace, then a double quote, one or more ASCII digits, a closing
    /// double quote. Everything else is refused rather than coerced, and three
    /// of those refusals are refusals of *meaning* rather than of syntax:
    ///
    /// - a **weak** validator (`W/"12"`) — RFC 9110 §13.1.1 forbids weak
    ///   validators in `If-Match`, because a weak comparison cannot decide
    ///   whether a write is safe;
    /// - the **wildcard** `*` — it means "if the resource exists at all", i.e.
    ///   *overwrite whatever is there*, which is precisely the silent overwrite
    ///   `fr-concurrent-edit` forbids;
    /// - a comma-separated **list** — an authoring mutation targets one known
    ///   version, so picking a member of a list would be guessing which one the
    ///   caller actually read.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidRequest`] naming what was wrong with the value, for
    /// every shape that is not one strong entity tag — the three refusals of
    /// meaning above (a weak validator, the wildcard `*`, a comma-separated
    /// list), and the five of syntax: an unquoted bare integer, an empty tag, a
    /// sign, a non-digit, and a version past `u64`.
    pub fn from_etag(raw: &str) -> Result<Self, DomainError> {
        let refuse = |why: &str| DomainError::InvalidRequest(format!("If-Match {raw}: {why}"));
        let digits = strong_tag_body(raw)?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(refuse("the tag must quote one or more ASCII digits"));
        }
        digits
            .parse::<u64>()
            .map(Self)
            .map_err(|_| refuse("the version is past the representable range"))
    }
}

/// Unwrap **one strong entity tag** and hand back the text inside its quotes.
///
/// Extracted from [`RowVersion::from_etag`] rather than copied, because a second
/// reader of `If-Match` arrived: a plan revision's tag has to name the revision
/// it was minted against as well as the version (`api::rest::preconditions`),
/// and if that reader re-implemented the envelope it would be free to accept a
/// weak validator or a wildcard on the very verbs where those are the defect.
/// The three refusals of *meaning* live here once, and both readers get them.
///
/// **This is `If-Match`'s envelope, not a general precondition parser**, which is
/// why the refusals name that header outright. `*` is refused here because on
/// `If-Match` it means *overwrite whichever version is current*; on
/// `If-None-Match` the same token is the legal spelling of "only if it does not
/// exist yet", so a reader of that header cannot take these rules and would be
/// refusing the one shape it exists to accept. Parameterizing the field name
/// would advertise a reuse the refusals do not support.
///
/// # Errors
///
/// [`DomainError::InvalidRequest`] for the wildcard `*`, a weak validator
/// (`W/"…"`), a comma-separated list, and anything not wrapped in double quotes.
/// What is **inside** the quotes is the caller's to interpret.
pub fn strong_tag_body(raw: &str) -> Result<&str, DomainError> {
    let refuse = |why: &str| DomainError::InvalidRequest(format!("If-Match {raw}: {why}"));
    let tag = raw.trim();

    if tag == "*" {
        return Err(refuse(
            "the wildcard matches any version and would overwrite whichever one is current",
        ));
    }
    if tag.starts_with("W/") {
        return Err(refuse(
            "RFC 9110 forbids a weak validator here; a weak comparison cannot decide whether a write is safe",
        ));
    }
    if tag.contains(',') {
        return Err(refuse(
            "one entity tag is expected, and a list does not say which version was read",
        ));
    }
    tag.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .ok_or_else(|| refuse("a strong entity tag is wrapped in double quotes"))
}

impl fmt::Display for RowVersion {
    /// The bare integer — the storage spelling and the body of the entity tag,
    /// so a log line and a wire value are read the same way.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Refuse a submit that is not working from the row's current version.
///
/// A free function rather than a method on a repository: the same comparison is
/// owed by the transport, which decides the conflict from a header, and by a
/// publish unit that has already read the row. One definition serves both, so
/// the surface can never end up laxer than the store.
///
/// # Errors
///
/// [`DomainError::StaleVersion`] naming **both** versions when they differ. An
/// operator reading the rejection needs to see which read the caller was working
/// from, not only that it was stale — that difference is what distinguishes a
/// caller that never refreshed from a genuine bulk-vs-interactive collision.
pub fn require_match(current: RowVersion, submitted: RowVersion) -> Result<(), DomainError> {
    if current == submitted {
        return Ok(());
    }
    Err(DomainError::StaleVersion(format!(
        "current {current}, submitted {submitted}"
    )))
}

/// The domain separator of [`PolicyTag`]'s preimage.
///
/// A tag is only ever compared with another tag of the same resource, so the
/// separator buys nothing about *collisions*; what it buys is that a digest
/// leaking out of some other framing in this crate — a content pin, an audit
/// hash — can never be a valid policy tag by accident, which is the property
/// that keeps a caller from satisfying this precondition with a string it read
/// somewhere else.
const POLICY_TAG_DOMAIN_SEP: &[u8] = b"cf.bss.pricing.approval_threshold_policy.etag.v1\x00";

/// The taxonomy resource's own domain separator.
///
/// **Distinct from [`POLICY_TAG_DOMAIN_SEP`], and that is the point of a
/// separator rather than a nicety.** Both resources render a [`PolicyTag`] and
/// both parse one back with [`PolicyTag::from_etag`], so without separation a
/// digest computed over one representation could be presented as an assertion
/// about the other — and `If-Match` would accept it. The class is folded in
/// below for the same reason one level down: four taxonomies share this
/// separator, and a tag for the brand list must not satisfy a `PUT` on the
/// partner list.
const TAXONOMY_TAG_DOMAIN_SEP: &[u8] = b"cf.bss.pricing.taxonomy.etag.v1\x00";

/// One taxonomy entry as [`PolicyTag::of_taxonomy`] digests it.
///
/// A named struct rather than a five-tuple, because the two `tax_*` members were
/// added after the tuple existed and a positional pair of `Option<&str>` beside a
/// `bool` is exactly the shape a caller transposes silently. Every field the
/// `GET` renders is here, and that correspondence is the type's whole job: a
/// field the response carries and this does not is a validator that cannot see
/// its own representation.
#[domain_model]
#[derive(Clone, Copy, Debug)]
pub struct TaxonomyTagEntry<'a> {
    /// The declared code.
    pub value: &'a str,
    /// `active` or `retired`.
    pub state: &'a str,
    /// The operator's label.
    pub display_name: &'a str,
    /// D-01's default tax category; `None` on the three non-region taxonomies
    /// and on a region with no default declared.
    pub tax_category: Option<&'a str>,
    /// D-01's tenant-declared rate marker; `false` on the other three.
    pub tax_rate_present: bool,
}

/// NULL-safe framing markers, [`crate::domain::approval::content_pin`]'s, for the
/// reason that module gives: a field's **absence** has to frame differently from
/// every value it could have held.
const TAG_ABSENT: u8 = 0x00;
/// The present marker; see [`TAG_ABSENT`].
const TAG_PRESENT: u8 = 0x01;

/// The entity tag of a tenant's **approval-threshold policy** representation
/// (D-186).
///
/// # Why this resource's tag is not a [`RowVersion`]
///
/// Every other `If-Match` on this surface names a mutable row's `row_version`
/// column. `pricing_approval_threshold` has no such column and never will: it is
/// append-only history, and a version is minted rather than bumped. But an entity
/// tag is a statement about a **representation**, not about a column — so the tag
/// is derived from the two facts the `GET` serves, and it changes exactly when
/// what the caller read changes.
///
/// # The two facts, and why both
///
/// `ThresholdPolicyView` carries `effective` and `pendingApproval`. A tag that
/// moved only with `effective` would not change when a proposal opens or is
/// withdrawn — and a tag that does not change when the representation changes is
/// a broken validator, not a lenient one. So the preimage covers the effective
/// version's **number or its absence** and the pending unit's **id or its
/// absence**.
///
/// # Absence is not version zero, and the framing is what makes that true
///
/// `ThresholdService::propose` mints a tenant's first version as `0`, so "no
/// effective version" and "effective version 0" are two states this tag must not
/// confuse — a bootstrap tag that equalled the tag of the first approved version
/// would accept a `PUT` authored before that approval. Absence frames as a lone
/// [`TAG_ABSENT`] marker and presence as [`TAG_PRESENT`] followed by the number's
/// eight big-endian bytes, so no absence shares a preimage with any value.
///
/// # Opaque to the caller
///
/// The body is a digest and the caller's only correct use of it is to copy it
/// verbatim out of an `ETag` response header and back into `If-Match` — which is
/// the argument
/// [`revision_etag`](crate::api::rest::preconditions::revision_etag) already
/// makes for the plan tag, arriving at a two-token form there because two
/// decimals are what that resource's state *is*. Here the state is a number that
/// may be absent beside a uuid that may be absent, and a digest is the rendering
/// that carries "absent" without spending a sentinel value the domain also uses.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PolicyTag(String);

impl PolicyTag {
    /// The tag of the representation these two facts describe.
    ///
    /// Total: there is no pair this cannot render, which is why it answers the
    /// tag rather than a `Result`.
    #[must_use]
    pub fn of(effective_version: Option<u64>, pending_unit: Option<uuid::Uuid>) -> Self {
        let mut buf = Vec::with_capacity(POLICY_TAG_DOMAIN_SEP.len() + 26);
        buf.extend_from_slice(POLICY_TAG_DOMAIN_SEP);
        match effective_version {
            Some(number) => {
                buf.push(TAG_PRESENT);
                buf.extend_from_slice(&number.to_be_bytes());
            }
            None => buf.push(TAG_ABSENT),
        }
        match pending_unit {
            Some(unit) => {
                buf.push(TAG_PRESENT);
                buf.extend_from_slice(unit.as_bytes());
            }
            None => buf.push(TAG_ABSENT),
        }
        Self(crate::domain::audit::hex_bytes(
            aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &buf).as_ref(),
        ))
    }

    /// The tag of one taxonomy's representation (`04-currency-tax.md` §5).
    ///
    /// The digest covers the **class** and **every field the `GET` renders** for
    /// each entry, in the order the repository reads them — which is ordered by
    /// value, so two reads of an unchanged taxonomy render one tag.
    ///
    /// # Every rendered field, and the one that was missed
    ///
    /// A tag that moved only with membership would not change when an operator
    /// re-labelled a value or retired one, and a validator that does not change
    /// when the representation changes is broken rather than lenient — the same
    /// argument the threshold policy's tag makes for covering its pending unit as
    /// well as its effective version.
    ///
    /// **That argument was stated here while the code did not hold it.** The
    /// first version digested `(value, state, display_name)` and omitted D-01's
    /// two region markers, which the `GET` renders and the `PUT` writes. The
    /// consequence was a lost update with no race in it: an operator changing a
    /// region's `tax_category` left the tag fixed, so a second operator holding
    /// the pre-change tag could revert it and be told the precondition held — and
    /// the reverted field is C4's `RegionTaxReadiness` input, so the blast radius
    /// is a publish blocked, or allowed, on a readiness nobody authored. It also
    /// made this a lying **strong** validator: a conditional `GET` answered 304
    /// for a body that had changed. Found by review; `the_tag_moves_when_only_
    /// the_region_tax_markers_change` is what holds it now.
    ///
    /// # Framing
    ///
    /// Each field is length-prefixed rather than delimited, because a delimiter
    /// is forgeable from inside a `display_name`: an operator could otherwise
    /// label one value so that two different taxonomies digest identically.
    ///
    /// An **absent** `tax_category` is framed distinctly from any present value,
    /// including the empty string, for [`PolicyTag::of`]'s reason — absence is a
    /// different fact from a value, and a scheme that rendered both as nothing
    /// would let one be substituted for the other.
    #[must_use]
    pub fn of_taxonomy<'a>(
        class: &str,
        entries: impl Iterator<Item = TaxonomyTagEntry<'a>>,
    ) -> Self {
        /// Append one length-framed field.
        ///
        /// A free function rather than a closure over `buf`, because the markers
        /// need to push their own presence byte between framed fields and a
        /// closure holding `&mut buf` forbids that.
        fn push(buf: &mut Vec<u8>, field: &str) {
            buf.extend_from_slice(&(field.len() as u64).to_be_bytes());
            buf.extend_from_slice(field.as_bytes());
        }

        let mut buf = Vec::new();
        buf.extend_from_slice(TAXONOMY_TAG_DOMAIN_SEP);
        push(&mut buf, class);
        for entry in entries {
            push(&mut buf, entry.value);
            push(&mut buf, entry.state);
            push(&mut buf, entry.display_name);
            match entry.tax_category {
                Some(category) => {
                    buf.push(TAG_PRESENT);
                    push(&mut buf, category);
                }
                None => buf.push(TAG_ABSENT),
            }
            buf.push(if entry.tax_rate_present {
                TAG_PRESENT
            } else {
                TAG_ABSENT
            });
        }
        Self(crate::domain::audit::hex_bytes(
            aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &buf).as_ref(),
        ))
    }

    /// The digest inside the quotes, for a diagnostic that has to name a tag.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The RFC 9110 **strong** entity tag, ready for an `ETag` response header.
    ///
    /// Strong for [`RowVersion::to_etag`]'s reason: a weak validator asserts
    /// semantic equivalence, and a write guard needs identity.
    #[must_use]
    pub fn to_etag(&self) -> String {
        format!("\"{}\"", self.0)
    }

    /// Parse one `If-Match` header value as a policy tag.
    ///
    /// The envelope is [`strong_tag_body`]'s — no wildcard, no weak validator, no
    /// list, quotes required — and what is inside it is checked to be a tag this
    /// surface could have **issued**: 64 lowercase hex characters. That check is
    /// what keeps the two refusals apart. A body of some other shape was never
    /// served by this resource, so the caller did not read it here and the
    /// request cannot be interpreted (400); a well-formed body that no longer
    /// matches is a premise that moved (409), and only the store can say so.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidRequest`] for the three refusals of meaning
    /// [`strong_tag_body`] owns, and for a body that is not exactly 64 lowercase
    /// hex characters.
    pub fn from_etag(raw: &str) -> Result<Self, DomainError> {
        let body = strong_tag_body(raw)?;
        if body.len() != 64
            || !body
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(DomainError::InvalidRequest(format!(
                "If-Match {raw}: an approval-threshold-policy tag is the opaque digest this \
                 surface issues, 64 lowercase hex characters; pass back the `ETag` the `GET` \
                 handed you, verbatim"
            )));
        }
        Ok(Self(body.to_owned()))
    }
}

impl fmt::Display for PolicyTag {
    /// The bare digest — the body of the entity tag, so a log line and a wire
    /// value are read the same way ([`RowVersion`]'s `Display` makes the same
    /// choice).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Refuse a policy proposal whose premise has moved.
///
/// [`require_match`]'s counterpart, and the same category: `STALE_VERSION` (409)
/// is `01-foundation.md` §3.3's name for an *`ETag`/row-version conflict*, and a
/// tag that no longer describes the resource is precisely that. It is deliberately
/// **not** a 412: the canonical error family this gear renders through carries no
/// such status, and §3.3 forbids minting one — the same argument that keeps 422
/// out of the gear.
///
/// # Errors
///
/// [`DomainError::StaleVersion`] naming **both** tags when they differ, for
/// [`require_match`]'s reason. The tags are opaque, so the detail also says what
/// the remedy is: re-read, and resubmit against what the read hands back.
pub fn require_policy_match(current: &PolicyTag, submitted: &PolicyTag) -> Result<(), DomainError> {
    if current == submitted {
        return Ok(());
    }
    Err(DomainError::StaleVersion(format!(
        "the approval-threshold policy has moved since it was read: current {current}, submitted \
         {submitted}. Either a version took effect or a proposal opened or closed; re-read the \
         policy and author against the `ETag` that read hands back"
    )))
}

#[cfg(test)]
#[path = "concurrency_tests.rs"]
mod concurrency_tests;
