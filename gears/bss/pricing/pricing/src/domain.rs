//! Pure pricing rules; no persistence, aggregation, or consumer state.

/// A stable validation code; doors attach their field and message context.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{code}")]
pub struct RuleError {
    pub code: &'static str,
    /// Typed seam reason when this is a commercial refusal.
    pub reason: Option<bss_pricing_sdk::acceptance::CommercialReason>,
}
impl RuleError {
    /// Construct a domain refusal.
    #[must_use]
    pub const fn new(code: &'static str) -> Self {
        Self { code, reason: None }
    }
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[toolkit_macros::domain_model]
        #[derive(Debug,Clone,Copy,PartialEq,Eq,serde::Serialize,serde::Deserialize)]
        #[serde(rename_all="snake_case")]
        pub enum $name { $(#[serde(rename=$value)] $variant),+ }
        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            #[must_use]
            pub const fn as_str(self) -> &'static str {match self {$(Self::$variant => $value),+}}
        }
        impl std::str::FromStr for $name {
            type Err = crate::domain::RuleError;
            fn from_str(s:&str) -> Result<Self,Self::Err> {match s {$($value=>Ok(Self::$variant),)+ _=>Err(crate::domain::RuleError::new("INVALID_ENUM"))}}
        }
    };
}

pub mod book;
pub mod caps;
pub mod dimension;
pub mod money;
pub mod plan;
pub mod price;
pub mod price_book_entry;
pub mod reference_op;
pub mod resolve;
#[cfg(test)]
mod test_support;
pub mod usage_policy;

pub mod commercial_terms;
impl From<bss_pricing_sdk::acceptance::CommercialReason> for RuleError {
    fn from(reason: bss_pricing_sdk::acceptance::CommercialReason) -> Self {
        Self {
            code: reason.code(),
            reason: Some(reason),
        }
    }
}
