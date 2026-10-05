//! Exact schema-1 scalar adapters. No float, precision truncation or implicit timestamp format.
#![allow(
    clippy::ref_option,
    clippy::trivially_copy_pass_by_ref,
    reason = "serde with adapters require references to the exact field type"
)]
use serde::{Deserialize, Deserializer};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

pub(super) fn instant_text(value: OffsetDateTime) -> Result<String, &'static str> {
    let v = value.to_offset(UtcOffset::UTC);
    if !(0..=9999).contains(&v.year()) {
        return Err("timestamp year outside schema-1 profile");
    }
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        v.year(),
        u8::from(v.month()),
        v.day(),
        v.hour(),
        v.minute(),
        v.second(),
        v.nanosecond()
    ))
}
pub(super) mod instant {
    use super::{OffsetDateTime, Rfc3339, instant_text};
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _, ser::Error as _};
    pub fn serialize<S: Serializer>(v: &OffsetDateTime, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&instant_text(*v).map_err(S::Error::custom)?)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<OffsetDateTime, D::Error> {
        let s = String::deserialize(d)?;
        let v = OffsetDateTime::parse(&s, &Rfc3339).map_err(D::Error::custom)?;
        if instant_text(v).map_err(D::Error::custom)? != s {
            return Err(D::Error::custom("noncanonical UTC instant"));
        }
        Ok(v)
    }
}
pub(super) mod decimal {
    use rust_decimal::Decimal;
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};
    pub fn serialize<S: Serializer>(v: &Decimal, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.normalize().to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Decimal, D::Error> {
        let s = String::deserialize(d)?;
        let v = Decimal::from_str_exact(&s).map_err(D::Error::custom)?;
        if v.normalize().to_string() != s {
            return Err(D::Error::custom("noncanonical exact decimal"));
        }
        Ok(v)
    }
}
pub(super) mod integer {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};
    pub fn serialize<T: std::fmt::Display, S: Serializer>(v: &T, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, T: std::str::FromStr + std::fmt::Display, D: Deserializer<'de>>(
        d: D,
    ) -> Result<T, D::Error> {
        let s = String::deserialize(d)?;
        let v: T = s
            .parse()
            .map_err(|_| D::Error::custom("integer overflow or syntax"))?;
        if v.to_string() != s {
            return Err(D::Error::custom("noncanonical integer"));
        }
        Ok(v)
    }
}
pub(super) mod digest {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};
    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&crate::infra::usage_policy_wire::digest_text(*v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        crate::infra::usage_policy_wire::parse_digest_text(&s)
            .ok_or_else(|| D::Error::custom("invalid digest"))
    }
}
pub(super) mod date {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};
    pub fn serialize<S: Serializer>(v: &time::Date, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<time::Date, D::Error> {
        let s = String::deserialize(d)?;
        let v = time::Date::parse(&s, &time::format_description::well_known::Iso8601::DATE)
            .map_err(D::Error::custom)?;
        if v.to_string() != s {
            return Err(D::Error::custom("noncanonical date"));
        }
        Ok(v)
    }
}
pub(super) mod decimal_option {
    use rust_decimal::Decimal;
    use serde::{Deserialize, Deserializer, Serializer};
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Exact(#[serde(with = "super::decimal")] Decimal);
    pub fn serialize<S: Serializer>(v: &Option<Decimal>, s: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&v.map(Exact), s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Decimal>, D::Error> {
        Ok(Option::<Exact>::deserialize(d)?.map(|v| v.0))
    }
}
pub(super) mod date_option {
    use serde::{Deserialize, Deserializer, Serializer};
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Exact(#[serde(with = "super::date")] time::Date);
    pub fn serialize<S: Serializer>(v: &Option<time::Date>, s: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&v.map(Exact), s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<time::Date>, D::Error> {
        Ok(Option::<Exact>::deserialize(d)?.map(|v| v.0))
    }
}

/// Serde must reject a missing nullable field instead of silently defaulting it.
pub(super) fn required_option<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}
pub(super) mod uuid {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};
    pub fn serialize<S: Serializer>(v: &::uuid::Uuid, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<::uuid::Uuid, D::Error> {
        let s = String::deserialize(d)?;
        let v = ::uuid::Uuid::parse_str(&s).map_err(D::Error::custom)?;
        if v.to_string() != s {
            return Err(D::Error::custom("noncanonical UUID"));
        }
        Ok(v)
    }
}
