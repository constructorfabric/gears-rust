//! Typed composition of persistent sequential and parallel stages.
use crate::contracts::{
    ActivityDefinition, ActivityInput, DataFlowSpec, ErasedActivity, ExecutionContract,
    ExecutionDefinition, InputSource,
};
use crate::{ActivityContext, ActivityError, ActivityId, DefinitionError, RetryPolicy};
use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    future::Future,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

/// Serializable values may cross process restarts; Rust handlers never do.
pub trait Payload: Serialize + DeserializeOwned + Send + Sync + 'static {}
impl<T: Serialize + DeserializeOwned + Send + Sync + 'static> Payload for T {}

#[async_trait]
pub trait Activity: Send + Sync + 'static {
    type Input: Payload;
    type Output: Payload;
    async fn execute(
        &self,
        context: StepContext,
        input: Self::Input,
    ) -> Result<Self::Output, ActivityError>;
}

pub struct StepContext {
    activity: ActivityContext,
    input: ActivityInput,
    dependencies: Vec<InputSource>,
    workflow: u64,
}
impl std::ops::Deref for StepContext {
    type Target = ActivityContext;
    fn deref(&self) -> &Self::Target {
        &self.activity
    }
}
impl StepContext {
    /// Only dependencies declared with `Step::uses` may be read.
    /// # Errors
    /// Returns a permanent activity error for undeclared or undecodable checkpoints.
    pub fn checkpoint<T: Payload>(&self, step: &StepRef<T>) -> Result<T, ActivityError> {
        if self.workflow != step.workflow || !self.dependencies.contains(&step.source) {
            return Err(ActivityError::permanent("undeclared_checkpoint"));
        }
        serde_json::from_value(step.source.read(&self.input)?)
            .map_err(|_| ActivityError::permanent("checkpoint_decode_failed"))
    }
}

#[derive(Clone)]
struct Dependency {
    workflow: u64,
    source: InputSource,
}
/// Typed reference to a saved step or a parallel stage's results.
pub struct StepRef<T> {
    workflow: u64,
    name: String,
    source: InputSource,
    marker: PhantomData<fn() -> T>,
}
impl<T> Clone for StepRef<T> {
    fn clone(&self) -> Self {
        Self {
            workflow: self.workflow,
            name: self.name.clone(),
            source: self.source.clone(),
            marker: PhantomData,
        }
    }
}
impl<T> StepRef<T> {
    #[must_use]
    pub fn workflow_name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub fn source(&self) -> &InputSource {
        &self.source
    }
}

