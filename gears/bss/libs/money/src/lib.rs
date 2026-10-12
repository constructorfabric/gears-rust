//! One money representation for the BSS gears.
//!
//! Money is a [`rust_decimal::Decimal`] in **major** currency units, carried next to
//! its currency code and posting scale ([`CurrencySpec`]). A [`PostedMoney`] is a
//! validated amount: at most 28 significant digits, scale 0–28, and a multiple of the
//! currency's posting increment (`12.34 EUR` at scale 2 is accepted, `0.047` is not).
//! Nothing here rounds an input.
//!
//! - [`parse_decimal`] / [`canonical_decimal`]: the one text form for the wire, storage
//!   and hashes (no exponent, no `+`, no leading zeros, no `-0`, no fractional trailing
//!   zeros on output; a parser accepts trailing zeros on input).
//! - `exact` (feature): [`exact::ExactAmount`], reduced fractions over big integers for
//!   intermediate arithmetic, narrowed to a [`PostedMoney`] only at an explicit point,
//!   either exactly or by one `HALF_EVEN` rounding; [`allocate::allocate`] for
//!   proportional shares with a deterministic residual.
//! - `serde` (feature): [`serde_text`], a `#[serde(with)]` adapter for canonical text.
//!
//! Currency scales come from each gear's registry or provisioning; this crate holds no
//! currency table.

mod money;

#[cfg(feature = "exact")]
pub mod allocate;
#[cfg(feature = "exact")]
pub mod exact;
#[cfg(feature = "serde")]
pub mod serde_text;

pub use money::{
    CurrencySpec, MoneyError, PostedMoney, canonical_decimal, parse_decimal, validate_amount,
};
