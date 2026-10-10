//! Capture slice logic (02 §2-§4; DESIGN 02 §4.1, §4.3): trigger selection from the shared field
//! classification, the registered capture guard predicates and authored term geometry.
//!
//! Everything here is pure and local. Draft authoring resolves no catalog reference, contract,
//! price or total (02 §4.1); its only guards are the structural ones below, which the engine runs
//! in registration order after authorization, idempotency, admissibility and both revisions.
use std::sync::Arc;

use bss_orders_lifecycle_sdk::authoring::{
    AuthoredTerm, BillingCycle, Category, HeaderPatch, LinePatch,
};
use bss_orders_lifecycle_sdk::catalog::{Reason, Trigger};
use uuid::Uuid;

use super::contributions::{FieldClassification, FieldScope, Selection};
use super::transition::{GuardBindings, GuardSubject, GuardVerdict};

/// The declared line cap baseline (DESIGN 02 §3.7: 200 lines, static per-gear configuration).
pub const LINE_CAP_BASELINE: u16 = 200;

/// Registered capture guard names (`domain::guards`), in registration order.
pub const MEMBERSHIP: &str = "capture.draft-line-membership";
pub const MIXED: &str = "capture.mixed-field-classes";
pub const SELLER_FIXED: &str = "capture.seller-fixed";
pub const CATEGORY: &str = "capture.category-admitted";
pub const CURRENCY: &str = "capture.currency-consistent";
pub const LINE_CAP: &str = "capture.line-cap";

/// What a request's named fields select, from the shared declaration alone (02 §3.1, D-145).
/// `None` means a named field is not classified: boundary `request-invalid`.
#[must_use]
pub fn select_header(fields: &FieldClassification, patch: &HeaderPatch) -> Option<Selection> {
    select(fields, FieldScope::Order, &patch.named_fields())
}

/// Line `PATCH` selection; line `DELETE` and `POST` always select `draft-mutate` (membership).
#[must_use]
pub fn select_line(fields: &FieldClassification, patch: &LinePatch) -> Option<Selection> {
    select(fields, FieldScope::Line, &patch.named_fields())
}

fn select(fields: &FieldClassification, scope: FieldScope, named: &[&str]) -> Option<Selection> {
    let named: Vec<(FieldScope, &str)> = named.iter().map(|n| (scope, *n)).collect();
    fields.select(&named)
}

/// The struct field names serde derives for an authoring body: exactly the wire names it
/// accepts. Read from the derived `Deserialize` impl itself, so a field added to an SDK body
/// cannot escape the classification check below. Empty when `T` is not a struct.
#[must_use]
pub fn wire_fields<T: serde::de::DeserializeOwned>() -> &'static [&'static str] {
    use serde::de::{Error as _, Visitor, value::Error};
    struct Probe<'a>(&'a mut &'static [&'static str]);
    impl<'de> serde::Deserializer<'de> for Probe<'_> {
        type Error = Error;
        fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
            Err(Error::custom("not a struct"))
        }
        fn deserialize_struct<V: Visitor<'de>>(
            self,
            _: &'static str,
            fields: &'static [&'static str],
            _: V,
        ) -> Result<V::Value, Error> {
            *self.0 = fields;
            Err(Error::custom("field names recorded"))
        }
        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
            option unit unit_struct newtype_struct seq tuple tuple_struct map enum identifier
            ignored_any
        }
    }
    let mut fields: &'static [&'static str] = &[];
    // The probe always errors once it has recorded the names; any success is not a struct.
    match T::deserialize(Probe(&mut fields)) {
        Ok(_) => &[],
        Err(_) => fields,
    }
}

