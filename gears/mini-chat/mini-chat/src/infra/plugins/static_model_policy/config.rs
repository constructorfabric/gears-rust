use mini_chat_sdk::{KillSwitches, ModelCatalogEntry, TierLimits};
use serde::Deserialize;

/// Plugin configuration.
///
/// `model_catalog` key is required during deserialization (no `#[serde(default)]`),
/// but an empty list is valid — the plugin operates with zero models.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticMiniChatPolicyPluginConfig {
    /// Vendor name for GTS instance registration.
    #[serde(default = "default_vendor")]
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    #[serde(default = "default_priority")]
    pub priority: i16,

    /// Static model catalog entries.
    pub model_catalog: Vec<ModelCatalogEntry>,

    /// Static kill switches (all disabled by default). A missing field is
    /// `false`; an unknown field is a config error, so a misspelled switch
    /// does not silently stay off.
    #[serde(default, deserialize_with = "strict_kill_switches")]
    pub kill_switches: KillSwitches,

    /// Static per-user tier limits (used for all users).
    #[serde(default = "default_standard_limits")]
    pub default_standard_limits: TierLimits,
    #[serde(default = "default_premium_limits")]
    pub default_premium_limits: TierLimits,
}

/// Operator-facing mirror of [`KillSwitches`]: every field may be omitted
/// (a missing one is `false`), and an unknown key is a config error.
#[allow(clippy::struct_excessive_bools)]
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct StrictKillSwitches {
    disable_premium_tier: bool,
    force_standard_tier: bool,
    disable_web_search: bool,
    disable_file_search: bool,
    disable_images: bool,
    disable_code_interpreter: bool,
}

fn strict_kill_switches<'de, D>(deserializer: D) -> Result<KillSwitches, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = StrictKillSwitches::deserialize(deserializer)?;
    Ok(KillSwitches {
        disable_premium_tier: s.disable_premium_tier,
        force_standard_tier: s.force_standard_tier,
        disable_web_search: s.disable_web_search,
        disable_file_search: s.disable_file_search,
        disable_images: s.disable_images,
        disable_code_interpreter: s.disable_code_interpreter,
    })
}

impl StaticMiniChatPolicyPluginConfig {
    /// Every catalog entry must price usage: both credit multipliers in
    /// `1..=MAX_MULT` (a zero multiplier would make usage free). Its
    /// `bytes_per_token_conservative` must be > 0: the estimator would clamp
    /// a zero to 1 and overestimate every request about fourfold.
    pub fn validate(&self) -> Result<(), String> {
        use crate::domain::service::credit_arithmetic::MAX_MULT;
        for entry in &self.model_catalog {
            if entry.estimation_budgets.bytes_per_token_conservative == 0 {
                return Err(format!(
                    "model '{}': estimation_budgets.bytes_per_token_conservative must be > 0",
                    entry.id
                ));
            }
            for (name, value) in [
                (
                    "input_tokens_credit_multiplier_micro",
                    entry.input_tokens_credit_multiplier_micro,
                ),
                (
                    "output_tokens_credit_multiplier_micro",
                    entry.output_tokens_credit_multiplier_micro,
                ),
            ] {
                if value == 0 || value > MAX_MULT {
                    return Err(format!(
                        "model '{}': {name} must be 1..={MAX_MULT}, got {value}",
                        entry.id
                    ));
                }
            }
        }
        Ok(())
    }
}

impl Default for StaticMiniChatPolicyPluginConfig {
    fn default() -> Self {
        Self {
            vendor: default_vendor(),
            priority: default_priority(),
            model_catalog: Vec::new(),
            kill_switches: KillSwitches::default(),
            default_standard_limits: default_standard_limits(),
            default_premium_limits: default_premium_limits(),
        }
    }
}

fn default_vendor() -> String {
    "constructorfabric".to_owned()
}

const fn default_priority() -> i16 {
    100
}

fn default_standard_limits() -> TierLimits {
    TierLimits {
        limit_daily_credits_micro: 100_000_000,
        limit_monthly_credits_micro: 1_000_000_000,
    }
}

fn default_premium_limits() -> TierLimits {
    TierLimits {
        limit_daily_credits_micro: 50_000_000,
        limit_monthly_credits_micro: 500_000_000,
    }
}
