//! Submit's `resolved_total` rows from the frozen Rating evaluation (S3-12; DESIGN §3.7
//! `orders_resolved_total`, D-197, D-216). Pure: Orders checks identity, completeness and
//! representability and copies the producer's figures; it never sums rows, converts cycles,
//! derives a horizon or rounds (DESIGN "Rating owns ... all aggregation").
//!
//! Per line (`scope = line`) and for the order roll-up (`scope = order`, line `00000000-…`), one
//! row per charge kind the producer states (DESIGN §3.7, PK `(…, scope, line_id, charge_kind)`):
//! each producer-declared committed component under its kind (`recurring` or `one_time`,
//! `amount_status = committed`), and, when the line carries usage charges, a `usage` row with
//! `amount_status = uncommitted_usage`, NULL gross/net and an unavailable discount
//! (`ck_bss_orders__resolved_total__5`, `__8`; usage has no committed amount, never a zero) whose
//! declarations (`exclusions` included) are the producer's usage declaration copied verbatim.
//! The roll-up has a usage row exactly when some line has one. The TCV is carried only on the
//! order/recurring row (`ck_bss_orders__resolved_total__6`); usage is excluded from it. When no
//! recurring order component exists, the producer must supply `order_tcv_carrier` with its own
//! totals and declaration. Explicit empty component lists state usage-only input; absent lists
//! retain the legacy single-component contract. Aggregate totals remain Preview/tax evidence.
//!
//! Money (D-216, `ck_bss_orders__resolved_total__9`): every `*_exact` column is canonical decimal
//! text (`bss_orders__decimal_valid(…, 28)`) and every `*_display` column canonical decimal text
//! at most at the currency scale. Rating's exact value is a fraction (`ExactMoney`, #5390); it is
//! rendered with the shared money library (`bss_money::exact::ExactAmount::parse_fraction`, then
//! `canonical_at_scale(28)`), so `21/2` is stored as `10.5`. Only a fraction without a terminating
//! decimal expansion within 28 places (`1/3`) cannot be stored: the evaluation is then unusable
//! for submit and the gate refuses `evaluation-unavailable`. The display estimate is rendered at
//! the currency scale through the same function (`10.00` is stored as `10`); a display finer
//! than the currency scale is unusable, never rounded. Both outputs are canonical by
//! construction (D-213). This is formatting, not arithmetic: the stored text denotes exactly the
//! producer's value, and Orders still sums, converts and rounds nothing.
//!
//! The evaluation must be in the frozen market currency (D-216 one currency): a different
//! currency is a producer contract defect ([`TotalsError::MarketCurrency`]), never a refusal.
//!
//! The non-money columns (`amount_basis`, `period_evidence`, `item_breakdown`,
//! `recurring_by_cycle`, `exclusions`, the TCV basis and evidence) are producer declarations
//! ([`TotalsDeclaration`], [`Tcv::basis`]); an evaluation that does not declare them is
//! unusable, never defaulted (DESIGN: "missing or inconsistent basis makes the answer
//! unusable"). A line flagged with usage charges needs the producer's usage declaration
//! ([`LineEvaluation::usage_declaration`], and [`OrderEvaluation::order_usage_declaration`] for
//! the roll-up); a usage declaration without usage charges is inconsistent. Either is unusable.
use super::answers::{
    Amount, Availability, CommittedComponent, EvaluationChargeKind, ExactMoney, LineEvaluation,
    OrderEvaluation, TcvBasis, Totals, TotalsDeclaration,
};
use super::commercial::persisted;
use bss_money::exact::ExactAmount;
use serde_json::Value;
use std::collections::BTreeSet;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// The largest scale (and digit count) `bss_orders__decimal_valid` admits.
pub const MAX_DECIMAL_SCALE: u8 = 28;
/// The longest decimal text `bss_orders__decimal_valid` admits.
const MAX_DECIMAL_TEXT: usize = 31;

