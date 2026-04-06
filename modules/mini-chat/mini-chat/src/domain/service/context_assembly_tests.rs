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
