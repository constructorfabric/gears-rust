#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_ast;
extern crate rustc_span;

use gts::{GtsIdSegment, GtsOps};
use rustc_ast::token::LitKind;
use rustc_ast::{AttrKind, Attribute, Expr, ExprKind, Item, ItemKind};
use rustc_lint::{EarlyContext, EarlyLintPass, LintContext};
use rustc_span::Span;
use std::cell::RefCell;
use std::collections::HashSet;

// Thread-local storage for spans to skip (inside starts_with calls)
thread_local! {
    static SKIP_SPANS: RefCell<HashSet<Span>> = RefCell::new(HashSet::new());
}

dylint_linting::declare_pre_expansion_lint! {
    /// ### What it does
    ///
    /// Validates GTS schema identifiers used by `gts-macros`.
    ///
    /// Checks:
    /// 1. `schema_id = "..."` in `#[struct_to_gts_schema(...)]` - must be valid GTS type schema
    /// 2. `gts_make_instance_id("...")` - must be valid GTS instance segment id
    /// 3. GTS-looking string literals - must be valid GTS entity id
    ///
    /// Uses `GtsOps::parse_id()` from the GTS library for validation.
    pub DE0901_GTS_STRING_PATTERN,
    Deny,
    "invalid GTS string pattern (DE0901)"
}

impl EarlyLintPass for De0901GtsStringPattern {
    fn check_crate_post(&mut self, _cx: &EarlyContext<'_>, _krate: &rustc_ast::Crate) {
        SKIP_SPANS.with(|s| s.borrow_mut().clear());
    }

    fn check_attribute(&mut self, cx: &EarlyContext<'_>, attr: &Attribute) {
        self.check_struct_to_gts_schema_attr(cx, attr);
    }

    /// Enforce naming convention for `const`/`static` items holding GTS wildcard strings.
    ///
    /// A wildcard GTS string (contains `*`) stored in a `const` or `static` item
    /// **must** have a name ending with `_WILDCARD`.  This makes wildcard constants
    /// explicitly opt-in and easy to audit.
    ///
    /// | Item name         | Value                             | Result  |
    /// |-------------------|-----------------------------------|---------|
    /// | `SRR_WILDCARD`    | `"gts.x.core.srr.resource.v1~*"` | ✅ allowed — name ends with `_WILDCARD` |
    /// | `SRR_PATTERN`     | `"gts.x.core.srr.resource.v1~*"` | ❌ flagged — name must end with `_WILDCARD` |
    ///
    /// Items with a compliant name are added to the skip set so their value span
    /// is not re-checked by `check_expr`.
    fn check_item(&mut self, cx: &EarlyContext<'_>, item: &Item) {
        // Extract both the item name and the initializer expression from const/static items.
        // Note: `Item` has no top-level `ident`; it lives inside `ConstItem` / `StaticItem`.
        let (item_name, init_expr): (&str, Option<&Expr>) = match &item.kind {
            ItemKind::Const(ci) => (ci.ident.name.as_str(), ci.expr.as_deref()),
            ItemKind::Static(si) => (si.ident.name.as_str(), si.expr.as_deref()),
            _ => return,
        };
        let Some(init) = init_expr else { return };

        // Only act on GTS wildcard string values (starts with "gts." and contains '*').
        let Some(s) = Self::string_lit_value(init) else {
            return;
        };
        if !s.starts_with("gts.") || !s.contains('*') {
            return;
        }

        // Validate the wildcard GTS pattern itself before skip-listing.
        let result = GtsOps::parse_id(s);
        if !result.ok {
            cx.span_lint(DE0901_GTS_STRING_PATTERN, item.span, |diag| {
                diag.primary_message(format!(
                    "invalid GTS wildcard pattern in `{item_name}`: '{s}' (DE0901)"
                ));
                diag.note(result.error);
                diag.help("Example: gts.x.core.srr.resource.v1~*");
            });
            // Still skip-list so check_expr doesn't double-report the literal.
            SKIP_SPANS.with(|spans| {
                collect_nested_spans(init, &mut spans.borrow_mut());
            });
            return;
        }

        if !item_name.ends_with("_WILDCARD") {
            cx.span_lint(DE0901_GTS_STRING_PATTERN, item.span, |diag| {
                diag.primary_message(format!(
                    "GTS wildcard string in `const`/`static` `{item_name}` must have a name ending with `_WILDCARD` (DE0901)"
                ));
                diag.note(format!(
                    "found wildcard GTS pattern `{s}` stored in `{item_name}`"
                ));
                diag.help(format!(
                    "rename to `{item_name}_WILDCARD` or use a non-wildcard value"
                ));
            });
        }

        // Skip-list the span so check_expr doesn't re-flag (or double-report) the literal.
        SKIP_SPANS.with(|spans| {
            collect_nested_spans(init, &mut spans.borrow_mut());
        });
    }

