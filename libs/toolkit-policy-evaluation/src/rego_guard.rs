//! Structural guards applied to a Rego document before anything walks its
//! syntax tree recursively.
//!
//! `regorus` parses, checks, analyses and evaluates a document with
//! recursive descent, and builds left-associative operator and reference
//! chains (`0 + 1 + 1 ...`, `input.a.a ...`) in loops that nest one level per
//! link without any limit of their own. A document of ordinary size can
//! therefore describe a tree deep enough to overflow the thread stack — an
//! abort, not an error — in the parser's post-parse checks, the analyser, the
//! interpreter, the facility's own introspection or even when the tree is
//! dropped. `regorus`' recursive-descent parser also disambiguates array,
//! object and set literals from comprehensions by trying one parse and
//! backtracking to the other, so a bracket nested inside a bracket of the
//! same kind can cost exponential time in the nesting depth **before** a tree
//! — deep or not — ever exists to bound. Guards keep every such walk, and
//! that backtracking, shallow:
//!
//! 1. [`check_source`] runs **before** the document reaches `regorus`. It
//!    lexes the source with `regorus`' own lexer (no parsing, no recursion)
//!    and computes two conservative token-level bounds: the deepest
//!    simultaneous nesting of `[`, `{`, `(` and `set(` literals, refused above
//!    [`MAX_BRACKET_NESTING`], and an upper bound of the tree depth the
//!    parser would build, refused above [`MAX_AST_DEPTH`]. The bracket check
//!    runs first, since it is what keeps the parser's own backtracking out of
//!    exponential territory; the chain bound protects the parser itself,
//!    whose unary-minus and `every` recursion is not covered by its own
//!    nesting limit.
//! 2. [`check_modules`] runs on the parsed modules **before** any recursive
//!    walk (introspection, analysis, evaluation). It measures the actual tree
//!    depth iteratively, with an explicit stack, and refuses the document above
//!    [`MAX_AST_DEPTH`].
//!
//! Every recursive walk in the facility runs only on trees that passed both
//! guards, so its recursion depth is at most [`MAX_AST_DEPTH`] levels.
//!
//! A third guard bounds the recursion the interpreter performs *across*
//! rules: evaluating a rule evaluates, on the same thread stack, every rule
//! and function it references. [`check_rule_dependencies`] builds the
//! dependency graph between the document's rules and functions iteratively,
//! refuses any cycle (the interpreter does not detect recursion through
//! functions and overflows the stack on it) and refuses a longest dependency
//! chain above [`MAX_RULE_DEPENDENCY_DEPTH`].

use std::collections::{BTreeMap, BTreeSet};

use regorus::Source;
use regorus::unstable::{
    Expr, Lexer, Literal, Module, Query, Ref, Rule, RuleHead, Token, TokenKind,
};

use crate::backend::CompileError;

/// Maximum nesting depth of a Rego document's syntax tree, counted in
/// expression and query levels. Documents nested deeper are refused with
/// [`CompileError::Unsupported`] by both syntax validation and compilation.
pub const MAX_AST_DEPTH: usize = 128;

/// Maximum simultaneous nesting of bracket literals (`[`, `{`, `(`, and
/// `set(`, each counted as one level) a document may contain. Refused with
/// [`CompileError::Unsupported`] by both syntax validation and compilation,
/// **before** [`MAX_AST_DEPTH`] is even evaluated.
///
/// `regorus`' recursive-descent parser disambiguates an array literal from an
/// array comprehension (and, worse, an object literal from a set literal from
/// an object comprehension) by first *trying* to parse the bracket's content
/// as a comprehension and, on failure, backtracking to reparse it as a
/// literal. For a bracket nested inside another bracket of the same kind,
/// that retry itself contains a nested retry, so the work roughly doubles per
/// nesting level — independent of [`MAX_AST_DEPTH`], which bounds the tree
/// built by a *successful* parse, not the work a failed, backtracked parse
/// path may have already done getting there.
///
/// Measured in a debug build (`cargo test`, unoptimized) by timing
/// `regorus::Engine::add_policy` directly on `condition := ` followed by `n`
/// concentric literals, the worse of the array (`[[[...]]]`) and object
/// (`{"a": {"a": ...}}`) shapes:
///
/// | nesting `n` | array    | object   |
/// |------------:|---------:|---------:|
/// |           8 |   1.4 ms |   1.0 ms |
/// |          10 |   1.8 ms |   3.6 ms |
/// |          11 |   3.6 ms | **7.3 ms** |
/// |          12 |   7.0 ms |  14.5 ms |
/// |          14 |  27.5 ms |  57.5 ms |
/// |          16 | 110.5 ms | 229.1 ms |
/// |          18 | 440.8 ms | 922.2 ms |
/// |          20 |    1.8 s |    3.7 s |
///
/// The cost roughly doubles every level, confirming the backtracking above. A
/// parenthesised expression (`(((...)))`) is not exponential on its own —
/// `regorus` parses it in flat, microsecond time at every depth measured —
/// but it counts toward the same cap because it composes with the array and
/// object shapes above (`[(...)]`, `{"a": (...)}`) and `regorus`' own
/// `max_expr_depth` (32 by default) would otherwise be reached only after
/// this exponential cost was already paid.
///
/// `n = 10` is the largest nesting whose worst case (object, 3.6 ms) stays
/// under the ~5 ms budget; `n = 11` already exceeds it (7.3 ms). A document
/// at the cap still compiles in low-single-digit milliseconds.
pub const MAX_BRACKET_NESTING: usize = 10;

