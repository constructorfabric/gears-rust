#![forbid(unsafe_code)]

//! Serde support for the `humantime` crate.
//!
//! Based on [this fork](https://github.com/jean-airoldie/humantime-serde).
//!
//! Currently `std::time::Duration` is supported.
//!
//! # Example
//! ```
//! use serde::{Serialize, Deserialize};
//! use std::time::Duration;
//!
//! #[derive(Serialize, Deserialize)]
//! struct Foo {
//!     #[serde(with = "modkit_utils::humantime_serde")]
//!     timeout: Duration,
//! }
//! ```

use std::fmt;
use std::time::Duration;

use humantime;
use serde::{Deserializer, Serializer, de};

/// Deserializes a `Duration` via the humantime crate.
///
/// This function can be used with `serde_derive`'s `with` and
/// `deserialize_with` annotations.
///
/// # Errors
///
/// Returns an error if the string is not a valid duration.
pub fn deserialize<'a, D>(d: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'a>,
{
    struct V;

    impl de::Visitor<'_> for V {
        type Value = Duration;

        fn expecting(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
            fmt.write_str("a duration")
        }

        fn visit_str<E>(self, v: &str) -> Result<Duration, E>
        where
            E: de::Error,
        {
            humantime::parse_duration(v)
                .map_err(|_| E::invalid_value(de::Unexpected::Str(v), &self))
        }
    }

    d.deserialize_str(V)
}

/// Serializes a `Duration` via the humantime crate.
///
/// This function can be used with `serde_derive`'s `with` and
/// `serialize_with` annotations.
///
/// # Errors
/// None
pub fn serialize<S>(d: &Duration, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.collect_str(&humantime::format_duration(*d))
}

pub mod option {
    //! Convenience module to allow serialization via `humantime_serde` for `Option`
    //!
    //! # Example
    //!
    //! ```
    //! use serde::{Serialize, Deserialize};
    //! use std::time::Duration;
    //!
    //! #[derive(Serialize, Deserialize)]
    //! struct Foo {
    //!     #[serde(with = "modkit_utils::humantime_serde::option")]
    //!     timeout: Option<Duration>,
    //! }
    //! ```

    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    /// Serializes an `Option<Duration>`
    ///
    /// This function can be used with `serde_derive`'s `with` and
    /// `serialize_with` annotations.
    ///
    /// # Errors
    /// None
    pub fn serialize<S>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match d {
            Some(d) => super::serialize(d, s),
            None => s.serialize_none(),
        }
    }

    /// Deserialize an `Option<Duration>`
    ///
    /// This function can be used with `serde_derive`'s `with` and
    /// `deserialize_with` annotations.
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not a valid duration.
    pub fn deserialize<'a, D>(d: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'a>,
    {
        struct Wrapper(Duration);

        impl<'de> Deserialize<'de> for Wrapper {
            fn deserialize<D>(d: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                super::deserialize(d).map(Wrapper)
            }
        }

        let v: Option<Wrapper> = Option::deserialize(d)?;
        Ok(v.map(|Wrapper(d)| d))
    }
}

#[cfg(test)]
#[path = "humantime_serde_tests.rs"]
mod test;
