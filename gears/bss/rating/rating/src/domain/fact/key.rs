//! Child identity (DESIGN §3.1): `WindowEvaluationKey = {fact_id, canonical_window_start,
//! aggregation_key}`, written `wk1|{fact_id}|{window_start}|{agg_key canonical JSON}`, and
//! `child_id = UUIDv5(NS_RATING_CHILD, that string)`. Every instant in a name is the one canonical
//! instant text: RFC 3339 in UTC, whole seconds, `Z`.

use bss_rating_core::engine::current::input::AggregationKey;
use time::{OffsetDateTime, UtcOffset};
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::digest::{Sha256Hex, canonical_json_text};
use crate::domain::error::DomainError;

/// Namespace of child ids (`UUIDv5`).
pub const NS_RATING_CHILD: Uuid = Uuid::from_u128(0x0b7e_4f2a_91c3_5d68_a4e1_7c2b_3f90_d815);

/// The identity of a window child.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChildKey {
    /// `UUIDv5(NS_RATING_CHILD, wk1 name)`.
    pub child_id: Uuid,
    /// SHA-256 of the aggregation key's canonical JSON (`agg_key_digest`).
    pub agg_key_digest: Sha256Hex,
}

/// The canonical instant text: `YYYY-MM-DDTHH:MM:SSZ` in UTC. Callers key only on whole seconds
/// (DESIGN 09 §4.1), so the text is exact.
#[must_use]
pub fn instant_text(at: OffsetDateTime) -> String {
    let at = at.to_offset(UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

/// The key of the window child of `fact_id` starting at `window_start` under `aggregation_key`.
///
/// # Errors
///
/// [`DomainError::Internal`] if the aggregation key does not serialize, which its types rule out.
pub fn window_child_key(
    fact_id: Uuid,
    window_start: OffsetDateTime,
    aggregation_key: &AggregationKey,
) -> Result<ChildKey, DomainError> {
    let value = serde_json::to_value(aggregation_key).map_err(|e| {
        DomainError::Internal(
            format!("aggregation key: {e}"),
            Some(crate::domain::error::Cause::new(e)),
        )
    })?;
    let agg = canonical_json_text(&value);
    let name = format!("wk1|{fact_id}|{}|{agg}", instant_text(window_start));
    Ok(ChildKey {
        child_id: Uuid::new_v5(&NS_RATING_CHILD, name.as_bytes()),
        agg_key_digest: Sha256Hex::of_bytes(agg.as_bytes()),
    })
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod tests;
