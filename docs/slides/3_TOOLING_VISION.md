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

| | You cannot… |
|---|---|
| **P1** | create a correct repository — only copy one |
| **P2** | reproduce CI |
| **P3** | reproduce the SDLC pipeline |
| **P4** | stay current |
| **P5** | tell whether a repository is still correct |
| **P6** | take only part of the conventions |

---

## Three more for the people working in one

| | You cannot… |
|---|---|
| **P7** | state the complete set of parts a gear needs — *gear developer* |
| **P8** | know which gear versions work together — *application developer* |
| **P9** | leave `gears-rust` and keep any of it — *both* |

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
| Installing conventions | Installing software — the platform, the runner image, the package manager |
| A command you run | Running a bot, hosting a service, opening pull requests |
| Checking that a check is wired | Deciding that check's verdict |

One report may carry all of it. **Aggregating a verdict is not recomputing it** —
every finding is attributed, and the tool never answers on an owner's behalf.

---

## The promise, and its limits

| | It promises | It does **not** promise |
|---|---|---|
| **Setup** | The chosen conventions and their wiring are in place, each pinned; anything missing is named | A green build |
| **Validation** | A truthful answer inside its footprint, with gaps in its own knowledge visible | That the repository is *good* |
| **Update** | Pins and managed files move reproducibly; what it cannot apply is reported as **not applied** | That every change applies by itself |

**A configured check is not a passing check.**
An unrun check is never reported as a passing one.

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