    fn check_expr(&mut self, cx: &EarlyContext<'_>, expr: &Expr) {
        // ── Phase 1: collect spans to skip ────────────────────────────────────
        if let ExprKind::MethodCall(method_call) = &expr.kind {
            let method_name = method_call.seg.ident.name.as_str();
            if method_name == "starts_with" {
                // Add the receiver and all arguments to skip list
                SKIP_SPANS.with(|spans| {
                    let mut spans = spans.borrow_mut();
                    spans.insert(method_call.receiver.span);
                    for arg in &method_call.args {
                        spans.insert(arg.span);
                    }
                });
                // Don't check anything in starts_with calls
                return;
            }
            if method_name == "resource_pattern"
                || method_name == "with_pattern"
                || method_name == "resolve_to_uuids"
            {
                // Validate nested string literals BEFORE skip-listing so that
                // deeply nested GTS strings (e.g. inside &["gts...".to_owned()])
                // are checked rather than silently escaping validation.
                for arg in &method_call.args {
                    self.validate_nested_gts_strings(cx, arg, true);
                }
                // Then add all nested sub-expression spans to the skip list
                // to prevent duplicate reports from check_expr.
                SKIP_SPANS.with(|spans| {
                    let mut spans = spans.borrow_mut();
                    for arg in &method_call.args {
                        collect_nested_spans(arg, &mut spans);
                    }
                });
            }
        }

        // Detect free-function calls: `GtsWildcard::new("...")` or `SomeType::new(...)` where
        // the path contains "GtsWildcard".  Arguments are allowed to contain wildcards.
        if let ExprKind::Call(func, args) = &expr.kind {
            if is_gts_wildcard_new_call(func) {
                SKIP_SPANS.with(|spans| {
                    let mut spans = spans.borrow_mut();
                    for arg in args {
                        collect_nested_spans(arg, &mut spans);
                    }
                });
                // Validate args (including nested literals) as wildcard-allowed
                // patterns and return early.
                for arg in args {
                    self.validate_nested_gts_strings(cx, arg, true);
                }
                return;
            }
        }

        // ── Phase 2: skip if this expression was marked ────────────────────────
        let should_skip = SKIP_SPANS.with(|spans| spans.borrow().contains(&expr.span));
        if should_skip {
            return;
        }

        self.check_gts_make_instance_id_call(cx, expr);

        // Check if this is a method call - handle resource_pattern and with_pattern specially
        if let ExprKind::MethodCall(method_call) = &expr.kind {
            let method_name = method_call.seg.ident.name.as_str();
            // Already validated in Phase 1 via validate_nested_gts_strings
            if method_name == "resource_pattern"
                || method_name == "with_pattern"
                || method_name == "resolve_to_uuids"
            {
                return;
            }

            // Check arguments of other method calls normally
            for arg in &method_call.args {
                self.check_gts_string_literal(cx, arg);
            }
            return;
        }

        // For non-method-call expressions, check normally
        self.check_gts_string_literal(cx, expr);
    }
}

