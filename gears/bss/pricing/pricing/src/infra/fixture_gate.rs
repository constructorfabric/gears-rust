//! The joint-conformance publish gate (`inst-fx-gate`).
//!
//! Publish of a catalog row is admissible only while a **green** joint fixture
//! pins what that row means on the pricing <-> Rating seam. The corpus lives in
//! `gears/bss/fixtures`; its generated `registry.toml` is the artifact this gate
//! reads, because the gate runs inside a publish transaction and cannot run an
//! oracle there.
//!
//! ## The gate is asked about a row, not about a kind
//!
//! A row needs **every fixture its shape requires**, and the design set names
//! four of them as registry *variants* (S3 design 6: the registry is keyed
//! `model_kind` / `variant`):
//!
//! | The row is... | and it additionally needs |
//! |---|---|
//! | anything | its `modelKind` variant (`inst-fx-gate`, `inst-fx-kinds`) |
//! | non-`sum` | the `level_aggregation` variant (`inst-la-fixture`, D-44) |
//! | a tiered **usage** kind | the `supersession_continuity` variant (D-22) |
//! | reserved | the `reserved` variant (`inst-fx-kinds`, Slice 10) |
//!
//! Asked about a bare `modelKind` the last three say nothing: a `peak` row would
//! publish on the strength of a `sum` row's fixture, and the level-aggregation,
//! supersession-continuity and reservation variants would gate nothing at all.
//! [`required_variants`] is where a row's variant set is decided, and it is the
//! whole content of that fix.
//!
//! The gear takes `bss-fixtures` with `default-features = false`. That surface
//! is `ModelKind` + `Variant` + `Registry` + `Registry::gate_open_for`, and the
//! question it answers per variant is deliberately `oracle && publish`: the
//! reference oracle stands in for Tariffs until Tariffs exists, and requiring
//! the `rating` half as well would block every publish at launch. An
//! unregistered `(kind, variant)` pair is never open.
//!
//! ## This is not `pricing_conformance_fixture_registry`
//!
//! S3 design 6 also specifies a **gear-side table**,
//! `pricing_conformance_fixture_registry` (`model_kind` / `variant`,
//! `fixture_ref`, `status`), tenant-global and read-only to every API path,
//! populated by the `FixtureRegistrySync` background task. That table is Slice
//! 3's and **does not exist yet**; nothing here builds it, and `fixture_ref` /
//! `status` appear nowhere in this gear.
//!
//! The two must not blur together. The corpus registry read here is a property
//! of the *gear build*: it ships in the artifact, it is regenerated from the
//! case files, and it carries no `fixture_ref` because the fixture it points at
//! is the corpus itself. When the table lands, this gate reads the table and the
//! generated file becomes what `FixtureRegistrySync` reconciles it *against* —
//! the variant vocabulary below is the same one, which is what will let the
//! swap be a swap rather than a second reading.
//!
//! **Fail-closed, in both directions.** There is no configuration value that
//! opens this gate, and a registry that cannot be read produces a *closed*
//! gate rather than a boot failure or a permissive default. The split is
//! deliberate: the read path this gear serves to Rating and Tariffs must keep
//! working when the fixtures artifact is missing from a deployment, while every
//! publish of every row is refused. A gate that cannot read its registry has no
//! basis on which to answer "green", and the only safe answer it has left is
//! "no".
//!
//! **"No" comes in two kinds, and they have different owners.** A row whose shape
//! has no green pair is `FIXTURE_MISSING`, a 400 naming `model_kind`: nobody has
//! agreed how that shape is evaluated, and the author's move is to change the row
//! or get the corpus row earned. A registry that could not be *read* is
//! `FixtureGateUnavailable`, a 503: the author has no move at all, and the
//! operator who does needs the boot log and
//! `PricingAlarm::FixtureGateUnreadable` rather than one refused publish at a
//! time.

use std::path::Path;

use bss_fixtures::{ModelKind, Registry, Variant};
use tracing::error;

use crate::domain::allowance::{is_presented_tiered, presented_model_kind};
use crate::domain::error::DomainError;
use crate::domain::price_row::{PriceRow, model_kind_wire};