/// Startup check (02 §3.1 step 1, DESIGN 02 §4.3, AC "startup rejects an unclassified authored
/// field"): every wire field of every authoring body is declared in the shared classification
/// under its scope, so no request field can reach a trigger selection unclassified.
///
/// # Errors
/// The first unclassified `(scope, field)`, or a body whose fields cannot be read.
pub fn check_wire_classification(fields: &FieldClassification) -> Result<(), String> {
    use bss_orders_lifecycle_sdk::authoring::{AddLine, CreateOrder};
    let bodies: [(&str, FieldScope, &'static [&'static str]); 4] = [
        (
            "CreateOrder",
            FieldScope::Order,
            wire_fields::<CreateOrder>(),
        ),
        (
            "HeaderPatch",
            FieldScope::Order,
            wire_fields::<HeaderPatch>(),
        ),
        ("AddLine", FieldScope::Line, wire_fields::<AddLine>()),
        ("LinePatch", FieldScope::Line, wire_fields::<LinePatch>()),
    ];
    for (body, scope, names) in bodies {
        if names.is_empty() {
            return Err(format!("{body}: wire fields unreadable"));
        }
        if let Some(name) = names.iter().find(|n| fields.class(scope, n).is_none()) {
            return Err(format!("{body}.{name} is not classified ({scope:?})"));
        }
    }
    Ok(())
}

/// One working-set member as the coherent draft snapshot holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingMember {
    pub line_id: Uuid,
    pub currency: String,
}

/// The facts the row-2 capture guards decide on: the request's own classification facts and the
/// coherent draft snapshot the engine verifies against its lock (prepared revision = locked
/// revision, OL-4), so the snapshot is the locked working set when the guards run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent request fact per registered capture guard"
)]
pub struct DraftGuardFacts {
    /// Commercial and administrative fields named together (D-118).
    pub mixed: bool,
    /// The request names `seller_tenant_id`, whatever its value (D-119).
    pub names_seller: bool,
    /// A category the request names; otherwise the locked category is checked.
    pub named_category: Option<Category>,
    /// Line edit/removal target (`line-not-found` when absent from the working set, D-116).
    pub target_line: Option<Uuid>,
    /// The currency a line insertion or a currency-changing edit proposes.
    pub proposed_currency: Option<String>,
    /// A line insertion, which alone consumes line-cap capacity.
    pub inserts_line: bool,
    pub working: Vec<WorkingMember>,
    pub line_cap: usize,
}

impl DraftGuardFacts {
    fn membership(&self) -> GuardVerdict {
        match self.target_line {
            Some(line) if !self.working.iter().any(|m| m.line_id == line) => {
                GuardVerdict::Fail(Reason::LineNotFound)
            }
            _ => GuardVerdict::Pass,
        }
    }
    fn mixed(&self) -> GuardVerdict {
        if self.mixed {
            GuardVerdict::Fail(Reason::MixedFieldClasses)
        } else {
            GuardVerdict::Pass
        }
    }
    fn seller(&self) -> GuardVerdict {
        if self.names_seller {
            GuardVerdict::Fail(Reason::TenantAxisImmutable)
        } else {
            GuardVerdict::Pass
        }
    }
    fn category(&self, locked: &str) -> GuardVerdict {
        category_verdict(self.named_category, locked)
    }
    /// One currency per order: every other member shares the proposed currency (02 §2.2).
    fn currency(&self) -> GuardVerdict {
        let Some(proposed) = &self.proposed_currency else {
            return GuardVerdict::Pass;
        };
        currency_verdict(
            self.working
                .iter()
                .filter(|m| Some(m.line_id) != self.target_line)
                .map(|m| m.currency.as_str())
                .chain(std::iter::once(proposed.as_str())),
        )
    }
    /// Only an insertion consumes capacity; edits and removals never do (02 §2.3 step 4).
    fn line_cap(&self) -> GuardVerdict {
        if self.inserts_line {
            line_cap_verdict(self.working.len() + 1, self.line_cap)
        } else {
            GuardVerdict::Pass
        }
    }

    /// Bind every registered row-2 capture guard to these facts.
    #[must_use]
    pub fn bindings(self) -> GuardBindings {
        let facts = Arc::new(self);
        let bind = |f: fn(&DraftGuardFacts, &GuardSubject<'_>) -> GuardVerdict| {
            let facts = Arc::clone(&facts);
            move |subject: &GuardSubject<'_>| f(&facts, subject)
        };
        GuardBindings::new()
            .bind(MEMBERSHIP, bind(|f, _| f.membership()))
            .bind(MIXED, bind(|f, _| f.mixed()))
            .bind(SELLER_FIXED, bind(|f, _| f.seller()))
            .bind(CATEGORY, bind(|f, s| f.category(&s.locked.category)))
            .bind(CURRENCY, bind(|f, _| f.currency()))
            .bind(LINE_CAP, bind(|f, _| f.line_cap()))
    }
}

