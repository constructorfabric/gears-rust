//! Derived usage types: a composite meter declared as versioned data, with one evaluator
//! (P-D-229, P-D-230).
//!
//! A derived usage type computes one quantity from other usage. A cloudlet is 128 MB of RAM and
//! 400 MHz of CPU, so a cloudlet-hour is the larger of the hour's two shares, each rounded up.
//! Products declares the formula as data ([`DerivedUsageDeclaration`]); Rating evaluates it per
//! subscription line and granule (rating T-D-39). Both call the functions of this module, so the
//! declaration Products accepts is the declaration Rating evaluates.
//!
//! # The order of the work
//!
//! 1. Rating folds each input over one granule (an hour), as the input's [`GranuleFold`] says.
//! 2. [`evaluate`] applies the formula to that granule's folded quantities.
//! 3. A rating window's output is the sum of its granule outputs ([`evaluate_window`]).
//!
//! The order matters: the larger share in each hour is not the larger of the hourly sums.
//!
//! # No serde, no hashing
//!
//! As in [`crate::usage_types`]: the gear's REST DTOs own serde and map onto these types.
//! [`canonical_bytes`] is a pure encoding; the products runtime hashes it and stores the digest.

use rust_decimal::{Decimal, RoundingStrategy};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// The reserved prefix of a derived meter id. A GTS usage type id never starts with it.
pub const DERIVED_METER_PREFIX: &str = "products.derived/";
/// The fewest inputs a declaration names. One is enough (P-D-251); zero is refused.
pub const MIN_INPUTS: usize = 1;
/// The deepest formula: a leaf alone has depth 1.
pub const MAX_DEPTH: usize = 32;
/// The most formula nodes: every [`Expr`] counts one.
pub const MAX_NODES: usize = 256;
/// The largest rounding scale, of the output and of [`Expr::Round`].
pub const MAX_SCALE: u32 = 12;
/// The largest hold of a [`GranuleFold::TimeWeighted`] input, in seconds (rating T-D-17).
pub const MAX_HOLD_SECONDS: u32 = 86_400;
/// The longest unit, in characters: the SKU unit cap (P-D-225).
pub const UNIT_MAX_CHARS: usize = 64;
/// The longest input usage type ref, in characters: the SKU usage-type ref cap (P-D-225).
pub const INPUT_REF_MAX_CHARS: usize = 512;
/// The longest input name, in characters.
pub const INPUT_NAME_MAX_CHARS: usize = 32;
/// The longest derived usage type code, in characters.
pub const CODE_MAX_CHARS: usize = 64;
/// The domain tag at the head of [`canonical_bytes`]. A change to the encoding is a new tag.
pub const CANONICAL_DOMAIN: &str = "products.derived_usage_declaration.v1";

/// One version of a derived usage type: what it is computed from and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedUsageDeclaration {
    /// The output's unit, for example `cloudlet·hour`. The selling SKU's unit equals it.
    pub output_unit: String,
    /// The span the formula applies to.
    pub granularity: Granularity,
    /// At least one input, each a raw GTS usage type, with a distinct name.
    pub inputs: Vec<DerivedInput>,
    /// How the inputs' folded quantities combine.
    pub formula: Expr,
    /// The scale the granule result is rounded to, `0..=12`.
    pub output_scale: u32,
    /// How the granule result is rounded.
    pub output_round: RoundMode,
}

/// One input of a declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedInput {
    /// The name [`Expr::Input`] refers to: `^[a-z][a-z0-9_]{0,31}$`.
    pub name: String,
    /// A GTS usage type id. Its version segment is the exact version read.
    pub usage_type_ref: String,
    /// How this input folds over one granule. Rating applies it.
    pub granule_fold: GranuleFold,
    /// The hold bound of a [`GranuleFold::TimeWeighted`] input, `1..=86_400` seconds (rating
    /// T-D-17). Required for that fold and refused for the others.
    pub max_hold_seconds: Option<u32>,
    /// The input's unit, for validation and documentation.
    pub unit: String,
}