/// Reserved words that start an independent part of a rule or statement
/// (a new subtree), so the expression chain before them ends there. Never
/// treated as such directly after `.`, where a reserved word is a field name.
const SEPARATING_KEYWORDS: &[&str] = &[
    "as", "contains", "default", "else", "every", "if", "import", "not", "package", "some", "with",
];

/// Reserved words that extend an expression chain by one level.
const CHAINING_KEYWORDS: &[&str] = &["in", "or"];

/// Weight of a unary minus: the parser spends several times the stack of a
/// binary operator on each nested prefix `-`, so it is counted double.
const UNARY_MINUS_WEIGHT: usize = 2;

fn too_deep() -> CompileError {
    CompileError::Unsupported {
        message: format!("document nesting exceeds the maximum supported depth of {MAX_AST_DEPTH}"),
    }
}

fn too_deep_brackets() -> CompileError {
    CompileError::Unsupported {
        message: format!(
            "document nests `[`, `{{`, `(` or `set(` literals more than {MAX_BRACKET_NESTING} \
             levels deep, which the parser cannot handle without excessive backtracking; the \
             maximum supported bracket nesting is {MAX_BRACKET_NESTING}"
        ),
    }
}

// ---- token-level bound -----------------------------------------------------------

/// Refuses `source` when its bracket nesting exceeds [`MAX_BRACKET_NESTING`]
/// or its token-level chain-nesting bound exceeds [`MAX_AST_DEPTH`]. Evaluates
/// and parses nothing.
///
/// The bracket check runs first and independently of [`MAX_AST_DEPTH`]: it
/// catches documents that would make `regorus` itself spend exponential time
/// backtracking through nested comprehension/literal disambiguation, long
/// before the resulting tree — if the parse ever finished — would be deep
/// enough to trip the chain-nesting bound.
///
/// # Errors
///
/// - [`CompileError::Syntax`] if `regorus` refuses to load the source text
///   (size or line-count limits);
/// - [`CompileError::Unsupported`] if the bracket nesting exceeds
///   [`MAX_BRACKET_NESTING`] or the chain bound exceeds [`MAX_AST_DEPTH`].
pub fn check_source(document_name: &str, source: &str) -> Result<(), CompileError> {
    let source = Source::from_contents(document_name.to_owned(), source.to_owned())
        .map_err(|err| syntax_error(&err))?;
    let bounds = source_bounds(&source);
    if bounds.max_bracket_nesting > MAX_BRACKET_NESTING {
        return Err(too_deep_brackets());
    }
    if bounds.chain > MAX_AST_DEPTH {
        return Err(too_deep());
    }
    Ok(())
}

/// One bracket level (`(`, `[`, `{` or `set(`), or the whole file.
#[derive(Debug, Default)]
struct Frame {
    /// Chain-extending tokens since the last separator in this frame.
    segment: usize,
    /// Deepest bound of a frame closed inside the current segment.
    nested: usize,
    /// Deepest segment bound seen in this frame so far.
    deepest: usize,
}

impl Frame {
    /// Ends the current segment: what follows is an independent subtree.
    fn separate(&mut self) {
        self.deepest = self.deepest.max(self.segment.saturating_add(self.nested));
        self.segment = 0;
        self.nested = 0;
    }

    fn bound(&self) -> usize {
        self.deepest
            .max(self.segment.saturating_add(self.nested))
            .saturating_add(1)
    }
}

/// What a token does to the bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    Open,
    Close,
    Separate,
    Extend(usize),
    None,
}

