//! Pure context assembly for LLM requests.
//!
//! Assembles system instructions, conversation messages, and tool definitions
//! from domain inputs. No I/O, no async — all data is gathered beforehand.

use modkit_macros::domain_model;

use crate::config::EstimationBudgets;
use crate::domain::llm::{
    ContentPart, ContextMessage, FileSearchFilter, LlmMessage, LlmTool, Role,
};

/// Token budget parameters for context truncation.
///
/// When present, `assemble_context` applies priority-based truncation to
/// fit the assembled context within the available token budget.
#[domain_model]
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct TokenBudget {
    /// Total context window of the effective model (tokens).
    pub context_window: u32,
    /// Max output tokens applied after preflight (reserved for generation).
    pub max_output_tokens_applied: i32,
    /// Per-model estimation budgets (bytes-per-token, surcharges, etc.).
    pub budgets: EstimationBudgets,
    /// Whether `file_search` tool is enabled (contributes tool surcharge).
    pub tools_enabled: bool,
    /// Whether `web_search` is enabled (contributes web search surcharge).
    pub web_search_enabled: bool,
    /// Whether `code_interpreter` is enabled — derived from non-empty file IDs
    /// at the call site (contributes code interpreter surcharge).
    pub code_interpreter_enabled: bool,
}

/// All inputs needed to assemble the LLM request context.
#[domain_model]
#[allow(clippy::struct_excessive_bools)]
pub struct ContextInput<'a> {
    /// System prompt from the model catalog (via preflight).
    pub system_prompt: &'a str,
    /// Guard instruction appended when `web_search` is enabled.
    pub web_search_guard: &'a str,
    /// Guard instruction appended when `file_search` is enabled.
    pub file_search_guard: &'a str,
    /// Thread summary content (if exists).
    pub thread_summary: Option<&'a str>,
    /// Recent messages from DB, already in chronological order.
    pub recent_messages: &'a [ContextMessage],
    /// Current user message text.
    pub user_message: &'a str,
    /// Whether `web_search` tool is enabled for this request.
    pub web_search_enabled: bool,
    /// Whether `file_search` tool is enabled for this request.
    pub file_search_enabled: bool,
    /// Vector store IDs for `file_search` (empty = no `file_search` tool).
    pub vector_store_ids: &'a [String],
    /// Optional metadata filter for file search (e.g. filter by `attachment_ids`).
    pub file_search_filters: Option<FileSearchFilter>,
    /// Search context size for `web_search` tool.
    pub web_search_context_size: crate::domain::llm::WebSearchContextSize,
    /// Max results for `file_search` tool (from CCM per-model config).
    pub file_search_max_num_results: u32,
    /// File IDs for `code_interpreter`. Non-empty = tool is enabled.
    pub code_interpreter_file_ids: Vec<String>,
    /// Token budget for context truncation. `None` = no truncation.
    pub token_budget: Option<TokenBudget>,
    /// Provider file IDs for image attachments on the current user message.
    pub image_file_ids: &'a [String],
}

/// Output of context assembly — ready to feed into `LlmRequestBuilder`.
#[domain_model]
pub struct AssembledContext {
    /// System instructions (None if empty).
    pub system_instructions: Option<String>,
    /// Conversation messages in normative order.
    pub messages: Vec<LlmMessage>,
    /// Tool definitions to include in the request.
    pub tools: Vec<LlmTool>,
}

/// Error during context assembly.
#[domain_model]
#[derive(Debug)]
pub enum ContextAssemblyError {
    /// Mandatory items (system instructions + current user message) exceed
    /// the available token budget.
    BudgetExceeded {
        required_tokens: u64,
        available_tokens: u64,
    },
}

impl std::fmt::Display for ContextAssemblyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BudgetExceeded {
                required_tokens,
                available_tokens,
            } => write!(
                f,
                "mandatory context items require {required_tokens} tokens but only {available_tokens} are available"
            ),
        }
    }
}

impl std::error::Error for ContextAssemblyError {}

/// Build the current user message with optional image content parts.
fn build_user_message(text: &str, image_file_ids: &[String]) -> LlmMessage {
    if image_file_ids.is_empty() {
        LlmMessage::user(text)
    } else {
        let mut content = vec![ContentPart::Text {
            text: text.to_owned(),
        }];
        for file_id in image_file_ids {
            content.push(ContentPart::Image {
                file_id: file_id.clone(),
            });
        }
        LlmMessage {
            role: Role::User,
            content,
        }
    }
}

/// Compute the available input token budget after deducting output reservation
/// and tool surcharges.
///
/// Returns `Err(BudgetExceeded)` if the budget is zero or negative.
pub fn compute_available_budget(budget: &TokenBudget) -> Result<u64, ContextAssemblyError> {
    let tool_surcharge = if budget.tools_enabled {
        u64::from(budget.budgets.tool_surcharge_tokens)
    } else {
        0
    } + if budget.web_search_enabled {
        u64::from(budget.budgets.web_search_surcharge_tokens)
    } else {
        0
    } + if budget.code_interpreter_enabled {
        u64::from(budget.budgets.code_interpreter_surcharge_tokens)
    } else {
        0
    };

    #[allow(clippy::cast_sign_loss)]
    let deductions = budget.max_output_tokens_applied as u64
        + tool_surcharge
        + u64::from(budget.budgets.fixed_overhead_tokens);

    let context_window = u64::from(budget.context_window);
    if deductions >= context_window {
        return Err(ContextAssemblyError::BudgetExceeded {
            required_tokens: deductions,
            available_tokens: context_window,
        });
    }

    Ok(context_window - deductions)
}