/// The span a formula applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    /// One UTC hour.
    Hour,
}

/// How an input folds over one granule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GranuleFold {
    /// The sum of the granule's quantities.
    Sum,
    /// The granule's largest level. A partial hour bills as a full one.
    Peak,
    /// The level integrated over time, as rating T-D-17 says, hold bound included.
    TimeWeighted,
}

/// A formula: a tree over the inputs' folded quantities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// The folded quantity of the input of this name.
    Input(String),
    /// A constant.
    Const(Decimal),
    /// The sum of two operands.
    Add(Box<Expr>, Box<Expr>),
    /// The difference of two operands.
    Sub(Box<Expr>, Box<Expr>),
    /// The product of two operands.
    Mul(Box<Expr>, Box<Expr>),
    /// An operand divided by a non-zero constant.
    DivConst(Box<Expr>, Decimal),
    /// The largest of at least two operands.
    Max(Vec<Expr>),
    /// The smallest of at least two operands.
    Min(Vec<Expr>),
    /// The smallest integer not below the operand.
    Ceil(Box<Expr>),
    /// The largest integer not above the operand.
    Floor(Box<Expr>),
    /// The operand rounded to a scale `0..=12` by a mode.
    Round(Box<Expr>, u32, RoundMode),
}

/// How a value is rounded to a scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundMode {
    /// A midpoint goes to the even neighbour (banker's rounding).
    HalfEven,
    /// A midpoint goes away from zero.
    HalfUp,
    /// Every value goes away from zero.
    Up,
    /// Every value goes toward zero.
    Down,
}

/// Where a refused unit sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitSite {
    /// The declaration's output unit.
    Output,
    /// The unit of the input of this name.
    Input(String),
}

/// Why [`validate`] refused a declaration: one variant per rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclarationError {
    /// A unit is empty or blank.
    EmptyUnit { site: UnitSite },
    /// A unit is longer than [`UNIT_MAX_CHARS`].
    UnitTooLong { site: UnitSite },
    /// A scale is above [`MAX_SCALE`]: the output's or a [`Expr::Round`]'s.
    ScaleTooLarge { scale: u32 },
    /// Fewer than [`MIN_INPUTS`] inputs.
    TooFewInputs { count: usize },
    /// An input name is off `^[a-z][a-z0-9_]{0,31}$`.
    InvalidInputName { name: String },
    /// Two inputs share a name.
    DuplicateInput { name: String },
    /// An input's usage type ref is empty or blank.
    EmptyInputRef { name: String },
    /// An input's usage type ref is longer than [`INPUT_REF_MAX_CHARS`].
    InputRefTooLong { name: String },
    /// An input is itself a derived usage type ([`DERIVED_METER_PREFIX`]).
    DerivedInput { name: String },
    /// A [`GranuleFold::TimeWeighted`] input has no `max_hold_seconds`.
    HoldMissing { name: String },
    /// A `Sum` or `Peak` input has a `max_hold_seconds`.
    HoldNotAllowed { name: String },
    /// A `max_hold_seconds` is outside `1..=86_400`.
    HoldOutOfRange { name: String, seconds: u32 },
    /// The formula refers to an input the declaration does not name.
    UnknownInput { name: String },
    /// The declaration names an input the formula does not use.
    UnusedInput { name: String },
    /// A [`Expr::DivConst`] divides by zero.
    DivisionByZero,
    /// A [`Expr::Max`] or [`Expr::Min`] has fewer than two operands.
    TooFewOperands { found: usize },
    /// The formula is deeper than [`MAX_DEPTH`].
    TooDeep,
    /// The formula has more than [`MAX_NODES`] nodes.
    TooManyNodes,
}

impl fmt::Display for UnitSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Output => f.write_str("the output unit"),
            Self::Input(name) => write!(f, "the unit of input `{name}`"),
        }
    }
}

