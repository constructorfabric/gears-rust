use std::time::Duration;

/// When a [`CircuitBreaker`](super::CircuitBreaker) opens, how long it stays
/// open, and how many probes it lets through afterwards.
///
/// Defaults: enabled, opens when at least half of at least 10 calls in the
/// last 30 seconds failed, stays open for 10 seconds, then lets one probe
/// through. [`with_consecutive_failures`](Self::with_consecutive_failures)
/// switches the trip condition to "k failures in a row".
///
/// With the `serde` feature the config deserializes from an object where every
/// key is optional and durations are humantime strings:
/// `{ failure_ratio: 0.5, minimum_calls: 10, window: "30s", open_for: "10s",
/// half_open_calls: 1 }`. Unknown keys and invalid values are rejected.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "RawConfig"))]
pub struct CircuitBreakerConfig {
    enabled: bool,
    failure_ratio: f64,
    minimum_calls: u32,
    window: Duration,
    open_for: Duration,
    half_open_calls: u32,
    consecutive_failures: Option<u32>,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            failure_ratio: 0.5,
            minimum_calls: 10,
            window: Duration::from_secs(30),
            open_for: Duration::from_secs(10),
            half_open_calls: 1,
            consecutive_failures: None,
        }
    }
}

/// A [`CircuitBreakerConfig`] value that cannot work.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// `failure_ratio` is outside `(0, 1]`.
    #[error("failure_ratio must be in (0, 1], got {0}")]
    FailureRatio(f64),
    /// A count or duration that must be positive is zero.
    #[error("{0} must be greater than zero")]
    Zero(&'static str),
}

impl CircuitBreakerConfig {
    /// A config that never rejects a call and records nothing.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// Turn the breaker on or off.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Share of failed calls in the window at which the breaker opens.
    #[must_use]
    pub fn with_failure_ratio(mut self, ratio: f64) -> Self {
        self.failure_ratio = ratio;
        self
    }

    /// Calls the window must hold before the failure ratio is evaluated.
    #[must_use]
    pub fn with_minimum_calls(mut self, calls: u32) -> Self {
        self.minimum_calls = calls;
        self
    }

    /// How far back the failure ratio looks.
    #[must_use]
    pub fn with_window(mut self, window: Duration) -> Self {
        self.window = window;
        self
    }

    /// How long the breaker rejects calls before it lets probes through.
    #[must_use]
    pub fn with_open_for(mut self, open_for: Duration) -> Self {
        self.open_for = open_for;
        self
    }

    /// Probes let through after `open_for`; all must succeed to close again.
    #[must_use]
    pub fn with_half_open_calls(mut self, calls: u32) -> Self {
        self.half_open_calls = calls;
        self
    }

    /// Open after `k` failures in a row instead of on the failure ratio.
    #[must_use]
    pub fn with_consecutive_failures(mut self, k: u32) -> Self {
        self.consecutive_failures = Some(k);
        self
    }

    /// Whether the breaker is on.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// See [`with_failure_ratio`](Self::with_failure_ratio).
    #[must_use]
    pub fn failure_ratio(&self) -> f64 {
        self.failure_ratio
    }

    /// See [`with_minimum_calls`](Self::with_minimum_calls).
    #[must_use]
    pub fn minimum_calls(&self) -> u32 {
        self.minimum_calls
    }

    /// See [`with_window`](Self::with_window).
    #[must_use]
    pub fn window(&self) -> Duration {
        self.window
    }

    /// See [`with_open_for`](Self::with_open_for).
    #[must_use]
    pub fn open_for(&self) -> Duration {
        self.open_for
    }

    /// See [`with_half_open_calls`](Self::with_half_open_calls).
    #[must_use]
    pub fn half_open_calls(&self) -> u32 {
        self.half_open_calls
    }

    /// See [`with_consecutive_failures`](Self::with_consecutive_failures).
    #[must_use]
    pub fn consecutive_failures(&self) -> Option<u32> {
        self.consecutive_failures
    }

    /// Check that every value can work.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for a ratio outside `(0, 1]` or a zero count or
    /// duration.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(self.failure_ratio > 0.0 && self.failure_ratio <= 1.0) {
            return Err(ConfigError::FailureRatio(self.failure_ratio));
        }
        if self.minimum_calls == 0 {
            return Err(ConfigError::Zero("minimum_calls"));
        }
        if self.window.is_zero() {
            return Err(ConfigError::Zero("window"));
        }
        if self.open_for.is_zero() {
            return Err(ConfigError::Zero("open_for"));
        }
        if self.half_open_calls == 0 {
            return Err(ConfigError::Zero("half_open_calls"));
        }
        if self.consecutive_failures == Some(0) {
            return Err(ConfigError::Zero("consecutive_failures"));
        }
        Ok(())
    }
}

#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    failure_ratio: Option<f64>,
    #[serde(default)]
    minimum_calls: Option<u32>,
    #[serde(default, with = "toolkit_utils::humantime_serde::option")]
    window: Option<Duration>,
    #[serde(default, with = "toolkit_utils::humantime_serde::option")]
    open_for: Option<Duration>,
    #[serde(default)]
    half_open_calls: Option<u32>,
    #[serde(default)]
    consecutive_failures: Option<u32>,
}

#[cfg(feature = "serde")]
impl TryFrom<RawConfig> for CircuitBreakerConfig {
    type Error = ConfigError;

    fn try_from(raw: RawConfig) -> Result<Self, Self::Error> {
        let d = Self::default();
        let config = Self {
            enabled: raw.enabled.unwrap_or(d.enabled),
            failure_ratio: raw.failure_ratio.unwrap_or(d.failure_ratio),
            minimum_calls: raw.minimum_calls.unwrap_or(d.minimum_calls),
            window: raw.window.unwrap_or(d.window),
            open_for: raw.open_for.unwrap_or(d.open_for),
            half_open_calls: raw.half_open_calls.unwrap_or(d.half_open_calls),
            consecutive_failures: raw.consecutive_failures,
        };
        config.validate()?;
        Ok(config)
    }
}