/// Recursively collect spans from all sub-expressions so that deeply nested
/// string literals (e.g. inside `&["gts...".to_owned()]`) are included in the
/// skip set.
fn collect_nested_spans(expr: &Expr, spans: &mut HashSet<Span>) {
    spans.insert(expr.span);
    match &expr.kind {
        ExprKind::MethodCall(mc) => {
            collect_nested_spans(&mc.receiver, spans);
            for arg in &mc.args {
                collect_nested_spans(arg, spans);
            }
        }
        ExprKind::AddrOf(_, _, inner) => {
            collect_nested_spans(inner, spans);
        }
        ExprKind::Array(elements) => {
            for elem in elements {
                collect_nested_spans(elem, spans);
            }
        }
        ExprKind::Call(func, args) => {
            collect_nested_spans(func, spans);
            for arg in args {
                collect_nested_spans(arg, spans);
            }
        }
        ExprKind::Tup(elements) => {
            for elem in elements {
                collect_nested_spans(elem, spans);
            }
        }
        ExprKind::Paren(inner) => {
            collect_nested_spans(inner, spans);
        }
        _ => {}
    }
}

/// Returns `true` if `func_expr` is a path call of the form `GtsWildcard::new`
/// (or `gts::GtsWildcard::new`, `<anything>::GtsWildcard::new`, etc.).
///
/// We check that:
/// 1. The expression is a `Path` with at least two segments.
/// 2. The last segment is named `new`.
/// 3. At least one other segment is named `GtsWildcard`.
fn is_gts_wildcard_new_call(func_expr: &Expr) -> bool {
    let ExprKind::Path(_, path) = &func_expr.kind else {
        return false;
    };
    let segments = &path.segments;
    if segments.len() < 2 {
        return false;
    }
    let last = segments.last().unwrap();
    if last.ident.name.as_str() != "new" {
        return false;
    }
    segments
        .iter()
        .any(|seg| seg.ident.name.as_str() == "GtsWildcard")
}

impl De0901GtsStringPattern {
    fn check_gts_make_instance_id_call(&self, cx: &EarlyContext<'_>, expr: &Expr) {
        let ExprKind::Call(func, args) = &expr.kind else {
            return;
        };

        if args.len() != 1 {
            return;
        }

        let Some(arg0) = args.get(0) else {
            return;
        };

        let Some(arg_str) = Self::string_lit_value(arg0) else {
            return;
        };

        // Detect `...::gts_make_instance_id("...")`
        let ExprKind::Path(_, path) = &func.kind else {
            return;
        };

        let Some(last) = path.segments.last() else {
            return;
        };

        if last.ident.name.as_str() != "gts_make_instance_id" {
            return;
        }

        self.validate_instance_id_segment(cx, expr.span, arg_str);
    }

    /// Recursively traverse an expression tree and validate any GTS string
    /// literals found within. Mirrors the structure of `collect_nested_spans`
    /// so that every string literal that would be skip-listed is also validated.
    fn validate_nested_gts_strings(
        &self,
        cx: &EarlyContext<'_>,
        expr: &Expr,
        allow_wildcards: bool,
    ) {
        // If this is a string literal, validate it directly
        if Self::string_lit_value(expr).is_some() {
            self.check_gts_string_literal_with_wildcard_flag(cx, expr, allow_wildcards);
            return;
        }
        // Otherwise, recurse into sub-expressions
        match &expr.kind {
            ExprKind::MethodCall(mc) => {
                self.validate_nested_gts_strings(cx, &mc.receiver, allow_wildcards);
                for arg in &mc.args {
                    self.validate_nested_gts_strings(cx, arg, allow_wildcards);
                }
            }
            ExprKind::AddrOf(_, _, inner) => {
                self.validate_nested_gts_strings(cx, inner, allow_wildcards);
            }
            ExprKind::Array(elements) => {
                for elem in elements {
                    self.validate_nested_gts_strings(cx, elem, allow_wildcards);
                }
            }
            ExprKind::Call(_, args) => {
                for arg in args {
                    self.validate_nested_gts_strings(cx, arg, allow_wildcards);
                }
            }
            ExprKind::Tup(elements) => {
                for elem in elements {
                    self.validate_nested_gts_strings(cx, elem, allow_wildcards);
                }
            }
            ExprKind::Paren(inner) => {
                self.validate_nested_gts_strings(cx, inner, allow_wildcards);
            }
            _ => {}
        }
    }