/// A classified token.
#[derive(Debug, Clone, Copy)]
struct Class {
    effect: Effect,
    /// The token can end an operand, so a line break after it may end the
    /// statement.
    ends_term: bool,
    /// The token continues the expression of the previous line when it
    /// starts a line.
    continues: bool,
}

/// The facts about the previous token the classification needs.
#[derive(Debug, Clone, Copy)]
struct Previous {
    line: u32,
    ends_term: bool,
    is_dot: bool,
}

fn classify(kind: &TokenKind, text: &str, previous: Option<Previous>) -> Class {
    let after_dot = previous.is_some_and(|p| p.is_dot);
    let after_term = previous.is_some_and(|p| p.ends_term);
    let class = |effect, ends_term, continues| Class {
        effect,
        ends_term,
        continues,
    };
    match kind {
        TokenKind::Symbol => match text {
            "(" | "[" | "{" => class(Effect::Open, false, true),
            ")" | "]" | "}" => class(Effect::Close, true, false),
            "," | ";" => class(Effect::Separate, false, false),
            "-" if !after_term => class(Effect::Extend(UNARY_MINUS_WEIGHT), false, true),
            _ => class(Effect::Extend(1), false, true),
        },
        // `set(` is lexed as one identifier token and closed by a `)`.
        TokenKind::Ident if text == "set(" => class(Effect::Open, false, true),
        TokenKind::Ident if !after_dot && SEPARATING_KEYWORDS.contains(&text) => {
            class(Effect::Separate, false, false)
        }
        TokenKind::Ident if !after_dot && CHAINING_KEYWORDS.contains(&text) => {
            class(Effect::Extend(1), false, true)
        }
        // A literal such as `-1` after an operand is a subtraction link.
        TokenKind::Number if text.starts_with(['-', '.']) => class(Effect::Extend(1), true, true),
        _ => class(Effect::None, true, false),
    }
}

fn close_frame(frames: &mut Vec<Frame>) {
    if frames.len() > 1
        && let Some(inner) = frames.pop()
        && let Some(outer) = frames.last_mut()
    {
        outer.nested = outer.nested.max(inner.bound());
    }
}

/// The two token-level bounds [`source_bounds`] computes in one lexer pass.
struct SourceBounds {
    /// Conservative upper bound of the syntax-tree depth `source` parses
    /// into, checked against [`MAX_AST_DEPTH`].
    chain: usize,
    /// Deepest simultaneous nesting of `[`, `{`, `(` and `set(` literals,
    /// checked against [`MAX_BRACKET_NESTING`].
    max_bracket_nesting: usize,
}

/// Computes [`SourceBounds`] for `source`.
///
/// Every token that can add a tree level (an operator, `.`, an opening
/// bracket, `in`) extends the current chain by one; a bracket level adds its
/// own bound on top of the chain it sits in; `,`, `;`, statement-starting
/// reserved words and line breaks that end a statement start an independent
/// chain. A line break ends a statement only after an operand and before a
/// token that cannot continue an expression — exactly where the parser
/// itself would stop the chain. Independently, every opening bracket token
/// also counts as one level of bracket nesting, tracked as the deepest
/// simultaneous depth of the frame stack.
///
/// Lexing stops at the first lexer error; the bounds of the prefix before it
/// are returned, which cover everything the parser reaches before failing.
fn source_bounds(source: &Source) -> SourceBounds {
    let mut lexer = Lexer::new(source);
    let mut frames = vec![Frame::default()];
    let mut previous: Option<Previous> = None;
    let mut max_bracket_nesting: usize = 0;

    while let Ok(Token(kind, span)) = lexer.next_token() {
        if kind == TokenKind::Eof {
            break;
        }
        let text = span.text();
        let class = classify(&kind, text, previous);

        if let Some(p) = previous
            && span.line > p.line
            && p.ends_term
            && !class.continues
            && let Some(frame) = frames.last_mut()
        {
            frame.separate();
        }

        match class.effect {
            Effect::Open => {
                if let Some(frame) = frames.last_mut() {
                    frame.segment = frame.segment.saturating_add(1);
                }
                frames.push(Frame::default());
                // The base frame (index 0) is the whole file, not a bracket,
                // so the bracket nesting is the stack depth minus one.
                max_bracket_nesting = max_bracket_nesting.max(frames.len().saturating_sub(1));
            }
            Effect::Close => close_frame(&mut frames),
            Effect::Separate => {
                if let Some(frame) = frames.last_mut() {
                    frame.separate();
                }
            }
            Effect::Extend(weight) => {
                if let Some(frame) = frames.last_mut() {
                    frame.segment = frame.segment.saturating_add(weight);
                }
            }
            Effect::None => {}
        }

        previous = Some(Previous {
            line: span.line,
            ends_term: class.ends_term,
            is_dot: kind == TokenKind::Symbol && text == ".",
        });
    }

    while frames.len() > 1 {
        close_frame(&mut frames);
    }
    SourceBounds {
        chain: frames.last().map_or(0, Frame::bound),
        max_bracket_nesting,
    }
}

