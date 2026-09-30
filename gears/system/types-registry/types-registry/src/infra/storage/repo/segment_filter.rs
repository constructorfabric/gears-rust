//! Stored GTS segments and the SQL-ready filters a pattern compiles to.
//!
//! `gts-rust` parses both the stored identifier and the pattern; this module
//! mirrors `GtsIdPattern::matches_views` field for field over the parsed
//! segments. Differential tests pin it to `GtsId::matches_pattern` on every
//! backend.

use gts::{
    GTS_ID_PREFIX, GtsId, GtsIdPattern, GtsIdPatternSegment, GtsIdSegment, GtsIdSegmentParts,
};

/// One parsed segment of a stored identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentRow {
    pub segment_no: i16,
    /// `vendor.package.namespace.type`.
    pub segment_name: String,
    pub major: i64,
    pub minor: Option<i64>,
    pub is_type: bool,
}

/// An identifier with no stored segment shape.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SegmentRowError {
    /// Managed identifiers refuse UUID tails (ADR-0001), so no row holds one.
    #[error("a UUID tail has no stored segment")]
    UuidTail,
    #[error("a named segment has no major version")]
    NoMajor,
    #[error("more segments than a smallint position holds")]
    TooManySegments,
}

/// The rows `id` is stored as, in chain order.
///
/// # Errors
/// [`SegmentRowError`] when a segment has no stored shape.
pub fn segment_rows(id: &GtsId) -> Result<Vec<SegmentRow>, SegmentRowError> {
    id.segments()
        .iter()
        .enumerate()
        .map(|(i, segment)| {
            let segment_no = i16::try_from(i).map_err(|_| SegmentRowError::TooManySegments)?;
            match segment {
                GtsIdSegment::Concrete(parts) => Ok(SegmentRow {
                    segment_no,
                    segment_name: name(parts),
                    major: i64::from(parts.ver_major_opt().ok_or(SegmentRowError::NoMajor)?),
                    minor: parts.ver_minor().map(i64::from),
                    is_type: parts.is_type(),
                }),
                GtsIdSegment::UuidTail(_) => Err(SegmentRowError::UuidTail),
            }
        })
        .collect()
}

/// A condition on `segment_name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameFilter {
    Exact(String),
    /// Leading dot-terminated tokens, e.g. `acme.crm.`.
    Prefix(String),
}

/// What the stored segment at `segment_no` must satisfy. The segment must exist;
/// `None` fields are unconstrained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentFilter {
    pub segment_no: i16,
    pub name: Option<NameFilter>,
    pub major: Option<i64>,
    pub minor: Option<i64>,
    pub is_type: Option<bool>,
}

/// A compiled pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatternPlan {
    /// No stored identifier can match.
    Never,
    /// Every filter must hold; positions without one are free, even absent.
    Segments(Vec<SegmentFilter>),
}

/// A wildcard whose given fields are not a leading run. `gts-id` 0.12 never
/// produces one; refusing keeps a future grammar from widening silently.
#[derive(Debug, thiserror::Error)]
#[error("pattern segment `{0}` has no stored-name filter")]
pub struct UnsupportedSegment(pub String);

/// Compile `pattern` to per-segment filters.
///
/// # Errors
/// [`UnsupportedSegment`] for a wildcard shape no name filter expresses.
pub fn compile(pattern: &GtsIdPattern) -> Result<PatternPlan, UnsupportedSegment> {
    let mut filters = Vec::new();
    for (i, segment) in pattern.segments().iter().enumerate() {
        let Ok(segment_no) = i16::try_from(i) else {
            return Ok(PatternPlan::Never);
        };
        match segment {
            GtsIdPatternSegment::Segment(GtsIdSegment::Concrete(parts)) => {
                // Stored segments always carry a major.
                let Some(major) = parts.ver_major_opt() else {
                    return Ok(PatternPlan::Never);
                };
                filters.push(SegmentFilter {
                    segment_no,
                    name: Some(NameFilter::Exact(name(parts))),
                    major: Some(i64::from(major)),
                    minor: parts.ver_minor().map(i64::from),
                    is_type: Some(parts.is_type()),
                });
            }
            GtsIdPatternSegment::Segment(GtsIdSegment::UuidTail(_)) => {
                return Ok(PatternPlan::Never);
            }
            GtsIdPatternSegment::Wildcard(parts) => {
                filters.extend(wildcard(segment_no, parts)?);
            }
        }
    }
    Ok(PatternPlan::Segments(filters))
}

/// A trailing wildcard: its given fields, never `is_type`. A bare `*` also
/// matches an absent segment, so it yields no filter at all.
fn wildcard(
    segment_no: i16,
    parts: &GtsIdSegmentParts,
) -> Result<Option<SegmentFilter>, UnsupportedSegment> {
    if parts.raw() == "*" {
        return Ok(None);
    }
    let fields = [
        parts.vendor(),
        parts.package(),
        parts.namespace(),
        parts.type_name(),
    ];
    let given = fields.iter().take_while(|f| !f.is_empty()).count();
    if fields[given..].iter().any(|f| !f.is_empty()) {
        return Err(UnsupportedSegment(parts.raw().to_owned()));
    }
    let name = match given {
        0 => None,
        4 => Some(NameFilter::Exact(fields.join("."))),
        n => Some(NameFilter::Prefix(format!("{}.", fields[..n].join(".")))),
    };
    Ok(Some(SegmentFilter {
        segment_no,
        name,
        major: parts.ver_major_opt().map(i64::from),
        minor: parts.ver_minor().map(i64::from),
        is_type: None,
    }))
}

/// A `gts_id` range `[lower, upper)` implied by the first segment's filter.
///
/// Wider than the filter (`…v1` also bounds `…v10~`), so it only gives SQL an
/// ordered access path; the segment filter still decides.
#[must_use]
pub fn id_range(filters: &[SegmentFilter]) -> Option<(String, Option<String>)> {
    let first = filters.iter().find(|f| f.segment_no == 0)?;
    let lower = match (&first.name, first.major) {
        (Some(NameFilter::Exact(name)), Some(major)) => format!("{GTS_ID_PREFIX}{name}.v{major}"),
        (Some(NameFilter::Exact(name)), None) => format!("{GTS_ID_PREFIX}{name}."),
        (Some(NameFilter::Prefix(prefix)), _) => format!("{GTS_ID_PREFIX}{prefix}"),
        (None, _) => return None,
    };
    let upper = upper_bound(&lower);
    Some((lower, upper))
}

/// Exclusive upper bound of a byte-order prefix range: the last byte plus one.
///
/// Exact under the binary collation every identifier column carries; GTS text
/// is ASCII, so the increment never overflows in practice.
#[must_use]
pub fn upper_bound(prefix: &str) -> Option<String> {
    let mut bytes = prefix.as_bytes().to_vec();
    let last = bytes.last_mut()?;
    if *last == u8::MAX {
        return None;
    }
    *last += 1;
    String::from_utf8(bytes).ok()
}

fn name(parts: &GtsIdSegmentParts) -> String {
    format!(
        "{}.{}.{}.{}",
        parts.vendor(),
        parts.package(),
        parts.namespace(),
        parts.type_name()
    )
}

#[cfg(test)]
#[path = "segment_filter_tests.rs"]
mod tests;
