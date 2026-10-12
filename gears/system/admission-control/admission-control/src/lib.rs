#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! Admission Control gear.
//!
//! Evaluates admission requests from platform gears against the single
//! selected engine plugin and returns the verdict.
//! Refusals and shadow findings are published best-effort as events on the
//! audit topic.
//!
//! The public contract is the SDK (`admission_control_sdk`): enforcing gears
//! resolve `dyn AdmissionClientV1` from `ClientHub`. Every module besides
//! [`gear`] is an implementation detail exposed only for tests.
#![forbid(unsafe_code)]

pub mod gear;
pub use gear::AdmissionControl;

#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;
