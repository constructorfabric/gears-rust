//! The weak, opaque validator served for a value the caller holds no own row
//! for (ADR-0004, D4; ADR-0009): the text of the REST weak `ETag`, shared by
//! the REST layer and the in-process `Secret`.

use uuid::Uuid;

/// The weak, opaque `ETag` for a `Credential` whose caller holds no own row
/// (ADR-0004, D4): `W/"<16-hex of sha256(tenant_id|reference|winner
/// id|winner version)>"`. Changes exactly when the winning (ancestor's) row
/// changes; never accepted as a valid `If-Match` (RFC 9110 requires the
/// strong comparison there).
#[must_use]
pub fn weak_etag(tenant_id: Uuid, reference: &str, winner_id: Uuid, winner_version: i64) -> String {
    let input = format!("{tenant_id}|{reference}|{winner_id}|{winner_version}");
    let digest = toolkit_security::crypto::hash::Sha256::digest(input.as_bytes());
    let hex: String = digest[..8].iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write as _;
        // Writing into a String cannot fail.
        let _written = write!(acc, "{b:02x}");
        acc
    });
    format!("W/\"{hex}\"")
}
