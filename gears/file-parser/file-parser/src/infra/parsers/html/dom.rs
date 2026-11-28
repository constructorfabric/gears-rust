//! A small DOM built from [`Tokenizer`] output, in linear time.
//!
//! This is not the HTML standard's tree construction: there is no adoption
//! agency, no foster parenting and, beyond self-closed `svg`/`math` content,
//! no foreign-content handling. It applies the implicit closes that matter
//! for text structure (a new `p`, `li`, `dt`/`dd`, `option`, cell or row
//! closing the previous one, a missing `tr` around cells), void elements,
//! and end tags that close up to the matching open element without crossing
//! a table, cell or caption (only `</template>` and `</select>` do). Cells,
//! captions and column groups outside a table are ignored. Elements nested
//! deeper than [`MAX_TREE_DEPTH`] are left out of the tree but their content
//! is kept in the deepest open element, so the tree stays shallow and the
//! text survives. Every token is handled in amortized constant time.

use std::collections::HashMap;

use crate::domain::error::{DomainError, ParseLimit};
use crate::infra::parsers::limits::limit_exceeded;

use super::tokenize::{Token, Tokenizer};

/// Deepest element nesting kept in the tree.
const MAX_TREE_DEPTH: usize = 512;

/// One node of the tree.
pub(super) enum Node {
    Element(Element),
    Text(String),
}

pub(super) struct Element {
    name: String,
    attrs: Vec<(&'static str, String)>,
}

impl Element {
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
}

struct Slot {
    node: Node,
    first_child: Option<usize>,
    last_child: Option<usize>,
    next_sibling: Option<usize>,
}

/// The parsed document; node 0 is the root `html` element.
pub(super) struct Dom {
    slots: Vec<Slot>,
    /// The first `title` outside `svg` and `math`.
    title: Option<usize>,
}

/// A node of a [`Dom`].
#[derive(Clone, Copy)]
pub(super) struct NodeRef<'a> {
    dom: &'a Dom,
    id: usize,
}

impl PartialEq for NodeRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && std::ptr::eq(self.dom, other.dom)
    }
}

impl<'a> NodeRef<'a> {
    pub(super) fn value(&self) -> &'a Node {
        &self.dom.slots[self.id].node
    }

    pub(super) fn children(&self) -> Children<'a> {
        Children {
            dom: self.dom,
            next: self.dom.slots[self.id].first_child,
        }
    }
}

pub(super) struct Children<'a> {
    dom: &'a Dom,
    next: Option<usize>,
}

impl<'a> Iterator for Children<'a> {
    type Item = NodeRef<'a>;

    fn next(&mut self) -> Option<NodeRef<'a>> {
        let id = self.next?;
        self.next = self.dom.slots[id].next_sibling;
        Some(NodeRef { dom: self.dom, id })
    }
}

impl Dom {
    pub(super) fn root(&self) -> NodeRef<'_> {
        NodeRef { dom: self, id: 0 }
    }

    /// The document title element: the first `title` outside `svg` and
    /// `math`, whose titles name their graphics.
    pub(super) fn title(&self) -> Option<NodeRef<'_>> {
        self.title.map(|id| NodeRef { dom: self, id })
    }

    /// Builds the tree of `html`, allowing at most `max_nodes` nodes.
    pub(super) fn parse(html: &str, max_nodes: usize) -> Result<Self, DomainError> {
        Ok(Builder::build(html, max_nodes)?.dom)
    }
}

