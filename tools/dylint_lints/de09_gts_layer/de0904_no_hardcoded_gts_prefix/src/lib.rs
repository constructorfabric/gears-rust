#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_span;

use clippy_utils::diagnostics::span_lint_and_then;
use rustc_ast::{AttrKind, Attribute, Expr, ExprKind, Item, MacCall, token::LitKind, visit};
use rustc_lint::{EarlyContext, EarlyLintPass, LintContext};
use rustc_span::Span;
use std::cell::RefCell;
use std::collections::HashSet;

thread_local! {
    static LINTED_SPANS: RefCell<HashSet<Span>> = RefCell::new(HashSet::new());
}

dylint_linting::declare_pre_expansion_lint! {
    /// DE0904: Do not hard-code the GTS ID prefix
    ///
    /// GTS IDs must be created with `gts_id!("<suffix>")`, so the
    /// `GTS_ID_PREFIX` build-time configuration remains effective. This is a
    /// pre-expansion lint: it inspects user-authored source before ToolKit's
    /// wrapper macros generate inventory and error-builder code.
    pub DE0904_NO_HARDCODED_GTS_PREFIX,
    Deny,
    "hard-coded GTS ID prefix; use gts_id!(\"<suffix>\") instead (DE0904)"
}

impl EarlyLintPass for De0904NoHardcodedGtsPrefix {
    fn check_crate_post(&mut self, _cx: &EarlyContext<'_>, _krate: &rustc_ast::Crate) {
        LINTED_SPANS.with(|spans| spans.borrow_mut().clear());
    }

    fn check_item(&mut self, cx: &EarlyContext<'_>, item: &Item) {
        let mut visitor = HardcodedPrefixVisitor { cx };
        visit::walk_item(&mut visitor, item);
    }

    fn check_attribute(&mut self, cx: &EarlyContext<'_>, attr: &Attribute) {
        let AttrKind::Normal(normal) = &attr.kind else {
            return;
        };
        let Some(last) = normal.item.path.segments.last() else {
            return;
        };
        if !matches!(
            last.ident.name.as_str(),
            "gts_type_schema" | "struct_to_gts_schema" | "resource_error"
        ) {
            return;
        }

        if source_contains_hardcoded_prefix(cx, attr.span) {
            lint_once(cx, attr.span);
        }
    }
}

struct HardcodedPrefixVisitor<'a, 'cx> {
    cx: &'a EarlyContext<'cx>,
}

impl<'ast, 'a, 'cx> visit::Visitor<'ast> for HardcodedPrefixVisitor<'a, 'cx> {
    fn visit_item(&mut self, _item: &'ast Item) {
        // `EarlyLintPass::check_item` is invoked for every item. Do not walk
        // nested items here, otherwise they are visited repeatedly.
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let ExprKind::Lit(lit) = &expr.kind
            && matches!(lit.kind, LitKind::Str | LitKind::StrRaw(_))
            && lit.symbol.as_str().starts_with("gts.")
        {
            lint_once(self.cx, expr.span);
        }
        visit::walk_expr(self, expr);
    }

    fn visit_mac_call(&mut self, mac_call: &'ast MacCall) {
        if source_contains_hardcoded_prefix(self.cx, mac_call.span()) {
            lint_once(self.cx, mac_call.span());
        }
    }
}

fn source_contains_hardcoded_prefix(cx: &EarlyContext<'_>, span: Span) -> bool {
    let Ok(source) = cx.sess().source_map().span_to_snippet(span) else {
        return false;
    };
    source.contains("\"gts.")
}

fn lint_once(cx: &EarlyContext<'_>, span: Span) {
    let is_new = LINTED_SPANS.with(|spans| spans.borrow_mut().insert(span));
    if !is_new {
        return;
    }
    span_lint_and_then(
        cx,
        DE0904_NO_HARDCODED_GTS_PREFIX,
        span,
        "hard-coded GTS ID prefix; use gts_id!(\"<suffix>\") instead (DE0904)",
        |diag| {
            diag.help("for example: gts_id!(\"cf.core.users.user.v1~\")");
        },
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn ui_examples() {
        dylint_testing::ui_test_examples(env!("CARGO_PKG_NAME"));
    }
}