/// Why an evaluation cannot be stored as submit's totals: the gate refuses
/// `evaluation-unavailable` for every variant.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TotalsError {
    /// The evaluation does not cover exactly the submitted lines (missing, extra or duplicate).
    #[error("the evaluation does not cover exactly the submitted lines")]
    Coverage,
    /// Repeated kind, conflicting legacy declaration, or an invalid carrier.
    #[error("the evaluation has conflicting component declarations")]
    Components,
    /// A component's storage facts are not declared by the producer.
    #[error("{0} is not declared by the evaluation")]
    Undeclared(&'static str),
    /// An exact amount has no canonical decimal text within the schema bounds: a fraction that
    /// does not terminate within 28 places (`1/3`), or more than 28 significant digits.
    #[error("{0} has no canonical decimal text")]
    Fraction(&'static str),
    /// A display estimate is not canonical decimal text at the currency scale.
    #[error("{0} display is not canonical decimal text at the currency scale")]
    Display(&'static str),
    /// An amount's currency or scale is not the evaluation's.
    #[error("{0} is not in the evaluation currency")]
    Currency(&'static str),
    /// The evaluation is not in the frozen market currency (D-216): a contract defect.
    #[error("the evaluation currency is not the market currency")]
    MarketCurrency,
    /// A line's usage charges and its usage declaration disagree: usage charges without a usage
    /// declaration, or a usage declaration without usage charges (`None`: the roll-up's).
    #[error("usage charges and the usage declaration disagree (line {0:?})")]
    Usage(Option<Uuid>),
}

impl TotalsError {
    /// Whether the producer broke the evaluation contract (an integration failure), rather than
    /// answering an evaluation the schema cannot store (`evaluation-unavailable`).
    #[must_use]
    pub const fn is_contract_defect(&self) -> bool {
        matches!(self, Self::MarketCurrency)
    }
}

/// Which roll-up a row is.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TotalScope {
    /// One line's component.
    Line,
    /// The order roll-up (line id nil).
    Order,
}
impl TotalScope {
    /// The stored token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Order => "order",
        }
    }
    /// The scope a stored token names, if any.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        [Self::Line, Self::Order]
            .into_iter()
            .find(|s| s.token() == token)
    }
}

/// Whether a row's amount is a commitment or usage-metered (`ck_bss_orders__resolved_total__2`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmountStatus {
    /// A committed recurring or one-time amount.
    Committed,
    /// A usage row: no committed amount exists.
    UncommittedUsage,
}
impl AmountStatus {
    /// The stored token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::UncommittedUsage => "uncommitted_usage",
        }
    }
    /// The status a stored token names, if any.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        [Self::Committed, Self::UncommittedUsage]
            .into_iter()
            .find(|s| s.token() == token)
    }
}

/// One `resolved_total` row's content (the engine supplies order, version and assessment).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotalRow {
    /// Line or order roll-up.
    pub scope: TotalScope,
    /// The line, or nil for the roll-up.
    pub line_id: Uuid,
    /// `recurring`, `one_time` or `usage`.
    pub charge_kind: EvaluationChargeKind,
    /// `committed`, or `uncommitted_usage` for a usage row.
    pub amount_status: AmountStatus,
    /// The evaluation currency.
    pub currency: String,
    /// The evaluation currency scale.
    pub currency_scale: u8,
    /// Producer exclusions.
    pub exclusions: Value,
    /// Producer item breakdown.
    pub item_breakdown: Value,
    /// Producer labelled recurring cycles.
    pub recurring_by_cycle: Value,
    /// Producer monetary horizon.
    pub amount_basis: String,
    /// Producer line/term/cycle evidence.
    pub period_evidence: Value,
    /// Gross exact decimal and display; `None` exactly for a usage row.
    pub gross: Option<(String, String)>,
    /// Net exact decimal and display; `None` exactly for a usage row.
    pub net: Option<(String, String)>,
    /// The discount, when available (never on a usage row).
    pub discount: Option<(String, String)>,
    /// The TCV exact decimal and display, its basis token and evidence (order/recurring only).
    pub tcv: Option<(String, String, TcvBasis, Value)>,
}

impl TotalRow {
    /// The persisted bytes of this row as `bss_orders__resolved_total` stores it, every column
    /// counted ([`persisted`]): order, version, scope, line, currency, assessment, scale, the
    /// producer declarations, amount status, amounts, discount status and amounts, charge kind
    /// and the TCV (the version limit, [`super::commercial::version_capacity`]).
    #[must_use]
    pub fn persisted_bytes(&self) -> usize {
        let pair = |p: Option<&(String, String)>| p.map_or(0, |(e, d)| e.len() + d.len());
        let discount_status = if self.discount.is_some() {
            "available"
        } else {
            "unavailable"
        };
        persisted::UUID
            + persisted::INT
            + persisted::text(self.scope.token())
            + persisted::UUID
            + persisted::text(&self.currency)
            + persisted::UUID
            + persisted::INT
            + persisted::json(&self.exclusions)
            + persisted::json(&self.item_breakdown)
            + persisted::json(&self.recurring_by_cycle)
            + persisted::text(self.amount_status.token())
            + persisted::text(&self.amount_basis)
            + persisted::json(&self.period_evidence)
            + pair(self.gross.as_ref())
            + pair(self.net.as_ref())
            + persisted::text(discount_status)
            + pair(self.discount.as_ref())
            + persisted::text(self.charge_kind.token())
            + self
                .tcv
                .as_ref()
                .map_or(0, |(exact, display, basis, evidence)| {
                    exact.len() + display.len() + basis.token().len() + persisted::json(evidence)
                })
    }
}

