//! ASC 606 revenue-recognition domain (Slice 4, design §4.2 / §4.4). Pure,
//! backend-agnostic deferral/timing derivation for the deferred Contract-liability
//! balance posted at invoice (Slice 1) — the **plan** half of recognition, with no
//! DB / txn / async I/O (the in-txn materialization is the Group C
//! `ScheduleBuilderSidecar`, the release is the Phase 2 `RecognitionRunner`).
//!
//! Two layers, both pure:
//!
//! - [`input`] — [`input::RecognitionInput`], the per-invoice-item recognition
//!   spec (the approved v1 interface). An item with no `RecognitionInput` is
//!   recognized now (`deferred = 0`, today's Variant-A behaviour); one with a
//!   `POINT_IN_TIME` timing is likewise undeferred; one with
//!   `STRAIGHT_LINE { periods, first_period_id }` defers its whole ex-tax amount to
//!   `CONTRACT_LIABILITY` and recognizes it over N equal segments.
//! - [`ports`] — the three resolver port traits ([`ports::DeferralPolicyResolver`],
//!   [`ports::SspResolver`], [`ports::VcResolver`]) the derivation calls to resolve
//!   deferral/timing (R1/R2 precedence), validate SSP-snapshot presence for a
//!   multi-PO line (§4.4), and carry the VC refs (VC posting is OUT of the MVP,
//!   N-revrec-4). v1 ships a config/input-backed default of each
//!   ([`ports::DefaultDeferralPolicyResolver`] etc.) that reads only the request
//!   input + tenant-config defaults — **no network, no snapshot tables** (those
//!   resolve locally in a later refinement, design §13 / I-6).
//! - [`builder`] — [`builder::ScheduleBuilder`], the pure derivation that turns the
//!   resolved policy + item context into a [`builder::ScheduleOutcome`]
//!   (`NoDeferral` or a [`builder::BuiltSchedule`] plan), or a
//!   [`crate::domain::error::DomainError`] block. The Group C sidecar reads the
//!   plan's public fields to build the `recognition_schedule` /
//!   `recognition_segment` insert rows — the builder itself never imports the
//!   repo (DE0301 — no infra in domain).
//!
//! Ports are **sync** (mirroring [`crate::domain::ports::metrics`]): the v1
//! resolvers are pure functions of local data, so there is no async I/O to model,
//! and a sync trait keeps [`builder::ScheduleBuilder`] callable from the in-txn
//! sidecar without an executor.

pub mod builder;
pub mod change;
pub mod input;
pub mod ports;
