//! Typed run identities and admission metadata.
use crate::workflow::WorkflowRef;
use crate::{RunId, StartResult};

/// A run pinned to its expected serialized workflow contract.
#[must_use]
pub struct RunHandle<I, O> {
    /// Durable run identifier.
    pub(crate) id: RunId,
    /// Expected contract checked on every result read.
    pub(crate) workflow: WorkflowRef<I, O>,
}
impl<I, O> RunHandle<I, O> {
    /// Identifier for progress, events and control commands.
    #[must_use]
    pub fn id(&self) -> RunId {
        self.id
    }
    /// The expected workflow contract, independent of the current worker's bindings.
    #[must_use]
    pub fn workflow(&self) -> &WorkflowRef<I, O> {
        &self.workflow
    }
}
impl<I, O> Clone for RunHandle<I, O> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            workflow: self.workflow.clone(),
        }
    }
}
impl<I, O> std::fmt::Debug for RunHandle<I, O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunHandle")
            .field("id", &self.id)
            .field("workflow", &self.workflow.name())
            .finish()
    }
}

/// Admission metadata plus the typed handle used to read results.
#[must_use]
pub struct StartedRun<I, O> {
    handle: RunHandle<I, O>,
    requested_generation: u64,
    coalesced: bool,
}
impl<I, O> StartedRun<I, O> {
    /// Pair admission metadata with a handle for `workflow`.
    pub(crate) fn new(result: StartResult, workflow: &WorkflowRef<I, O>) -> Self {
        Self {
            handle: workflow.attach(result.run_id),
            requested_generation: result.requested_generation,
            coalesced: result.coalesced,
        }
    }
    /// Identifier of the admitted or replayed run.
    #[must_use]
    pub fn run_id(&self) -> RunId {
        self.handle.id
    }
    /// Typed handle for result reads.
    pub fn handle(&self) -> &RunHandle<I, O> {
        &self.handle
    }
    /// Keep only the typed handle.
    pub fn into_handle(self) -> RunHandle<I, O> {
        self.handle
    }
    /// Coalescing generation of this request.
    #[must_use]
    pub fn requested_generation(&self) -> u64 {
        self.requested_generation
    }
    /// Whether this request joined already admitted work.
    #[must_use]
    pub fn coalesced(&self) -> bool {
        self.coalesced
    }
}
impl<I, O> std::fmt::Debug for StartedRun<I, O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartedRun")
            .field("handle", &self.handle)
            .field("requested_generation", &self.requested_generation)
            .field("coalesced", &self.coalesced)
            .finish()
    }
}
