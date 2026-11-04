# Gears tooling — Vision

> Written 2026-11-04. Sections 2 and 3 describe a state, and they date. The counts, the versions and
> the work in progress that they name are a record of that date, not a claim about now. Section 5
> cites outside projects, and those move too.
>
> The direction does not date. Building the tool answers this document; it does not change it. A
> change to the direction is a new decision, and it needs a reason that is not "we shipped it".

## At a glance

**The problem.** A team creates a Gears repository by copying `gears-rust`, and nothing afterwards
reports that the copy went stale or wrong.

**The answer.** One tool sets a repository up, checks that it still matches the conventions the
project chose, and carries those conventions forward. The conventions are referenced by version, not
copied, so the ordinary upgrade is a change of pins. A maintainer runs the tool to set a repository
up and keep it correct, a gear developer runs it before a push to learn what is missing, and a bot
runs it on a schedule to deliver an upgrade that nobody asked for.

**The limit.** The tool checks only what it installed or what the project declared. It installs no
software, runs no bot, and never merges an upstream change into a file that a project edited.

**What this is, and is not.** It gives the direction and the limits of the scope. It does not give
the architecture, the commands, the file formats or the implementation; those belong in `DESIGN.md`
and in the ADRs after it. It is written for the maintainers of `gears-rust`, `cargo-gears`,
`gearbox` and the `studio-kit-gears` kit.

**How to read it.** The whole document takes about twenty-five minutes, and section 4 is half of
that. Skip section 3 if you already know what `cargo gears`, `gears.mk`, Studio and Gearbox each
cover today. If you only need the decisions, read this block and section 6.

## Table of Contents