// ---- parsed-tree depth -----------------------------------------------------------

/// A syntax-tree node that counts as one depth level.
#[derive(Clone, Copy)]
pub(super) enum Node<'a> {
    Expr(&'a Expr),
    Query(&'a Query),
}

/// Refuses parsed `modules` whose syntax tree is deeper than
/// [`MAX_AST_DEPTH`] expression and query levels. Iterative: the walk uses an
/// explicit stack, never the thread stack.
///
/// # Errors
///
/// [`CompileError::Unsupported`] if any module exceeds [`MAX_AST_DEPTH`].
pub fn check_modules(modules: &[Ref<Module>]) -> Result<(), CompileError> {
    let mut stack: Vec<(Node<'_>, usize)> = Vec::new();
    let mut children: Vec<Node<'_>> = Vec::new();
    for module in modules {
        stack.push((Node::Expr(&module.package.refr), 1));
        for import in &module.imports {
            stack.push((Node::Expr(&import.refr), 1));
        }
        for rule in &module.policy {
            rule_roots(rule, &mut children);
        }
        stack.extend(children.drain(..).map(|root| (root, 1)));
    }

    while let Some((node, depth)) = stack.pop() {
        if depth > MAX_AST_DEPTH {
            return Err(too_deep());
        }
        match node {
            Node::Expr(expr) => expr_children(expr, &mut children),
            Node::Query(query) => query_children(query, &mut children),
        }
        let child_depth = depth.saturating_add(1);
        stack.extend(children.drain(..).map(|child| (child, child_depth)));
    }
    Ok(())
}

/// The top-level nodes of one rule definition: its head reference, function
/// arguments, values and bodies.
///
/// Shared with the introspection walk (`AstScan` in `rego.rs`), so the depth
/// guard measures exactly the tree the scan inspects.
pub(super) fn rule_roots<'a>(rule: &'a Rule, out: &mut Vec<Node<'a>>) {
    let mut push = |expr: &'a Expr| out.push(Node::Expr(expr));
    match rule {
        Rule::Spec { head, bodies, .. } => {
            match head {
                RuleHead::Compr { refr, assign, .. } => {
                    push(refr);
                    if let Some(assign) = assign {
                        push(&assign.value);
                    }
                }
                RuleHead::Set { refr, key, .. } => {
                    push(refr);
                    if let Some(key) = key {
                        push(key);
                    }
                }
                RuleHead::Func {
                    refr, args, assign, ..
                } => {
                    push(refr);
                    for arg in args {
                        push(arg);
                    }
                    if let Some(assign) = assign {
                        push(&assign.value);
                    }
                }
            }
            for body in bodies {
                if let Some(assign) = &body.assign {
                    push(&assign.value);
                }
            }
            out.extend(bodies.iter().map(|body| Node::Query(&body.query)));
        }
        Rule::Default {
            refr, args, value, ..
        } => {
            push(refr);
            for arg in args {
                push(arg);
            }
            push(value);
        }
    }
}

/// The direct children of every statement of `query`, `with` targets and
/// replacements included.
fn query_children<'a>(query: &'a Query, out: &mut Vec<Node<'a>>) {
    for stmt in &query.stmts {
        literal_children(&stmt.literal, out);
        for modifier in &stmt.with_mods {
            out.push(Node::Expr(&modifier.refr));
            out.push(Node::Expr(&modifier.r#as));
        }
    }
}

/// The direct children of one statement's literal, without its `with`
/// modifiers (the introspection walk treats those itself).
pub(super) fn literal_children<'a>(literal: &'a Literal, out: &mut Vec<Node<'a>>) {
    match literal {
        Literal::SomeVars { .. } => {}
        Literal::SomeIn {
            key,
            value,
            collection,
            ..
        } => {
            if let Some(key) = key {
                out.push(Node::Expr(key));
            }
            out.push(Node::Expr(value));
            out.push(Node::Expr(collection));
        }
        Literal::Expr { expr, .. } | Literal::NotExpr { expr, .. } => {
            out.push(Node::Expr(expr));
        }
        Literal::Every { domain, query, .. } => {
            out.push(Node::Expr(domain));
            out.push(Node::Query(query));
        }
    }
}

/// The direct children of `expr`. The one exhaustive match over [`Expr`] in
/// the facility: a new expression kind fails the build here, and both the
/// depth guard and the introspection walk see its children. The walk handles
/// variables, calls and references itself and every other kind through this
/// function, so a new kind that names a builtin or reads `input` needs a
/// case there too.
pub(super) fn expr_children<'a>(expr: &'a Expr, out: &mut Vec<Node<'a>>) {
    match expr {
        Expr::String { .. }
        | Expr::RawString { .. }
        | Expr::Number { .. }
        | Expr::Bool { .. }
        | Expr::Null { .. }
        | Expr::Var { .. } => {}
        Expr::Array { items, .. } | Expr::Set { items, .. } => {
            out.extend(items.iter().map(|item| Node::Expr(item)));
        }
        Expr::Object { fields, .. } => {
            for (_, key, value) in fields {
                out.push(Node::Expr(key));
                out.push(Node::Expr(value));
            }
        }
        Expr::ArrayCompr { term, query, .. } | Expr::SetCompr { term, query, .. } => {
            out.push(Node::Expr(term));
            out.push(Node::Query(query));
        }
        Expr::ObjectCompr {
            key, value, query, ..
        } => {
            out.push(Node::Expr(key));
            out.push(Node::Expr(value));
            out.push(Node::Query(query));
        }
        Expr::Call { fcn, params, .. } => {
            out.push(Node::Expr(fcn));
            out.extend(params.iter().map(|param| Node::Expr(param)));
        }
        Expr::UnaryExpr { expr, .. } | Expr::RefDot { refr: expr, .. } => {
            out.push(Node::Expr(expr));
        }
        Expr::RefBrack { refr, index, .. } => {
            out.push(Node::Expr(refr));
            out.push(Node::Expr(index));
        }
        Expr::BinExpr { lhs, rhs, .. }
        | Expr::BoolExpr { lhs, rhs, .. }
        | Expr::ArithExpr { lhs, rhs, .. }
        | Expr::AssignExpr { lhs, rhs, .. } => {
            out.push(Node::Expr(lhs));
            out.push(Node::Expr(rhs));
        }
        Expr::Membership {
            key,
            value,
            collection,
            ..
        } => {
            if let Some(key) = key {
                out.push(Node::Expr(key));
            }
            out.push(Node::Expr(value));
            out.push(Node::Expr(collection));
        }
    }
}