impl fmt::Display for DeclarationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyUnit { site } => write!(f, "{site} is empty"),
            Self::UnitTooLong { site } => {
                write!(f, "{site} is longer than {UNIT_MAX_CHARS} characters")
            }
            Self::ScaleTooLarge { scale } => {
                write!(f, "scale {scale} is above {MAX_SCALE}")
            }
            Self::TooFewInputs { count } => {
                write!(
                    f,
                    "{count} inputs; a derived usage type has at least {MIN_INPUTS}"
                )
            }
            Self::InvalidInputName { name } => {
                write!(f, "input name `{name}` is not ^[a-z][a-z0-9_]{{0,31}}$")
            }
            Self::DuplicateInput { name } => write!(f, "input `{name}` is named twice"),
            Self::EmptyInputRef { name } => {
                write!(f, "input `{name}` names no usage type")
            }
            Self::InputRefTooLong { name } => write!(
                f,
                "the usage type of input `{name}` is longer than {INPUT_REF_MAX_CHARS} characters"
            ),
            Self::DerivedInput { name } => {
                write!(f, "input `{name}` is a derived usage type; inputs are raw")
            }
            Self::HoldMissing { name } => {
                write!(f, "time-weighted input `{name}` has no max_hold_seconds")
            }
            Self::HoldNotAllowed { name } => write!(
                f,
                "input `{name}` has a max_hold_seconds; only a time-weighted input has one"
            ),
            Self::HoldOutOfRange { name, seconds } => write!(
                f,
                "input `{name}` holds {seconds} seconds; the bound is 1..={MAX_HOLD_SECONDS}"
            ),
            Self::UnknownInput { name } => {
                write!(f, "the formula uses `{name}`, which is not an input")
            }
            Self::UnusedInput { name } => {
                write!(f, "input `{name}` is not used by the formula")
            }
            Self::DivisionByZero => f.write_str("the formula divides by zero"),
            Self::TooFewOperands { found } => {
                write!(f, "max or min has {found} operands; it needs at least 2")
            }
            Self::TooDeep => write!(f, "the formula is deeper than {MAX_DEPTH}"),
            Self::TooManyNodes => write!(f, "the formula has more than {MAX_NODES} nodes"),
        }
    }
}

impl std::error::Error for DeclarationError {}

/// Why [`evaluate`] gave no quantity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// The declaration is refused by [`validate`].
    Invalid(DeclarationError),
    /// The granule has no quantity for this input.
    MissingInput { name: String },
    /// The granule has a quantity for a name that is not an input.
    ExtraInput { name: String },
    /// An input's quantity is below zero.
    NegativeInput { name: String },
    /// The rounded, normalized result is below zero.
    NegativeResult,
    /// An operation left the range of a decimal.
    Overflow,
}

impl From<DeclarationError> for EvalError {
    fn from(error: DeclarationError) -> Self {
        Self::Invalid(error)
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => write!(f, "invalid declaration: {error}"),
            Self::MissingInput { name } => write!(f, "no quantity for input `{name}`"),
            Self::ExtraInput { name } => write!(f, "`{name}` is not an input"),
            Self::NegativeInput { name } => write!(f, "input `{name}` is negative"),
            Self::NegativeResult => f.write_str("the result is negative"),
            Self::Overflow => f.write_str("the computation overflowed"),
        }
    }
}

impl std::error::Error for EvalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(error) => Some(error),
            _ => None,
        }
    }
}

/// Checks every rule of a declaration (P-D-230). The first refusal found is answered.
///
/// # Errors
///
/// [`DeclarationError`], one variant per rule.
pub fn validate(decl: &DerivedUsageDeclaration) -> Result<(), DeclarationError> {
    check_unit(&decl.output_unit, || UnitSite::Output)?;
    check_scale(decl.output_scale)?;
    if decl.inputs.len() < MIN_INPUTS {
        return Err(DeclarationError::TooFewInputs {
            count: decl.inputs.len(),
        });
    }
    let mut names = BTreeSet::new();
    for input in &decl.inputs {
        check_input(input)?;
        if !names.insert(input.name.as_str()) {
            return Err(DeclarationError::DuplicateInput {
                name: input.name.clone(),
            });
        }
    }
    let used = check_formula(&decl.formula, &names)?;
    match decl
        .inputs
        .iter()
        .find(|input| !used.contains(input.name.as_str()))
    {
        Some(unused) => Err(DeclarationError::UnusedInput {
            name: unused.name.clone(),
        }),
        None => Ok(()),
    }
}

