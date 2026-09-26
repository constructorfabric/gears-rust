# GDL

GDL (Gears Description Language) is the declarative DSL a `gear.gdl` and a `product.gdl` are
written in. A file states facts; the resolver decides.

**The reference moved.** It now lives at `docs/gdl.md` in the **`gearbox`** checkout, beside
the interpreter that evaluates it: grammar, lexicon, namespaces, every parameter signature,
the curated diagnostics table, and the two worked examples.

It went there because three things it points at exist only there -- the generated diagnostics
catalogue it defers to for the full code list, and the four source files that hold the
vocabulary it describes -- and because its examples are executed by a test in that repository,
which reads them out of the document itself.

What stays here is the corpus it describes: the `gear.gdl` files beside the gear crates.
[GEARBOX.md](GEARBOX.md) explains what those files are for and what is asked of gear authors.