    fn check_gts_string_literal(&self, cx: &EarlyContext<'_>, expr: &Expr) {
        self.check_gts_string_literal_with_wildcard_flag(cx, expr, false);
    }

    fn check_gts_string_literal_with_wildcard_flag(
        &self,
        cx: &EarlyContext<'_>,
        expr: &Expr,
        allow_wildcards: bool,
    ) {
        if let Some(s) = Self::string_lit_value(expr) {
            let s = s.trim();

            // Option 1: String starts with "gts." - validate directly
            if s.starts_with("gts.") {
                if allow_wildcards {
                    self.validate_any_gts_id_allow_wildcards(cx, expr.span, s);
                } else {
                    self.validate_any_gts_id(cx, expr.span, s);
                }
                return;
            }

            // Option 2: String contains ":" - this is a permission string format
            // Permission strings ALWAYS allow wildcards in their GTS parts
            if s.contains(':') {
                for part in s.split(':') {
                    if part.trim().starts_with("gts.") {
                        self.validate_any_gts_id_allow_wildcards(cx, expr.span, part.trim());
                        break; // Only validate the first GTS part found
                    }
                }
            }
        }
    }

    fn string_lit_value(expr: &Expr) -> Option<&str> {
        match &expr.kind {
            ExprKind::Lit(lit) => match lit.kind {
                LitKind::Str | LitKind::StrRaw(_) => Some(lit.symbol.as_str()),
                _ => None,
            },
            _ => None,
        }
    }

    fn check_struct_to_gts_schema_attr(&self, cx: &EarlyContext<'_>, attr: &Attribute) {
        let AttrKind::Normal(normal_attr) = &attr.kind else {
            return;
        };

        // We only care about #[struct_to_gts_schema(...)]
        if normal_attr.item.path.segments.len() != 1
            || normal_attr.item.path.segments[0].ident.name.as_str() != "struct_to_gts_schema"
        {
            return;
        }

        let Some(items) = normal_attr.item.meta_item_list() else {
            return;
        };

        for nested in items {
            let Some(mi) = nested.meta_item() else {
                continue;
            };

            if mi.path.segments.len() != 1 || mi.path.segments[0].ident.name.as_str() != "schema_id"
            {
                continue;
            }

            let Some(val) = mi.value_str() else {
                continue;
            };

            self.validate_schema_id(cx, mi.span, val.as_str());
        }
    }

    /// Validate a GTS schema_id using GtsOps::parse_id()
    /// schema_id must be a valid GTS type schema (ending with ~)
    fn validate_schema_id(&self, cx: &EarlyContext<'_>, span: rustc_span::Span, s: &str) {
        let s = s.trim();

        // Wildcards are NOT allowed in schema_id
        if s.contains('*') {
            cx.span_lint(DE0901_GTS_STRING_PATTERN, span, |diag| {
                diag.primary_message(format!("wildcards are not allowed in schema_id: '{}' (DE0901)", s));
                diag.note("Wildcards (*) are only allowed in permission strings, not in schema_id attributes");
                diag.help("Use concrete type names in schema_id");
            });
            return;
        }

        // Use GtsOps::parse_id() for validation - it gives us parsed segments
        let result = GtsOps::parse_id(s);

        if !result.ok {
            cx.span_lint(DE0901_GTS_STRING_PATTERN, span, |diag| {
                diag.primary_message(format!("invalid GTS schema_id: '{}' (DE0901)", s));
                diag.note(result.error);
                diag.help("Example: gts.x.core.events.type.v1~");
            });
            return;
        }

        // Ensure it's actually a schema (type), not an instance
        if result.is_schema != Some(true) {
            cx.span_lint(DE0901_GTS_STRING_PATTERN, span, |diag| {
                diag.primary_message(format!(
                    "schema_id must be a type schema, not an instance: '{}' (DE0901)",
                    s
                ));
                diag.note("schema_id must end with '~' to indicate it's a type schema");
                diag.help("Example: gts.x.core.events.type.v1~");
            });
        }
    }