fn check_unit(unit: &str, site: impl FnOnce() -> UnitSite) -> Result<(), DeclarationError> {
    if unit.trim().is_empty() {
        return Err(DeclarationError::EmptyUnit { site: site() });
    }
    if unit.chars().count() > UNIT_MAX_CHARS {
        return Err(DeclarationError::UnitTooLong { site: site() });
    }
    Ok(())
}

const fn check_scale(scale: u32) -> Result<(), DeclarationError> {
    if scale > MAX_SCALE {
        Err(DeclarationError::ScaleTooLarge { scale })
    } else {
        Ok(())
    }
}

/// `^[a-z][a-z0-9_]{0,31}$`.
fn is_input_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && name.len() <= INPUT_NAME_MAX_CHARS
}

fn check_input(input: &DerivedInput) -> Result<(), DeclarationError> {
    let name = || input.name.clone();
    if !is_input_name(&input.name) {
        return Err(DeclarationError::InvalidInputName { name: name() });
    }
    if input.usage_type_ref.trim().is_empty() {
        return Err(DeclarationError::EmptyInputRef { name: name() });
    }
    if input.usage_type_ref.chars().count() > INPUT_REF_MAX_CHARS {
        return Err(DeclarationError::InputRefTooLong { name: name() });
    }
    if input.usage_type_ref.starts_with(DERIVED_METER_PREFIX) {
        return Err(DeclarationError::DerivedInput { name: name() });
    }
    check_unit(&input.unit, || UnitSite::Input(name()))?;
    match (input.granule_fold, input.max_hold_seconds) {
        (GranuleFold::TimeWeighted, None) => Err(DeclarationError::HoldMissing { name: name() }),
        (GranuleFold::TimeWeighted, Some(seconds))
            if !(1..=MAX_HOLD_SECONDS).contains(&seconds) =>
        {
            Err(DeclarationError::HoldOutOfRange {
                name: name(),
                seconds,
            })
        }
        (GranuleFold::Sum | GranuleFold::Peak, Some(_)) => {
            Err(DeclarationError::HoldNotAllowed { name: name() })
        }
        (GranuleFold::TimeWeighted, Some(_)) | (GranuleFold::Sum | GranuleFold::Peak, None) => {
            Ok(())
        }
    }
}

/// The formula walk, without recursion: a formula is judged before its depth is known to be
/// bounded, so the walk keeps its own stack and stops at the first bound it crosses.
struct Walk<'a> {
    stack: Vec<(&'a Expr, usize)>,
    nodes: usize,
}

impl<'a> Walk<'a> {
    fn push(&mut self, expr: &'a Expr, depth: usize) -> Result<(), DeclarationError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(DeclarationError::TooManyNodes);
        }
        if depth > MAX_DEPTH {
            return Err(DeclarationError::TooDeep);
        }
        self.stack.push((expr, depth));
        Ok(())
    }
}

