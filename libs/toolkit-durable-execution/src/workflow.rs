//! Typed composition of persistent sequential, parallel and fan-out stages.
use crate::authoring::{Compensation, FanOutOptions};
use crate::contracts::{
    ActivityDefinition, DataFlowSpec, DefinitionParts, ExecutionContract, ExecutionDefinition,
    InputSource,
};
use crate::runtime::{ActivityInput, ErasedActivity, ErasedCompensationBinding};
use crate::{
    ActivityContext, ActivityError, ActivityId, DefinitionError, DefinitionErrorKind, RetryPolicy,
    RunId,
};
use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

/// A value that may cross process restarts. Rust handlers never do.
pub trait Payload: Serialize + DeserializeOwned + Send + Sync + 'static {}
impl<T: Serialize + DeserializeOwned + Send + Sync + 'static> Payload for T {}

/// A typed checkpoint accepted by [`Step::with_deps`] and result reads.
pub trait CheckpointReference {
    /// The confirmed value stored as the checkpoint.
    type Output: Payload;
    /// Runtime selector and contract identity.
    fn as_step_ref(&self) -> &StepRef<Self::Output>;
}

/// Activity implemented by a type that owns its provider client.
///
/// Prefer this over closures for stateful steps: the step shares one instance
/// across invocations, so no per-call cloning is needed.
///
/// ```
/// use toolkit_durable_execution::prelude::*;
/// struct Greet;
/// impl Activity for Greet {
///     type Input = String;
///     type Output = String;
///     async fn execute(&self, _: ActivityContext, name: String) -> Result<String, ActivityError> {
///         Ok(format!("hello {name}"))
///     }
/// }
/// let step = Step::activity("greet", std::time::Duration::from_secs(5), Greet);
/// # let _ = step;
/// ```
pub trait Activity: Send + Sync + 'static {
    /// Invocation input, persisted for recovery and compensation.
    type Input: Payload;
    /// Confirmed output, persisted as the checkpoint.
    type Output: Payload;
    /// Execute an at-least-once invocation. Deduplicate effects with
    /// [`ActivityContext::idempotency_key`] and honor cancellation.
    ///
    /// # Errors
    /// Returns a safe retryable or permanent code, or confirmed cancellation.
    fn execute(
        &self,
        context: ActivityContext,
        input: Self::Input,
    ) -> impl Future<Output = Result<Self::Output, ActivityError>> + Send;
}

/// Sequential authoring state.
#[derive(Debug)]
pub struct Sequential;
/// Static parallel authoring state.
#[derive(Debug)]
pub struct Parallel {
    branches: Vec<InputSource>,
}
/// Per-run list expansion authoring state.
#[derive(Debug)]
pub struct FanOut {
    step: String,
}

/// Typed reference to one saved checkpoint.
pub struct StepRef<T> {
    workflow: u64,
    name: String,
    fingerprint: Arc<OnceLock<Arc<str>>>,
    source: InputSource,
    marker: PhantomData<fn() -> T>,
}
impl<T: Payload> CheckpointReference for StepRef<T> {
    type Output = T;
    fn as_step_ref(&self) -> &StepRef<T> {
        self
    }
}
impl<T> Clone for StepRef<T> {
    fn clone(&self) -> Self {
        self.with_source(self.source.clone())
    }
}
impl<T> StepRef<T> {
    /// Contract fingerprint of the owning workflow.
    ///
    /// # Errors
    /// [`DefinitionErrorKind::NotFinalized`] until the owning workflow is built.
    pub fn fingerprint(&self) -> Result<&str, DefinitionError> {
        self.fingerprint
            .get()
            .map(AsRef::as_ref)
            .ok_or_else(|| DefinitionError::new(DefinitionErrorKind::NotFinalized))
    }
    /// Versioned name of the owning workflow.
    #[must_use]
    pub fn workflow_name(&self) -> &str {
        &self.name
    }
    /// Serialized selector used for runtime reads.
    #[must_use]
    pub fn source(&self) -> &InputSource {
        &self.source
    }
    fn with_source<U>(&self, source: InputSource) -> StepRef<U> {
        StepRef {
            workflow: self.workflow,
            name: self.name.clone(),
            fingerprint: Arc::clone(&self.fingerprint),
            source,
            marker: PhantomData,
        }
    }
}
impl<T> std::fmt::Debug for StepRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepRef")
            .field("workflow", &self.name)
            .field("source", &self.source)
            .field("finalized", &self.fingerprint.get().is_some())
            .finish()
    }
}