// ---- rule dependency depth -------------------------------------------------------

/// Maximum length, counted in rules and functions, of a chain in which each
/// rule or function references the next. Documents with a longer chain, or
/// with any dependency cycle, are refused with [`CompileError::Unsupported`]
/// by both syntax validation and compilation.
///
/// The interpreter evaluates a referenced rule or function recursively on
/// the caller's stack, so this chain length — not the syntax-tree depth — is
/// what bounds its stack use across rules. On a 2 MiB thread stack a debug
/// build aborts at a chain of about 100 minimal rules (a release build at
/// 1 000 – 3 000); 32 leaves a threefold margin for chains whose every link
/// also carries an expression nested up to [`MAX_AST_DEPTH`] levels deep.
pub const MAX_RULE_DEPENDENCY_DEPTH: usize = 32;

/// Maximum evaluation depth of a dependency chain: the sum, over every rule
/// or function on the chain, of [`RULE_LINK_WEIGHT`] plus the depth of its
/// syntax tree (in the levels [`MAX_AST_DEPTH`] bounds).
///
/// A reference can sit at the bottom of a deeply nested expression, so the
/// interpreter's stack at the next rule is the sum of both kinds of nesting
/// along the chain; neither limit alone bounds it.
pub const MAX_EVALUATION_DEPTH: usize = 512;

/// Evaluation levels one rule or function link weighs on top of its syntax
/// tree: the interpreter's frames for evaluating a rule cost about as much
/// stack as this many expression levels.
const RULE_LINK_WEIGHT: usize = 12;

/// Full path of a rule or function below `data`, as static components
/// (`data.p.a.b` is `["p", "a", "b"]`).
type RulePath = Vec<String>;

/// The static prefix of a reference and the parts of it that are evaluated.
pub(super) struct Reference<'a> {
    /// Root variable name followed by the static segments up to the first
    /// dynamic one; `None` when the root is not a variable.
    pub(super) path: Option<Vec<String>>,
    /// Dynamic indices, and a non-variable root, which are expressions in
    /// their own right.
    pub(super) dynamic: Vec<&'a Expr>,
}

