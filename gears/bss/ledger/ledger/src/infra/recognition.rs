//! Infra-side ASC 606 recognition (Slice 4): the DB-touching half of the
//! recognition flow whose pure derivation lives in
//! [`crate::domain::recognition`]. Group C ships the in-transaction
//! materialization sidecar; Group D adds the release runner; the Phase 3
//! disaggregation read API is NOT here.
//!
//! - [`sidecar`] — [`sidecar::ScheduleBuilderSidecar`], the [`PostSidecar`] that
//!   materializes the derived `recognition_schedule` + `recognition_segment` rows
//!   in the SAME serializable transaction as the invoice post's
//!   `CR CONTRACT_LIABILITY` credit, idempotent on the `SCHEDULE_BUILD` claim;
//!   [`sidecar::RecognitionStampSidecar`], the release-side [`PostSidecar`]
//!   that bumps `recognized_minor` (under the over-recognition cap CHECK) and
//!   stamps the segment `DONE` in the same txn as the `DR CL / CR Revenue` post;
//!   plus [`sidecar::RecognitionReversalSidecar`] (Group F1), the clawback-side
//!   [`PostSidecar`] that DECREMENTS `recognized_minor` (under the non-negative
//!   cap CHECK) in the same txn as the compensating `DR Revenue / CR CL` post,
//!   leaving the reversed segment `DONE`.
//! - [`runner`] — [`runner::RecognitionRunner`], the S6 release mechanism
//!   (Group D): posts one balanced `DR CONTRACT_LIABILITY / CR REVENUE` entry per
//!   due `PENDING` segment through the Slice 1 [`PostingService`], idempotent per
//!   `(tenant, RECOGNITION, schedule_id:segment_no)`; plus the reversal mechanism
//!   (Group F1) keyed `schedule_id:segment_no:reversal`, the §9 recognition
//!   metrics, and the `RECOGNITION_PERIOD_QUEUED` / `RECOGNITION_DOUBLE_CREDIT`
//!   alarms.
//! - [`run_service`] — [`run_service::RecognitionRunService`], the Group E
//!   orchestration that brackets one [`runner::RecognitionRunner`] pass with the
//!   `recognition_run` row lifecycle (dedup → `RUNNING` → `DONE`/`FAILED`) and
//!   maps the pass summary onto the SDK [`RecognitionRunOutcome`].
//!
//! [`PostSidecar`]: crate::infra::posting::service::PostSidecar
//! [`PostingService`]: crate::infra::posting::service::PostingService
//! [`RecognitionRunOutcome`]: bss_ledger_sdk::RecognitionRunOutcome

pub mod change_service;
pub mod run_service;
pub mod runner;
pub mod sidecar;