/// A static parallel stage whose branch shape is known at build time.
pub struct ParallelStageRef<T> {
    base: StepRef<T>,
    branches: Vec<InputSource>,
}
impl<T: Payload> CheckpointReference for ParallelStageRef<T> {
    type Output = T;
    fn as_step_ref(&self) -> &StepRef<T> {
        &self.base
    }
}
impl<T> Clone for ParallelStageRef<T> {
    fn clone(&self) -> Self {
        Self {
            base: self.base.clone(),
            branches: self.branches.clone(),
        }
    }
}
impl<T> std::fmt::Debug for ParallelStageRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ParallelStageRef").field(&self.base).finish()
    }
}
impl<T: Payload> ParallelStageRef<Vec<T>> {
    /// Checkpoints of a list parallel stage, in declaration order.
    #[must_use]
    pub fn branches(&self) -> Vec<StepRef<T>> {
        self.branches
            .iter()
            .map(|source| self.base.with_source(source.clone()))
            .collect()
    }
}
macro_rules! tuple_checkpoint_refs {
    ($(($($t:ident:$n:tt),+)),+) => {$(
        impl<$($t: Payload),+> ParallelStageRef<($($t,)+)> {
            /// Checkpoints of a tuple parallel stage, in declaration order.
            #[must_use]
            pub fn branches(&self) -> ($(StepRef<$t>,)+) {
                ($(self.base.with_source(self.branches[$n].clone()),)+)
            }
        }
    )+};
}
tuple_checkpoint_refs!(
    (A:0, B:1),
    (A:0, B:1, C:2),
    (A:0, B:1, C:2, D:3),
    (A:0, B:1, C:2, D:3, E:4),
    (A:0, B:1, C:2, D:3, E:4, F:5),
    (A:0, B:1, C:2, D:3, E:4, F:5, G:6),
    (A:0, B:1, C:2, D:3, E:4, F:5, G:6, H:7)
);

/// A fan-out stage whose ordered branch set is determined by each run's data.
pub struct FanOutStageRef<T> {
    base: StepRef<Vec<T>>,
    step: String,
}
impl<T: Payload> CheckpointReference for FanOutStageRef<T> {
    type Output = Vec<T>;
    fn as_step_ref(&self) -> &StepRef<Vec<T>> {
        &self.base
    }
}
impl<T> Clone for FanOutStageRef<T> {
    fn clone(&self) -> Self {
        Self {
            base: self.base.clone(),
            step: self.step.clone(),
        }
    }
}
impl<T> std::fmt::Debug for FanOutStageRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FanOutStageRef")
            .field("base", &self.base)
            .field("step", &self.step)
            .finish()
    }
}
impl<T> FanOutStageRef<T> {
    /// Select one branch; result reads verify that this index exists in the run.
    #[must_use]
    pub fn branch(&self, index: u32) -> StepRef<T> {
        self.base.with_source(InputSource::FanOutBranch {
            step: self.step.clone(),
            index,
        })
    }
}

use sealed::Dependency;

/// Earlier checkpoints read by a step: one reference or a tuple of up to four.
pub trait Dependencies: sealed::Dependencies {}
impl<T: sealed::Dependencies> Dependencies for T {}

type Reader<V> = fn(&[InputSource], &ActivityInput) -> Result<V, ActivityError>;
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type Handler<I, O> =
    dyn Fn(ActivityContext, I, &ActivityInput) -> BoxFuture<Result<O, ActivityError>> + Send + Sync;

