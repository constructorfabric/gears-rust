//! ISO civil dates shared by REST DTOs and broker payloads.
use serde::{Deserialize, Deserializer, Serializer};
use time::Date;

/// Serialize a date as `YYYY-MM-DD`.
/// # Errors
/// Returns serialization or format errors.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde with requires a borrowed field serializer"
)]
pub fn serialize<S: Serializer>(date: &Date, serializer: S) -> Result<S::Ok, S::Error> {
    let format = time::format_description::parse_borrowed::<1>("[year]-[month]-[day]")
        .map_err(serde::ser::Error::custom)?;
    let text = date.format(&format).map_err(serde::ser::Error::custom)?;
    serializer.serialize_str(&text)
}
/// Parse an ISO civil date, rejecting invalid calendar dates.
/// # Errors
/// Returns deserialization or date-parse errors.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Date, D::Error> {
    let text = String::deserialize(deserializer)?;
    let format = time::format_description::parse_borrowed::<1>("[year]-[month]-[day]")
        .map_err(serde::de::Error::custom)?;
    Date::parse(&text, &format).map_err(serde::de::Error::custom)
}
/// Nullable date fields use the same ISO representation; `None` is JSON null.
pub mod option {
    use super::{Date, Deserialize, Deserializer, Serializer};
    /// Serialize an optional civil date.
    /// # Errors
    /// Returns serialization or format errors.
    #[allow(
        clippy::ref_option,
        clippy::trivially_copy_pass_by_ref,
        reason = "serde with requires a borrowed optional field serializer"
    )]
    pub fn serialize<S: Serializer>(date: &Option<Date>, serializer: S) -> Result<S::Ok, S::Error> {
        match date {
            Some(date) => super::serialize(date, serializer),
            None => serializer.serialize_none(),
        }
    }
    /// Parse an optional civil date.
    /// # Errors
    /// Returns deserialization or date-parse errors.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Date>, D::Error> {
        let text = Option::<String>::deserialize(deserializer)?;
        text.map(|s| {
            let format = time::format_description::parse_borrowed::<1>("[year]-[month]-[day]")
                .map_err(serde::de::Error::custom)?;
            Date::parse(&s, &format).map_err(serde::de::Error::custom)
        })
        .transpose()
    }
}
