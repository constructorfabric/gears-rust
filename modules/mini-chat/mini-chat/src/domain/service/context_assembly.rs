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
mod tests {
    use super::*;

    fn make_message(role: Role, content: &str) -> ContextMessage {
        ContextMessage {
            role,
            content: content.to_owned(),
        }
    }

    // 5.6: empty system prompt + no tools → system_instructions: None, tools: []
    #[test]
    fn empty_system_prompt_no_tools() {
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &[],
            user_message: "hello",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        assert!(result.system_instructions.is_none());
        assert!(result.tools.is_empty());
        assert_eq!(result.messages.len(), 1);
    }

    // 5.7: system prompt + web_search enabled → guard appended
    #[test]
    fn system_prompt_with_web_search_guard() {
        let result = assemble_context(&ContextInput {
            system_prompt: "You are helpful.",
            web_search_guard: "Use web_search only if needed.",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &[],
            user_message: "hello",
            web_search_enabled: true,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        let instructions = result.system_instructions.unwrap();
        assert!(instructions.contains("You are helpful."));
        assert!(instructions.contains("Use web_search only if needed."));
    }

    // 5.8: system prompt + file_search enabled → guard appended
    #[test]
    fn system_prompt_with_file_search_guard() {
        let result = assemble_context(&ContextInput {
            system_prompt: "You are helpful.",
            web_search_guard: "",
            file_search_guard: "Use file_search for documents.",
            thread_summary: None,
            recent_messages: &[],
            user_message: "hello",
            web_search_enabled: false,
            file_search_enabled: true,
            vector_store_ids: &["vs-1".to_owned()],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        let instructions = result.system_instructions.unwrap();
        assert!(instructions.contains("You are helpful."));
        assert!(instructions.contains("Use file_search for documents."));
    }

    // 5.9: both guards appended when both tools enabled
    #[test]
    fn both_guards_appended() {
        let result = assemble_context(&ContextInput {
            system_prompt: "Base prompt.",
            web_search_guard: "web guard",
            file_search_guard: "file guard",
            thread_summary: None,
            recent_messages: &[],
            user_message: "hello",
            web_search_enabled: true,
            file_search_enabled: true,
            vector_store_ids: &["vs-1".to_owned()],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        let instructions = result.system_instructions.unwrap();
        assert!(instructions.contains("Base prompt."));
        assert!(instructions.contains("web guard"));
        assert!(instructions.contains("file guard"));
    }

    // 5.10: thread summary present → included as first message with prefix
    #[test]
    fn thread_summary_included_as_first_message() {
        let recent = vec![make_message(Role::User, "prior question")];
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: Some("Summary of prior conversation."),
            recent_messages: &recent,
            user_message: "new question",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        // First message should be the thread summary
        assert_eq!(result.messages.len(), 3); // summary + recent + current
        let first_content = &result.messages[0].content;
        match &first_content[0] {
            crate::domain::llm::ContentPart::Text { text } => {
                assert!(text.contains("[Thread Summary]"));
                assert!(text.contains("Summary of prior conversation."));
            }
            crate::domain::llm::ContentPart::Image { .. } => {
                panic!("Expected text content")
            }
        }
    }

    // 5.11: no thread summary → messages start with recent history
    #[test]
    fn no_thread_summary_starts_with_recent() {
        let recent = vec![
            make_message(Role::User, "first"),
            make_message(Role::Assistant, "response"),
        ];
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &recent,
            user_message: "second",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        assert_eq!(result.messages.len(), 3); // 2 recent + current
    }

    // 5.12: recent messages mapped by role (user/assistant), system role skipped
    #[test]
    fn system_role_skipped() {
        let recent = vec![
            make_message(Role::User, "hello"),
            make_message(Role::System, "system msg"),
            make_message(Role::Assistant, "hi"),
        ];
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &recent,
            user_message: "bye",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        // system message skipped: 2 recent (user+assistant) + 1 current = 3
        assert_eq!(result.messages.len(), 3);
    }

    // 5.13: current user message always last
    #[test]
    fn current_user_message_is_last() {
        let recent = vec![make_message(Role::Assistant, "prior")];
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &recent,
            user_message: "current input",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        let last = result.messages.last().unwrap();
        match &last.content[0] {
            crate::domain::llm::ContentPart::Text { text } => {
                assert_eq!(text, "current input");
            }
            crate::domain::llm::ContentPart::Image { .. } => {
                panic!("Expected text content")
            }
        }
    }

    // 5.14: tools vec populated correctly for file_search + web_search combinations
    #[test]
    fn tools_populated_correctly() {
        // Both enabled with vector store
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &[],
            user_message: "hello",
            web_search_enabled: true,
            file_search_enabled: true,
            vector_store_ids: &["vs-123".to_owned()],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::High,
            file_search_max_num_results: 7,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        assert_eq!(result.tools.len(), 2);
        assert!(matches!(
            &result.tools[0],
            LlmTool::FileSearch {
                max_num_results: Some(7),
                ..
            }
        ));
        assert!(matches!(
            &result.tools[1],
            LlmTool::WebSearch {
                search_context_size: crate::domain::llm::WebSearchContextSize::High
            }
        ));

        // file_search enabled but no vector store IDs → no file_search tool
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &[],
            user_message: "hello",
            web_search_enabled: false,
            file_search_enabled: true,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        assert!(result.tools.is_empty());

        // Only web_search
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &[],
            user_message: "hello",
            web_search_enabled: true,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Medium,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        assert_eq!(result.tools.len(), 1);
        assert!(matches!(
            &result.tools[0],
            LlmTool::WebSearch {
                search_context_size: crate::domain::llm::WebSearchContextSize::Medium
            }
        ));
    }

    // ── Helper: default budgets for truncation tests ──

    fn test_budgets() -> EstimationBudgets {
        EstimationBudgets {
            bytes_per_token_conservative: 4,
            fixed_overhead_tokens: 100,
            safety_margin_pct: 10,
            image_token_budget: 1000,
            tool_surcharge_tokens: 500,
            web_search_surcharge_tokens: 500,
            code_interpreter_surcharge_tokens: 1000,
            minimal_generation_floor: 128,
        }
    }

    fn test_budget(context_window: u32, max_output: i32) -> TokenBudget {
        TokenBudget {
            context_window,
            max_output_tokens_applied: max_output,
            budgets: test_budgets(),
            tools_enabled: false,
            web_search_enabled: false,
            code_interpreter_enabled: false,
        }
    }

    // 5.15: budget computation with no tools
    #[test]
    fn budget_no_tools() {
        let budget = test_budget(128_000, 4096);
        // available = 128_000 - 4096 - 0 (no tools) - 100 (fixed overhead)
        let available = compute_available_budget(&budget).unwrap();
        assert_eq!(available, 128_000 - 4096 - 100);
    }

    // 5.16: budget computation with file_search and web_search
    #[test]
    fn budget_with_tools() {
        let budget = TokenBudget {
            context_window: 128_000,
            max_output_tokens_applied: 4096,
            budgets: test_budgets(),
            tools_enabled: true,
            web_search_enabled: true,
            code_interpreter_enabled: false,
        };
        // available = 128_000 - 4096 - 500 (tool) - 500 (web) - 100 (overhead)
        let available = compute_available_budget(&budget).unwrap();
        assert_eq!(available, 128_000 - 4096 - 500 - 500 - 100);
    }

    // 5.17: budget computation with zero context_window
    #[test]
    fn budget_zero_context_window() {
        let budget = test_budget(0, 4096);
        let result = compute_available_budget(&budget);
        assert!(matches!(
            result,
            Err(ContextAssemblyError::BudgetExceeded { .. })
        ));
    }

    // 5.18: per-item estimation — verify bytes-per-token heuristic with margin
    #[test]
    fn item_estimation() {
        let budgets = test_budgets();
        // 400 bytes / 4 bpt = 100 tokens + 100 overhead = 200 base
        // 200 * (100 + 10) / 100 = 220
        assert_eq!(estimate_item_tokens(400, &budgets), 220);

        // 0 bytes: 0/4 = 0 + 100 = 100 base → 100 * 110 / 100 = 110
        assert_eq!(estimate_item_tokens(0, &budgets), 110);

        // 1 byte: ceil(1/4) = 1 + 100 = 101 → 101 * 110 / 100 = 111 (int div)
        assert_eq!(estimate_item_tokens(1, &budgets), 111);
    }

    // 5.19: truncation drops thread summary (P3) when budget tight
    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn truncation_drops_thread_summary() {
        // Budget just enough for system + user message but not summary
        let budgets = test_budgets();
        let sys_cost = estimate_item_tokens(10, &budgets); // small system prompt
        let user_cost = estimate_item_tokens(5, &budgets); // small user message
        // Set context_window so available = mandatory + 1 (not enough for summary)
        let overhead = 4096 + 100; // max_output + fixed_overhead
        let context_window = (overhead as u64 + sys_cost + user_cost + 1) as u32;

        let result = assemble_context(&ContextInput {
            system_prompt: "0123456789", // 10 bytes
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: Some("A very long summary that should be dropped"),
            recent_messages: &[],
            user_message: "hello", // 5 bytes
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: Some(test_budget(context_window, 4096)),
            image_file_ids: &[],
        })
        .unwrap();

        // Only the current user message should remain (summary dropped)
        assert_eq!(result.messages.len(), 1);
    }

    // 5.20: truncation drops oldest recent messages (P4) when budget tight
    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn truncation_drops_oldest_messages() {
        let budgets = test_budgets();
        // Each message: "msg" = 3 bytes → estimate_item_tokens(3, budgets) = ceil(3/4)+100 = 101 → 101*110/100 = 111
        let msg_cost = estimate_item_tokens(3, &budgets);
        let user_cost = estimate_item_tokens(5, &budgets);
        // Budget for mandatory + exactly 1 message
        let overhead = 4096u64 + 100;
        let context_window = (overhead + user_cost + msg_cost) as u32;

        let recent = vec![
            make_message(Role::User, "msg"),      // oldest — should be dropped
            make_message(Role::Assistant, "msg"), // newer — should be kept
        ];

        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &recent,
            user_message: "hello",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: Some(test_budget(context_window, 4096)),
            image_file_ids: &[],
        })
        .unwrap();

        // 1 kept recent message + 1 current user message = 2
        assert_eq!(result.messages.len(), 2);
    }

    // 5.21: truncation drops thread summary (P3) when it doesn't fit
    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn truncation_drops_summary_keeps_messages() {
        let budgets = test_budgets();
        let msg_cost = estimate_item_tokens(3, &budgets);
        let user_cost = estimate_item_tokens(5, &budgets);
        // Make summary expensive enough that it won't fit alongside 2 messages
        let big_summary = "X".repeat(2000);
        let summary_cost = estimate_item_tokens(
            (big_summary.len() + "[Thread Summary]\n".len()) as u64,
            &budgets,
        );
        // Budget: mandatory + 2 messages, but NOT enough for summary
        let overhead = 4096u64 + 100;
        let context_window = (overhead + user_cost + 2 * msg_cost) as u32;
        // Verify summary truly doesn't fit
        assert!(
            summary_cost > 2 * msg_cost,
            "summary should be more expensive than 2 messages for this test"
        );

        let recent = vec![
            make_message(Role::User, "msg"),
            make_message(Role::Assistant, "msg"),
        ];

        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: Some(&big_summary),
            recent_messages: &recent,
            user_message: "hello",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: Some(test_budget(context_window, 4096)),
            image_file_ids: &[],
        })
        .unwrap();

        // 2 recent + 1 current = 3 (summary dropped)
        assert_eq!(result.messages.len(), 3);
    }

    // 5.22: BudgetExceeded when mandatory items exceed budget
    #[test]
    fn budget_exceeded_mandatory_too_large() {
        // Context window so small that even system + user message don't fit
        let result = assemble_context(&ContextInput {
            system_prompt: "A".repeat(100_000).as_str(),
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &[],
            user_message: "hello",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: Some(test_budget(5000, 4096)),
            image_file_ids: &[],
        });

        assert!(matches!(
            result,
            Err(ContextAssemblyError::BudgetExceeded { .. })
        ));
    }

    // 5.23: token_budget: None skips truncation entirely — all items included
    #[test]
    fn no_budget_includes_everything() {
        let recent = vec![
            make_message(Role::User, "A".repeat(50_000).as_str()),
            make_message(Role::Assistant, "B".repeat(50_000).as_str()),
        ];

        let result = assemble_context(&ContextInput {
            system_prompt: "sys",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: Some("summary"),
            recent_messages: &recent,
            user_message: "hello",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();

        // summary + 2 recent + current = 4
        assert_eq!(result.messages.len(), 4);
    }

    // 5.24: ContextBudgetExceeded maps to HTTP 422
    // (Tested at integration level via stream_error_to_response in turns.rs;
    //  here we verify the error type and message.)
    #[test]
    fn budget_exceeded_error_message() {
        let err = ContextAssemblyError::BudgetExceeded {
            required_tokens: 50_000,
            available_tokens: 10_000,
        };
        let msg = err.to_string();
        assert!(msg.contains("50000"));
        assert!(msg.contains("10000"));
    }

    // 5.25: code_interpreter tool added when enabled and file_ids non-empty
    #[test]
    fn code_interpreter_tool_added_when_enabled_with_file_ids() {
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &[],
            user_message: "analyze this",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec!["file-abc123".to_owned()],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        assert_eq!(result.tools.len(), 1);
        assert!(matches!(
            &result.tools[0],
            LlmTool::CodeInterpreter { file_ids } if file_ids == &["file-abc123"]
        ));
    }

    // 5.26: code_interpreter tool not added when file_ids is empty
    #[test]
    fn code_interpreter_tool_not_added_when_no_file_ids() {
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &[],
            user_message: "analyze this",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        assert!(result.tools.is_empty());
    }

    // 5.28: code_interpreter surcharge deducted from budget when enabled
    #[test]
    fn budget_with_code_interpreter_surcharge() {
        let budget = TokenBudget {
            context_window: 128_000,
            max_output_tokens_applied: 4096,
            budgets: test_budgets(),
            tools_enabled: false,
            web_search_enabled: false,
            code_interpreter_enabled: true,
        };
        // available = 128_000 - 4096 - 1000 (code_interpreter) - 100 (overhead)
        let available = compute_available_budget(&budget).unwrap();
        assert_eq!(available, 128_000 - 4096 - 1000 - 100);
    }

    // ── Image inlining tests ──

    #[test]
    fn single_image_produces_image_content_part() {
        let images = vec!["file-abc".to_owned()];
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &[],
            user_message: "Describe this",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &images,
        })
        .unwrap();
        assert_eq!(result.messages.len(), 1);
        let msg = &result.messages[0];
        assert_eq!(msg.content.len(), 2);
        assert!(matches!(&msg.content[0], ContentPart::Text { text } if text == "Describe this"));
        assert!(matches!(&msg.content[1], ContentPart::Image { file_id } if file_id == "file-abc"));
    }

    #[test]
    fn multiple_images_produce_multiple_content_parts() {
        let images = vec!["file-1".to_owned(), "file-2".to_owned()];
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &[],
            user_message: "Compare these",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &images,
        })
        .unwrap();
        let msg = &result.messages[0];
        assert_eq!(msg.content.len(), 3);
        assert!(matches!(&msg.content[1], ContentPart::Image { file_id } if file_id == "file-1"));
        assert!(matches!(&msg.content[2], ContentPart::Image { file_id } if file_id == "file-2"));
    }