/// A typed handler with its serializable forward and optional reverse policies.
#[must_use = "add the step to a workflow builder"]
pub struct Step<I, O> {
    id: String,
    timeout: Duration,
    retry: RetryPolicy,
    compensation: Option<ErasedCompensationBinding>,
    handler: Arc<Handler<I, O>>,
    dependencies: Vec<Dependency>,
}
impl<I: Payload, O: Payload> Step<I, O> {
    /// A step running `handler` with a timeout validated at build as positive and
    /// at most one day.
    pub fn new<F, Fut>(id: impl Into<String>, timeout: Duration, handler: F) -> Self
    where
        F: Fn(ActivityContext, I) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O, ActivityError>> + Send + 'static,
    {
        Self::from_handler(
            id,
            timeout,
            Vec::new(),
            move |ctx, input, _| -> BoxFuture<Result<O, ActivityError>> {
                Box::pin(handler(ctx, input))
            },
        )
    }

    /// A step that also receives earlier checkpoints, decoded before the handler runs.
    ///
    /// `deps` is one reference or a tuple of up to four. Build rejects references
    /// to other workflows, the run input, or the current or later stages.
    ///
    /// ```
    /// use toolkit_durable_execution::prelude::*;
    /// use std::time::Duration;
    /// let builder = WorkflowBuilder::<u32>::new("docs.deps.v1")
    ///     .then(Step::new("double", Duration::from_secs(5), |_, n: u32| async move { Ok(n * 2) }));
    /// let doubled = builder.checkpoint();
    /// let workflow = builder
    ///     .then(Step::new("render", Duration::from_secs(5), |_, n: u32| async move { Ok(n.to_string()) }))
    ///     .then(Step::with_deps("join", Duration::from_secs(5), &doubled, |_, text: String, doubled: u32| async move {
    ///         Ok(format!("{text}/{doubled}"))
    ///     }))
    ///     .build()?;
    /// # let _ = workflow;
    /// # Ok::<(), toolkit_durable_execution::DefinitionError>(())
    /// ```
    pub fn with_deps<D, F, Fut>(
        id: impl Into<String>,
        timeout: Duration,
        deps: D,
        handler: F,
    ) -> Self
    where
        D: Dependencies,
        F: Fn(ActivityContext, I, D::Values) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O, ActivityError>> + Send + 'static,
    {
        Self::with_reader(id, timeout, deps.dependencies(), D::read, handler)
    }

    // Generic over the decoded values only, so the handler closure stays `'static`
    // when `deps` borrows its references.
    fn with_reader<V, F, Fut>(
        id: impl Into<String>,
        timeout: Duration,
        dependencies: Vec<Dependency>,
        read: Reader<V>,
        handler: F,
    ) -> Self
    where
        V: Send + 'static,
        F: Fn(ActivityContext, I, V) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O, ActivityError>> + Send + 'static,
    {
        let sources: Vec<_> = dependencies.iter().map(|d| d.source.clone()).collect();
        Self::from_handler(
            id,
            timeout,
            dependencies,
            move |ctx, input, raw| -> BoxFuture<Result<O, ActivityError>> {
                match read(&sources, raw) {
                    Ok(values) => Box::pin(handler(ctx, input, values)),
                    Err(error) => Box::pin(std::future::ready(Err(error))),
                }
            },
        )
    }

    /// A step backed by an [`Activity`] implementation.
    pub fn activity<A: Activity<Input = I, Output = O>>(
        id: impl Into<String>,
        timeout: Duration,
        activity: A,
    ) -> Self {
        let activity = Arc::new(activity);
        Self::new(id, timeout, move |ctx, input| {
            let activity = Arc::clone(&activity);
            async move { activity.execute(ctx, input).await }
        })
    }

