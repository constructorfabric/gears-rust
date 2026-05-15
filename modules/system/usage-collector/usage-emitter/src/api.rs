/// Source-facing trait for the process-lifetime usage-emitter runtime.
///
/// Obtain from `ClientHub`: `hub.get::<dyn UsageEmitterRuntimeV1>()?`
///
/// Registered in `ClientHub` by the host module (`usage-collector` for in-process
/// delivery, `usage-collector-rest-client` for HTTP delivery to a remote collector).
/// The runtime owns the outbox worker and shared Arcs for the entire process.
///
/// Call [`Self::factory`] with the module's name constant to obtain a
/// [`crate::domain::factory::UsageEmitterFactory`] bound to that module. Modules store
/// one factory per module and clone it per call to apply scope overrides via the fluent
/// `.with_*()` chain before invoking `.authorize()`.
///
/// # Crate shape note
///
/// This trait is intentionally published from the implementation crate
/// (`cyberware-usage-emitter`) rather than a thin `*-sdk` crate. The trait returns a
/// concrete [`crate::domain::factory::UsageEmitterFactory`], which in turn produces
/// concrete [`crate::domain::emitter::UsageEmitter`] /
/// [`crate::domain::usage_record_builder::UsageRecordBuilder`] handles carrying private
/// state that enforces the security invariant — those types cannot be reduced to opaque
/// trait objects. Consumers therefore always need the trait *and* the concrete types
/// together; splitting into trait-SDK + impl crates would not remove the impl-crate
/// dependency. See the crate-level `lib.rs` docs and `README.md` for the full rationale.
pub trait UsageEmitterRuntimeV1: Send + Sync {
    /// Obtain a [`UsageEmitterFactory`] bound to `module_name`.
    fn factory(&self, module_name: &str) -> crate::domain::factory::UsageEmitterFactory;
}