/// The rows of `evaluation` for exactly `lines`, priced in `market_currency` (module doc).
///
/// # Errors
/// [`TotalsError`]: the evaluation is unusable for submit (`evaluation-unavailable`), or, when
/// [`TotalsError::is_contract_defect`], not the evaluation of this market.
pub fn total_rows(
    evaluation: &OrderEvaluation,
    lines: &[Uuid],
    market_currency: &str,
) -> Result<Vec<TotalRow>, TotalsError> {
    if evaluation.currency != market_currency {
        return Err(TotalsError::MarketCurrency);
    }
    let wanted: BTreeSet<Uuid> = lines.iter().copied().collect();
    let stated: BTreeSet<Uuid> = evaluation.lines.iter().map(|l| l.line_id).collect();
    if wanted.len() != lines.len() || stated.len() != evaluation.lines.len() || wanted != stated {
        return Err(TotalsError::Coverage);
    }
    let currency = |what| Currency {
        code: &evaluation.currency,
        scale: evaluation.currency_scale,
        what,
    };
    let mut rows = Vec::with_capacity(3 * lines.len() + 3);
    let mut line_kinds = BTreeSet::new();
    currency("order aggregate").check_totals(&evaluation.order_totals)?;
    for line in &evaluation.lines {
        currency("line aggregate").check_totals(&line.totals)?;
        let components = components(
            line.committed_components.as_deref(),
            line.declaration.as_ref(),
            &line.totals,
            "a line component",
        )?;
        if components.is_empty() && !line.usage_flagged {
            return Err(TotalsError::Undeclared("a line component"));
        }
        for (declaration, totals) in components {
            line_kinds.insert(committed_kind(declaration.charge_kind)?);
            rows.push(row(
                TotalScope::Line,
                line.line_id,
                declaration,
                totals,
                currency("line totals"),
                None,
            )?);
        }
        if let Some(usage) = line_usage(line)? {
            rows.push(usage_row(
                TotalScope::Line,
                line.line_id,
                usage,
                currency("line usage"),
            )?);
        }
    }
    rows.extend(order_rows(evaluation, &line_kinds)?);
    Ok(rows)
}

/// Build the order roll-up from declared components and its unique TCV carrier.
fn order_rows(
    evaluation: &OrderEvaluation,
    line_kinds: &BTreeSet<EvaluationChargeKind>,
) -> Result<Vec<TotalRow>, TotalsError> {
    let mut rows = Vec::with_capacity(3);
    let currency = |what| Currency {
        code: &evaluation.currency,
        scale: evaluation.currency_scale,
        what,
    };
    let any_usage = evaluation.lines.iter().any(|l| l.usage_flagged);
    let mut order = components(
        evaluation.order_committed_components.as_deref(),
        evaluation.order_declaration.as_ref(),
        &evaluation.order_totals,
        "the order component",
    )?;
    let order_kinds: BTreeSet<_> = order
        .iter()
        .map(|(d, _)| committed_kind(d.charge_kind))
        .collect::<Result<_, _>>()?;
    if let Some(carrier) = &evaluation.order_tcv_carrier {
        if carrier.declaration.charge_kind != EvaluationChargeKind::Recurring
            || order_kinds.contains(&EvaluationChargeKind::Recurring)
        {
            return Err(TotalsError::Components);
        }
        order.push((&carrier.declaration, &carrier.totals));
    }
    let has_carrier = order
        .iter()
        .any(|(d, _)| d.charge_kind == EvaluationChargeKind::Recurring);
    if !has_carrier {
        return Err(TotalsError::Undeclared("a TCV carrier"));
    }
    // Coverage compares identities only: producer amounts are never summed or equated to
    // the independent Preview/tax aggregate.
    if &order_kinds != line_kinds {
        return Err(TotalsError::Components);
    }
    for (declaration, totals) in order {
        let tcv = if declaration.charge_kind == EvaluationChargeKind::Recurring {
            let tcv = evaluation
                .tcv
                .as_ref()
                .ok_or(TotalsError::Undeclared("the TCV"))?;
            let basis = tcv.basis.ok_or(TotalsError::Undeclared("the TCV basis"))?;
            let evidence = tcv
                .evidence
                .clone()
                .ok_or(TotalsError::Undeclared("the TCV evidence"))?;
            let (exact, display) = currency("tcv").pair(&tcv.exact, &tcv.display)?;
            Some((exact, display, basis, evidence))
        } else {
            None
        };
        rows.push(row(
            TotalScope::Order,
            Uuid::nil(),
            declaration,
            totals,
            currency("order totals"),
            tcv,
        )?);
    }
    match (any_usage, &evaluation.order_usage_declaration) {
        (true, Some(usage)) => rows.push(usage_row(
            TotalScope::Order,
            Uuid::nil(),
            usage,
            currency("order usage"),
        )?),
        (false, None) => {}
        _ => return Err(TotalsError::Usage(None)),
    }
    Ok(rows)
}