    fn from_handler<F>(
        id: impl Into<String>,
        timeout: Duration,
        dependencies: Vec<Dependency>,
        handler: F,
    ) -> Self
    where
        F: Fn(ActivityContext, I, &ActivityInput) -> BoxFuture<Result<O, ActivityError>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            id: id.into(),
            timeout,
            retry: RetryPolicy::default(),
            compensation: None,
            handler: Arc::new(handler),
            dependencies,
        }
    }

    /// Set the automatic forward retry budget and backoff.
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Reverse a confirmed successful invocation from its original input and output.
    pub fn compensate(mut self, compensation: Compensation<I, O>) -> Self {
        self.compensation = Some(compensation.erase());
        self
    }

    fn erase(self, source: InputSource, fanout: Option<FanOutOptions>) -> sealed::Part {
        sealed::Part {
            source: source.clone(),
            definition: ActivityDefinition {
                compensation: self.compensation,
                fanout,
                id: ActivityId(self.id),
                timeout: self.timeout,
                retry: self.retry,
                handler: Arc::new(Adapter {
                    handler: self.handler,
                    source,
                }),
            },
            dependencies: self.dependencies,
        }
    }
}
impl<I, O> std::fmt::Debug for Step<I, O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Step")
            .field("id", &self.id)
            .field("timeout", &self.timeout)
            .field("retry", &self.retry)
            .field("compensation", &self.compensation)
            .finish_non_exhaustive()
    }
}

struct Adapter<I, O> {
    handler: Arc<Handler<I, O>>,
    source: InputSource,
}
#[async_trait]
impl<I: Payload, O: Payload> ErasedActivity for Adapter<I, O> {
    async fn execute(
        &self,
        context: ActivityContext,
        input: ActivityInput,
    ) -> Result<Value, ActivityError> {
        let selected = match &input.selected_input {
            Some(selected) => selected.clone(),
            None => self.source.read(&input)?,
        };
        let value = serde_json::from_value(selected)
            .map_err(|_| ActivityError::permanent("activity_input_decode_failed"))?;
        let result = (self.handler)(context, value, &input).await?;
        serde_json::to_value(result)
            .map_err(|_| ActivityError::permanent("activity_output_encode_failed"))
    }
}

mod sealed {
    use super::{
        ActivityDefinition, ActivityError, ActivityInput, CheckpointReference, InputSource,
        Payload, Step,
    };

    /// An earlier checkpoint declared by a step.
    #[derive(Clone)]
    pub struct Dependency {
        pub(super) workflow: u64,
        pub(super) source: InputSource,
    }

    /// Erased authoring part produced by a step.
    pub struct Part {
        pub(super) source: InputSource,
        pub(super) definition: ActivityDefinition,
        pub(super) dependencies: Vec<Dependency>,
    }

    pub trait Parallel<I> {
        type Output: Payload;
        /// Erased branches and whether the output is a tuple.
        fn parts(self, source: &InputSource) -> (Vec<Part>, bool);
    }
    impl<I: Payload, O: Payload> Parallel<I> for Vec<Step<I, O>> {
        type Output = Vec<O>;
        fn parts(self, source: &InputSource) -> (Vec<Part>, bool) {
            (
                self.into_iter()
                    .map(|s| s.erase(source.clone(), None))
                    .collect(),
                false,
            )
        }
    }
    macro_rules! tuples {
        ($(($($o:ident:$n:tt),+)),+) => {$(
            impl<I: Payload, $($o: Payload),+> Parallel<I> for ($(Step<I, $o>,)+) {
                type Output = ($($o,)+);
                fn parts(self, source: &InputSource) -> (Vec<Part>, bool) {
                    (vec![$(self.$n.erase(source.clone(), None),)+], true)
                }
            }
        )+};
    }
    tuples!((A:0,B:1),(A:0,B:1,C:2),(A:0,B:1,C:2,D:3),(A:0,B:1,C:2,D:3,E:4),(A:0,B:1,C:2,D:3,E:4,F:5),(A:0,B:1,C:2,D:3,E:4,F:5,G:6),(A:0,B:1,C:2,D:3,E:4,F:5,G:6,H:7));