- [1. Terms](#1-terms)
- [2. Problems](#2-problems)
- [3. How far they are covered today](#3-how-far-they-are-covered-today)
- [4. Vision](#4-vision)
- [4.1 Modular by construction](#41-modular-by-construction)
- [4.2 Scope](#42-scope)
- [5. What the successful scaffolds share](#5-what-the-successful-scaffolds-share)
- [6. Principles](#6-principles)
- [7. What success looks like](#7-what-success-looks-like)
- [8. Assumptions and constraints](#8-assumptions-and-constraints)
- [9. Risks](#9-risks)

---

## 1. Terms

This document uses these terms with one meaning each.

| Term | Meaning |
|---|---|
| **convention** | A rule or a configuration that a Gears project follows. |
| **the tool** | The Gears CLI that this document describes. |
| **upstream** | The source that owns a convention and releases new versions of it. |
| **pin** | A version number that a repository records for a convention. |
| **install** | The tool puts a convention into a repository: it writes the pin, and any file that the convention generates or copies. The word never means obtaining a program. |
| **reference** | Upstream keeps the file. The repository holds only a pin. An upgrade changes the pin and nothing else, because the content is not here to change. |
| **generate** | Upstream supplies a rule, not a file. The tool derives the content, and it can derive the same content again at any time. An upgrade runs the rule again. |
| **copy** | Upstream supplies the file itself. The tool cannot derive the content. An upgrade replaces the whole file with a newer version from upstream. |
| **managed file** | A file that the tool generated or copied. A person must not edit it. |
| **bot** | An automation that runs the tool without attention and then delivers the result, usually as a pull request. The integrating project selects it and runs it. |
| **update wiring** | The files in a repository that let a bot drive the tool: the scheduled job, the pull-request check, and the updater configuration for the pins. |
| **drift** | The repository no longer agrees with the conventions that it follows. |

Reference, generate and copy are an order of preference, best first. A reference leaves no copy to
drift. A generated file repairs itself on the next run. A copy is the last resort, and on upgrade the
tool replaces it whole and never merges it.

## 2. Problems

These are the nine problems that this vision answers. They describe the state on the date above. A
team creates a Gears repository by copying `gears-rust`, and nothing afterwards reports that the
copy went stale or wrong. Everything else follows from those two facts.

P1 to P6 belong to the person who owns a repository. P7 to P9 belong to the people who work in one
every day. Those people are the developer of a gear and the developer of an application that uses
gears. Section 3 then shows how far each problem is covered today.

| # | You cannot… | What is missing | Evidence |
|---|---|---|---|
| **P1** | create a correct repository — only copy one | `cargo gears new` makes an application workspace. Nothing makes the make driver, CI, lint configuration, documentation layout or SDLC setup | The conventions are written down in no single place. They live in a working repository, and you cannot apply a working repository to a different one |
| **P2** | reproduce CI | The checks that make Gears code reviewable have no portable form: `dylint` via `cargo gears lint` with a per-project skip list, `cargo-deny` in two configurations, clippy with `cargo-hack` passes, `shear` on a pinned nightly, `nextest` with JUnit enrichment, FIPS and database suites, GTS and API-contract checks | They live across `gears.mk`, `gears-rust.mk`, `Gears.toml` and 30 workflow files, together with every pinned tool version and action SHA. A new project rebuilds all of it, or runs weaker checks and learns later |
| **P3** | reproduce the SDLC pipeline | Studio reproduces the document-first order well — PRD, DESIGN, ADR, DECOMPOSITION, FEATURE, then code with `cpt-*` markers, then design-led review. But that pipeline is one half of a repository | The build, CI and lint configuration are the other half, and nothing carries the two together. A team assembles them separately and must know how they fit first. Agent files Studio does not own drift with no signal |
| **P4** | stay current | For almost every part the upgrade path is a person who notices a change in `gears-rust` and copies the files again | `gears.mk` gains targets, lint rules change, pinned tool and action versions move, templates are revised. Projects fall behind with no signal |
| **P5** | tell whether a repository is still correct | Nothing reports a repository that moved away: an edited managed file, a deleted workflow, a gear in the wrong place, a missing tool. It still builds, and a reviewer sees one change rather than the whole shape | `artifacts.toml` holds 37 `[[ignore]]` blocks, 13 of them recording that a module has no PRD or DESIGN — conformance debt kept as suppressions. The `toolkit-pr-review` prompt exists five times, once per vendor, and four copies no longer match. `cfs` stopped with an unclear error on an uninitialised `guidelines/DNA`, which is why PR #5326 adds `ensure-dna.py` |
| **P6** | take only part of the conventions | Adoption means copying `gears-rust`, which gives everything. No list of conventions, no name for any one, no way to state which a project follows | An application project needs neither the gear document pipeline nor descriptor coverage; a single-gear project needs neither FIPS nor database suites. Each selects a subset by hand and records nothing. This also blocks P5: you cannot check against a standard never stated |
| **P7** | state the complete set of parts a gear needs | A gear needs its five documents, the layered crate, the SDK crate, a `gear.gdl` descriptor, two workspace member entries, an alias in `[workspace.dependencies]`, registration in `artifacts.toml`, and publish settings for release-plz. No document holds that list | A developer learns it by reading another gear and copying what that one happens to have. Hence 13 of the `[[ignore]]` blocks, and four example gears with no descriptor. This is the developer's half of P5: nobody states what "complete" means, so intent is not enough |
| **P8** | know which gear versions work together | Inside this repository `[workspace.dependencies]` aligns 256 entries. An application outside receives no such file and picks each version alone | Rust offers no mechanism; Maven has a BOM and Spring publishes one. Gearbox owns the problem and has the artefact, `product.lock`, but registry sources and version resolution are not built, so an outside application has no resolved set to start from |
| **P9** | leave `gears-rust` and keep any of it | The conventions, kit, rules and documents are written from inside this repository | The kit's facades pointed at this repository's Makefile targets and paths, which is why PR #5326 adds `cf-gears-init` to discover what another project has. An outside repository starts with `cargo gears new` and stops: no make driver, no CI, no statement of which rules apply |

One of these fails for a reason worth naming on its own. P4 is not hard to compute — copying a newer
file is simple. It fails because it waits for a person to look. An ability to update does not stop
drift; only an update that runs on a schedule, without attention, stops drift. That is what Renovate
and Dependabot added to dependency management, and P4 needs the same property.

### How the problems compound

P1, P2 and P3 make a project expensive to start. P4 repeats that cost for its whole life. P5 hides
all four, because nothing reports the result, and P6 removes the choice to take less.

P7, P8 and P9 move the same cost onto the people doing the work. A gear developer cannot see the
target. An application developer cannot assemble a working set of versions. Neither can leave this
repository and keep any of it.

## 3. How far they are covered today

No problem in section 2 is untouched. Five mechanisms cover parts of them: `cargo gears`, the shared
make rules in `.gears-dna/gears.mk`, Constructor Studio (`cfs`) with the `studio-kit-gears` kit,
Gearbox with GDL, and PR #5326, open on the date above. Each has a different owner, a different
release cycle and a different manifest, and each stops at a different place.

| Problem | What covers it today | Where the coverage stops |
|---|---|---|
| P1 — you can only copy a correct repository | `cargo gears new` (pinned at 0.0.8) creates an application workspace and generates modules, configs and servers from `Gears.toml` | It writes no make driver, no CI, no lint configuration, no documentation layout and no SDLC setup, and knows nothing about repository conventions or review |
| P2 — you cannot reproduce CI | `.gears-dna/gears.mk`, version 1.1.0, holds 32 reusable targets — the only part of the build system prepared for reuse | A person copies that file by hand; its own header says *"Do not edit a copy of this file. To update it, replace it with a newer version from the source."* The other 30 workflows, 74 targets and every pinned version stay in `gears-rust` |
| P3 — you cannot reproduce the SDLC pipeline | Studio covers it: its skills own the artifacts, the kit carries templates, rules, checklists, codebase rules and stage routers, and Studio manages its own agent configuration | It covers documents and prompts only. The repository, the build and CI stay outside it, and agent files that Studio does not own stay with whoever wrote them |
| P4 — you cannot stay current | The kit has a version and a lockfile — the only mechanism here with an upgrade path | Nothing else has one. For the build rules, CI and lint configuration, the upgrade is a person who notices and copies again |
| P5 — you cannot tell whether a repository is correct | `cfs`, Gearbox, dylint and `cargo-deny` each judge their own area | Nobody checks the repository as a whole, so debt is recorded as suppressions instead of reported |
| P6 — the conventions are all or nothing | Nothing | The conventions have no names and no list, so a project cannot state a subset |
| P7 — nobody states the complete set for a gear | `cargo gears generate module` creates the crate | Nothing states the documents, the descriptor, the workspace entries and the publish settings that must follow |
| P8 — nothing states which gear versions work together | Gearbox reads a catalogue from the 64 `gear.gdl` descriptors, resolves a product and writes `product.lock` | Sources are local paths and version resolution is not built, so a consumer outside this repository cannot use it. Composition also cannot see a gear without a descriptor, and Gearbox ships no migration tool by decision |
| P9 — everything assumes you work inside `gears-rust` | PR #5326 adds `scripts/ci-commands.toml`, `scripts/project-paths.toml` and a `cf-gears-init` workflow that discovers a project's own commands and paths | It adapts the kit to a project. It gives the project no conventions, no CI and no build system |

Two facts outlive this snapshot. Two of the four mechanisms in use generate an application from
different manifests — `cargo gears` from `Gears.toml`, Gearbox from `gear.gdl` with `product.gdl` —
so three files describe one gear, under one rule: each fact has exactly one owner. And a long list
belongs to none of the five: the 30 workflows, the thirteen lint, audit, toolchain and test
configuration files, the `gears/` + `libs/` + `apps/` layout holding 20 gears and 37 libraries, the
`guidelines/DNA` submodule, and the document set under each gear.

## 4. Vision

One tool, `[cargo ]gears <cmd> <parameters>`, owns the setup and the upkeep of Gears projects. A
project **declares its conventions and pins their versions. It does not copy them.** A copied
convention starts to drift at once; a referenced one cannot, because its content is not here.

It has three responsibilities, and they do not weigh the same. **Setup** installs the conventions a
project chose. **Validation** reports when the repository stops matching them. **The update loop**
carries them forward. Setup happens once. The other two happen for the life of the project.

**Why this needs a tool at all is a Rust gap.** A reference works for the build rules, as a
versioned `gears.mk` that a pin resolves, and for CI, through reusable workflows that a tag selects.
It does not work for `clippy.toml`, `rustfmt.toml`, `deny.toml`, `rust-toolchain.toml`,
`.config/nextest.toml` or `.cargo/config.toml`. Rust has no `extends`, no parent manifest and no
remote configuration inheritance, and `[workspace.lints]` stops at the workspace. Those files must
be copied, which is why `gears-rust` copies them and why no existing scaffolding platform solves
this for us.

The mandate follows. **Reference what can be referenced. Generate what can be generated. Reduce
copying to the files that Rust leaves no alternative for.** That set is small and listable, so
replacing a whole file under a version check is enough for it.

One precondition is not ours to meet. The conventions must live where a pin can select them, and
somebody must put them there. Section 3 records that on the date above neither place existed, and
that work changes `gears-rust` before it changes any new project.

### Repository setup

The tool takes an empty repository to a working one. It installs the directory layout, the make
driver and the project make file, the CI workflows, and the configuration for lint, format, audit,
toolchain and tests. It sets Studio up. It records a pin for every convention that it installs.

Setup also includes the AI wiring. That is scaffolding, not a separate responsibility. It holds the
skills and prompts for review, document generation and code generation. It also holds the agent
configuration that connects them to whatever agent the developer runs.

It also installs the update wiring. Delivery belongs to the integrator, so a new project would
otherwise start with no automation at all. The wiring is the part of a repository that lets a bot
drive the tool. It puts the update on a schedule. It runs the check on every change. It tells the
updater that the project already uses about the new pins. It is a working default, not an example in
a README. The project owns it from the first day, and can change the schedule, change the bot, or
remove it.

Once the required tools are present, the result builds and passes its own checks before the
repository holds any product code. The tool does not install those tools; it reports the ones that
are missing.

### Conformance validation

A maintainer asks one question: is my repository still correct? P5 names this responsibility, and
nothing answers it today. The tool answers it on every pull request, not once at the start, and the
answer covers what the tool itself put there and what the project declared — nothing else.

One thing is easy to miss. A reference trades one failure for a smaller one: its content can no
longer be wrong, but the reference can fail to resolve, and then nothing works and the cause is far
away. P5 records a submodule that stopped `cfs` with an unclear error. Every reference therefore
needs its unresolved state checked, not assumed.

**It judges the repository against the maintainer's choice, not against `gears-rust`.** The
conventions are a menu, not a standard. Document-first development, descriptor coverage, crate
layering, FIPS and the full CI pipeline are each a separate choice, and a project that builds
applications needs a different set from one that publishes gears.

A project that never took a convention is in a correct, declared state. A project that took one and
drifted away from it has failed. Only the second is an error. Two things follow. The report must
show what the project did not choose beside what passed, or a project following almost nothing reads
as perfect. And a profile is only a named preset of the menu, not a separate product.

**A declared convention replaces a gear generator.** A generator applies "documents before code"
once, to a developer who already meant to follow it. A convention that CI enforces applies it
permanently, to everybody, and it also reaches the gears that already exist and are wrong — section 2
names four. The tool creates no gear and judges none. `cargo gears generate module` creates the
crate, the author writes the descriptor, and the Gearbox catalogue check reports what is missing.

**It checks its own footprint and nothing else.** The boundary is testable: for any check, ask
whether the tool created, copied, referenced or declared the thing under test. If not, the check
belongs elsewhere. `cfs` owns document structure, Gearbox owns descriptor errors, dylint owns crate
layering, `cargo-deny` owns dependency policy. A second implementation of any of those gives a
project two answers to one question. The tool still makes them effective, because it verifies that
they run: it confirms the CI job exists and its pins are current, and never decides the verdict
again.

**It installs conventions. It does not install software.** Installation depends on the platform, the
runner image and the package manager of the user, and a tool that installs software becomes a package
manager. The split already exists here: `gears.mk` has `install-tools`, and
`cargo gears tools check-version` verifies minimums. The tool takes the check half. It reports the
missing tool, the required version and the installed version, and stops. It may start a program to
read its version. It never starts one to do the work.

### The update loop

A project asks three questions. Which versions do I pin? What moved upstream? What does the
difference mean for me? The project then applies the upgrade. Without this loop, the tool is a
one-time copy with extra steps, and P4 returns.

**The loop is not a merge engine.** A project references its conventions by version, so the normal
upgrade is a change of pins. The tool cannot reference a small set of files, and it replaces each of
those as a whole file, under a version check. This vision rejects one model outright: a three-way
merge of upstream changes onto files that a project edited. Section 5 gives the reason.

**The loop is not a bot either, but a bot must be able to drive it.** That is a requirement, not a
convenience. Section 2 shows that P4 fails on human attention, so the loop has value only if it runs
without attention.

The tool must not run that automation itself. Two reasons decide it:

- a repository that uses Gears can sit outside this organization, where a push model would need
  credentials inside each one, so only a pull model reaches it;
- a tool that opens a pull request is a GitHub tool, while a tool that writes only the working tree
  also serves GitLab, a cron job and a person at a terminal.

Two more support it. Such repositories almost always run Dependabot or Renovate already, and a
hosted service is a commitment that never ends.

So the loop is a **command-line contract**: check, report, apply. The tool reads a manifest, writes a
working tree, and reports. It never creates a branch, a commit or a pull request. Whatever opens the
pull request comes after it and is replaceable, and the integrating project chooses what that is.

The contract is narrow so that any automation can drive it without knowing anything about Gears. It
is strict so that the same inputs always produce the same output. A bot that cannot predict the
result cannot be trusted with it. A bot and a developer run the same command and take the same code
path; there is no automation mode. That discipline pays on the human side too, because every change
is then reproducible by hand and reviewable by a person.

### What this changes for `gears-rust`

`gears-rust` becomes the reference implementation of these conventions, and a consumer of the same
tool. It stops being the repository that everybody copies from.

It also has to choose a direction. A tool can adapt to the build system that a project already has,
or it can give a project the build system that it should have. Section 3 records an effort inside
this repository that took the first direction; this vision takes the second. Both can exist, but
only one can be the default, and that choice has to be made deliberately rather than by whichever
lands first.

### 4.1 Modular by construction

The out-of-scope list in 4.2 is not permanent, and the tool must let that line move without a
rebuild. A plugin boundary added to a finished monolith is a rewrite, so it has to be there first.

It also buys one specific thing. The boundary between this tool, `cargo gears`, `cfs` and Gearbox is
a decision, and this document does not make it. A tool that can take a capability as an extension
lets that question be **answered later**. A guess made early makes the tool either a duplicate of
three existing tools, or too narrow to take any of them.

The scope moves along four directions, and the first release must be open along all four: the
commands, the checks, the menu of conventions, and the places a reference resolves from. Which seams
exist, and what each may contribute, is a design decision.

Three limits hold whatever is added.

| Limit | Why it holds |
|---|---|
| An extension is itself a declared and pinned reference; the tool never finds one in the environment | The bot contract depends on the same behaviour on every machine |
| The core keeps the contract: exit codes, report schema and lifecycle are not extensible | First-party capability then uses the same mechanism as everybody else, so the public path cannot decay |
| An extension adds checks and commands, never new concepts | A tool whose core owns the model stays a tool; one that lets extensions rewrite it becomes a framework |

### 4.2 Scope

Setup answers P1, P2, P3 and P9. Validation answers P5 and P7. The update loop answers P4. The
declaration answers P6. Two answers carry a condition: P2 and P9 wait until the conventions live
where a pin can select them, and P8 stays with Gearbox, because only Gearbox can produce a resolved
set of gear versions.

**In scope** are the three responsibilities above, and with them the declaration of the conventions
a repository follows and the presence check for the tools those conventions need. The rest divides
into what an extension may add later, and what an extension must not add either.

| Capability | Status | Who owns it, or why not |
|---|---|---|
| Generating the code of a gear | Out of scope today | `cargo gears generate module` |
| Authoring and validating documents | Out of scope today | `cfs` |
| Composition and deployment resolution | Out of scope today | Gearbox |
| Release orchestration | Out of scope today | release-plz |
| Migrating a consumer across a breaking platform change | Out of scope today | Would be a versioned codemod; nobody owns it yet |
| Adopting a repository that does not use Gears | Out of scope today | A later question |
| A three-way merge of upstream and local edits to one file | Excluded by design | A file that needs it belongs with a reference or with generation |
| Installing tooling | Excluded by design | The platform, the runner image or the user's package manager |
| A second implementation of another validator's judgement | Excluded by design | `cfs`, Gearbox, dylint and `cargo-deny` keep their own verdicts |
| Any runtime responsibility | Excluded by design | This tool works at development time only |
| Operating a bot, hosting a service, owning pull-request delivery | Excluded by design | The integrating project |
| Replacing `cargo`, `make` or the CI provider | Excluded by design | They stay; the tool only wires and checks them |
| Managing multi-repository workspaces | Excluded by design | Out of the tool's unit of work, which is one repository |
| A copy of `gears-rust` source in a downstream project | Excluded by design | Conventions travel, source does not |

## 5. What the successful scaffolds share

This is not our preference. It is where the industry arrived after trying the alternative. Seven
patterns repeat across the scaffolds that people use every day and that survive.

| Pattern | Evidence | What it decides here |
|---|---|---|
| **Generate as little as possible** | Create React App produced about 1300 packages and 350 MB; a Vite project uses about 23 and 40 MB. The React team deprecated Create React App in February 2025 for having no active maintainers | Measure setup by what it leaves behind. A large output is a larger surface somebody must keep correct in every project that received it |
| **What survives at organization scale avoids generation** | Maven parent POMs and BOMs, Gradle convention plugins, GitHub Actions reusable workflows, ESLint shareable configs, `tsconfig` `extends` — all hold a reference and a version, and upgrade by version bump | They exist to prevent our failure, where one change to a best practice needs one pull request per repository that copied it |
| **A template is a versioned artifact, not a repository you copy** | `dotnet new` installs a template as a package at an exact version, from a public feed, a private server or a shared folder, with a version history | A convention with a version and a feed can be selected, pinned and rolled back. One in somebody's `main` branch can only be copied |
| **Regenerate instead of edit, and make the exception explicit** | Expo regenerates native directories from the app config, so an upgrade is a bump plus a regeneration — the answer to what the 2022 React Native survey called the library's largest weakness. It never overwrites existing directories, and offers a named patch route | Prefer generation wherever a rule can produce the content. Where a project must diverge, give it a declared exception, or it will edit the file and say nothing |
| **Real update machinery is a codemod, not a re-render** | `ng update`, `nx migrate`, `rails app:update` and `ember-cli-update` ship versioned transformations owned by the framework that caused the change. A three-way merge of a template fails quietly: no conflict markers, and the file still wrong | Why the update loop replaces whole files and never merges them |
| **The road is a default, not a mandate** | Netflix's Paved Road is a set of recommended products and practices that engineers follow by default and leave deliberately | A tool that fails a project for a convention it never took is a tool that teams route around |
| **Diagnose, report, leave the repair to the operator** | `brew doctor` repairs nothing: it warns, exits non-zero, and leaves the command to the person. `flutter doctor` reports versions and missing parts. Both are the first command run when something breaks | The check command has the same shape |

## 6. Principles

These are the decisions above, in one list. Sections 5 to 7 carry the reasons.

- **Write as few files as possible.** Judge setup by what it leaves behind, not by what it offers.
- **Give divergence a declared route.** Without one, a project edits a managed file and says nothing.
- **A command, not a service.** No action needs a hosted component, an account, or a specific forge.
- **Delivery belongs to the integrator.** The project picks the bot, the schedule and the approvers.
- **A check is worth more than a generator.** A generator acts once; a check acts always.
- **Check your own footprint.** Where another validator owns a judgement, confirm that it runs and
  leave the verdict to its owner.
- **Check the tools. Do not install them.** Report the missing tool and its required version.
- **Generated content and owned content stay separate.** An upgrade never asks a person which lines
  were theirs.
- **One fact, one owner.** Several manifests describe one gear. Each fact belongs to one of them.
- **Documents before code.** A refusal enforces the order. A generator does not.
- **One place to ask, not one place to run.** `make` and CI run the work; the tool answers questions
  about the repository.
- **Extensible at the edges, fixed at the core.** Exit codes, report schema and lifecycle stay fixed.
- **Conventions are a choice, and the project declares it.** Silence is never consent.
- **State what you cannot do.** Name what you cannot configure; never invent a substitute.

## 7. What success looks like

Success is a repository that nobody had to assemble, that says what it follows, and that corrects
itself when the platform moves. A maintainer learns about drift from a report instead of an
incident. A developer learns what is missing before a push instead of after one. An upgrade arrives
in a week when nobody touched the repository.

The measurable form of each statement belongs in the requirements, not here. One signal, however,
outranks every measure. If teams keep copying `gears-rust` by hand while the tool exists, then the
tool failed, whatever any other number says.

## 8. Assumptions and constraints

This vision rests on the following. Each one, if it changes, changes the vision.

| Assumption | What it decides | If it stops holding |
|---|---|---|
| Rust gives no configuration inheritance across repositories | Why a set of files must be copied at all | An `extends` mechanism shrinks that set, and part of section 4 stops applying |
| A consumer can sit outside this organization | Pull model, and an opt-in menu rather than a standard | A push model becomes possible, and the menu becomes a convenience |
| GitHub is the forge we have evidence for, not the only one | The design may target it first, but may not assume it | Nothing; the constraint only ever loosens |
| `gears-rust` is both upstream of the conventions and their largest consumer | Every change lands here before any new project sees it | The reference implementation stops being the first consumer, and regressions reach others first |
| Somebody other than this tool publishes the conventions where a pin can select them | The tool cannot create its own precondition | Nothing to reference; the tool degrades into a copier |
| The other validators stay maintained | The tool reports their verdicts instead of computing them | An unmaintained validator becomes an unchecked convention |
| A project is a git repository whose CI can run a command on a schedule and on a pull request | The update loop has somewhere to be delivered | Below that, there is no delivery at all |

## 9. Risks

A team that watches for the signal sees the risk before it costs anything.

| Risk | How it fails | Early signal |
|---|---|---|
| The conventions never move upstream | No pin has anything to point at, and the tool degrades into a copier with a manifest | The list of copied files grows release after release |
| Nobody runs the automation | The wiring belongs to the project, so a project removes it and the loop P4 needs never runs | Repositories sit on old pins with no open pull request |
| Divergence has no declared route | A project edits a managed file instead of declaring an exception, so whole-file replacement stops being enough | Somebody asks for a merge mode |
| The tool becomes a framework | Extensions redefine the model instead of adding checks, and the maintainer stops getting one clear answer | An extension that needs a new concept in the core |
| A second source of truth returns | A check computes a verdict that `cfs`, Gearbox, dylint or `cargo-deny` already owns | Two reports disagree about one file |
| The menu fragments | Every project selects a unique set, so one piece of advice helps no second repository | No two projects share a selection |
| The tool lives only in CI | Developers meet it after a push, at the slowest point, and learn to work around it | The check command is absent from local use |