/// Whether the publishing row carries a reservation.
///
/// It stays a **parameter** rather than becoming a plain read inside
/// [`required_variants`], now that Slice 10 has landed `reserved_rate` /
/// `reservation_flavor` and [`Reservation::of_slice3_row`] answers from the row.
/// Two reasons, and neither is inertia:
///
/// - the mapping from a row to the variants it needs is a statement of the
///   design set, and keeping the reservation an argument is what lets a test
///   assert `required_variants` for a reserved row without constructing one —
///   the separability [`required_variants`] is a free function for;
/// - a future caller holding a reservation the row does not carry (a compiled
///   or overlay-derived one) can still ask the gate the right question, which is
///   the property the parameter was introduced for and which outliving the hole
///   does not invalidate.
///
/// The production path does **not** exercise that freedom: `infra::publish`
/// calls `Reservation::of_slice3_row(&record.row)`, so a reserved row is gated
/// on `Variant::Reserved` by reading itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reservation {
    /// The row reserves nothing.
    #[default]
    Unreserved,
    /// The row is reserved, and therefore needs [`Variant::Reserved`].
    Reserved,
}

impl Reservation {
    /// What the row says about itself — **a real read since Slice 10**.
    ///
    /// This body was `{ Unreserved }`, a constant, for as long as [`PriceRow`]
    /// was the Slice-3 shape and carried neither `reserved_rate` nor
    /// `reservation_flavor`. The hole was stated here rather than left silent
    /// precisely so that closing it would be one function: Slice 10 added the
    /// pair, this reads it, and `infra::publish`'s single call site — which
    /// already asked through this function rather than assuming — began gating
    /// reserved rows with **no call site moving**, exactly as the note it
    /// replaced promised.
    ///
    /// **Either half counts.** [`PriceRow::is_reserved`] is a disjunction
    /// because the pairing is a publish rule
    /// ([`RESERVATION_PAIR_INCOMPLETE`](crate::domain::publish::rules::RESERVATION_PAIR_INCOMPLETE)),
    /// so a half-authored reservation is a state a draft can legitimately be in.
    /// Reading it as a conjunction would let a flavor-without-rate row past this
    /// gate on its way to that refusal — the wrong order to meet the two
    /// problems in, and a row that publishes against no reservation fixture at
    /// all if the pairing rule were ever weakened.
    #[must_use]
    pub const fn of_slice3_row(row: &PriceRow) -> Self {
        if row.is_reserved() {
            Self::Reserved
        } else {
            Self::Unreserved
        }
    }
}

/// Every registry variant a publishing row must be green in, in the order the
/// refusal reports them.
///
/// **This function is the mapping.** Each clause is one instruction of the
/// design set:
///
/// - [`Variant::ModelKind`] unconditionally — `inst-fx-gate` blocks publish of
///   any `modelKind` lacking a green joint fixture, and `inst-fx-kinds` names
///   `package` and `per_unit` as needing their own.
/// - [`Variant::LevelAggregation`] on a non-`sum` row — `inst-la-fixture`:
///   "publish of any non-`sum` row without the green joint fixture is blocked
///   (`FIXTURE_MISSING`), exactly like a `modelKind` variant". Read off
///   [`PriceRow::is_level`], which resolves the `sum` default, so an unauthored
///   `aggregationFunction` and an authored `sum` are one row here as everywhere.
/// - [`Variant::SupersessionContinuity`] on a **tiered usage** row — D-22, which
///   ratified the continuity fixture as gating "the first publish of any tiered
///   usage kind (alongside that kind's own fixture)". Both halves are required:
///   tieredness because the fixture is about a tier counter, and `is_usage`
///   because a non-usage row has no counter to continue. Tieredness is read off
///   the **presented** kind
///   ([`is_presented_tiered`](crate::domain::allowance::is_presented_tiered)),
///   which differs from [`PriceRow::is_tiered`] on exactly one shape: an
///   untiered `per_unit` row carrying an `includedAllowance`, which publishes as
///   a band ladder (`inst-ac-band`).
/// - [`Variant::Reserved`] on a reserved row — `inst-rv-fixture`: "the
///   reservation variant requires its own joint golden fixture before publish
///   (registered into Slice 3's `FixtureGate`)". **This clause is that
///   registration**, and it is the half of `inst-rv-fixture` this gear owns; the
///   other half — negotiated RI-style rates — stays in Contracts (D-108 clause
///   (b)) and no rate path here reaches it. Read through
///   [`Reservation::of_slice3_row`] on the production path — see
///   [`Reservation`] for why that is a parameter.
///
/// It is deliberately a free function and not a method on [`FixtureGate`]: what
/// a row needs is a property of the **row**, decided by the design set, and it
/// does not vary with which registry happens to be loaded. Keeping it separable
/// is what lets a test assert the mapping without a registry at all.
#[must_use]
pub fn required_variants(row: &PriceRow, reservation: Reservation) -> Vec<Variant> {
    let mut required = vec![Variant::ModelKind];
    if row.is_level() {
        required.push(Variant::LevelAggregation);
    }
    // The **presented** shape, not the authored one (`inst-ac-band`, D-45): an
    // untiered `per_unit` row carrying an `includedAllowance` publishes as a
    // band ladder, so the continuity fixture is about a counter it does have.
    // Every other row presents what it authored, so this reads as `is_tiered`
    // everywhere the compile does not fire.
    if is_presented_tiered(row) && row.is_usage() {
        required.push(Variant::SupersessionContinuity);
    }
    if matches!(reservation, Reservation::Reserved) {
        required.push(Variant::Reserved);
    }
    required
}