/// Select explicit components or the legacy single component, rejecting ambiguous declarations.
fn components<'a>(
    explicit: Option<&'a [CommittedComponent]>,
    legacy: Option<&'a TotalsDeclaration>,
    aggregate: &'a Totals,
    what: &'static str,
) -> Result<Vec<(&'a TotalsDeclaration, &'a Totals)>, TotalsError> {
    let Some(explicit) = explicit else {
        return Ok(vec![(
            legacy.ok_or(TotalsError::Undeclared(what))?,
            aggregate,
        )]);
    };
    if let Some(legacy) = legacy
        && (explicit.len() != 1
            || explicit[0].declaration != *legacy
            || explicit[0].totals != *aggregate)
    {
        return Err(TotalsError::Components);
    }
    let mut kinds = BTreeSet::new();
    let mut result = Vec::with_capacity(explicit.len());
    for component in explicit {
        if !kinds.insert(committed_kind(component.declaration.charge_kind)?) {
            return Err(TotalsError::Components);
        }
        result.push((&component.declaration, &component.totals));
    }
    Ok(result)
}

/// The line's usage declaration exactly when it carries usage charges (module doc).
fn line_usage(line: &LineEvaluation) -> Result<Option<&TotalsDeclaration>, TotalsError> {
    match (line.usage_flagged, &line.usage_declaration) {
        (true, Some(usage)) => Ok(Some(usage)),
        (false, None) => Ok(None),
        _ => Err(TotalsError::Usage(Some(line.line_id))),
    }
}

/// A usage row: the declarations verbatim, no committed amount, no discount, no TCV.
fn usage_row(
    scope: TotalScope,
    line_id: Uuid,
    declaration: &TotalsDeclaration,
    currency: Currency<'_>,
) -> Result<TotalRow, TotalsError> {
    if declaration.charge_kind != EvaluationChargeKind::Usage {
        return Err(TotalsError::Usage(
            (scope == TotalScope::Line).then_some(line_id),
        ));
    }
    Ok(TotalRow {
        scope,
        line_id,
        charge_kind: EvaluationChargeKind::Usage,
        amount_status: AmountStatus::UncommittedUsage,
        currency: currency.code.to_owned(),
        currency_scale: currency.scale,
        exclusions: declaration.exclusions.clone(),
        item_breakdown: declaration.item_breakdown.clone(),
        recurring_by_cycle: declaration.recurring_by_cycle.clone(),
        amount_basis: declaration.amount_basis.clone(),
        period_evidence: declaration.period_evidence.clone(),
        gross: None,
        net: None,
        discount: None,
        tcv: None,
    })
}