/// Checks every node of the formula and answers the input names it uses.
fn check_formula<'a>(
    formula: &'a Expr,
    names: &BTreeSet<&str>,
) -> Result<BTreeSet<&'a str>, DeclarationError> {
    let mut used = BTreeSet::new();
    let mut walk = Walk {
        stack: Vec::new(),
        nodes: 0,
    };
    walk.push(formula, 1)?;
    while let Some((expr, depth)) = walk.stack.pop() {
        let below = depth + 1;
        match expr {
            Expr::Input(name) => {
                if !names.contains(name.as_str()) {
                    return Err(DeclarationError::UnknownInput { name: name.clone() });
                }
                used.insert(name.as_str());
            }
            Expr::Const(_) => {}
            Expr::Add(left, right) | Expr::Sub(left, right) | Expr::Mul(left, right) => {
                walk.push(left, below)?;
                walk.push(right, below)?;
            }
            Expr::DivConst(arg, divisor) => {
                if divisor.is_zero() {
                    return Err(DeclarationError::DivisionByZero);
                }
                walk.push(arg, below)?;
            }
            Expr::Max(args) | Expr::Min(args) => {
                if args.len() < 2 {
                    return Err(DeclarationError::TooFewOperands { found: args.len() });
                }
                for arg in args {
                    walk.push(arg, below)?;
                }
            }
            Expr::Ceil(arg) | Expr::Floor(arg) => walk.push(arg, below)?,
            Expr::Round(arg, scale, _) => {
                check_scale(*scale)?;
                walk.push(arg, below)?;
            }
        }
    }
    Ok(used)
}

/// One granule's output: the formula applied to the granule's folded input quantities, keyed by
/// input name, rounded to the output scale and normalized.
///
/// # Errors
///
/// [`EvalError`]; [`EvalError::Invalid`] when [`validate`] refuses the declaration.
pub fn evaluate(
    decl: &DerivedUsageDeclaration,
    granule: &BTreeMap<String, Decimal>,
) -> Result<Decimal, EvalError> {
    validate(decl)?;
    evaluate_valid(decl, granule)
}

/// A window's output: the sum of its granules' outputs, each from [`evaluate`].
///
/// # Errors
///
/// [`EvalError`] of the first granule that has one, or [`EvalError::Overflow`] for the sum.
pub fn evaluate_window(
    decl: &DerivedUsageDeclaration,
    granules: &[BTreeMap<String, Decimal>],
) -> Result<Decimal, EvalError> {
    validate(decl)?;
    let sum = granules.iter().try_fold(Decimal::ZERO, |sum, granule| {
        let output = evaluate_valid(decl, granule)?;
        sum.checked_add(output).ok_or(EvalError::Overflow)
    })?;
    Ok(sum.normalize())
}

/// [`evaluate`] of a declaration [`validate`] accepted: the formula is at most [`MAX_DEPTH`]
/// deep, so the recursion below is bounded.
fn evaluate_valid(
    decl: &DerivedUsageDeclaration,
    granule: &BTreeMap<String, Decimal>,
) -> Result<Decimal, EvalError> {
    check_granule(decl, granule)?;
    let output = eval(&decl.formula, granule)?
        .round_dp_with_strategy(decl.output_scale, strategy(decl.output_round))
        .normalize();
    if output < Decimal::ZERO {
        return Err(EvalError::NegativeResult);
    }
    Ok(output)
}

fn check_granule(
    decl: &DerivedUsageDeclaration,
    granule: &BTreeMap<String, Decimal>,
) -> Result<(), EvalError> {
    for input in &decl.inputs {
        match granule.get(&input.name) {
            None => {
                return Err(EvalError::MissingInput {
                    name: input.name.clone(),
                });
            }
            Some(quantity) if *quantity < Decimal::ZERO => {
                return Err(EvalError::NegativeInput {
                    name: input.name.clone(),
                });
            }
            Some(_) => {}
        }
    }
    match granule
        .keys()
        .find(|key| !decl.inputs.iter().any(|input| &input.name == *key))
    {
        Some(extra) => Err(EvalError::ExtraInput {
            name: extra.clone(),
        }),
        None => Ok(()),
    }
}