    pub trait Dependencies {
        /// Decoded checkpoint values passed to the handler.
        type Values: Send + 'static;
        fn dependencies(self) -> Vec<Dependency>;
        fn read(
            sources: &[InputSource],
            input: &ActivityInput,
        ) -> Result<Self::Values, ActivityError>;
    }
    fn decode<T: Payload>(
        source: Option<&InputSource>,
        input: &ActivityInput,
    ) -> Result<T, ActivityError> {
        let source = source.ok_or_else(|| ActivityError::permanent("checkpoint_missing"))?;
        serde_json::from_value(source.read(input)?)
            .map_err(|_| ActivityError::permanent("checkpoint_decode_failed"))
    }
    fn dependency<R: CheckpointReference>(reference: &R) -> Dependency {
        let step = reference.as_step_ref();
        Dependency {
            workflow: step.workflow,
            source: step.source.clone(),
        }
    }
    impl<R: CheckpointReference> Dependencies for &R {
        type Values = R::Output;
        fn dependencies(self) -> Vec<Dependency> {
            vec![dependency(self)]
        }
        fn read(
            sources: &[InputSource],
            input: &ActivityInput,
        ) -> Result<Self::Values, ActivityError> {
            decode(sources.first(), input)
        }
    }
    macro_rules! dependency_tuples {
        ($(($($r:ident:$n:tt),+)),+) => {$(
            impl<$($r: CheckpointReference),+> Dependencies for ($(&$r,)+) {
                type Values = ($($r::Output,)+);
                fn dependencies(self) -> Vec<Dependency> {
                    vec![$(dependency(self.$n),)+]
                }
                fn read(sources: &[InputSource], input: &ActivityInput) -> Result<Self::Values, ActivityError> {
                    Ok(($(decode::<$r::Output>(sources.get($n), input)?,)+))
                }
            }
        )+};
    }
    dependency_tuples!((A:0), (A:0, B:1), (A:0, B:1, C:2), (A:0, B:1, C:2, D:3));
}

/// Fixed branches: a tuple of 2..=8 steps, or a list of steps with one output type.
pub trait ParallelSteps<I>: sealed::Parallel<I> {}
impl<I, T: sealed::Parallel<I>> ParallelSteps<I> for T {}

