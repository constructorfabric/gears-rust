---
marp: true
title: "Gears tooling — Vision"
description: "One tool to set a Gears repository up, keep it conformant, and carry its conventions forward"
theme: rose-pine-moon
paginate: true
---

<!-- _class: lead -->

# Gears tooling

### One tool to set a repository up, keep it correct, and carry its conventions forward

Vision · PR #5729 · `docs/arch/tooling/VISION.md`

*Set up → check → update → check again*

---

## The problem, in one sentence

> A team creates a Gears repository by **copying `gears-rust`**,
> and nothing afterwards reports that the copy went **stale or wrong**.

Both halves hurt, and the second one outlives the first.

- Copying is expensive **once**.
- Having no signal is expensive **forever**.

---

## Six problems for whoever owns the repository

| | Name | You cannot… |
|---|---|---|
| **P1** | Bootstrap | create a correct repository — only copy one |
| **P2** | CI reproduction | reproduce CI |
| **P3** | SDLC reproduction | reproduce the SDLC pipeline |
| **P4** | Staying current | stay current |
| **P5** | Conformance signal | tell whether a repository is still correct |
| **P6** | All or nothing | take only part of the conventions |

---

## Three more for the people working in one

| | Name | You cannot… |
|---|---|---|
| **P7** | Gear completeness | state the complete set of parts a gear needs — *gear developer* |
| **P8** | Version alignment | know which gear versions work together — *application developer* |
| **P9** | Portability | leave `gears-rust` and keep any of it — *both* |

P1–P6 cost a team **once per repository**.
P7–P9 cost somebody **every working day**.

---

## The evidence is in this repository

- `artifacts.toml` holds **37 `[[ignore]]` blocks** — 13 of them record that a module
  has no PRD or DESIGN. Conformance debt kept as suppressions, because nothing else records it.
- The `toolkit-pr-review` prompt exists **five times**, once per agent vendor.
  **Four copies no longer match.** Nobody chose that, and nothing reported it.
- `cfs` stopped with an unclear error on an uninitialised `guidelines/DNA` —
  which is why PR #5326 has to add `ensure-dna.py`.

None of this was found by a check. It was found by looking.

---

## Five mechanisms cover parts of it

| Mechanism | Stops at |
|---|---|
| `cargo gears` | no make driver, CI, lint config or SDLC setup |
| `gears.mk` | a person copies the file by hand |
| Studio + kit | not the repository, the build or CI |
| Gearbox + GDL | local paths, no version resolution |
| PR #5326 | adapts the kit to a project, not a project to conventions |

Different owners, release cycles and manifests — and **nothing owns the repository itself**.

---

## The answer: three responsibilities

**Setup** installs the conventions a project chose.

**Validation** reports when the repository stops matching them.

**The update loop** carries them forward.

> Setup happens **once**.
> The other two happen for the **life of the project**.

The product is the arc, not any one step: **set up → check → update → check again**.

---

## What a convention is, concretely

**Installed and pinned** — build rules (`gears.mk`, 32 targets) · CI (30 workflows, 4 composite actions, 24 pipeline scripts) · Rust lint and toolchain (6 config files) · dependency baseline (256 deps, ~125 lints) · document-first development (the Studio kit) · agent configuration (5 vendors, one source) · platform guidelines (`guidelines/DNA`) · repository boilerplate

**Checked only — nothing is installed** — repository layout · gear layout: the crate, `gear.gdl`, the documents, the workspace entry, the publish settings

> A project takes the subset it wants — that is **P6**.
> Two conventions write no file at all, which is why **the check is not an afterthought to setup**.

---

## The one idea everything follows from

> A project **declares its conventions and pins their versions.**
> **It does not copy them.**

In that order of preference, best first:

1. **Reference** — upstream keeps the file, the repository holds a pin. Nothing here can drift.
2. **Generate** — upstream supplies a rule; a stale file repairs itself on the next run.
3. **Copy** — last resort. Replaced whole under a version check, **never merged**.

A pin has to designate content that **cannot change afterwards**.
A name somebody can move later is not a pin.

---

## When a project needs something different

**Usually it is a missing parameter, not a difference.** `gears.mk` settings are overridable from the project's own make file; `Gears.toml` carries this project's dylint skip list. The file stays managed, the project supplies a value.

**When the convention did not anticipate it**, the project declares an exception and the tool stops writing that file — but not reporting it. You still see the version you diverged from, and when upstream moves past it.

> An exception costs **automatic application**, not visibility.

A convention that collects many exceptions is a convention that needs a parameter.

---

## So why does this need a tool at all?

A reference works for the **build rules** and for **CI**.

It does **not** work for `clippy.toml`, `rustfmt.toml`, `deny.toml`,
`rust-toolchain.toml`, `.config/nextest.toml`, `.cargo/config.toml`.

> Rust has no `extends`, no parent manifest, no remote configuration inheritance.
> `[workspace.lints]` stops at the workspace.

Those files have to be copied — which is exactly what `gears-rust` does today,
and why no existing scaffolding platform solves this for us.

**The mandate:** reference what can be referenced, generate what can be generated,
and shrink copying to the residue Rust leaves no alternative for.

---

## Boundaries — what it will not do

| It owns | Somebody else owns |
|---|---|
| Repository-level conformance: **presence and placement** | **Content and correctness** — `cfs`, Gearbox, dylint, `cargo-deny` |
| **Configuring** a validator or a runner | **Running** it — that is `make`, CI, or the developer |
| Validating its own files | Judging the code — dylint, `cargo-deny`, Gearbox, `cfs` |
| A command you run | Running a bot, hosting a service, opening pull requests |

It writes `clippy.toml` and the workflow that calls clippy. It never calls clippy.
**It replaces no runner and no validator**, and never stands between a project and its own results.

---

## The promise, and its limits

| | It promises | It does **not** promise |
|---|---|---|
| **Setup** | Every convention installed and pinned, update wiring handed over, anything missing named | A green build |
| **Validation** | A truthful answer inside its footprint, with gaps in its own knowledge visible | That the repository is *good* |
| **Update** | Pins and managed files move reproducibly; what it cannot apply is reported as **not applied** | That every change applies by itself |

**A configured check is not a passing check**, and an unrun one is never reported as passing.
A repository on 1.2 while upstream ships 1.3 is **conformant, not drifting**.

---

## What success looks like

Success is visible in **what stops happening**.

- Nobody assembles a repository by reading `gears-rust` and copying from it.
- Drift is found by a check on a pull request, not by an incident.
- A developer learns what a gear is missing **before** pushing, not in review.
- An upgrade lands in a week when nobody touched the repository.
- A convention improves in **one place** and reaches everyone that pinned it.

Two ways to fail even if every feature ships:
teams keep copying by hand — or a green check comes to mean *"nothing ran"*.

---

## What we have to decide

1. **Where do the conventions live?** A pin needs something to point at. Nothing publishes `gears.mk` under tags today, and CI is not reusable. **This vision is inert until somebody does that work** — and it changes `gears-rust` first.
2. **Which direction is the default?** PR #5326's facades adapt the kit to the build system a project already has. This vision gives a project the build system it should have. Both can exist; only one can be the default.
3. **How granular is the convention menu?** Too fine and no two projects compare;
   too coarse and the choice is useless.
4. **Where is the boundary** with `cargo gears`, `cfs` and Gearbox?

---

<!-- _class: lead -->

# Discussion

**`docs/arch/tooling/VISION.md`** · PR #5729

*Prefer a simple, safe and reproducible path over a universal one.
Automate the routine, expose the exceptional,
and never hide uncertainty behind a successful exit code.*