fn eval(expr: &Expr, granule: &BTreeMap<String, Decimal>) -> Result<Decimal, EvalError> {
    match expr {
        Expr::Input(name) => granule
            .get(name)
            .copied()
            .ok_or_else(|| EvalError::MissingInput { name: name.clone() }),
        Expr::Const(value) => Ok(*value),
        Expr::Add(left, right) => checked(eval(left, granule)?.checked_add(eval(right, granule)?)),
        Expr::Sub(left, right) => checked(eval(left, granule)?.checked_sub(eval(right, granule)?)),
        Expr::Mul(left, right) => checked(eval(left, granule)?.checked_mul(eval(right, granule)?)),
        Expr::DivConst(arg, divisor) => checked(eval(arg, granule)?.checked_div(*divisor)),
        Expr::Max(args) => extremum(args, granule, Ord::max),
        Expr::Min(args) => extremum(args, granule, Ord::min),
        Expr::Ceil(arg) => {
            let value = eval(arg, granule)?;
            let whole = value.trunc();
            if value > whole {
                checked(whole.checked_add(Decimal::ONE))
            } else {
                Ok(whole)
            }
        }
        Expr::Floor(arg) => {
            let value = eval(arg, granule)?;
            let whole = value.trunc();
            if value < whole {
                checked(whole.checked_sub(Decimal::ONE))
            } else {
                Ok(whole)
            }
        }
        Expr::Round(arg, scale, mode) => {
            Ok(eval(arg, granule)?.round_dp_with_strategy(*scale, strategy(*mode)))
        }
    }
}

fn checked(value: Option<Decimal>) -> Result<Decimal, EvalError> {
    value.ok_or(EvalError::Overflow)
}

fn extremum(
    args: &[Expr],
    granule: &BTreeMap<String, Decimal>,
    pick: fn(Decimal, Decimal) -> Decimal,
) -> Result<Decimal, EvalError> {
    let mut best: Option<Decimal> = None;
    for arg in args {
        let value = eval(arg, granule)?;
        best = Some(best.map_or(value, |b| pick(b, value)));
    }
    best.ok_or(EvalError::Invalid(DeclarationError::TooFewOperands {
        found: args.len(),
    }))
}

const fn strategy(mode: RoundMode) -> RoundingStrategy {
    match mode {
        RoundMode::HalfEven => RoundingStrategy::MidpointNearestEven,
        RoundMode::HalfUp => RoundingStrategy::MidpointAwayFromZero,
        RoundMode::Up => RoundingStrategy::AwayFromZero,
        RoundMode::Down => RoundingStrategy::ToZero,
    }
}

/// The declaration's deterministic encoding, which the products runtime hashes.
///
/// Canonical JSON (RFC 8785 strings, keys in byte order) of
/// `{"domain": CANONICAL_DOMAIN, "payload": <the declaration>}`:
/// - every decimal is its normalized text, so `128` and `128.0` give the same bytes;
/// - the inputs are in name order, because a name, not a position, is what the formula reads;
/// - every field is written, an absent hold as `null`.
///
/// The encoding recurses as deep as the formula. [`validate`] bounds that at [`MAX_DEPTH`], so
/// encode a declaration it accepted.
#[must_use]
pub fn canonical_bytes(decl: &DerivedUsageDeclaration) -> Vec<u8> {
    let mut inputs: Vec<&DerivedInput> = decl.inputs.iter().collect();
    inputs.sort_by(|a, b| a.name.cmp(&b.name));
    let payload = Canon::Obj(vec![
        ("output_unit", Canon::text(&decl.output_unit)),
        (
            "granularity",
            Canon::text(match decl.granularity {
                Granularity::Hour => "hour",
            }),
        ),
        (
            "inputs",
            Canon::Arr(inputs.into_iter().map(canon_input).collect()),
        ),
        ("formula", canon_expr(&decl.formula)),
        ("output_scale", Canon::Int(decl.output_scale)),
        ("output_round", Canon::text(round_token(decl.output_round))),
    ]);
    let document = Canon::Obj(vec![
        ("domain", Canon::text(CANONICAL_DOMAIN)),
        ("payload", payload),
    ]);
    let mut out = String::new();
    document.encode(&mut out);
    out.into_bytes()
}