/// Typed authoring chain. The stage marker `K` selects the checkpoint shape.
#[must_use = "a workflow builder does nothing until `build` is called"]
pub struct WorkflowBuilder<I, O = I, K = Sequential> {
    saga: bool,
    fingerprint: Arc<OnceLock<Arc<str>>>,
    name: String,
    identity: u64,
    activities: Vec<ActivityDefinition>,
    groups: Vec<Vec<ActivityId>>,
    flow: DataFlowSpec,
    error: Option<DefinitionError>,
    stage: K,
    marker: PhantomData<fn(I) -> O>,
}
impl<Root: Payload> WorkflowBuilder<Root, Root> {
    /// Begin a named, versioned contract such as `orders.reserve.v1`.
    pub fn new(name: impl Into<String>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            saga: false,
            fingerprint: Arc::new(OnceLock::new()),
            name: name.into(),
            identity: NEXT.fetch_add(1, Ordering::Relaxed),
            activities: vec![],
            groups: vec![],
            flow: DataFlowSpec {
                version: 1,
                inputs: BTreeMap::new(),
                dependencies: BTreeMap::new(),
                output: InputSource::RunInput,
            },
            error: None,
            stage: Sequential,
            marker: PhantomData,
        }
    }
}
impl<I: Payload, O: Payload> WorkflowBuilder<I, O, Sequential> {
    /// Reference the current sequential checkpoint.
    #[must_use]
    pub fn checkpoint(&self) -> StepRef<O> {
        self.checkpoint_ref()
    }
}
impl<I: Payload, O: Payload> WorkflowBuilder<I, O, Parallel> {
    /// Reference the current static parallel stage.
    #[must_use]
    pub fn checkpoint(&self) -> ParallelStageRef<O> {
        ParallelStageRef {
            base: self.checkpoint_ref(),
            branches: self.stage.branches.clone(),
        }
    }
}
impl<I: Payload, T: Payload> WorkflowBuilder<I, Vec<T>, FanOut> {
    /// Reference the current per-run expansion stage.
    #[must_use]
    pub fn checkpoint(&self) -> FanOutStageRef<T> {
        FanOutStageRef {
            base: self.checkpoint_ref(),
            step: self.stage.step.clone(),
        }
    }
}
impl<I: Payload, T: Payload, K> WorkflowBuilder<I, Vec<T>, K> {
    /// Run `step` once per list item as independently checkpointed branches.
    /// The output keeps input order; an empty list calls no handler.
    pub fn for_each<N: Payload>(
        mut self,
        step: Step<T, N>,
        options: FanOutOptions,
    ) -> WorkflowBuilder<I, Vec<N>, FanOut> {
        let part = step.erase(self.flow.output.clone(), Some(options));
        let id = part.definition.id.0.clone();
        self.flow.output = InputSource::FanOutResults(id.clone());
        self.append(part);
        self.cast(FanOut { step: id })
    }
}
impl<I: Payload, O: Payload, K> WorkflowBuilder<I, O, K> {
    fn checkpoint_ref<T>(&self) -> StepRef<T> {
        StepRef {
            workflow: self.identity,
            name: self.name.clone(),
            fingerprint: Arc::clone(&self.fingerprint),
            source: self.flow.output.clone(),
            marker: PhantomData,
        }
    }
    /// Append a sequential step consuming the current output.
    pub fn then<N: Payload>(mut self, step: Step<O, N>) -> WorkflowBuilder<I, N> {
        let part = step.erase(self.flow.output.clone(), None);
        self.flow.output = InputSource::Checkpoint(part.definition.id.0.clone());
        self.append(part);
        self.cast(Sequential)
    }
    /// Append fixed branches that each receive the current output; results join
    /// in declaration order.
    pub fn parallel<P: ParallelSteps<O>>(
        mut self,
        steps: P,
    ) -> WorkflowBuilder<I, P::Output, Parallel> {
        let (parts, tuple) = steps.parts(&self.flow.output);
        if parts.is_empty() {
            self.fail(DefinitionError::new(
                DefinitionErrorKind::InvalidParallelStage,
            ));
        }
        let ids: Vec<_> = parts.iter().map(|p| p.definition.id.clone()).collect();
        if ids.len() > 1 {
            self.groups.push(ids.clone());
        }
        let branches: Vec<_> = ids
            .into_iter()
            .map(|id| InputSource::Checkpoint(id.0))
            .collect();
        self.flow.output = if tuple {
            InputSource::Tuple(branches.clone())
        } else {
            InputSource::List(branches.clone())
        };
        for part in parts {
            self.append(part);
        }
        self.cast(Parallel { branches })
    }
    /// Compensate confirmed effects after a definitive forward failure or Abort.
    pub fn saga(mut self) -> Self {
        self.saga = true;
        self
    }
    fn fail(&mut self, error: DefinitionError) {
        self.error.get_or_insert(error);
    }
    fn append(&mut self, part: sealed::Part) {
        let id = part.definition.id.0.clone();
        if part
            .dependencies
            .iter()
            .any(|d| d.workflow != self.identity || d.source == InputSource::RunInput)
        {
            self.fail(DefinitionError::new(DefinitionErrorKind::InvalidDependency).for_step(&id));
        }
        self.flow.inputs.insert(id.clone(), part.source);
        self.flow.dependencies.insert(
            id,
            part.dependencies.into_iter().map(|d| d.source).collect(),
        );
        self.activities.push(part.definition);
    }
    fn cast<N: Payload, Next>(self, stage: Next) -> WorkflowBuilder<I, N, Next> {
        WorkflowBuilder {
            saga: self.saga,
            fingerprint: self.fingerprint,
            name: self.name,
            identity: self.identity,
            activities: self.activities,
            groups: self.groups,
            flow: self.flow,
            error: self.error,
            stage,
            marker: PhantomData,
        }
    }
    /// Validate the definition and finalize every checkpoint reference taken from it.
    ///
    /// # Errors
    /// Returns the first invalid name, step ID, policy, stage or dependency.
    pub fn build(self) -> Result<Workflow<I, O>, DefinitionError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let definition = DefinitionParts {
            saga: self.saga,
            name: self.name,
            activities: self.activities,
            parallel_groups: self.groups,
            flow: self.flow,
        }
        .build()?;
        let contract = definition.contract().clone();
        let fingerprint: Arc<str> = definition.fingerprint().into();
        self.fingerprint
            .set(Arc::clone(&fingerprint))
            .map_err(|_| DefinitionError::new(DefinitionErrorKind::AlreadyFinalized))?;
        Ok(Workflow {
            definition,
            reference: WorkflowRef {
                contract: Arc::new(contract),
                fingerprint,
                marker: PhantomData,
            },
        })
    }
}
impl<I, O, K: std::fmt::Debug> std::fmt::Debug for WorkflowBuilder<I, O, K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowBuilder")
            .field("name", &self.name)
            .field("saga", &self.saga)
            .field("activities", &self.activities)
            .field("stage", &self.stage)
            .finish_non_exhaustive()
    }
}

