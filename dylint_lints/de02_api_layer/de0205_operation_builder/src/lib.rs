#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_ast;
extern crate rustc_hir;
extern crate rustc_span;

use clippy_utils::consts::{ConstEvalCtxt, Constant};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::Span;

dylint_linting::declare_late_lint! {
    /// DE0205: Operation builder must have tag and summary
    ///
    /// Ensures that all `OperationBuilder` instances call both `.tag(...)` and `.summary(...)`
    /// with properly formatted values. Tags must contain whitespace-separated words where each
    /// word starts with a capital letter. Tags must be string literals or references to `const`
    /// string items. Summaries must be non-empty string literals or const strings.
    ///
    /// ### Why is this bad?
    ///
    /// Operation builders without tags or summaries, or with improperly formatted tags,
    /// make it difficult to organize and categorize API endpoints in OpenAPI documentation
    /// and UI. Proper documentation is essential for API usability.
    ///
    /// ### Example
    ///
    /// ```rust
    /// // Invalid - missing summary and bad tag format
    /// OperationBuilder::post("/users")
    ///     .operation_id("create_user")
    ///     .tag("simple resource registry");
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust
    /// // Proper tag format and summary
    /// OperationBuilder::post("/users")
    ///     .operation_id("create_user")
    ///     .tag("User Management")
    ///     .summary("Create a new user");
    /// ```
    pub DE0205_OPERATION_BUILDER,
    Deny,
    "operation builder must have tag and summary (DE0205)"
}