/// The publish-time conformance gate over a loaded [`Registry`].
///
/// Constructed once at gear init and read on every publish. Cheap to consult —
/// the registry is a handful of rows held in memory — which is what lets the
/// gate sit inside the publish transaction where it belongs.
#[derive(Debug, Clone)]
pub struct FixtureGate {
    registry: Registry,
    /// Whether this gate is closed because the artifact could not be read.
    ///
    /// Not the same fact as "the registry is empty", which is why it is a field
    /// and not a derivation from `registry`: [`FixtureGate::closed`] is a
    /// deliberately empty gate a caller asked for, and a corpus whose every pair
    /// is red is a corpus that was read. Only [`FixtureGate::load`]'s error arm
    /// sets it, so the flag answers *why* rather than *what*, and the refusal
    /// [`FixtureGate::check`] gives can name the party who can act.
    gate_unreadable: bool,
}

impl FixtureGate {
    /// Load the generated registry from `path`.
    ///
    /// **Never returns an error, and records that it could not read one.** An
    /// unreadable or unparseable registry yields a gate closed for every row,
    /// logged at `error!` with the path: the gear still boots and still serves
    /// reads. Returning a `Result` here would invite a caller to treat the error
    /// as "gate unknown" and proceed, which is the one outcome this type exists
    /// to make unreachable.
    ///
    /// The gate carries [`Self::gate_unreadable`] afterwards, so
    /// [`Self::check`]'s refusal can say whose fault the deployment is. Without
    /// it every publish of every kind answered a 400 naming `model_kind` —
    /// telling the operator their row had no green fixture, with a remedy they
    /// could not perform, when the artifact was simply not deployed.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        match Registry::load(path) {
            Ok(registry) => Self {
                registry,
                gate_unreadable: false,
            },
            Err(e) => {
                error!(
                    path = %path.display(),
                    error = %e,
                    "bss-pricing: conformance fixture registry unreadable; the publish gate is \
                     CLOSED for every model kind until it can be read"
                );
                Self {
                    registry: Registry::default(),
                    gate_unreadable: true,
                }
            }
        }
    }

    /// A gate that is closed for every row.
    ///
    /// The empty registry is not a placeholder for "unknown": an unregistered
    /// `(kind, variant)` pair is never open, so an empty variant list refuses
    /// everything by the same rule that refuses a pair whose fixtures are red.
    #[must_use]
    pub fn closed() -> Self {
        Self {
            registry: Registry::default(),
            // A gate a caller asked to be closed is not a broken deployment, and
            // must not answer 503: the refusal it gives is about the row.
            gate_unreadable: false,
        }
    }

    /// Whether this gate is closed because its artifact could not be read.
    ///
    /// For the boot line and the alarm, which are the only places an operator
    /// learns the difference before the first refused publish.
    #[must_use]
    pub const fn gate_unreadable(&self) -> bool {
        self.gate_unreadable
    }

    /// The kinds whose **own** (`model_kind`) fixture is green, in the corpus
    /// spelling.
    ///
    /// For the boot log, not for the publish path: an operator has to be able
    /// to read from the startup line which kinds are publishable, because the
    /// difference between "the corpus is green" and "the registry file was not
    /// deployed" is otherwise only visible one failed publish at a time.
    ///
    /// It is the `model_kind` variant alone, so it is a **floor** and not the
    /// whole answer — a kind listed here still cannot publish a level or a
    /// tiered usage row unless the matching cross-cutting variant is green too.
    /// [`FixtureGate::open_variants`] is what shows that, and it is what the
    /// boot line logs beside this.
    #[must_use]
    pub fn open_kinds(&self) -> Vec<&'static str> {
        ModelKind::ALL
            .into_iter()
            .filter(|kind| self.registry.gate_open_for(*kind, Variant::ModelKind))
            .map(model_kind_wire)
            .collect()
    }

    /// Every green `(kind, variant)` pair, rendered `kind/variant`.
    ///
    /// The honest boot line. `open_kinds` alone would tell an operator that
    /// `volume` is open while `volume/level_aggregation` is not registered at
    /// all, and the first refusal of a `peak` `volume` row would then read as a
    /// bug rather than as the state of the corpus.
    #[must_use]
    pub fn open_variants(&self) -> Vec<String> {
        let mut open = Vec::new();
        for kind in ModelKind::ALL {
            for variant in Variant::ALL {
                if self.registry.gate_open_for(kind, variant) {
                    open.push(format!("{}/{}", model_kind_wire(kind), variant.wire()));
                }
            }
        }
        open
    }

    /// Whether `row` may be published.
    ///
    /// Asks the registry once per variant in [`required_variants`] and refuses
    /// on the first one that is not green, **naming it** — an operator's next
    /// move is to look that variant up in the registry, and "this row is not
    /// gated" without saying by what is not remediable.
    ///
    /// # Errors
    /// [`DomainError::FixtureMissing`] when a variant the row requires has no
    /// green joint fixture — the corpus has no row for the pair, its oracle half
    /// is unearned, or this gear's validator has not yet reproduced its publish
    /// cases. All three are the same refusal from the caller's side: nobody has
    /// agreed how this shape is evaluated, so it must not become
    /// consumer-visible.
    ///
    /// Also [`DomainError::FixtureMissing`] when the row states no `modelKind`.
    /// The kind is the registry's first key, so there is no fixture to look up
    /// at all; `inst-mk-explicit` has already refused such a row under
    /// `MODEL_KIND_MISSING`, and this gate must not be the one place that reads
    /// an absent kind as an admissible one.
    ///
    /// [`DomainError::FixtureGateUnavailable`] — a **503** — when the gate is
    /// closed because [`Self::load`] could not read the artifact. It is answered
    /// ahead of every refusal below, including the absent-kind one: a gate with
    /// no registry has no opinion about any row, so anything it said about this
    /// row would attribute a deployment fault to the request. The cost is that a
    /// genuinely kindless row gets the deployment's answer on such a deployment
    /// rather than its own — which is the right trade, because on that
    /// deployment no row of any shape can publish and the author's remedy is the
    /// same either way.
    pub fn check(&self, row: &PriceRow, reservation: Reservation) -> Result<(), DomainError> {
        if self.gate_unreadable {
            return Err(DomainError::FixtureGateUnavailable(
                "the joint conformance fixture registry could not be read, so no row's shape \
                 can be gated; this is a deployment state and not a property of the request"
                    .to_owned(),
            ));
        }

        // The **presented** kind (`inst-ac-band`, D-45). A compiled allowance row
        // is rated as a `graduated` ladder whatever it was authored as, so gating
        // it on the authored `per_unit` fixture would admit a shape no joint
        // fixture covers. `presented_model_kind` answers the authored kind on
        // every row the compile does not fire for, which is all of them but this
        // one.
        let Some(kind) = presented_model_kind(row) else {
            return Err(DomainError::FixtureMissing(
                "the row states no modelKind, so no conformance fixture resolves for it; \
                 a kind is authored explicitly and is never inferred at publish"
                    .to_owned(),
            ));
        };

        for variant in required_variants(row, reservation) {
            if !self.registry.gate_open_for(kind, variant) {
                return Err(DomainError::FixtureMissing(format!(
                    "model kind `{}` is not gated by a green joint conformance fixture for the \
                     `{}` variant",
                    model_kind_wire(kind),
                    variant.wire()
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "fixture_gate_tests.rs"]
mod fixture_gate_tests;