impl<T> StepRef<Vec<T>> {
    /// References to the checkpoints of a dynamic parallel stage, in declaration order.
    /// Returns `None` for a sequential step whose output happens to be a vector.
    #[must_use]
    pub fn branches(&self) -> Option<Vec<StepRef<T>>> {
        let InputSource::List(sources) = &self.source else {
            return None;
        };
        Some(
            sources
                .iter()
                .map(|source| self.branch(source.clone()))
                .collect(),
        )
    }
}
impl<T> StepRef<T> {
    fn branch<U>(&self, source: InputSource) -> StepRef<U> {
        StepRef {
            workflow: self.workflow,
            name: self.name.clone(),
            source,
            marker: PhantomData,
        }
    }
}
macro_rules! tuple_checkpoint_refs {
    ($(($($t:ident:$n:tt),+)),+) => {$(
        impl<$($t),+> StepRef<($($t,)+)> {
            /// References to the checkpoints of a tuple parallel stage, in declaration order.
            /// Returns `None` for a sequential step whose output happens to be a tuple.
            #[must_use]
            pub fn branches(&self) -> Option<($(StepRef<$t>,)+)> {
                let InputSource::Tuple(sources) = &self.source else { return None; };
                Some(($(self.branch(sources.get($n)?.clone()),)+))
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

type Handler<I, O> = dyn Fn(StepContext, I) -> std::pin::Pin<Box<dyn Future<Output = Result<O, ActivityError>> + Send>>
    + Send
    + Sync;
pub struct Step<I, O> {
    id: String,
    handler: Arc<Handler<I, O>>,
    timeout: Duration,
    retry: RetryPolicy,
    dependencies: Vec<Dependency>,
}
impl<I: Payload, O: Payload> Step<I, O> {
    pub fn new<F, Fut>(id: impl Into<String>, handler: F) -> Self
    where
        F: Fn(StepContext, I) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O, ActivityError>> + Send + 'static,
    {
        Self {
            id: id.into(),
            handler: Arc::new(move |ctx, input| Box::pin(handler(ctx, input))),
            timeout: Duration::ZERO,
            retry: RetryPolicy::default(),
            dependencies: vec![],
        }
    }
    pub fn activity<A: Activity<Input = I, Output = O>>(
        id: impl Into<String>,
        activity: A,
    ) -> Self {
        let activity = Arc::new(activity);
        Self::new(id, move |ctx, input| {
            let activity = activity.clone();
            async move { activity.execute(ctx, input).await }
        })
    }
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    #[must_use]
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }
    #[must_use]
    pub fn uses<T>(mut self, step: &StepRef<T>) -> Self {
        self.dependencies.push(Dependency {
            workflow: step.workflow,
            source: step.source.clone(),
        });
        self
    }
    fn erase(self, source: InputSource, workflow: u64) -> Part {
        let dependencies = self.dependencies.clone();
        Part {
            source: source.clone(),
            definition: ActivityDefinition {
                id: ActivityId(self.id),
                timeout: self.timeout,
                retry: self.retry,
                handler: Arc::new(Adapter {
                    handler: self.handler,
                    source,
                    workflow,
                    dependencies: dependencies.iter().map(|d| d.source.clone()).collect(),
                }),
            },
            dependencies,
        }
    }
}
struct Adapter<I, O> {
    handler: Arc<Handler<I, O>>,
    source: InputSource,
    workflow: u64,
    dependencies: Vec<InputSource>,
}
#[async_trait]
impl<I: Payload, O: Payload> ErasedActivity for Adapter<I, O> {
    async fn execute(
        &self,
        context: ActivityContext,
        input: ActivityInput,
    ) -> Result<Value, ActivityError> {
        let value = serde_json::from_value(self.source.read(&input)?)
            .map_err(|_| ActivityError::permanent("activity_input_decode_failed"))?;
        let context = StepContext {
            activity: context,
            input,
            dependencies: self.dependencies.clone(),
            workflow: self.workflow,
        };
        let result = (self.handler)(context, value).await?;
        serde_json::to_value(result)
            .map_err(|_| ActivityError::permanent("activity_output_encode_failed"))
    }
}
#[doc(hidden)]
pub struct Part {
    source: InputSource,
    definition: ActivityDefinition,
    dependencies: Vec<Dependency>,
}

mod sealed {
    use super::{InputSource, Part, Payload, Step};
    pub trait Parallel<I> {
        type Output: Payload;
        fn parts(self, source: InputSource, workflow: u64) -> (Vec<Part>, bool);
    }
    impl<I: Payload, O: Payload> Parallel<I> for Vec<Step<I, O>> {
        type Output = Vec<O>;
        fn parts(self, source: InputSource, workflow: u64) -> (Vec<Part>, bool) {
            (
                self.into_iter()
                    .map(|s| s.erase(source.clone(), workflow))
                    .collect(),
                false,
            )
        }
    }
    macro_rules! tuples {
        ($(($($o:ident:$n:tt),+)),+) => {$(
            impl<I: Payload, $($o: Payload),+> Parallel<I> for ($(Step<I, $o>,)+) {
                type Output = ($($o,)+);
                fn parts(self, source: InputSource, workflow: u64) -> (Vec<Part>, bool) { (vec![$(self.$n.erase(source.clone(), workflow),)+], true) }
            }
        )+};
    }
    tuples!((A:0,B:1),(A:0,B:1,C:2),(A:0,B:1,C:2,D:3),(A:0,B:1,C:2,D:3,E:4),(A:0,B:1,C:2,D:3,E:4,F:5),(A:0,B:1,C:2,D:3,E:4,F:5,G:6),(A:0,B:1,C:2,D:3,E:4,F:5,G:6,H:7));
}
/// A tuple of 2..=8 branches, or a dynamic nonempty list of homogeneous branches.
pub trait ParallelSteps<I>: sealed::Parallel<I> {}
impl<I, T: sealed::Parallel<I>> ParallelSteps<I> for T {}

pub struct WorkflowBuilder<I, O = I> {
    name: String,
    identity: u64,
    activities: Vec<ActivityDefinition>,
    groups: Vec<Vec<ActivityId>>,
    flow: DataFlowSpec,
    invalid: bool,
    marker: PhantomData<fn(I) -> O>,
}
impl<Root: Payload> WorkflowBuilder<Root, Root> {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
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
            invalid: false,
            marker: PhantomData,
        }
    }
}
impl<I: Payload, O: Payload> WorkflowBuilder<I, O> {
    #[must_use]
    pub fn checkpoint(&self) -> StepRef<O> {
        StepRef {
            workflow: self.identity,
            name: self.name.clone(),
            source: self.flow.output.clone(),
            marker: PhantomData,
        }
    }
    #[must_use]
    pub fn then<N: Payload>(mut self, step: Step<O, N>) -> WorkflowBuilder<I, N> {
        let part = step.erase(self.flow.output.clone(), self.identity);
        self.flow.output = InputSource::Checkpoint(part.definition.id.0.clone());
        self.append(part);
        self.cast()
    }
    #[must_use]
    pub fn parallel<P: ParallelSteps<O>>(
        mut self,
        steps: P,
    ) -> WorkflowBuilder<I, <P as sealed::Parallel<O>>::Output> {
        let (parts, tuple) = steps.parts(self.flow.output.clone(), self.identity);
        self.invalid |= parts.is_empty();
        let ids: Vec<_> = parts.iter().map(|p| p.definition.id.clone()).collect();
        if ids.len() > 1 {
            self.groups.push(ids.clone());
        }
        let sources = ids
            .into_iter()
            .map(|id| InputSource::Checkpoint(id.0))
            .collect();
        self.flow.output = if tuple {
            InputSource::Tuple(sources)
        } else {
            InputSource::List(sources)
        };
        for part in parts {
            self.append(part);
        }
        self.cast()
    }
    fn append(&mut self, part: Part) {
        self.invalid |= part
            .dependencies
            .iter()
            .any(|d| d.workflow != self.identity || matches!(d.source, InputSource::RunInput));
        let id = part.definition.id.0.clone();
        self.flow.inputs.insert(id.clone(), part.source);
        self.flow.dependencies.insert(
            id,
            part.dependencies.into_iter().map(|d| d.source).collect(),
        );
        self.activities.push(part.definition);
    }
    fn cast<N: Payload>(self) -> WorkflowBuilder<I, N> {
        WorkflowBuilder {
            name: self.name,
            identity: self.identity,
            activities: self.activities,
            groups: self.groups,
            flow: self.flow,
            invalid: self.invalid,
            marker: PhantomData,
        }
    }
    /// # Errors
    /// Rejects invalid IDs, policies, stages or checkpoint dependencies.
    pub fn build(self) -> Result<Workflow<I, O>, DefinitionError> {
        if self.invalid {
            return Err(DefinitionError::new(
                "invalid checkpoint dependency or empty parallel stage",
            ));
        }
        let definition = ExecutionDefinition {
            name: self.name,
            activities: self.activities,
            parallel_groups: self.groups,
            flow: Some(self.flow),
        };
        definition.validate()?;
        Ok(Workflow {
            definition,
            marker: PhantomData,
        })
    }
}
/// Local bindings plus the serializable contract; no running tasks are owned here.
pub struct Workflow<I, O> {
    definition: ExecutionDefinition,
    marker: PhantomData<fn(I) -> O>,
}
impl<I, O> Workflow<I, O> {
    #[must_use]
    pub fn contract(&self) -> ExecutionContract {
        self.definition.contract()
    }
    #[must_use]
    pub fn reference(&self) -> WorkflowRef<I, O> {
        WorkflowRef {
            contract: self.contract(),
            marker: PhantomData,
        }
    }
    #[must_use]
    pub fn into_definition(self) -> ExecutionDefinition {
        self.definition
    }
}
pub struct WorkflowRef<I, O> {
    contract: ExecutionContract,
    marker: PhantomData<fn(I) -> O>,
}
impl<I, O> Clone for WorkflowRef<I, O> {
    fn clone(&self) -> Self {
        Self {
            contract: self.contract.clone(),
            marker: PhantomData,
        }
    }
}
impl<I, O> WorkflowRef<I, O> {
    /// The caller owns the mapping between serialized payloads and Rust types.
    /// # Errors
    /// Returns the contract validation error before creating the reference.
    pub fn from_contract(contract: ExecutionContract) -> Result<Self, DefinitionError> {
        contract.validate()?;
        Ok(Self {
            contract,
            marker: PhantomData,
        })
    }
    #[must_use]
    pub fn contract(&self) -> &ExecutionContract {
        &self.contract
    }
    #[must_use]
    pub fn name(&self) -> &str {
        &self.contract.name
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/workflow_tests.rs"]
mod tests;