impl<'tcx> LateLintPass<'tcx> for De0205OperationBuilder {
    fn check_stmt(&mut self, cx: &LateContext<'tcx>, stmt: &'tcx rustc_hir::Stmt<'tcx>) {
        // Check statements for complete builder chains
        if let rustc_hir::StmtKind::Let(local) = stmt.kind {
            if let Some(init) = local.init {
                check_complete_builder_chain(cx, init);
            }
        } else if let rustc_hir::StmtKind::Semi(expr) | rustc_hir::StmtKind::Expr(expr) = stmt.kind {
            check_complete_builder_chain(cx, expr);
        }
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx rustc_hir::Expr<'tcx>) {
        // Validate tag/summary format when called
        if let rustc_hir::ExprKind::MethodCall(path, receiver, args, _span) = expr.kind {
            if is_operation_builder_type(cx, receiver) {
                let method_name = path.ident.name.as_str();

                match method_name {
                    "tag" => {
                        if let Some(tag_arg) = args.first() {
                            if let Some(tag_string) = extract_tag_value(cx, tag_arg) {
                                if !is_valid_tag_format(&tag_string) {
                                    cx.span_lint(DE0205_OPERATION_BUILDER, tag_arg.span, |diag| {
                                        diag.primary_message("tag format is invalid");
                                        diag.help("tags must contain whitespace-separated words, each starting with a capital letter");
                                        diag.note("example: \"User Management\", \"Simple Resource Registry\"");
                                    });
                                }
                            } else {
                                cx.span_lint(DE0205_OPERATION_BUILDER, tag_arg.span, |diag| {
                                    diag.primary_message("tag must be a string literal or const string");
                                    diag.help("use a string literal like `.tag(\"Your Tag\")` or a const string");
                                });
                            }
                        }
                    }
                    "summary" => {
                        if let Some(summary_arg) = args.first() {
                            if let Some(summary_string) = extract_tag_value(cx, summary_arg) {
                                if summary_string.trim().is_empty() {
                                    cx.span_lint(DE0205_OPERATION_BUILDER, summary_arg.span, |diag| {
                                        diag.primary_message("summary cannot be empty");
                                        diag.help("provide a meaningful summary for the operation");
                                    });
                                }
                            } else {
                                cx.span_lint(DE0205_OPERATION_BUILDER, summary_arg.span, |diag| {
                                    diag.primary_message("summary must be a string literal or const string");
                                    diag.help("use a string literal like `.summary(\"Your summary\")` or a const string");
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn check_complete_builder_chain(cx: &LateContext<'_>, expr: &rustc_hir::Expr<'_>) {
    // Only check if this expression contains an OperationBuilder constructor
    if contains_operation_builder_constructor(expr) {
        let mut has_tag = false;
        let mut has_summary = false;

        // Walk the expression tree to find tag and summary calls
        check_builder_chain(expr, &mut has_tag, &mut has_summary);

        // Report missing calls
        let builder_span = get_builder_constructor_span(expr);
        if !has_tag || !has_summary {
            cx.span_lint(DE0205_OPERATION_BUILDER, builder_span, |diag| {
                match (has_tag, has_summary) {
                    (false, false) => {
                        diag.primary_message("operation builder missing .tag() and .summary() calls");
                        diag.help("add .tag(\"Your Tag\") with properly formatted tag");
                        diag.note("tags must contain whitespace-separated words, each starting with a capital letter");
                        diag.help("add .summary(\"Your summary\") with a meaningful description");
                    }
                    (false, true) => {
                        diag.primary_message("operation builder missing .tag() call");
                        diag.help("add .tag(\"Your Tag\") with properly formatted tag");
                        diag.note("tags must contain whitespace-separated words, each starting with a capital letter");
                    }
                    (true, false) => {
                        diag.primary_message("operation builder missing .summary() call");
                        diag.help("add .summary(\"Your summary\") with a meaningful description");
                    }
                    (true, true) => {}
                }
            });
        }
    }
}

fn contains_operation_builder_constructor(expr: &rustc_hir::Expr<'_>) -> bool {
    match expr.kind {
        rustc_hir::ExprKind::Call(func, _) => {
            if let rustc_hir::ExprKind::Path(qpath) = &func.kind {
                if let rustc_hir::QPath::TypeRelative(ty, segment) = qpath {
                    if let rustc_hir::TyKind::Path(rustc_hir::QPath::Resolved(_, path)) = &ty.kind {
                        let type_str = format!("{:?}", path);
                        let method_name = segment.ident.name.as_str();
                        return type_str.contains("OperationBuilder")
                            && type_str.contains("modkit")
                            && matches!(method_name, "get" | "post" | "put" | "delete" | "patch" | "head" | "options");
                    }
                }
            }
            false
        }
        rustc_hir::ExprKind::MethodCall(_, receiver, _, _) => {
            contains_operation_builder_constructor(receiver)
        }
        _ => false,
    }
}

fn get_builder_constructor_span(expr: &rustc_hir::Expr<'_>) -> Span {
    match expr.kind {
        rustc_hir::ExprKind::Call(_, _) => expr.span,
        rustc_hir::ExprKind::MethodCall(_, receiver, _, _) => {
            get_builder_constructor_span(receiver)
        }
        _ => expr.span,
    }
}

fn check_builder_chain(
    expr: &rustc_hir::Expr<'_>,
    has_tag: &mut bool,
    has_summary: &mut bool,
) {
    match expr.kind {
        rustc_hir::ExprKind::MethodCall(path, receiver, _, _) => {
            let method_name = path.ident.name.as_str();
            if method_name == "tag" {
                *has_tag = true;
            } else if method_name == "summary" {
                *has_summary = true;
            }
            check_builder_chain(receiver, has_tag, has_summary);
        }
        _ => {}
    }
}

fn is_operation_builder_type(cx: &LateContext<'_>, expr: &rustc_hir::Expr<'_>) -> bool {
    let ty = cx.typeck_results().expr_ty(expr);
    let type_str = format!("{:?}", ty);
    type_str.contains("OperationBuilder") && type_str.contains("modkit")
}

fn extract_tag_value(cx: &LateContext<'_>, expr: &rustc_hir::Expr<'_>) -> Option<String> {
    if let rustc_hir::ExprKind::Lit(lit) = expr.kind {
        if let rustc_ast::LitKind::Str(symbol, _) = lit.node {
            return Some(symbol.to_string());
        }
    }

    if let Some(Constant::Str(s)) = ConstEvalCtxt::new(cx).eval(expr) {
        return Some(s);
    }

    None
}

fn is_valid_tag_format(tag: &str) -> bool {
    if tag.is_empty() {
        return false;
    }

    // Split by whitespace and check each word
    let words: Vec<&str> = tag.split_whitespace().collect();

    // Must have at least one word
    if words.is_empty() {
        return false;
    }

    // Each word must start with a capital letter
    for word in words {
        if word.is_empty() || !word.chars().next().unwrap().is_uppercase() {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn ui_examples() {
        dylint_testing::ui_test_examples(env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn test_comment_annotations_match_stderr() {
        let ui_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
        lint_utils::test_comment_annotations_match_stderr(&ui_dir, "DE0205", "Operation builder");
    }
}