/// Estimate token count for a text item.
///
/// Uses the conservative bytes-per-token ratio with safety margin and
/// per-item fixed overhead. No image handling, no tool surcharges.
#[must_use]
pub fn estimate_item_tokens(text_bytes: u64, budgets: &EstimationBudgets) -> u64 {
    let bpt = u64::from(budgets.bytes_per_token_conservative.max(1));
    let base = text_bytes.div_ceil(bpt) + u64::from(budgets.fixed_overhead_tokens);
    #[allow(clippy::integer_division)]
    {
        base * (100 + u64::from(budgets.safety_margin_pct)) / 100
    }
}

/// Assemble the LLM request context from gathered domain inputs.
///
/// When `token_budget` is `Some`, applies priority-based truncation:
/// - P1 (system instructions) and P2 (current user message) are mandatory
/// - P3 (thread summary) is dropped if it doesn't fit
/// - P4 (recent messages) are dropped oldest-first
///
/// When `token_budget` is `None`, all items are included without truncation.
pub fn assemble_context(
    input: &ContextInput<'_>,
) -> Result<AssembledContext, ContextAssemblyError> {
    // ── System instructions ──
    let system_instructions = build_system_instructions(
        input.system_prompt,
        input.web_search_enabled,
        input.web_search_guard,
        input.file_search_enabled,
        input.file_search_guard,
    );

    // ── Tools ──
    let mut tools = Vec::new();
    if input.file_search_enabled && !input.vector_store_ids.is_empty() {
        tools.push(LlmTool::FileSearch {
            vector_store_ids: input.vector_store_ids.to_vec(),
            filters: input.file_search_filters.clone(),
            max_num_results: Some(input.file_search_max_num_results),
        });
    }
    if input.web_search_enabled {
        tools.push(LlmTool::WebSearch {
            search_context_size: input.web_search_context_size,
        });
    }
    if !input.code_interpreter_file_ids.is_empty() {
        tools.push(LlmTool::CodeInterpreter {
            file_ids: input.code_interpreter_file_ids.clone(),
        });
    }

    // ── Truncation ──
    if let Some(ref budget) = input.token_budget {
        let available = compute_available_budget(budget)?;
        let budgets = &budget.budgets;

        // P1: System instructions (mandatory)
        let sys_tokens = system_instructions
            .as_ref()
            .map_or(0, |s| estimate_item_tokens(s.len() as u64, budgets));

        // P2: Current user message (mandatory)
        let user_tokens = estimate_item_tokens(input.user_message.len() as u64, budgets);
        let image_tokens = (input.image_file_ids.len() as u64)
            .saturating_mul(u64::from(budgets.image_token_budget));

        let mandatory = sys_tokens + user_tokens + image_tokens;
        if mandatory > available {
            return Err(ContextAssemblyError::BudgetExceeded {
                required_tokens: mandatory,
                available_tokens: available,
            });
        }

        let mut remaining = available - mandatory;

        // P3: Thread summary (droppable)
        let keep_summary = if let Some(summary) = input.thread_summary {
            let cost =
                estimate_item_tokens((summary.len() + "[Thread Summary]\n".len()) as u64, budgets);
            if cost <= remaining {
                remaining -= cost;
                true
            } else {
                false
            }
        } else {
            false
        };

        // P4: Recent messages — iterate newest→oldest, keep while they fit
        let mut keep_from_index = input.recent_messages.len();
        for (i, msg) in input.recent_messages.iter().enumerate().rev() {
            if matches!(msg.role, Role::System) {
                continue; // system messages are skipped in output
            }
            let cost = estimate_item_tokens(msg.content.len() as u64, budgets);
            if cost <= remaining {
                remaining -= cost;
                keep_from_index = i;
            } else {
                break;
            }
        }

        // ── Build messages in chronological order ──
        let mut messages = Vec::new();

        if keep_summary && let Some(summary) = input.thread_summary {
            messages.push(LlmMessage::user(format!("[Thread Summary]\n{summary}")));
        }

        for msg in &input.recent_messages[keep_from_index..] {
            match msg.role {
                Role::User => messages.push(LlmMessage::user(&msg.content)),
                Role::Assistant => messages.push(LlmMessage::assistant(&msg.content)),
                Role::System => {}
            }
        }

        messages.push(build_user_message(input.user_message, input.image_file_ids));

        Ok(AssembledContext {
            system_instructions,
            messages,
            tools,
        })
    } else {
        // No budget — include everything without truncation.
        let mut messages = Vec::new();

        if let Some(summary) = input.thread_summary {
            messages.push(LlmMessage::user(format!("[Thread Summary]\n{summary}")));
        }

        for msg in input.recent_messages {
            match msg.role {
                Role::User => messages.push(LlmMessage::user(&msg.content)),
                Role::Assistant => messages.push(LlmMessage::assistant(&msg.content)),
                Role::System => {}
            }
        }

        messages.push(build_user_message(input.user_message, input.image_file_ids));

        Ok(AssembledContext {
            system_instructions,
            messages,
            tools,
        })
    }
}

/// Build system instructions from base prompt + conditional guard strings.
/// Returns `None` if the result would be empty.
fn build_system_instructions(
    system_prompt: &str,
    web_search_enabled: bool,
    web_search_guard: &str,
    file_search_enabled: bool,
    file_search_guard: &str,
) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();

    if !system_prompt.is_empty() {
        parts.push(system_prompt);
    }
    if web_search_enabled && !web_search_guard.is_empty() {
        parts.push(web_search_guard);
    }
    if file_search_enabled && !file_search_guard.is_empty() {
        parts.push(file_search_guard);
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}
#[cfg(test)]
#[path = "context_assembly_tests.rs"]
mod tests;