impl Builder {
    fn build(html: &str, max_nodes: usize) -> Result<Self, DomainError> {
        let mut builder = Builder {
            dom: Dom {
                slots: vec![Slot {
                    node: Node::Element(Element {
                        name: "html".to_owned(),
                        attrs: Vec::new(),
                    }),
                    first_child: None,
                    last_child: None,
                    next_sibling: None,
                }],
                title: None,
            },
            stack: vec![0],
            segments: vec![Segment {
                at: 0,
                open: HashMap::new(),
            }],
            scopes: Vec::new(),
            foreign: 0,
            templates: 0,
            selects: 0,
            head_at: None,
            drop_next_text: false,
            max_nodes,
            #[cfg(test)]
            work: 0,
        };
        let mut tokens = Tokenizer::new(html);
        while let Some(token) = tokens.next() {
            match token {
                Token::Text(text) => builder.text(text)?,
                Token::Start {
                    name,
                    attrs,
                    self_closing,
                } => {
                    // A self-closed `<title/>` or `<style/>` in foreign
                    // content has no text to read.
                    if self_closing && builder.foreign > 0 {
                        tokens.skip_raw_text();
                    }
                    builder.start(name, attrs, self_closing)?;
                }
                Token::End { name } => builder.end(&name)?,
            }
        }
        Ok(builder)
    }
}

/// Elements that never have content.
fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
            | "basefont"
            | "bgsound"
            | "frame"
            | "image"
    )
}

/// Start tags that close an open `p` right on top of the stack.
fn closes_p(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "center"
            | "dd"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "li"
            | "listing"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "search"
            | "section"
            | "summary"
            | "table"
            | "ul"
    )
}

/// Elements whose boundaries separate words when they are flattened.
fn separates_words(name: &str) -> bool {
    closes_p(name)
        || matches!(
            name,
            "td" | "th" | "tr" | "tbody" | "thead" | "tfoot" | "caption"
        )
}

/// Whether a new `name` element closes the open element `top`.
fn closes(name: &str, top: &str) -> bool {
    match top {
        "p" => closes_p(name),
        "li" => name == "li",
        "dt" | "dd" => matches!(name, "dt" | "dd"),
        "option" => matches!(name, "option" | "optgroup"),
        "td" | "th" => matches!(name, "td" | "th" | "tr" | "tbody" | "thead" | "tfoot"),
        "tr" => matches!(name, "tr" | "tbody" | "thead" | "tfoot"),
        "thead" | "tbody" | "tfoot" => matches!(name, "tbody" | "thead" | "tfoot"),
        _ => false,
    }
}

struct Builder {
    dom: Dom,
    /// Open elements, root first.
    stack: Vec<usize>,
    /// The open elements split at each open table, cell and caption. An end
    /// tag closes only within the innermost segment, as in browsers, and
    /// cells and rows close through anything left open inside them.
    segments: Vec<Segment>,
    /// Open table, list and definition elements with their stack positions:
    /// a new item or term closes the nearest open one even under unclosed
    /// inline elements (`<li><b>a<li>b`), but not across a nested list or
    /// table.
    scopes: Vec<(Scope, usize)>,
    /// Open `svg` and `math` elements.
    foreign: usize,
    /// Open `template` and `select` elements, whose end tags close across
    /// segments.
    templates: usize,
    selects: usize,
    /// Stack position of an open `head`, which closes implicitly.
    head_at: Option<usize>,
    /// The next text token is the content of a raw-text element left out of
    /// the tree (too deep) and is not document text.
    drop_next_text: bool,
    max_nodes: usize,
    /// Stack entries looked at, for tests of the cost.
    #[cfg(test)]
    work: usize,
}

/// Open elements from one table, cell or caption (at stack position `at`)
/// up to the next.
struct Segment {
    at: usize,
    /// How many elements of each name are open in the segment, its first
    /// element not counted: end tags without a match are ignored without
    /// searching the stack.
    open: HashMap<String, usize>,
}

impl Segment {
    fn has(&self, name: &str) -> bool {
        self.open.get(name).is_some_and(|&n| n > 0)
    }
}

/// Elements that start a segment.
fn starts_segment(name: &str) -> bool {
    matches!(name, "table" | "td" | "th" | "caption")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Table,
    Cell,
    List,
    Item,
    DefList,
    Def,
}

