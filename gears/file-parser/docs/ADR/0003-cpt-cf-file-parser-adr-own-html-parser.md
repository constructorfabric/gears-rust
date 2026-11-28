---
status: accepted
date: 2026-11-25
decision-makers: file-parser gear maintainers
---

# Read HTML with file-parser's own tokenizer and tree builder instead of html5ever

**ID**: `cpt-cf-file-parser-adr-own-html-parser`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [html5ever 0.40.1](#html5ever-0401)
  - [html5ever main](#html5ever-main)
  - [html5ever with a smaller input cap](#html5ever-with-a-smaller-input-cap)
  - [Own tokenizer and tree builder](#own-tokenizer-and-tree-builder)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

[ADR-0002](./0002-cpt-cf-file-parser-adr-own-document-parsers.md) replaced kreuzberg with own parsers. For HTML the usual choice is html5ever, Servo's parser, which implements the WHATWG parsing algorithm in full. file-parser chose its own smaller parser instead, and this record explains why with measurements.

file-parser reads untrusted uploads of up to 100 MiB (`max_file_size_mb`) on a blocking-pool thread. It needs text and structure: headings, paragraphs, lists, tables, links and images. It does not need a DOM that matches a browser's in every case.

## Decision Drivers

* Parse time stays linear in the input size for any input.
* `ParseLimits` are charged while parsing, not after a full tree is built.
* No recursion over input structure, so the parser works on a 2 MiB thread stack.
* Output structure is good enough for language models and search. Exact browser behaviour is not required.

## Considered Options

* html5ever 0.40.1, the latest release, with `markup5ever_rcdom`
* html5ever `main` (commit `492f52a02`), which adds a nesting limit of 512 ([servo/html5ever#811](https://github.com/servo/html5ever/pull/811))
* html5ever with a much smaller input cap for HTML
* Own tokenizer and tree builder

## Decision Outcome

Chosen option: "Own tokenizer and tree builder". It was the only option that parsed every hostile input in linear time. html5ever, both the release and `main`, takes seconds to tens of seconds on inputs of a few hundred kilobytes.

Measured on an Apple M5 Pro with release builds, one run per input and a 30 s cap. html5ever builds an `RcDom`. Our parser does more work: it parses the input and builds the output blocks. The inputs come from html5ever's own issue tracker. The table shows the largest of three sizes. Each size doubles the previous one.

| Input | Source | Size | html5ever 0.40.1 | html5ever `main` | own |
|---|---|---|---|---|---|
| `<svg>`, 64,000 `<g>`, then `</x>` ×64,000 | [#800](https://github.com/servo/html5ever/pull/800) | 437 KB | > 30 s | 28.3 s | 9 ms |
| `<p><b x{i}>t</p>` ×16,000 | [#812](https://github.com/servo/html5ever/issues/812) | 270 KB | > 30 s | 15.3 s, plus 11.4 s to drop the DOM | 5 ms |
| `<table>x</table>` ×160,000 | #812 | 2.5 MB | 6.8 s | 6.8 s | 49 ms |
| 2,000 `<body>` tags, 64 new attributes each | #812 | 1 MB | 5.8 s | 5.7 s | 7 ms |
| 64,000 nested `<ul><li>` | [#788](https://github.com/servo/html5ever/issues/788) | 500 KB | 26 s | 0.28 s | 9 ms |
| 64,000 nested `<div>`, then `</p>` ×64,000 | #800 | 562 KB | 23 s | 0.26 s | 9 ms |
| one tag with 80,000 attributes | [#804](https://github.com/servo/html5ever/pull/804) | 692 KB | 1.2 s | 1.2 s | 1 ms |
| `<template><object></template>` ×20,000, then `<b></b>` ×80,000 | [#801](https://github.com/servo/html5ever/issues/801) | 1.1 MB | 0.72 s | 0.72 s | 28 ms |

* On the first four rows, html5ever's time grows about four times each time the input doubles.
* Our parser's time about doubles with each size, both with the default limits and with the limits lifted.
* The nesting limit in `main` fixes deep nesting only.
* No parser crashed or overflowed its stack.

### Consequences

* The HTML parser is ours to maintain: about 2,900 lines in `src/infra/parsers/html/`.
* It follows the WHATWG algorithm only as far as text and structure need it: implied end tags, table scope, foster parenting, raw text and script states, `<template>` and `<select>`. Rare cases may give a different tree than a browser. The result is slightly worse Markdown, never a crash or unbounded work.
* Very large real pages may hit `max_html_nodes` (500,000 by default) before they are fully read. The single-page HTML standard (16 MB) is one such page.

### Confirmation

* Unit tests in `src/infra/parsers/html/tests.rs` cover the cases above and each limit.
* All 1,792 tree-construction inputs from html5lib-tests (the last version before they moved to web-platform-tests) parse without errors or panics. The resulting trees were not compared with the expected ones.

## Pros and Cons of the Options

### html5ever 0.40.1

* Good, because it is complete, maintained and widely used.
* Bad, because the inputs above take from seconds to over 30 s at 0.3–2.5 MB.
* Bad, because limits could only be checked on the finished `RcDom`, and dropping that DOM can itself take seconds.

### html5ever main

* Good, because the nesting limit removes the quadratic cost of deep nesting.
* Bad, because the other cases (#800 foreign content, #801, #804, #812) are still quadratic.
* Bad, because it is not released yet.

### html5ever with a smaller input cap

* Good, because it keeps a complete parser.
* Bad, because 270 KB already takes 15 s. A cap low enough to bound the worst case would reject many ordinary pages.

### Own tokenizer and tree builder

* Good, because every step is linear, and `ParseLimits` are checked while tokens are read.
* Good, because it builds the output directly with no intermediate DOM.
* Bad, because it covers only part of the WHATWG algorithm and must be maintained.

## More Information

Revisit this decision when an html5ever release handles all the inputs above in linear time. Each input is described in the linked issue and is a few lines of code to build.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-file-parser-constraint-parse-limits`: the HTML parser charges `ParseLimits` while it parses.
* `cpt-cf-file-parser-adr-own-document-parsers`: this record expands on why html5ever was rejected there.