/// The restricted JSON [`canonical_bytes`] writes: no floats, so a number's meaning is its text.
enum Canon {
    Null,
    Int(u32),
    Str(String),
    Arr(Vec<Canon>),
    Obj(Vec<(&'static str, Canon)>),
}

impl Canon {
    fn text(value: &str) -> Self {
        Self::Str(value.to_owned())
    }

    fn decimal(value: Decimal) -> Self {
        Self::Str(value.normalize().to_string())
    }

    fn encode(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Int(value) => out.push_str(&value.to_string()),
            Self::Str(value) => encode_string(value, out),
            Self::Arr(values) => {
                out.push('[');
                for (i, value) in values.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    value.encode(out);
                }
                out.push(']');
            }
            Self::Obj(fields) => {
                let mut fields: Vec<&(&str, Self)> = fields.iter().collect();
                fields.sort_by_key(|(key, _)| *key);
                out.push('{');
                for (i, (key, value)) in fields.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    encode_string(key, out);
                    out.push(':');
                    value.encode(out);
                }
                out.push('}');
            }
        }
    }
}

/// RFC 8785 string escaping, as pricing's canonical JSON writes it.
fn encode_string(value: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\0'..='\u{1f}' => {
                let n = ch as usize;
                out.push_str("\\u00");
                out.push(char::from(HEX[n >> 4]));
                out.push(char::from(HEX[n % 16]));
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
}

fn canon_input(input: &DerivedInput) -> Canon {
    Canon::Obj(vec![
        ("name", Canon::text(&input.name)),
        ("usage_type_ref", Canon::text(&input.usage_type_ref)),
        (
            "granule_fold",
            Canon::text(match input.granule_fold {
                GranuleFold::Sum => "sum",
                GranuleFold::Peak => "peak",
                GranuleFold::TimeWeighted => "time_weighted",
            }),
        ),
        (
            "max_hold_seconds",
            input.max_hold_seconds.map_or(Canon::Null, Canon::Int),
        ),
        ("unit", Canon::text(&input.unit)),
    ])
}

fn canon_expr(expr: &Expr) -> Canon {
    let op = |name: &str| ("op", Canon::text(name));
    let pair = |name: &str, left: &Expr, right: &Expr| {
        Canon::Obj(vec![
            op(name),
            ("left", canon_expr(left)),
            ("right", canon_expr(right)),
        ])
    };
    let unary = |name: &str, arg: &Expr| Canon::Obj(vec![op(name), ("arg", canon_expr(arg))]);
    let many = |name: &str, args: &[Expr]| {
        Canon::Obj(vec![
            op(name),
            ("args", Canon::Arr(args.iter().map(canon_expr).collect())),
        ])
    };
    match expr {
        Expr::Input(name) => Canon::Obj(vec![op("input"), ("name", Canon::text(name))]),
        Expr::Const(value) => Canon::Obj(vec![op("const"), ("value", Canon::decimal(*value))]),
        Expr::Add(left, right) => pair("add", left, right),
        Expr::Sub(left, right) => pair("sub", left, right),
        Expr::Mul(left, right) => pair("mul", left, right),
        Expr::DivConst(arg, divisor) => Canon::Obj(vec![
            op("div_const"),
            ("arg", canon_expr(arg)),
            ("divisor", Canon::decimal(*divisor)),
        ]),
        Expr::Max(args) => many("max", args),
        Expr::Min(args) => many("min", args),
        Expr::Ceil(arg) => unary("ceil", arg),
        Expr::Floor(arg) => unary("floor", arg),
        Expr::Round(arg, scale, mode) => Canon::Obj(vec![
            op("round"),
            ("arg", canon_expr(arg)),
            ("scale", Canon::Int(*scale)),
            ("mode", Canon::text(round_token(*mode))),
        ]),
    }
}

const fn round_token(mode: RoundMode) -> &'static str {
    match mode {
        RoundMode::HalfEven => "half_even",
        RoundMode::HalfUp => "half_up",
        RoundMode::Up => "up",
        RoundMode::Down => "down",
    }
}