fn scope_of(name: &str) -> Option<Scope> {
    Some(match name {
        "table" => Scope::Table,
        "td" | "th" => Scope::Cell,
        "ul" | "ol" | "menu" | "dir" => Scope::List,
        "li" => Scope::Item,
        "dl" => Scope::DefList,
        "dt" | "dd" => Scope::Def,
        _ => return None,
    })
}

/// Elements that may stay inside an open `head`; anything else closes it.
fn allowed_in_head(name: &str) -> bool {
    matches!(
        name,
        "head"
            | "base"
            | "basefont"
            | "bgsound"
            | "link"
            | "meta"
            | "noframes"
            | "noscript"
            | "script"
            | "style"
            | "template"
            | "title"
    )
}

/// Raw-text elements whose content is never document text.
fn is_skipped_raw(name: &str) -> bool {
    matches!(
        name,
        "script" | "style" | "noscript" | "iframe" | "noembed" | "noframes" | "title" | "textarea"
    )
}

impl Builder {
    fn top(&self) -> usize {
        self.stack.last().copied().unwrap_or(0)
    }

    fn name_of(&self, id: usize) -> &str {
        match &self.dom.slots[id].node {
            Node::Element(e) => &e.name,
            Node::Text(_) => "",
        }
    }

    fn append(&mut self, node: Node) -> Result<usize, DomainError> {
        if self.dom.slots.len() >= self.max_nodes {
            return Err(limit_exceeded(
                ParseLimit::MaxHtmlNodes,
                format!("more than {} elements and text nodes", self.max_nodes),
            ));
        }
        let parent = self.top();
        let id = self.dom.slots.len();
        self.dom.slots.push(Slot {
            node,
            first_child: None,
            last_child: None,
            next_sibling: None,
        });
        match self.dom.slots[parent].last_child {
            Some(last) => self.dom.slots[last].next_sibling = Some(id),
            None => self.dom.slots[parent].first_child = Some(id),
        }
        self.dom.slots[parent].last_child = Some(id);
        Ok(id)
    }

    fn text(&mut self, text: String) -> Result<(), DomainError> {
        if std::mem::take(&mut self.drop_next_text) || text.is_empty() {
            return Ok(());
        }
        // Text directly in `head` (not in its title or script) closes it.
        if let Some(head) = self.head_at
            && head + 1 == self.stack.len()
            && !text.bytes().all(|b| b.is_ascii_whitespace())
        {
            self.pop_to(head);
        }
        // Adjacent text joins the previous text node.
        let parent = self.top();
        if let Some(last) = self.dom.slots[parent].last_child
            && let Node::Text(prev) = &mut self.dom.slots[last].node
        {
            prev.push_str(&text);
            return Ok(());
        }
        self.append(Node::Text(text))?;
        Ok(())
    }

    fn pop(&mut self) {
        if self.stack.len() > 1
            && let Some(id) = self.stack.pop()
        {
            #[cfg(test)]
            {
                self.work += 1;
            }
            let position = self.stack.len();
            // Fields borrowed apart, so the name is not copied.
            let name = match &self.dom.slots[id].node {
                Node::Element(e) => e.name.as_str(),
                Node::Text(_) => "",
            };
            if self.segments.len() > 1 && self.segments.last().is_some_and(|s| s.at == position) {
                self.segments.pop();
            } else if let Some(n) = self.segments.last_mut().and_then(|s| s.open.get_mut(name)) {
                *n = n.saturating_sub(1);
            }
            match name {
                "svg" | "math" => self.foreign = self.foreign.saturating_sub(1),
                "template" => self.templates = self.templates.saturating_sub(1),
                "select" => self.selects = self.selects.saturating_sub(1),
                _ => {}
            }
            if self.scopes.last().is_some_and(|&(_, at)| at == position) {
                self.scopes.pop();
            }
            if self.head_at == Some(position) {
                self.head_at = None;
            }
        }
    }