/// `bss_orders__decimal_valid(value, max_scale)` (07 §integrity): canonical decimal text, no
/// exponent, no leading `+`, no leading zeros, no trailing fractional zeros, no `-0`, at most 28
/// significant digits and `max_scale` fractional digits, at most 31 characters.
#[must_use]
pub fn decimal_valid(value: &str, max_scale: u8) -> bool {
    if max_scale > MAX_DECIMAL_SCALE || value.is_empty() || value.len() > MAX_DECIMAL_TEXT {
        return false;
    }
    if value == "-0" {
        return false;
    }
    let unsigned = value.strip_prefix('-').unwrap_or(value);
    let (int, frac) = match unsigned.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (unsigned, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(int) || (int.len() > 1 && int.starts_with('0')) {
        return false;
    }
    if let Some(frac) = frac
        && (!digits(frac) || frac.ends_with('0') || frac.len() > usize::from(max_scale))
    {
        return false;
    }
    let significant = unsigned
        .bytes()
        .filter(u8::is_ascii_digit)
        .skip_while(|b| *b == b'0')
        .count();
    significant <= usize::from(MAX_DECIMAL_SCALE)
}

/// The canonical decimal text of a producer's exact fraction (module doc): rendered at scale 28
/// by the shared money library, never rounded.
///
/// # Errors
/// [`TotalsError::Fraction`] for a fraction that does not terminate within 28 places or text the
/// schema rejects (more than 28 significant digits).
pub fn exact_decimal(money: &ExactMoney, what: &'static str) -> Result<String, TotalsError> {
    let text = ExactAmount::parse_fraction(money.numerator(), money.denominator())
        .ok()
        .and_then(|exact| exact.canonical_at_scale(MAX_DECIMAL_SCALE).ok())
        .ok_or(TotalsError::Fraction(what))?;
    if !decimal_valid(&text, MAX_DECIMAL_SCALE) {
        return Err(TotalsError::Fraction(what));
    }
    Ok(text)
}

/// The canonical decimal text of a producer's display estimate at the currency `scale` (module
/// doc): its value is kept exactly, only the spelling is normalized (`10.00` is `10`).
///
/// # Errors
/// [`TotalsError::Display`] for text that is not a plain decimal, carries more fractional digits
/// than `scale` (other than trailing zeros) or more than 28 significant digits.
pub fn display_decimal(
    display: &str,
    scale: u8,
    what: &'static str,
) -> Result<String, TotalsError> {
    let text = bss_money::parse_decimal(display)
        .ok()
        .and_then(|value| {
            ExactAmount::from_decimal(value)
                .canonical_at_scale(scale)
                .ok()
        })
        .ok_or(TotalsError::Display(what))?;
    if !decimal_valid(&text, scale) {
        return Err(TotalsError::Display(what));
    }
    Ok(text)
}

/// The evaluation currency every amount must carry.
#[derive(Clone, Copy)]
struct Currency<'a> {
    code: &'a str,
    scale: u8,
    what: &'static str,
}
impl Currency<'_> {
    fn check_currency(self, exact: &ExactMoney) -> Result<(), TotalsError> {
        if exact.currency() != self.code || exact.currency_scale() != self.scale {
            return Err(TotalsError::Currency(self.what));
        }
        Ok(())
    }
    fn check_totals(self, totals: &Totals) -> Result<(), TotalsError> {
        self.check_currency(&totals.gross.exact)?;
        self.check_currency(&totals.net.exact)?;
        if let Availability::Available(amount) = &totals.discount {
            self.check_currency(&amount.exact)?;
        }
        Ok(())
    }
    fn pair(self, exact: &ExactMoney, display: &str) -> Result<(String, String), TotalsError> {
        self.check_currency(exact)?;
        let exact = exact_decimal(exact, self.what)?;
        let display = display_decimal(display, self.scale, self.what)?;
        Ok((exact, display))
    }
    fn amount(self, amount: &Amount) -> Result<(String, String), TotalsError> {
        self.pair(&amount.exact, &amount.display)
    }
}

const fn committed_kind(kind: EvaluationChargeKind) -> Result<EvaluationChargeKind, TotalsError> {
    match kind {
        EvaluationChargeKind::Recurring | EvaluationChargeKind::OneTime => Ok(kind),
        // A usage component carries no committed amount; the mirror never states one here.
        EvaluationChargeKind::Usage => Err(TotalsError::Undeclared("a committed charge kind")),
    }
}

fn row(
    scope: TotalScope,
    line_id: Uuid,
    declaration: &TotalsDeclaration,
    totals: &Totals,
    currency: Currency<'_>,
    tcv: Option<(String, String, TcvBasis, Value)>,
) -> Result<TotalRow, TotalsError> {
    let charge_kind = committed_kind(declaration.charge_kind)?;
    let discount = match &totals.discount {
        Availability::Available(amount) => Some(currency.amount(amount)?),
        Availability::Unavailable => None,
    };
    Ok(TotalRow {
        scope,
        line_id,
        charge_kind,
        amount_status: AmountStatus::Committed,
        currency: currency.code.to_owned(),
        currency_scale: currency.scale,
        exclusions: declaration.exclusions.clone(),
        item_breakdown: declaration.item_breakdown.clone(),
        recurring_by_cycle: declaration.recurring_by_cycle.clone(),
        amount_basis: declaration.amount_basis.clone(),
        period_evidence: declaration.period_evidence.clone(),
        gross: Some(currency.amount(&totals.gross)?),
        net: Some(currency.amount(&totals.net)?),
        discount,
        tcv,
    })
}