/// A validated contract plus process-local handlers. Owns no running tasks.
#[must_use = "register the workflow with `WorkflowRegistry::register_workflow`"]
pub struct Workflow<I, O> {
    /// Low-level definition, also read by the in-memory test runtime.
    pub(crate) definition: ExecutionDefinition,
    reference: WorkflowRef<I, O>,
}
impl<I, O> Workflow<I, O> {
    /// The serializable contract, without handlers or credentials.
    #[must_use]
    pub fn contract(&self) -> &ExecutionContract {
        self.reference.contract()
    }
    /// Typed contract identity used to start runs and read results.
    #[must_use]
    pub fn reference(&self) -> WorkflowRef<I, O> {
        self.reference.clone()
    }
    /// The internal definition with its handlers.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn into_definition(self) -> ExecutionDefinition {
        self.definition
    }
    /// Split into the typed reference and the internal definition.
    #[must_use]
    pub(crate) fn into_parts(self) -> (WorkflowRef<I, O>, ExecutionDefinition) {
        (self.reference, self.definition)
    }
}
impl<I, O> std::fmt::Debug for Workflow<I, O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workflow")
            .field("contract", self.contract())
            .finish()
    }
}

/// Expected versioned contract with a caller-owned Rust payload mapping. Cheap to clone.
pub struct WorkflowRef<I, O> {
    contract: Arc<ExecutionContract>,
    fingerprint: Arc<str>,
    marker: PhantomData<fn(I) -> O>,
}
impl<I, O> Clone for WorkflowRef<I, O> {
    fn clone(&self) -> Self {
        Self {
            contract: Arc::clone(&self.contract),
            fingerprint: Arc::clone(&self.fingerprint),
            marker: PhantomData,
        }
    }
}
impl<I, O> WorkflowRef<I, O> {
    /// Restore a handle for a saved run ID. Reads no storage and grants no access;
    /// every result read verifies this expected contract.
    pub fn attach(&self, id: RunId) -> crate::RunHandle<I, O> {
        crate::RunHandle {
            id,
            workflow: self.clone(),
        }
    }
    /// Reference a contract received from the workflow owner. The caller owns the
    /// mapping between the serialized payloads and `I`/`O`.
    ///
    /// # Errors
    /// Returns the contract validation error.
    pub fn from_contract(contract: ExecutionContract) -> Result<Self, DefinitionError> {
        let fingerprint = contract.fingerprint()?.into();
        Ok(Self {
            contract: Arc::new(contract),
            fingerprint,
            marker: PhantomData,
        })
    }
    /// The expected contract checked on every result read.
    #[must_use]
    pub fn contract(&self) -> &ExecutionContract {
        &self.contract
    }
    /// Versioned registration name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.contract.name
    }
    /// Contract fingerprint, computed once.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}
impl<I, O> std::fmt::Debug for WorkflowRef<I, O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowRef")
            .field("name", &self.name())
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/workflow_tests.rs"]
mod tests;