    /// Pops the element at stack position `position` and everything above it.
    fn pop_to(&mut self, position: usize) {
        while self.stack.len() > position.max(1) {
            self.pop();
        }
    }

    /// Closes the open element of kind `scope` if it is the nearest scope entry.
    fn close_scope(&mut self, scope: Scope) {
        if let Some(&(kind, at)) = self.scopes.last()
            && kind == scope
        {
            self.pop_to(at);
        }
    }

    /// The name of the element that starts the innermost segment.
    fn segment_name(&self) -> &str {
        let at = self.segments.last().map_or(0, |s| s.at);
        self.stack.get(at).map_or("", |&id| self.name_of(id))
    }

    /// Closes an open cell or caption with everything open inside it.
    fn close_cell_or_caption(&mut self) {
        if matches!(self.segment_name(), "td" | "th" | "caption")
            && let Some(at) = self.segments.last().map(|s| s.at)
        {
            self.pop_to(at);
        }
    }

    /// Closes the innermost element named one of `names` if the innermost
    /// segment has one open, with everything above it.
    fn close_in_segment(&mut self, names: &[&str]) {
        if !self
            .segments
            .last()
            .is_some_and(|s| names.iter().any(|n| s.has(n)))
        {
            return;
        }
        while self.stack.len() > 1 {
            let matched = names.contains(&self.name_of(self.top()));
            self.pop();
            if matched {
                break;
            }
        }
    }