/// The shared single-currency structural guard (`capture.currency-consistent`, rows 2, 4, 18-20;
/// Foundation step 19): the proposed working set or candidate lines carry one currency.
#[must_use]
pub fn currency_verdict<'c>(currencies: impl IntoIterator<Item = &'c str>) -> GuardVerdict {
    let mut currencies = currencies.into_iter();
    let first = currencies.next();
    if currencies.any(|c| Some(c) != first) {
        GuardVerdict::Fail(Reason::CurrencyMixed)
    } else {
        GuardVerdict::Pass
    }
}

/// The shared declared line cap (`capture.line-cap`, rows 2, 4, 18-20): the proposed line count
/// stays within the configured cap (200-line baseline).
#[must_use]
pub fn line_cap_verdict(proposed_lines: usize, cap: usize) -> GuardVerdict {
    if proposed_lines > cap {
        GuardVerdict::Fail(Reason::LineCapExceeded)
    } else {
        GuardVerdict::Pass
    }
}

/// `category-not-admitted` for a named or locked category this phase does not admit (02 §2.2,
/// D-176). An unregistered stored value fails closed the same way.
#[must_use]
pub fn category_verdict(named: Option<Category>, locked: &str) -> GuardVerdict {
    let category = named.or_else(|| Category::parse(locked));
    if category.is_some_and(Category::is_admitted) {
        GuardVerdict::Pass
    } else {
        GuardVerdict::Fail(Reason::CategoryNotAdmitted)
    }
}

/// The create row's only guard, over the proposed initial facts (02 *Create Draft Order* step 1).
#[must_use]
pub fn create_bindings() -> GuardBindings {
    GuardBindings::new().bind(CATEGORY, |subject: &GuardSubject<'_>| {
        category_verdict(None, &subject.locked.category)
    })
}

/// The trigger a commercial capture request runs; administrative-only requests are row 3.
#[must_use]
pub fn is_draft_mutation(selection: Selection) -> bool {
    selection.trigger == Trigger::DraftMutate
}

/// Stored representation of authored term intent (D-193; `bss_orders__term_valid`):
/// `(term_kind, authored_term, term_duration)`. A period count without an authored cycle keeps its
/// intent and no interval; the interval follows the cycle once authored.
#[must_use]
pub fn term_columns(
    term: Option<&AuthoredTerm>,
    cycle: Option<BillingCycle>,
) -> (&'static str, serde_json::Value, Option<String>) {
    match term {
        None => ("missing", serde_json::json!({"kind": "missing"}), None),
        Some(AuthoredTerm::Rolling) => ("rolling", serde_json::json!({"kind": "rolling"}), None),
        Some(t @ AuthoredTerm::Periods { count }) => {
            let months = match cycle {
                Some(BillingCycle::Month) => Some(i64::from(*count)),
                Some(BillingCycle::Year) => Some(i64::from(*count) * 12),
                None => None,
            };
            ("finite", term_json(t), months.map(|m| interval(m, 0, 0)))
        }
        Some(
            t @ AuthoredTerm::Calendar {
                years,
                months,
                days,
                microseconds,
            },
        ) => (
            "finite",
            term_json(t),
            Some(interval(
                i64::from(*years) * 12 + i64::from(*months),
                i64::from(*days),
                i64::try_from(*microseconds).unwrap_or(i64::MAX),
            )),
        ),
    }
}

/// The authored term a stored row carries; `None` is the missing intent.
///
/// # Errors
/// A stored shape outside the closed D-193 set (the database CHECK makes this unreachable).
pub fn stored_term(kind: &str, authored: &serde_json::Value) -> Result<Option<AuthoredTerm>, ()> {
    match kind {
        "missing" => Ok(None),
        "rolling" | "finite" => serde_json::from_value(authored.clone())
            .map(Some)
            .map_err(|_| ()),
        _ => Err(()),
    }
}

fn term_json(term: &AuthoredTerm) -> serde_json::Value {
    serde_json::to_value(term).unwrap_or(serde_json::Value::Null)
}

/// PostgreSQL interval text in exact calendar units (never months converted to days).
fn interval(months: i64, days: i64, microseconds: i64) -> String {
    format!("{months} mons {days} days {microseconds} microseconds")
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