/// Splits a `Var`, `RefDot` or `RefBrack` chain into its static path and its
/// evaluated parts. Iterative along the chain. Shared by the dependency
/// graph and the introspection walk.
pub(super) fn reference(expr: &Expr) -> Reference<'_> {
    let mut segments: Vec<Option<String>> = Vec::new();
    let mut dynamic = Vec::new();
    let mut current = expr;
    let root = loop {
        match current {
            Expr::RefDot { refr, field, .. } => {
                segments.push(Some(field.0.text().to_owned()));
                current = refr;
            }
            Expr::RefBrack { refr, index, .. } => {
                match index.as_ref() {
                    Expr::String { value, .. } | Expr::RawString { value, .. } => {
                        segments.push(value.as_string().ok().map(ToString::to_string));
                    }
                    other => {
                        segments.push(None);
                        dynamic.push(other);
                    }
                }
                current = refr;
            }
            Expr::Var { span, .. } => break Some(span.text().to_owned()),
            other => {
                dynamic.push(other);
                break None;
            }
        }
    };
    let path = root.map(|root| {
        std::iter::once(root)
            .chain(segments.into_iter().rev().map_while(|segment| segment))
            .collect()
    });
    Reference { path, dynamic }
}

/// Name resolution context of one module.
struct Scope {
    /// The module's package below `data`.
    package: Vec<String>,
    /// Import alias -> full imported path, rooted at `data` or `input`.
    imports: BTreeMap<String, Vec<String>>,
}

/// The import aliases of `module`: alias (the `as` name, else the last path
/// component) -> full imported path, for imports rooted at `data` or
/// `input`. Shared by the dependency graph and the introspection walk.
pub(super) fn import_aliases(module: &Module) -> BTreeMap<String, Vec<String>> {
    let mut imports = BTreeMap::new();
    for import in &module.imports {
        let Some(path) = reference(&import.refr).path else {
            continue;
        };
        if !matches!(path.first().map(String::as_str), Some("data" | "input")) {
            continue;
        }
        let alias = import
            .r#as
            .as_ref()
            .map(|span| span.text().to_owned())
            .or_else(|| path.last().cloned());
        if let Some(alias) = alias {
            imports.insert(alias, path);
        }
    }
    imports
}

impl Scope {
    fn of(module: &Module) -> Self {
        Self {
            package: reference(&module.package.refr).path.unwrap_or_default(),
            imports: import_aliases(module),
        }
    }

    /// Every rule path `raw` (a reference path starting with its root
    /// variable) may denote. Over-approximates: a bare name is taken as a
    /// same-package rule even where a local variable shadows it, and an
    /// import alias also as a like-named rule.
    fn resolve(&self, raw: &[String]) -> Vec<RulePath> {
        let Some((head, rest)) = raw.split_first() else {
            return Vec::new();
        };
        let below_data = |full: &[String]| match full.split_first() {
            Some((root, rest)) if root == "data" => Some(rest.to_vec()),
            _ => None,
        };
        match head.as_str() {
            "data" => vec![rest.to_vec()],
            "input" => Vec::new(),
            _ => {
                let mut candidates = vec![self.package.iter().chain(raw).cloned().collect()];
                if let Some(import) = self.imports.get(head) {
                    let full: Vec<String> = import.iter().chain(rest).cloned().collect();
                    candidates.extend(below_data(&full));
                }
                candidates
            }
        }
    }
}

/// The rules and functions of a document and which ones each references.
struct DependencyGraph {
    names: Vec<RulePath>,
    edges: Vec<Vec<usize>>,
    /// Deepest syntax tree among each node's definitions.
    tree_depth: Vec<usize>,
}

/// The longest dependency chain below a node, measured both ways.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Chain {
    /// Rules and functions on the chain.
    links: usize,
    /// [`RULE_LINK_WEIGHT`] plus the syntax-tree depth, summed over the chain.
    weight: usize,
}

