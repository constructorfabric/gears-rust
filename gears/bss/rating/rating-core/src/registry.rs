//! `EngineRegistry`: dispatch by engine generation, never substitution (DESIGN 01 §3.2, §4.2).

use std::collections::BTreeMap;

use crate::engine::gen_1;
use crate::{ArithmeticBudget, EngineId, EvaluationError, EvaluationInput, EvaluationOutcome};

/// One engine generation: every operation of the evaluation contract is a method of one
/// generation (T-D-65).
pub trait Engine: Send + Sync {
    /// The generation and the digest of its frozen tree, as results record them.
    fn id(&self) -> EngineId;

    /// The arithmetic budget the generation computes under.
    fn budget(&self) -> ArithmeticBudget;

    /// Evaluates one child input.
    ///
    /// # Errors
    ///
    /// An [`EvaluationError`] of the generation's closed taxonomy.
    fn evaluate(&self, input: &EvaluationInput) -> Result<EvaluationOutcome, EvaluationError>;
}

/// Generation 1 (`engine::gen_1`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Gen1;

impl Engine for Gen1 {
    fn id(&self) -> EngineId {
        gen_1::engine_id()
    }

    fn budget(&self) -> ArithmeticBudget {
        gen_1::ArithmeticBudget::GEN_1
    }

    fn evaluate(&self, input: &EvaluationInput) -> Result<EvaluationOutcome, EvaluationError> {
        gen_1::evaluate(input)
    }
}

/// Why a set of engines is not a registry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// Two engines claim one generation.
    #[error("engine generation {0} is registered twice")]
    DuplicateGeneration(u32),
    /// A retained generation is not older than the current one.
    #[error("retained generation {retained} is not older than current generation {current}")]
    RetainedNotOlder { retained: u32, current: u32 },
}

/// The engine generations compiled into a build, by `engine_generation`.
///
/// The current generation evaluates new work; the retained ones evaluate corrections of the
/// children whose engine-of-record they are, re-rates that target them, and replays. A requested
/// generation that is not here, or whose digest differs from the one a result recorded, is
/// `engine_unavailable`; the registry never substitutes another generation.
pub struct EngineRegistry {
    current: Box<dyn Engine>,
    retained: BTreeMap<u32, Box<dyn Engine>>,
}

impl EngineRegistry {
    // @cpt-begin:cpt-cf-bss-rating-algo-core-engine-generations:p1:inst-eg-retain
    /// Every generation compiled into this build; the newest is current.
    #[must_use]
    pub fn compiled() -> Self {
        Self {
            current: Box::new(Gen1),
            retained: BTreeMap::new(),
        }
    }

    // @cpt-end:cpt-cf-bss-rating-algo-core-engine-generations:p1:inst-eg-retain

    /// A registry of `current` and the `retained` generations before it. The gear uses
    /// [`Self::compiled`]; this constructor exists for replay tests across upgrades.
    ///
    /// # Errors
    ///
    /// [`RegistryError`] when two engines share a generation or a retained one is not older.
    pub fn from_engines(
        current: Box<dyn Engine>,
        retained: Vec<Box<dyn Engine>>,
    ) -> Result<Self, RegistryError> {
        let newest = current.id().engine_generation;
        let mut by_generation = BTreeMap::new();
        for engine in retained {
            let generation = engine.id().engine_generation;
            if generation == newest || by_generation.contains_key(&generation) {
                return Err(RegistryError::DuplicateGeneration(generation));
            }
            if generation > newest {
                return Err(RegistryError::RetainedNotOlder {
                    retained: generation,
                    current: newest,
                });
            }
            by_generation.insert(generation, engine);
        }
        Ok(Self {
            current,
            retained: by_generation,
        })
    }

    /// The engine of `engine_generation`, if it is compiled in.
    #[must_use]
    pub fn get(&self, engine_generation: u32) -> Option<&dyn Engine> {
        if self.current.id().engine_generation == engine_generation {
            return Some(self.current.as_ref());
        }
        self.retained.get(&engine_generation).map(AsRef::as_ref)
    }

    /// The generation new work is evaluated with.
    #[must_use]
    pub fn current(&self) -> &dyn Engine {
        self.current.as_ref()
    }

    /// Every compiled-in generation with its digest, oldest first: what start-up verifies against
    /// `bss_rating__engine_generation` before the rater runs.
    #[must_use]
    pub fn generations(&self) -> Vec<EngineId> {
        self.retained
            .values()
            .map(|engine| engine.id())
            .chain(std::iter::once(self.current.id()))
            .collect()
    }

    /// The engine a result recorded: its generation must be compiled in with the same digest.
    ///
    /// # Errors
    ///
    /// `engine_unavailable` when the generation is not compiled in or its digest differs.
    // @cpt-begin:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-registry
    pub fn engine_for(&self, recorded: &EngineId) -> Result<&dyn Engine, EvaluationError> {
        let engine = self.get(recorded.engine_generation).ok_or_else(|| {
            EvaluationError::EngineUnavailable {
                detail: format!(
                    "engine generation {} is not compiled into this build; deploy a build \
                         that contains it (never substituted)",
                    recorded.engine_generation
                ),
            }
        })?;
        let compiled = engine.id();
        if compiled.engine_digest != recorded.engine_digest {
            return Err(EvaluationError::EngineUnavailable {
                detail: format!(
                    "engine generation {} is compiled in with digest {}, not the recorded {}",
                    recorded.engine_generation, compiled.engine_digest, recorded.engine_digest
                ),
            });
        }
        Ok(engine)
    }
    // @cpt-end:cpt-cf-bss-rating-flow-core-evaluate:p1:inst-ev-registry

    /// Evaluates `input` with the generation it names.
    ///
    /// # Errors
    ///
    /// `engine_unavailable` as [`Self::engine_for`], else the generation's own error.
    pub fn evaluate(&self, input: &EvaluationInput) -> Result<EvaluationOutcome, EvaluationError> {
        self.engine_for(&input.engine)?.evaluate(input)
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