    fn start(
        &mut self,
        name: String,
        attrs: Vec<(&'static str, String)>,
        self_closing: bool,
    ) -> Result<(), DomainError> {
        if name == "html" {
            // Attributes of a repeated html tag go to the root.
            if let Node::Element(root) = &mut self.dom.slots[0].node {
                for (k, v) in attrs {
                    if root.attr(k).is_none() {
                        root.attrs.push((k, v));
                    }
                }
            }
            return Ok(());
        }
        self.drop_next_text = false;
        if let Some(head) = self.head_at
            && head + 1 == self.stack.len()
            && !allowed_in_head(&name)
        {
            self.pop_to(head);
        }
        match name.as_str() {
            // Outside a table these are ignored, as in browsers, and their
            // content stays in the open element; a cell still separates words.
            "td" | "th" | "caption" | "colgroup" if self.segments.len() == 1 => {
                if separates_words(&name) {
                    self.text(" ".to_owned())?;
                }
                return Ok(());
            }
            "td" | "th" => {
                self.close_cell_or_caption();
                self.close_in_segment(&["colgroup"]);
                // A cell right in a table gets the row it is missing.
                if self.segment_name() == "table"
                    && !self.segments.last().is_some_and(|s| s.has("tr"))
                {
                    self.start("tr".to_owned(), Vec::new(), false)?;
                }
            }
            "tr" => {
                self.close_cell_or_caption();
                self.close_in_segment(&["colgroup"]);
                self.close_in_segment(&["tr"]);
            }
            // A caption or column group also goes right in the table.
            "thead" | "tbody" | "tfoot" | "colgroup" | "caption" => {
                self.close_cell_or_caption();
                self.close_in_segment(&["colgroup"]);
                self.close_in_segment(&["tr"]);
                self.close_in_segment(&["thead", "tbody", "tfoot"]);
            }
            "li" => self.close_scope(Scope::Item),
            "dt" | "dd" => self.close_scope(Scope::Def),
            _ => {}
        }
        while self.stack.len() > 1 && closes(&name, self.name_of(self.top())) {
            self.pop();
        }
        // In `svg` and `math`, `/>` closes any element.
        let void = is_void(&name)
            || (self_closing && (self.foreign > 0 || matches!(name.as_str(), "svg" | "math")));
        let too_deep = self.stack.len() >= MAX_TREE_DEPTH;
        if too_deep && !void {
            // Left out of the tree; its content goes to the deepest element,
            // and a block boundary still separates words.
            if separates_words(&name) {
                self.text(" ".to_owned())?;
            }
            self.drop_next_text = is_skipped_raw(&name);
            return Ok(());
        }
        let id = self.append(Node::Element(Element {
            name: name.clone(),
            attrs,
        }))?;
        if name == "title" && self.foreign == 0 && self.dom.title.is_none() {
            self.dom.title = Some(id);
        }
        if !void {
            let position = self.stack.len();
            if let Some(scope) = scope_of(&name) {
                self.scopes.push((scope, position));
            }
            if name == "head" && self.head_at.is_none() {
                self.head_at = Some(position);
            }
            match name.as_str() {
                "svg" | "math" => self.foreign += 1,
                "template" => self.templates += 1,
                "select" => self.selects += 1,
                _ => {}
            }
            if starts_segment(&name) {
                self.segments.push(Segment {
                    at: position,
                    open: HashMap::new(),
                });
            } else if let Some(segment) = self.segments.last_mut() {
                *segment.open.entry(name).or_insert(0) += 1;
            }
            self.stack.push(id);
        }
        Ok(())
    }

    fn end(&mut self, name: &str) -> Result<(), DomainError> {
        self.drop_next_text = false;
        // `</br>` is a line break, as in browsers.
        if name == "br" {
            return self.start("br".to_owned(), Vec::new(), false);
        }
        // A table or cell left open in a template or select ends with it.
        let open = match name {
            "template" => self.templates,
            "select" => self.selects,
            _ => 0,
        };
        if open > 0 {
            while self.stack.len() > 1 {
                let matched = self.name_of(self.top()) == name;
                self.pop();
                if matched {
                    break;
                }
            }
            return Ok(());
        }
        while let Some(segment) = self.segments.last() {
            let at = segment.at;
            #[cfg(test)]
            {
                self.work += 1;
            }
            if segment.has(name) {
                self.close_in_segment(&[name]);
                return Ok(());
            }
            let first = self.segment_name();
            if at > 0 && first == name {
                self.pop_to(at);
                return Ok(());
            }
            // The end of a row, section or table closes an open cell first,
            // and the end of a table an open caption, if the element is open
            // in the enclosing segment.
            let through = match first {
                "td" | "th" => matches!(name, "tr" | "tbody" | "thead" | "tfoot" | "table"),
                "caption" => name == "table",
                _ => false,
            };
            let outer = self.segments.len().checked_sub(2).and_then(|i| {
                let outer = self.segments.get(i)?;
                Some((outer.has(name), self.stack.get(outer.at).copied()?))
            });
            match outer {
                Some((open, first_id)) if through && (open || self.name_of(first_id) == name) => {
                    self.pop_to(at);
                }
                _ => break,
            }
        }
        // Ignored. The end of a block left out of the tree (too deep) still
        // separates words.
        if self.stack.len() >= MAX_TREE_DEPTH && separates_words(name) {
            self.text(" ".to_owned())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tree as `name(children)` text, for compact assertions.
    fn shape(node: NodeRef<'_>) -> String {
        match node.value() {
            Node::Text(t) => format!("{t:?}"),
            Node::Element(e) => {
                let children: Vec<String> = node.children().map(shape).collect();
                format!("{}({})", e.name(), children.join(","))
            }
        }
    }

    fn tree(html: &str) -> String {
        shape(Dom::parse(html, 1_000_000).unwrap().root())
    }

    #[test]
    fn implicit_closes() {
        assert_eq!(tree("<p>a<p>b"), r#"html(p("a"),p("b"))"#);
        assert_eq!(tree("<ul><li>a<li>b</ul>"), r#"html(ul(li("a"),li("b")))"#);
        assert_eq!(tree("<p>x<div>y</div>"), r#"html(p("x"),div("y"))"#);
        assert_eq!(
            tree("<table><tr><td>1<td>2<tr><td>3</table>"),
            r#"html(table(tr(td("1"),td("2")),tr(td("3"))))"#
        );
        assert_eq!(tree("<dl><dt>t<dd>d</dl>"), r#"html(dl(dt("t"),dd("d")))"#);
    }

    #[test]
    fn end_tags_close_to_the_match_or_are_ignored() {
        assert_eq!(tree("<div><b>x</div>y"), r#"html(div(b("x")),"y")"#);
        assert_eq!(tree("<div>x</span></zz>y</div>"), r#"html(div("xy"))"#);
        assert_eq!(tree("a</br>b"), r#"html("a",br(),"b")"#);
    }

    #[test]
    fn void_elements_and_html_attributes() {
        let dom = Dom::parse(
            r#"<html lang="en"><img alt="a">t<html lang="fr" title="x">"#,
            100,
        )
        .unwrap();
        let Node::Element(root) = dom.root().value() else {
            panic!("root is an element");
        };
        assert_eq!(root.attr("lang"), Some("en"));
        assert_eq!(root.attr("title"), Some("x"));
        assert_eq!(shape(dom.root()), r#"html(img(),"t")"#);
    }

    #[test]
    fn nesting_past_the_cap_is_kept_as_content() {
        let depth = 100_000;
        let html = format!("{}x{}", "<div>".repeat(depth), "</div>".repeat(depth));
        let dom = Dom::parse(&html, 1_000_000).unwrap();
        // The root, MAX_TREE_DEPTH - 1 divs and the text, which survives.
        assert_eq!(dom.slots.len(), MAX_TREE_DEPTH + 1);
        // Dropped divs leave word separators around it; the converter
        // collapses them.
        assert!(
            matches!(dom.slots.last().map(|s| &s.node), Some(Node::Text(t)) if t.trim() == "x")
        );
    }

    #[test]
    fn unmatched_end_tags_cost_nothing() {
        // Searching the open elements for each end tag would look at 400
        // entries per tag here.
        let tags = 400_000;
        let html = format!("{}{}x", "<span>".repeat(400), "</zz>".repeat(tags));
        let builder = Builder::build(&html, 1_000_000).unwrap();
        assert!(builder.work <= tags + 400, "{}", builder.work);
        // Nothing was closed: the text is in the innermost span.
        assert_eq!(builder.stack.len(), 401);
        assert_eq!(builder.dom.slots.len(), 402);
    }

    #[test]
    fn misnested_formatting_does_not_multiply_nodes() {
        let html = "<p><b id=i></p>".repeat(8_000);
        let dom = Dom::parse(&html, 1_000_000).unwrap();
        assert!(dom.slots.len() <= 16_001, "{}", dom.slots.len());
    }

    #[test]
    fn node_limit() {
        let err = Dom::parse(&"<p>x".repeat(100), 50).err().expect("limit");
        assert!(err.to_string().contains("max_html_nodes"), "{err}");
    }

    #[test]
    fn head_closes_implicitly() {
        assert_eq!(
            tree("<html><head><title>T</title><body><p>Hello</p>"),
            r#"html(head(title("T")),body(p("Hello")))"#
        );
        assert_eq!(
            tree("<head><meta charset=utf-8><title>T</title><p>Hello"),
            r#"html(head(meta(),title("T")),p("Hello"))"#
        );
        assert_eq!(
            tree("<head>\n<title>T</title> text"),
            "html(head(\"\\n\",title(\"T\")),\" text\")"
        );
    }

    #[test]
    fn cells_items_and_terms_close_through_unclosed_inline_elements() {
        assert_eq!(
            tree("<table><tr><td><font>a<td><font>b<tr><td>c</table>"),
            r#"html(table(tr(td(font("a")),td(font("b"))),tr(td("c"))))"#
        );
        assert_eq!(
            tree("<ul><li><b>a<li>b</ul>"),
            r#"html(ul(li(b("a")),li("b")))"#
        );
        assert_eq!(
            tree("<dl><dt><b>x<dd>y</dl>"),
            r#"html(dl(dt(b("x")),dd("y")))"#
        );
        // A cell of a nested table does not close the outer cell.
        assert_eq!(
            tree("<table><tr><td>a<table><tr><td>b</table>c<td>d</table>"),
            r#"html(table(tr(td("a",table(tr(td("b"))),"c"),td("d"))))"#
        );
    }

    #[test]
    fn end_tags_stay_inside_tables_and_cells() {
        // `</font>` and `</div>` close nothing in a cell they are outside of.
        assert_eq!(
            tree("<font><table><tr><td>a</font><td>b</table></font>c"),
            r#"html(font(table(tr(td("a"),td("b")))),"c")"#
        );
        // A second `</td>` in a table nested in the cell is ignored.
        assert_eq!(
            tree("<table><tr><td><table><tr><td>a</td></td>b</table>c</table>"),
            r#"html(table(tr(td(table(tr(td("a"),"b")),"c"))))"#
        );
        // The end of a row, section or table closes the cell around it.
        assert_eq!(
            tree("<table><tbody><tr><td><b>a</tr><tr><td>b</tbody></table>"),
            r#"html(table(tbody(tr(td(b("a"))),tr(td("b")))))"#
        );
        assert_eq!(
            tree("<table><caption>c</table>x"),
            r#"html(table(caption("c")),"x")"#
        );
        // A cell outside a table is ignored, and so is `</tr>`.
        assert_eq!(tree("<td>a</tr>b"), r#"html(" ab")"#);
        // `</template>` closes a table left open in it.
        assert_eq!(
            tree("<template><table><tr><td>a</template>b"),
            r#"html(template(table(tr(td("a")))),"b")"#
        );
    }

    #[test]
    fn rows_cells_colgroups_and_captions_close_implicitly() {
        assert_eq!(
            tree("<table><colgroup><col><tr><td>a</table>"),
            r#"html(table(colgroup(col()),tr(td("a"))))"#
        );
        assert_eq!(
            tree("<table><caption>c<tbody><tr><td>a</table>"),
            r#"html(table(caption("c"),tbody(tr(td("a")))))"#
        );
        assert_eq!(
            tree("<table><tr><td><ul><li>x<td>y<tr><td>z</table>"),
            r#"html(table(tr(td(ul(li("x"))),td("y")),tr(td("z"))))"#
        );
        assert_eq!(
            tree("<table><td>a<td>b</table>"),
            r#"html(table(tr(td("a"),td("b"))))"#
        );
        // A row nested deeper in the segment is still closed.
        assert_eq!(
            tree("<table><tr><ul><tr><td>a</table>"),
            r#"html(table(tr(ul()),tr(td("a"))))"#
        );
    }

    #[test]
    fn raw_text_left_out_of_the_tree_is_dropped() {
        let html = format!("{}<script>var x;</script>kept", "<div>".repeat(600));
        let dom = Dom::parse(&html, 1_000_000).unwrap();
        let text: String = dom
            .slots
            .iter()
            .filter_map(|s| match &s.node {
                Node::Text(t) => Some(t.as_str()),
                Node::Element(_) => None,
            })
            .collect();
        assert!(!text.contains("var"), "{text:?}");
        assert!(text.contains("kept"));
    }

    #[test]
    fn title_outside_svg_and_math() {
        let dom = Dom::parse(
            "<body><svg><title>Icon</title></svg><title>t</title></body>",
            100,
        )
        .unwrap();
        assert_eq!(dom.title().map(shape).as_deref(), Some(r#"title("t")"#));
        let dom = Dom::parse("<math><title>m</title></math><p>x", 100).unwrap();
        assert!(dom.title().is_none());
    }
}