/// Deepest syntax tree of one rule definition, in the levels
/// [`check_modules`] counts. Iterative.
fn rule_tree_depth(rule: &Rule) -> usize {
    let mut children: Vec<Node<'_>> = Vec::new();
    rule_roots(rule, &mut children);
    let mut stack: Vec<(Node<'_>, usize)> = children.drain(..).map(|root| (root, 1)).collect();
    let mut deepest = 0;
    while let Some((node, depth)) = stack.pop() {
        deepest = deepest.max(depth);
        match node {
            Node::Expr(expr) => expr_children(expr, &mut children),
            Node::Query(query) => query_children(query, &mut children),
        }
        let child_depth = depth.saturating_add(1);
        stack.extend(children.drain(..).map(|child| (child, child_depth)));
    }
    deepest
}

/// The static path a rule is defined at, relative to its package, and the
/// head expressions its definition evaluates.
fn rule_head(rule: &Rule) -> (&Expr, Vec<&Expr>) {
    match rule {
        Rule::Spec { head, bodies, .. } => {
            let mut evaluated: Vec<&Expr> = Vec::new();
            let refr = match head {
                RuleHead::Compr { refr, assign, .. } => {
                    evaluated.extend(assign.iter().map(|assign| &*assign.value));
                    refr
                }
                RuleHead::Set { refr, key, .. } => {
                    evaluated.extend(key.iter().map(|key| &**key));
                    refr
                }
                // Arguments are patterns binding local variables.
                RuleHead::Func { refr, assign, .. } => {
                    evaluated.extend(assign.iter().map(|assign| &*assign.value));
                    refr
                }
            };
            for body in bodies {
                evaluated.extend(body.assign.iter().map(|assign| &*assign.value));
            }
            (refr, evaluated)
        }
        Rule::Default { refr, value, .. } => (refr, vec![value]),
    }
}

/// Collects the reference paths an expression tree evaluates. Iterative.
fn collect_references(roots: Vec<Node<'_>>, out: &mut Vec<Vec<String>>) {
    let mut stack = roots;
    while let Some(node) = stack.pop() {
        match node {
            Node::Query(query) => query_children(query, &mut stack),
            Node::Expr(expr @ (Expr::Var { .. } | Expr::RefDot { .. } | Expr::RefBrack { .. })) => {
                let found = reference(expr);
                out.extend(found.path);
                stack.extend(found.dynamic.into_iter().map(Node::Expr));
            }
            Node::Expr(Expr::Call { fcn, params, .. }) => {
                stack.push(Node::Expr(fcn));
                stack.extend(params.iter().map(|param| Node::Expr(param)));
            }
            Node::Expr(other) => expr_children(other, &mut stack),
        }
    }
}

impl DependencyGraph {
    fn build(modules: &[Ref<Module>]) -> Self {
        let mut index: BTreeMap<RulePath, usize> = BTreeMap::new();
        let mut names: Vec<RulePath> = Vec::new();
        let mut definitions: Vec<(usize, usize, &Rule)> = Vec::new();
        let scopes: Vec<Scope> = modules.iter().map(|module| Scope::of(module)).collect();

        for (module_index, (module, scope)) in modules.iter().zip(&scopes).enumerate() {
            for rule in &module.policy {
                let (refr, _) = rule_head(rule);
                let path: RulePath = scope
                    .package
                    .iter()
                    .cloned()
                    .chain(reference(refr).path.unwrap_or_default())
                    .collect();
                let node = *index.entry(path.clone()).or_insert_with(|| {
                    names.push(path);
                    names.len() - 1
                });
                definitions.push((node, module_index, rule));
            }
        }

        let mut tree_depth = vec![0; names.len()];
        for &(node, _, rule) in &definitions {
            if let Some(depth) = tree_depth.get_mut(node) {
                *depth = (*depth).max(rule_tree_depth(rule));
            }
        }

        let mut edges: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); names.len()];
        let mut raw: Vec<Vec<String>> = Vec::new();
        for (node, module_index, rule) in definitions {
            let (refr, evaluated) = rule_head(rule);
            let mut roots: Vec<Node<'_>> = reference(refr)
                .dynamic
                .into_iter()
                .map(Node::Expr)
                .collect();
            roots.extend(evaluated.into_iter().map(Node::Expr));
            if let Rule::Spec { bodies, .. } = rule {
                roots.extend(bodies.iter().map(|body| Node::Query(&body.query)));
            }
            raw.clear();
            collect_references(roots, &mut raw);
            let Some(scope) = scopes.get(module_index) else {
                continue;
            };
            for path in raw.iter().flat_map(|raw| scope.resolve(raw)) {
                if let Some(targets) = edges.get_mut(node) {
                    targets.extend(matching_rules(&index, &path));
                }
            }
        }
        Self {
            names,
            edges: edges.into_iter().map(|e| e.into_iter().collect()).collect(),
            tree_depth,
        }
    }

    /// The longest dependency chain, by links and by weight (each maximised
    /// independently), or the node of a cycle. Iterative depth-first search.
    fn longest_chain(&self) -> Result<Chain, usize> {
        let mut chains: Vec<Option<Chain>> = vec![None; self.edges.len()];
        let mut on_path = vec![false; self.edges.len()];
        let mut longest = Chain::default();
        for start in 0..self.edges.len() {
            if chains[start].is_some() {
                continue;
            }
            let mut path: Vec<(usize, usize)> = vec![(start, 0)];
            on_path[start] = true;
            while let Some(&(node, next)) = path.last() {
                let successors = &self.edges[node];
                if let Some(&child) = successors.get(next) {
                    if let Some(top) = path.last_mut() {
                        top.1 = next.saturating_add(1);
                    }
                    if on_path[child] {
                        return Err(child);
                    }
                    if chains[child].is_none() {
                        on_path[child] = true;
                        path.push((child, 0));
                    }
                } else {
                    let below = successors.iter().filter_map(|&child| chains[child]).fold(
                        Chain::default(),
                        |acc, chain| Chain {
                            links: acc.links.max(chain.links),
                            weight: acc.weight.max(chain.weight),
                        },
                    );
                    let own_weight = RULE_LINK_WEIGHT
                        .saturating_add(self.tree_depth.get(node).copied().unwrap_or(0));
                    let own = Chain {
                        links: below.links.saturating_add(1),
                        weight: below.weight.saturating_add(own_weight),
                    };
                    chains[node] = Some(own);
                    longest = Chain {
                        links: longest.links.max(own.links),
                        weight: longest.weight.max(own.weight),
                    };
                    on_path[node] = false;
                    path.pop();
                }
            }
        }
        Ok(longest)
    }
}

