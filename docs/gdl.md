# GDL

GDL (Gears Description Language) is the declarative DSL a `gear.gdl` and a `product.gdl` are
written in. A file states facts; the resolver decides.

**The reference is in the Gearbox repository:**
[`constructorfabric/gearbox` → `docs/gdl.md`](https://github.com/constructorfabric/gearbox/blob/main/docs/gdl.md).
It is beside the interpreter that evaluates GDL. It has the grammar, the lexicon, the
namespaces, each parameter, the diagnostics table, a full template of `gear.gdl` and two
worked examples.

It went there because three things it points at exist only there -- the generated diagnostics
catalogue it defers to for the full code list, and the four source files that hold the
vocabulary it describes -- and because its examples are executed by a test in that repository,
which reads them out of the document itself.

What stays here is the corpus it describes: the `gear.gdl` files beside the gear crates.
[GEARBOX.md](GEARBOX.md) explains what those files are for and what is asked of gear authors.
