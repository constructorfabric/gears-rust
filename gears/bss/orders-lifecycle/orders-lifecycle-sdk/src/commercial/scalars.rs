//! Strict, lossless schema-2 scalar encodings. No binary floating point.
use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

pub fn required_option<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}
macro_rules! integer {
    ($name:ident, $type:ty) => {
        pub mod $name {
            use super::*;
            pub fn serialize<S: Serializer>(v: &$type, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&v.to_string())
            }
            pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<$type, D::Error> {
                let text = String::deserialize(d)?;
                let value: $type = text.parse().map_err(D::Error::custom)?;
                if value.to_string() != text {
                    return Err(D::Error::custom("noncanonical integer"));
                }
                Ok(value)
            }
        }
    };
}
integer!(unsigned, u64);
integer!(signed, i64);

pub mod decimal {
    use super::*;
    use rust_decimal::Decimal;
    pub fn serialize<S: Serializer>(v: &Decimal, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Decimal, D::Error> {
        let text = String::deserialize(d)?;
        let value = Decimal::from_str_exact(&text).map_err(D::Error::custom)?;
        if value.to_string() != text {
            return Err(D::Error::custom("noncanonical or lossy decimal"));
        }
        Ok(value)
    }
}
pub mod digest {
    use super::*;
    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        use std::fmt::Write as _;
        let mut text = String::with_capacity(64);
        for byte in v {
            write!(text, "{byte:02x}").map_err(serde::ser::Error::custom)?;
        }
        s.serialize_str(&text)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        if text.len() != 64
            || !text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(D::Error::custom("expected 64 lowercase hexadecimal digits"));
        }
        let mut result = [0; 32];
        for (slot, pair) in result.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
            let nibble = |b: u8| {
                if b.is_ascii_digit() {
                    b - b'0'
                } else {
                    b - b'a' + 10
                }
            };
            *slot = nibble(pair[0]) * 16 + nibble(pair[1]);
        }
        Ok(result)
    }
}
pub mod instant {
    use super::*;
    use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};
    fn text(v: OffsetDateTime) -> String {
        let v = v.to_offset(UtcOffset::UTC);
        format!(
            "{}T{:02}:{:02}:{:02}.{:09}Z",
            v.date(),
            v.hour(),
            v.minute(),
            v.second(),
            v.nanosecond()
        )
    }
    pub fn serialize<S: Serializer>(v: &OffsetDateTime, s: S) -> Result<S::Ok, S::Error> {
        let encoded = text(*v);
        OffsetDateTime::parse(&encoded, &Rfc3339).map_err(serde::ser::Error::custom)?;
        s.serialize_str(&encoded)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<OffsetDateTime, D::Error> {
        let input = String::deserialize(d)?;
        let v = OffsetDateTime::parse(&input, &Rfc3339).map_err(D::Error::custom)?;
        if text(v) != input {
            return Err(D::Error::custom(
                "expected UTC instant with nine fractional digits",
            ));
        }
        Ok(v)
    }
}
pub mod date {
    use super::*;
    use time::{Date, OffsetDateTime, format_description::well_known::Rfc3339};
    #[allow(clippy::trivially_copy_pass_by_ref)] // Serde with-hook signature.
    pub fn serialize<S: Serializer>(v: &Date, s: S) -> Result<S::Ok, S::Error> {
        if !(0..=9999).contains(&v.year()) {
            return Err(serde::ser::Error::custom("date outside schema-2 range"));
        }
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Date, D::Error> {
        let input = String::deserialize(d)?;
        let v = OffsetDateTime::parse(&format!("{input}T00:00:00Z"), &Rfc3339)
            .map_err(D::Error::custom)?
            .date();
        if v.to_string() != input {
            return Err(D::Error::custom("noncanonical date"));
        }
        Ok(v)
    }
}
pub mod price_state {
    use super::*;
    use bss_pricing_sdk::read::PriceState;
    #[allow(clippy::trivially_copy_pass_by_ref)] // Serde with-hook signature.
    pub fn serialize<S: Serializer>(v: &PriceState, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match v {
            PriceState::Approved => "approved",
            PriceState::Cancelled => "cancelled",
            _ => return Err(serde::ser::Error::custom("unsupported price state")),
        })
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<PriceState, D::Error> {
        match String::deserialize(d)?.as_str() {
            "approved" => Ok(PriceState::Approved),
            "cancelled" => Ok(PriceState::Cancelled),
            _ => Err(D::Error::custom("unsupported price state")),
        }
    }
}