/// Every rule a reference to `path` evaluates: those defined at a prefix of
/// it (the reference reads into their value) and those defined below it (the
/// reference reads a document containing them).
fn matching_rules(index: &BTreeMap<RulePath, usize>, path: &[String]) -> Vec<usize> {
    let mut found: Vec<usize> = (1..=path.len())
        .filter_map(|len| {
            path.get(..len)
                .and_then(|prefix| index.get(prefix))
                .copied()
        })
        .collect();
    found.extend(
        index
            .range(path.to_vec()..)
            .take_while(|(key, _)| key.starts_with(path))
            .map(|(_, &node)| node),
    );
    found
}

/// Refuses documents whose rules and functions depend on each other in a
/// cycle, in a chain longer than [`MAX_RULE_DEPENDENCY_DEPTH`], or in a chain
/// whose [evaluation depth](MAX_EVALUATION_DEPTH) exceeds
/// [`MAX_EVALUATION_DEPTH`].
///
/// The graph over-approximates the interpreter's resolution (a bare name is
/// always taken as a same-package rule reference, even where a local
/// variable of the same name shadows it; `with` targets count as
/// references), so a chain or cycle it finds may be longer than the one
/// evaluated, never shorter.
///
/// # Errors
///
/// [`CompileError::Unsupported`] on a cycle or an over-long chain.
pub fn check_rule_dependencies(modules: &[Ref<Module>]) -> Result<(), CompileError> {
    let graph = DependencyGraph::build(modules);
    let unsupported = |message| Err(CompileError::Unsupported { message });
    match graph.longest_chain() {
        Err(node) => unsupported(format!(
            "rule `data.{}` depends on itself through its references; recursive rules and \
             functions are not supported",
            graph
                .names
                .get(node)
                .map(|path| path.join("."))
                .unwrap_or_default()
        )),
        Ok(chain) if chain.links > MAX_RULE_DEPENDENCY_DEPTH => unsupported(format!(
            "rules and functions depend on each other {} deep; the maximum supported rule \
             dependency depth is {MAX_RULE_DEPENDENCY_DEPTH}",
            chain.links
        )),
        Ok(chain) if chain.weight > MAX_EVALUATION_DEPTH => unsupported(format!(
            "a chain of rule dependencies nests {} evaluation levels; the maximum supported \
             evaluation depth is {MAX_EVALUATION_DEPTH}",
            chain.weight
        )),
        Ok(_) => Ok(()),
    }
}

/// [`CompileError::Syntax`] from a `regorus` load or parse failure, with the
/// position its message reports.
pub(super) fn syntax_error(err: &anyhow::Error) -> CompileError {
    let message = format!("{err:#}");
    let (line, column) = error_position(&message);
    CompileError::Syntax {
        message: message.trim().to_owned(),
        line,
        column,
    }
}

/// Extracts `line:column` from regorus' `--> <file>:<line>:<column>` marker.
pub(super) fn error_position(message: &str) -> (Option<u32>, Option<u32>) {
    message
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("--> "))
        .and_then(|location| {
            let mut parts = location.trim_end().rsplitn(3, ':');
            let column = parts.next()?.parse().ok()?;
            let line = parts.next()?.parse().ok()?;
            Some((Some(line), Some(column)))
        })
        .unwrap_or((None, None))
}