    fn validate_instance_id_segment(&self, cx: &EarlyContext<'_>, span: rustc_span::Span, s: &str) {
        let s = s.trim();

        // `gts_make_instance_id` accepts a single *segment id* (no `gts.` prefix),
        // so we must not validate it as a full GTS ID.
        // If the input contains delimiters for chained ids / permission strings,
        // it is not a single segment.
        if s.contains('~') || s.contains(':') {
            cx.span_lint(DE0901_GTS_STRING_PATTERN, span, |diag| {
                diag.primary_message(format!(
                    "gts_make_instance_id expects a single GTS segment, got: '{}' (DE0901)",
                    s
                ));
                diag.help("Example: vendor.package.sku.abc.v1");
            });
            return;
        }

        if s.contains('*') {
            cx.span_lint(DE0901_GTS_STRING_PATTERN, span, |diag| {
                diag.primary_message(format!(
                    "wildcards are not allowed in instance id segments: '{}' (DE0901)",
                    s
                ));
                diag.help("Example: vendor.package.sku.abc.v1");
            });
            return;
        }

        if let Err(e) = GtsIdSegment::new(0, 0, s) {
            cx.span_lint(DE0901_GTS_STRING_PATTERN, span, |diag| {
                diag.primary_message(format!("invalid GTS segment: '{}' (DE0901)", s));
                diag.note(e.to_string());
                diag.help("Example: vendor.package.sku.abc.v1");
            });
        }
    }

    fn validate_any_gts_id(&self, cx: &EarlyContext<'_>, span: rustc_span::Span, s: &str) {
        let s = s.trim();

        // Wildcards are NOT allowed in regular GTS strings (only in permission strings)
        if s.contains('*') {
            cx.span_lint(DE0901_GTS_STRING_PATTERN, span, |diag| {
                diag.primary_message(format!("invalid GTS string (wildcards not allowed): '{}' (DE0901)", s));
                diag.note("Wildcards (*) are only allowed in permission strings, not in regular GTS identifiers");
                diag.help("Use concrete type names");
            });
            return;
        }

        let result = GtsOps::parse_id(s);

        if !result.ok {
            cx.span_lint(DE0901_GTS_STRING_PATTERN, span, |diag| {
                diag.primary_message(format!("invalid GTS string: '{}' (DE0901)", s));
                diag.note(result.error);
            });
        }
    }

    fn validate_any_gts_id_allow_wildcards(
        &self,
        cx: &EarlyContext<'_>,
        span: rustc_span::Span,
        s: &str,
    ) {
        let s = s.trim();

        // For resource_pattern calls, we allow wildcards but still validate the GTS structure
        let result = GtsOps::parse_id(s);

        if !result.ok {
            cx.span_lint(DE0901_GTS_STRING_PATTERN, span, |diag| {
                diag.primary_message(format!("invalid GTS string: '{}' (DE0901)", s));
                diag.note(result.error);
            });
        }
    }
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
        lint_utils::test_comment_annotations_match_stderr(
            &ui_dir,
            "DE0901",
            "invalid GTS", // Matches both "invalid GTS string" and "invalid GTS schema_id string"
        );
    }
}