/// Why [`MeterId::parse`] or [`MeterId::new`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterIdError {
    /// The id does not start with [`DERIVED_METER_PREFIX`].
    MissingPrefix,
    /// The id has no `@<n>`.
    MissingVersion,
    /// The code is off `^[a-z0-9][a-z0-9._-]{0,63}$`.
    InvalidCode,
    /// The version after `@` is empty.
    EmptyVersion,
    /// The version is not a canonical decimal: a non-digit or a leading zero.
    NonCanonicalVersion,
    /// The version is 0; versions start at 1.
    ZeroVersion,
    /// The version does not fit a `u32`.
    VersionOutOfRange,
}

impl fmt::Display for MeterIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingPrefix => "a derived meter id starts with products.derived/",
            Self::MissingVersion => "a derived meter id ends with @<version>",
            Self::InvalidCode => "the code is not ^[a-z0-9][a-z0-9._-]{0,63}$",
            Self::EmptyVersion => "the version is empty",
            Self::NonCanonicalVersion => "the version is not a canonical decimal",
            Self::ZeroVersion => "versions start at 1",
            Self::VersionOutOfRange => "the version is out of range",
        })
    }
}

impl std::error::Error for MeterIdError {}

/// A derived meter id, `products.derived/<code>@<n>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MeterId {
    code: String,
    version: u32,
}

impl std::fmt::Display for MeterId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{DERIVED_METER_PREFIX}{}@{}", self.code, self.version)
    }
}

impl std::str::FromStr for MeterId {
    type Err = MeterIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl MeterId {
    /// A meter id from its parts.
    ///
    /// # Errors
    ///
    /// [`MeterIdError::InvalidCode`] or [`MeterIdError::ZeroVersion`].
    pub fn new(code: impl Into<String>, version: u32) -> Result<Self, MeterIdError> {
        let code = code.into();
        if !is_code(&code) {
            return Err(MeterIdError::InvalidCode);
        }
        if version == 0 {
            return Err(MeterIdError::ZeroVersion);
        }
        Ok(Self { code, version })
    }

    /// Parses `products.derived/<code>@<n>`.
    ///
    /// # Errors
    ///
    /// [`MeterIdError`], one variant per rule.
    pub fn parse(id: &str) -> Result<Self, MeterIdError> {
        let rest = id
            .strip_prefix(DERIVED_METER_PREFIX)
            .ok_or(MeterIdError::MissingPrefix)?;
        let (code, version) = rest.split_once('@').ok_or(MeterIdError::MissingVersion)?;
        if !is_code(code) {
            return Err(MeterIdError::InvalidCode);
        }
        Ok(Self {
            code: code.to_owned(),
            version: Self::parse_version(version)?,
        })
    }

    /// Parses a canonical version: digits, no leading zero, at least 1.
    ///
    /// # Errors
    ///
    /// [`MeterIdError::EmptyVersion`], [`MeterIdError::NonCanonicalVersion`],
    /// [`MeterIdError::ZeroVersion`] or [`MeterIdError::VersionOutOfRange`].
    pub fn parse_version(version: &str) -> Result<u32, MeterIdError> {
        if version.is_empty() {
            return Err(MeterIdError::EmptyVersion);
        }
        if !version.bytes().all(|b| b.is_ascii_digit()) {
            return Err(MeterIdError::NonCanonicalVersion);
        }
        if version == "0" {
            return Err(MeterIdError::ZeroVersion);
        }
        if version.starts_with('0') {
            return Err(MeterIdError::NonCanonicalVersion);
        }
        version.parse().map_err(|_| MeterIdError::VersionOutOfRange)
    }

    /// The code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// `products.derived/<code>@<n>`.
    #[must_use]
    pub fn format(&self) -> String {
        self.to_string()
    }

    /// The pricing meter reference: `(usage_type_id, version)`, which is
    /// `("products.derived/<code>@<n>", "<n>")`.
    #[must_use]
    pub fn meter_ref(&self) -> (String, String) {
        (self.format(), self.version.to_string())
    }
}

/// `^[a-z0-9][a-z0-9._-]{0,63}$`.
fn is_code(code: &str) -> bool {
    let mut chars = code.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        && code.len() <= CODE_MAX_CHARS
}

#[cfg(test)]
#[path = "derived_tests.rs"]
mod tests;