    #[test]
    fn no_images_produces_text_only() {
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &[],
            user_message: "hello",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: None,
            image_file_ids: &[],
        })
        .unwrap();
        let msg = &result.messages[0];
        assert_eq!(msg.content.len(), 1);
        assert!(matches!(&msg.content[0], ContentPart::Text { .. }));
    }

    #[test]
    fn image_tokens_included_in_budget_mandatory() {
        let images = vec!["file-1".to_owned(), "file-2".to_owned()];
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &[],
            user_message: "hi",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: Some(test_budget(10_000, 4096)),
            image_file_ids: &images,
        });
        assert!(result.is_ok());
    }

    #[test]
    fn image_tokens_cause_budget_exceeded() {
        let images = vec!["file-1".to_owned(), "file-2".to_owned()];
        let result = assemble_context(&ContextInput {
            system_prompt: "",
            web_search_guard: "",
            file_search_guard: "",
            thread_summary: None,
            recent_messages: &[],
            user_message: "hi",
            web_search_enabled: false,
            file_search_enabled: false,
            vector_store_ids: &[],
            file_search_filters: None,
            web_search_context_size: crate::domain::llm::WebSearchContextSize::Low,
            file_search_max_num_results: 5,
            code_interpreter_file_ids: vec![],
            token_budget: Some(test_budget(5100, 4096)),
            image_file_ids: &images,
        });
        assert!(matches!(
            result,
            Err(ContextAssemblyError::BudgetExceeded { .. })
        ));
    }

    #[test]
    fn build_user_message_helper_text_only() {
        let msg = super::build_user_message("hello", &[]);
        assert_eq!(msg.content.len(), 1);
        assert!(matches!(&msg.content[0], ContentPart::Text { text } if text == "hello"));
    }

    #[test]
    fn build_user_message_helper_with_images() {
        let ids = vec!["f1".to_owned(), "f2".to_owned()];
        let msg = super::build_user_message("look", &ids);
        assert_eq!(msg.content.len(), 3);
        assert!(matches!(&msg.content[0], ContentPart::Text { text } if text == "look"));
        assert!(matches!(&msg.content[1], ContentPart::Image { file_id } if file_id == "f1"));
        assert!(matches!(&msg.content[2], ContentPart::Image { file_id } if file_id == "f2"));
    }
}
