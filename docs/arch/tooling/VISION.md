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
copied, so the ordinary upgrade is a change of pins. A maintainer runs it to set a repository up and
keep it correct. A gear developer runs it before a push, to learn what is missing. A bot runs it on
a schedule, to deliver an upgrade that nobody asked for.

**The limit.** The tool checks only what it installed or what the project declared, and leaves domain
judgements to the validators that own them. It installs no software, runs no bot, and never merges
an upstream change into a managed file that a project edited.

**What this is, and is not.** It gives the direction and the limits of the scope. It does not give
the architecture, the commands, the file formats or the implementation; those belong in `DESIGN.md`
and in the ADRs after it. It is written for the maintainers of `gears-rust`, `cargo-gears`,
`gearbox` and the `studio-kit-gears` kit.

**How to read it.** About thirty minutes end to end, and section 4 is half of that. Skip section 3
if you already know what `cargo gears`, `gears.mk`, Studio and Gearbox each cover today. If you only
need the decisions, read this block, the walk-through at the top of section 4, and section 6.

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
| **pin** | What a repository records to select one exact version of a convention. It has to designate content that cannot change afterwards; a name somebody can move later is not a pin. |
| **install** | The tool puts a convention into a repository: it writes the pin, and any file that the convention generates or copies. The word never means obtaining a program. |
| **reference** | Upstream keeps the file. The repository holds only a pin. An upgrade changes the pin and nothing else, because the content is not here to change. |
| **generate** | Upstream supplies a rule, not a file. The tool derives the content, and it can derive the same content again at any time. An upgrade runs the rule again. |
| **copy** | Upstream supplies the file itself. The tool cannot derive the content. An upgrade replaces the whole file with a newer version from upstream. |
| **managed file** | A file that the tool generated or copied, and still owns. A person does not edit it; a project that needs different content declares an exception, and the tool stops managing that file. The split is about ownership, not authorship: everything else is project-owned, and setup may write a project-owned file once as a starting point without managing it afterwards. |
| **profile** | A named preset of conventions. A convenient starting selection, not a separate product. |
| **bot** | An automation that runs the tool without attention and then delivers the result, usually as a pull request. The integrating project selects it and runs it. |
| **update wiring** | The files in a repository that let a bot drive the tool: the scheduled job, the pull-request check, and the updater configuration for the pins. Setup writes them once; they are project-owned, carry no pin, and are never managed. |
| **drift** | The repository no longer agrees with the conventions it declared, at the versions it pinned. A pin that upstream has moved past is not drift. |

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

P1, P2 and P3 make a project expensive to start, and P4 repeats that cost for its whole life. P5
hides all four, because nothing reports the result, and P6 removes the choice to take less. P7, P8
and P9 move the same cost onto the people doing the work: a gear developer cannot see the target, an
application developer cannot assemble a working set of versions, and neither can leave this
repository and keep any of it.

## 3. How far they are covered today

Eight of the nine problems in section 2 are touched by something; P6 is not. Five mechanisms cover
parts of them: `cargo gears`, the shared
make rules in `.gears-dna/gears.mk`, Constructor Studio (`cfs`) with the `studio-kit-gears` kit,
Gearbox with GDL, and PR #5326, open on the date above. Each has a different owner, a different
release cycle and a different manifest, and each stops at a different place.

| Problem | What covers it today | Where the coverage stops |
|---|---|---|
| P1 — you can only copy a correct repository | `cargo gears new` (pinned at 0.0.8) creates an application workspace and generates modules, configs and servers from `Gears.toml` | It writes no make driver, no CI, no lint configuration, no documentation layout and no SDLC setup, and knows nothing about repository conventions or review |
| P2 — you cannot reproduce CI | `.gears-dna/gears.mk`, version 1.1.0, holds 32 reusable targets — the only part of the build system prepared for reuse | A person copies that file by hand; its own header says *"Do not edit a copy of this file. To update it, replace it with a newer version from the source."* The other 30 workflows, 74 targets and every pinned version stay in `gears-rust` |
| P3 — you cannot reproduce the SDLC pipeline | Studio covers it: its skills own the artifacts, the kit carries templates, rules, checklists, codebase rules and stage routers, and Studio manages its own agent configuration | It covers documents and prompts only. The repository, the build and CI stay outside it, and agent files that Studio does not own stay with whoever wrote them |
| P4 — you cannot stay current | The kit has a version and a lockfile, so a tool performs its upgrade. `gears.mk` is versioned too, but its upgrade path is an instruction in its own header | Only the kit's is mechanised. For the build rules, CI and lint configuration, the upgrade is a person who notices and copies again |
| P5 — you cannot tell whether a repository is correct | `cfs`, Gearbox, dylint and `cargo-deny` each judge their own area | Nobody checks the repository as a whole, so debt is recorded as suppressions instead of reported |
| P6 — the conventions are all or nothing | Nothing | The conventions have no names and no list, so a project cannot state a subset |
| P7 — nobody states the complete set for a gear | `cargo gears generate module` creates the crate | Nothing states the documents, the descriptor, the workspace entries and the publish settings that must follow |
| P8 — nothing states which gear versions work together | Gearbox reads a catalogue from the 64 `gear.gdl` descriptors, resolves a product and writes `product.lock` | Sources are local paths and version resolution is not built, so a consumer outside this repository cannot use it. Composition also cannot see a gear without a descriptor, and Gearbox ships no migration tool by decision |
| P9 — everything assumes you work inside `gears-rust` | PR #5326 adds `scripts/ci-commands.toml`, `scripts/project-paths.toml` and a `cf-gears-init` workflow that discovers a project's own commands and paths | It adapts the kit to a project. It gives the project no conventions, no CI and no build system |

Two facts outlive this snapshot. Two of the four mechanisms in use generate an application, from
different manifests: `cargo gears` from `Gears.toml`, Gearbox from `gear.gdl` with `product.gdl`.
Three files therefore describe one gear, under one rule — each fact has exactly one owner.

And a long list belongs to none of the five. It holds the 30 workflows, the thirteen lint, audit,
toolchain and test configuration files, the `gears/` + `libs/` + `apps/` layout with its 20 gears
and 37 libraries, the `guidelines/DNA` submodule, and the document set under each gear.

## 4. Vision

One tool, `[cargo ]gears <cmd> <parameters>`, owns the setup and the upkeep of Gears projects. A
project **declares its conventions and pins their versions. It does not copy them.**

It has three responsibilities, and they do not weigh the same. **Setup** installs the conventions a
project chose. **Validation** reports when the repository stops matching them. **The update loop**
carries them forward. Setup happens once. The other two happen for the life of the project.

**Why this needs a tool at all is a Rust gap.** A reference works for the build rules, as a
versioned `gears.mk` that a pin resolves, and for CI, through reusable workflows that a pin selects.
It does not work for `clippy.toml`, `rustfmt.toml`, `deny.toml`, `rust-toolchain.toml`,
`.config/nextest.toml` or `.cargo/config.toml`: Rust has no `extends`, no parent manifest and no
remote configuration inheritance, and `[workspace.lints]` stops at the workspace. Those files have
to be copied, which is why `gears-rust` copies them and why no scaffolding platform solves this for
us. The mandate follows. **Reference what can be referenced. Generate what can be generated. Reduce
copying to the files Rust leaves no alternative for.** That set is small and listable, so replacing
a whole file under a version check is enough for it.

One precondition is not ours to meet. The conventions have to live where a pin can select them, and
somebody has to put them there. Section 3 records that on the date above neither place existed, and
that work changes `gears-rust` before it changes any new project.

### One repository, start to finish

A maintainer starts a gear repository. They pick a profile and adjust it: document-first development
yes, FIPS no. Setup writes the layout, the build and CI wiring, the lint and toolchain
configuration and the Studio setup, with a pin for each convention, and hands over a working update
wiring that the project owns from then on. The report ends by naming the two tools missing from
their machine. Nothing is hidden: the selection is a file in the repository, and so is every pin.

A week later a developer adds a gear and runs the check before pushing. The crate is there, but the
DESIGN document and the descriptor the declared conventions call for are not — the parts a new gear
needs that nobody had written down for them. That absence is the tool's own finding; once the files
exist, whether they are any good is Studio's and Gearbox's verdict, relayed in the same report. The
check also runs on the pull request, so the answer does not depend on anyone remembering.

A month later a convention moves upstream. The scheduled job runs the update, and a pull request
arrives changing four pins and two managed files. The diff is small, the reasoning is in the report,
and it merges.

The next upgrade is not like that. The new version of the build rules renames a target that the
project's own make file calls. Working out the upgrade, the tool sees that completing it would mean
editing that file — and that file belongs to the project. So it does not apply the upgrade. It
reports which conventions it could not advance and the one change the project has to make first,
and it does not claim an upgrade that did not happen. Somebody makes that change deliberately, the
upgrade then applies, and the check confirms conformance again.

That arc — **set up, check, update, check again** — is the product. The first step happens once; the
rest repeat for the life of the repository, and the last is what makes the third safe.

### Repository setup

The tool takes an empty repository to a working one. It installs the directory layout, the make
driver and the project make file, the CI workflows, and the configuration for lint, format, audit,
toolchain and tests. It sets Studio up. It records a pin for every convention that it installs.

Setup also includes the AI wiring: the skills and prompts for review, document generation and code
generation, plus the agent configuration that connects them to whatever agent the developer runs.
That is scaffolding, not a separate responsibility.

And it installs the update wiring, because delivery belongs to the integrator and a new project
would otherwise start with no automation at all. The wiring is what lets a bot drive the tool: it
puts the update on a schedule, runs the check on every change, and tells the updater the project
already uses about the new pins. It is a working default, not an example in a README.

Setup writes that wiring once and hands it over. It is project-owned from the first day — no pin, no
management, and the project is free to change the schedule, change the bot, or remove it. That is
the one thing setup writes and does not keep: everything else it installs stays managed and pinned.
The cost is deliberate, and section 9 names it as a risk: wiring a project removed is wiring that
delivers nothing.

### Conformance validation

A maintainer asks one question: is my repository still correct? P5 names this responsibility, and
nothing answers it today. The tool answers it on every pull request, not once at the start. The
answer covers what the tool itself put there and what the project declared, and nothing else.

A reference trades one failure for a smaller one: its content can no longer be wrong, but it can
fail to resolve, and then nothing works and the cause is far away — P5 records a submodule that
stopped `cfs` with an unclear error. An unresolved reference is therefore checked, not assumed.

**It judges the repository against the maintainer's choice, at the versions that choice pinned.**
The conventions are a menu: document-first development, descriptor coverage, crate layering, FIPS
and the full CI pipeline are each a separate choice, and a project that builds applications needs a
different set from one that publishes gears. A repository on version 1.2 while upstream ships 1.3 is
conformant, not drifting — a newer release is the update loop's business, and the two answers are
never merged into one verdict. A project that never took a convention is in a correct,
declared state; one that took it and drifted away has failed. Only the second is an error, so the
report shows what the project did not choose beside what passed — otherwise a project following
almost nothing reads as perfect.

**A declared convention replaces a gear generator.** A generator applies "documents before code"
once, to a developer who already meant to follow it. A convention that CI enforces applies it
permanently, to everybody, and it also reaches the gears that already exist and are wrong — section 2
names four. The tool creates no gear and judges no gear's content. `cargo gears generate module`
creates the crate and the author writes the descriptor; the tool reports that a declared part is
absent, and Gearbox says whether the one that exists is right.

**It owns repository-level conformance. Domain judgements stay with their owners.** The line is
presence and placement against content and correctness. That a declared convention requires a DESIGN
document, and that none exists where the convention says it should, is a repository-level fact the
tool established by installing that convention. Whether an existing DESIGN document is well formed
and traceable is `cfs`'s judgement, descriptor correctness is Gearbox's, crate layering is dylint's,
dependency policy is `cargo-deny`'s. A second implementation of any of those gives a project two
answers to one question.

One report may still carry all of it. Aggregating a verdict is not recomputing it, and the reader
should not have to run four tools to learn where a repository stands. But every finding is
attributed, and the tool never invents an answer on a domain owner's behalf.

That boundary creates the one thing this responsibility cannot get wrong. **A configured check is
not a passing check**, and the report must not let the two blur. For every convention a project
declared there are five distinct states, and only the last is good news:

- the convention is declared;
- its reference resolves;
- its configuration is intact — the declared pin resolves, and managed content matches that pin;
- the check that enforces it is wired and ran;
- that check passed.

The tool answers the first four itself and relays the fifth from its owner. Where it cannot reach an
answer — a validator that did not run, a check it has no access to — it says so. **An unrun check is
never reported as a passing one**, and a convention the project never took is reported as unselected
rather than failed. A reader should be able to tell what was checked, what was not, and why.

**It installs conventions. It does not install software.** Obtaining a program depends on the
platform, the runner image and the user's package manager, and a tool that does it becomes a package
manager. The split already exists here: `gears.mk` has `install-tools`, `cargo gears tools
check-version` verifies minimums. The tool takes the check half — it reports the missing tool, its
required version and the installed one, and stops. It may start a program to read its version, never
to do the work.

### The update loop

A project asks what moved upstream, what the difference means for it, and then applies the upgrade.
Without this loop the tool is a one-time copy with extra steps, and P4 returns.

**Routine upgrades should be version changes. Exceptional ones must be explicit.** A project
references its conventions by version, so most upgrades move pins and touch nothing else, and the
small set of files that cannot be referenced is replaced whole under a version check.

What makes an upgrade exceptional is where its effect lands, not how large it is. One that stays
inside referenced or managed content is routine. One that reaches **project-owned content** is not:
a renamed build target the project's own make file calls, a setting the project customised, a
contract change its code depends on. The tool does not edit what the project owns, so it does not
apply those upgrades: it names what the project has to change first, and reports them as not
applied. A repository is never left partly upgraded with no account of what happened.

One model is rejected outright: a **three-way merge of upstream changes into a managed file that the
project edited**. Section 5 gives the reason — that merge fails quietly, producing no conflict
markers and a file that is still wrong.

Read that narrowly: it forbids silently reconciling two sets of edits to one file. It does not
forbid a future versioned migration that states what it changes and why, nor a project from
diverging. **Divergence is allowed when it is declared** — the project records the exception, the
tool stops managing that file, and the report shows it as a declared exception rather than drift.
Without that route a project edits a managed file and says nothing, and nobody can tell a deliberate
change from a mistake.

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

The tool owes automation a contract it can drive. It cannot owe that the automation runs: a bot the
project removed delivers nothing, and neither does a CI provider that is down. Reliability is
therefore shared — the tool owns determinism and honest reporting, the project owns delivery — which
is why setup ships working wiring rather than leaving the common case to a decision.

### What the tool promises

Each promise is bounded by what the tool actually controls.

| | It promises | It does not promise |
|---|---|---|
| **Setup** | Every selected convention is installed and pinned, a working update wiring is handed over, and anything missing is named | A green build. That needs tools installed, services configured and code written |
| **Validation** | A truthful answer within its footprint, with the five states above kept apart and every gap in its own knowledge visible | That the repository is good — only that it is, or is not, what the project said it would be |
| **Update** | Pins and managed files move reproducibly; an upgrade it cannot apply is reported as not applied, with the reason | That every upstream change applies by itself, or that anything delivers it |

Three safety expectations hold across all three. Conventions resolve only from sources the project
declared, so an upgrade cannot quietly start following somebody else's. Resolution is reproducible,
so the same pins yield the same content for a developer today and an unattended run next week — and
that holds only where a pin designates something immutable, which a movable tag or branch is not.
`gears-rust` already settled this for its own CI by pinning action SHAs rather than tags (#2093);
the conventions need the same discipline, and how each kind of artefact is identified is a design
decision. And the execution boundary is explicit: read manifests, write a working tree, report. A
project knows what it agreed to run when the scheduled job fires at three in the morning.

### What this changes for `gears-rust`

`gears-rust` becomes the reference implementation of these conventions, and a consumer of the same
tool. It stops being the repository that everybody copies from.

It also has to choose a direction. A tool can adapt to the build system that a project already has,
or it can give a project the build system that it should have. Section 3 records an effort inside
this repository that took the first direction; this vision takes the second. Both can exist, but
only one can be the default, and that choice has to be made deliberately rather than by whichever
lands first.

### 4.1 Modular by construction

The out-of-scope list in 4.2 is not permanent, and the tool should let that line move without a
rewrite later.

It also buys one specific thing. The boundary between this tool, `cargo gears`, `cfs` and Gearbox is
a decision, and this document does not make it. A tool that can take a capability as an extension
lets that question be **answered later**. A guess made early makes the tool either a duplicate of
three existing tools, or too narrow to take any of them.

The scope moves along four directions: the commands, the checks, the menu of conventions, and the
places a reference resolves from. The obligation on the first release is to avoid decisions that
foreclose them — not to ship a mechanism for any of them. Architectural room is cheap to keep and
expensive to recover; a plugin surface is neither, and nothing here requires one. Which seams
eventually exist, and what each may contribute, is a design decision.

Three limits hold whatever is eventually added.

| Limit | Why it holds |
|---|---|
| An extension is itself a declared and pinned reference; the tool never finds one in the environment | The bot contract depends on the same behaviour on every machine |
| The core keeps the contract: exit codes, report schema and lifecycle are not extensible | First-party capability then uses the same mechanism as everybody else, so the public path cannot decay |
| An extension adds checks and commands, never new concepts | A tool whose core owns the model stays a tool; one that lets extensions rewrite it becomes a framework |

Open at the edges is not the same as built at the edges. The first useful version needs none of these:
a dependency solver, a package manager, a hosted service, a migration engine, a plugin runtime, or
a replacement for any build, CI or validation tool that already exists. A capability that may be worth
building later is not an argument for building it first. **A small set of conventions that can be
installed, checked and updated beats a large catalogue that only supports the first step** — and a
tool that cannot complete that cycle for one convention will not complete it for fifty.

### 4.2 Scope

Setup answers P1, P2, P3 and P9. Validation answers P5 and P7. The update loop answers P4. The
declaration answers P6. Two answers carry a condition. P2 and P9 wait until the conventions live
where a pin can select them. P8 stays with Gearbox, because only Gearbox can produce a resolved set
of gear versions.

**In scope** are the three responsibilities above, and with them the declaration of the conventions
a repository follows and the presence check for the tools those conventions need.

Three starting points are not the same problem. A **new repository** is the primary case. An
**existing Gears repository** comes under management by declaring what it already follows and
letting the check report the gap — a declaration plus a backlog, not a rewrite. An **unrelated
repository** is out of scope today.

The rest divides into what an extension may add later, and what an extension must not add either.

| Capability | Status | Who owns it, or why not |
|---|---|---|
| Generating the code of a gear | Out of scope today | `cargo gears generate module` |
| Authoring and validating documents | Out of scope today | `cfs` |
| Composition and deployment resolution | Out of scope today | Gearbox |
| Release orchestration | Out of scope today | release-plz |
| Migrating a consumer across a breaking platform change | Out of scope today | Would be a versioned codemod; nobody owns it yet |
| Adopting an unrelated repository that does not use Gears | Out of scope today | A later question; the first version serves Gears projects |
| Rewriting an existing repository to match a convention it declared | Out of scope today | The check reports the gap; closing it is the team's work, not a silent edit |
| A three-way merge of upstream and local edits to one managed file | Excluded by design | It fails quietly. A project that needs different content declares an exception instead; an upgrade that needs a real transformation states what it changes |
| Installing tooling | Excluded by design | The platform, the runner image or the user's package manager |
| A second implementation of another validator's judgement | Excluded by design | `cfs`, Gearbox, dylint and `cargo-deny` keep their own verdicts |
| Any runtime responsibility | Excluded by design | This tool works at development time only |
| Operating a bot, hosting a service, owning pull-request delivery | Excluded by design | The integrating project |
| Replacing `cargo`, `make` or the CI provider | Excluded by design | They stay; the tool only wires and checks them |
| Managing multi-repository workspaces | Excluded by design | Out of the tool's unit of work, which is one repository |
| A copy of `gears-rust` source in a downstream project | Excluded by design | Conventions travel, source does not |

## 5. What the successful scaffolds share

These are observations about tools people use every day, not a law. Each one is here because it
decides something specific below. Taken together they point at one conclusion: **minimize
unmanaged duplication, and make the lifecycle of whatever is generated or copied explicit.**

| Pattern | Evidence | What it decides here |
|---|---|---|
| **The cost of a scaffold is what it leaves unmanaged** | Create React App kept its build configuration inside a versioned dependency, `react-scripts`, and that part upgraded by bumping a version. What the generator wrote into the repository, and everything past `eject`, could not be upgraded at all. The React team deprecated it in February 2025 for having no active maintainers | Judge setup by how much of its output stays managed afterwards, not by how little it writes. An unmanaged file is a file somebody must keep correct by hand in every project that received it |
| **Conventions that outlive their authors are distributed as versioned references** | Maven parent POMs and BOMs, Gradle convention plugins, GitHub Actions reusable workflows, ESLint shareable configs and `tsconfig` `extends` all put the content upstream and a version in the repository. This is about distribution, not about whether anything is generated — Expo below generates heavily and still upgrades cleanly | Prefer a reference where one is available, because one change then needs one release instead of one pull request per repository that copied it |
| **A template is a versioned artifact, not a repository you copy** | `dotnet new` installs a template as a package at an exact version, from a public feed, a private server or a shared folder, with a version history | A convention with a version and a feed can be selected, pinned and rolled back. One in somebody's `main` branch can only be copied |
| **Regenerate instead of edit, and make the exception explicit** | Expo regenerates native directories from the app config, so an upgrade is a bump plus a regeneration — the answer to what the 2022 React Native survey called the library's largest weakness. It never overwrites existing directories, and offers a named patch route | Prefer generation wherever a rule can produce the content. Where a project must diverge, give it a declared exception, or it will edit the file and say nothing |
| **Where upgrades need to change a repository, they are explicit transformations, not merges** | `ng update`, `nx migrate`, `rails app:update` and `ember-cli-update` ship versioned transformations owned by the framework that caused the change, and each states what it touches. A three-way merge of a template fails quietly instead: no conflict markers, and the file still wrong | Why a managed file is replaced whole rather than merged, and why an upgrade that cannot be expressed that way stops and says so rather than guessing |
| **The road is a default, not a mandate** | Netflix's Paved Road is a set of recommended products and practices that engineers follow by default and leave deliberately | A tool that fails a project for a convention it never took is a tool that teams route around |
| **Diagnose, report, leave the repair to the operator** | `brew doctor` repairs nothing: it warns, exits non-zero, and leaves the command to the person. `flutter doctor` reports versions and missing parts. Both are the first command run when something breaks | The check command has the same shape |

## 6. Principles

These are the decisions above, in one list. Sections 4 and 5 carry the reasons. One sentence holds
the rest together:

> **Prefer a simple, safe and reproducible path over a universal one. Automate the routine, expose
> the exceptional, and never hide uncertainty behind a successful exit code.**

**What the tool is**

- **A command, not a service.** No action needs a hosted component, an account, or a specific forge.
- **One place to ask, not one place to run.** `make` and CI run the work; the tool answers questions
  about the repository.
- **The easiest path is the correct path.** A developer benefits from the convention model without
  having to learn it. Defaults are good and visible, diagnostics say what to do next, and advanced
  choices appear only when a project needs them. Nobody should have to configure every convention by
  hand to get a working repository.

**What it owns, and what it leaves alone**

- **Own conformance, not domain judgement.** Presence and placement against the declared
  conventions are the tool's. Content and correctness belong to the validator that owns them; relay
  their verdict, attributed, and never substitute your own.
- **Integrate rather than replace.** Reuse the build, CI, generation and validation that exist.
- **One fact, one owner.** Several manifests describe one gear. Each fact belongs to one of them.
- **Check the tools. Do not install them.** Report the missing tool and its required version.
- **Managed content and owned content stay distinct.** An upgrade never asks a person which lines
  were theirs, and the tool never takes ownership of a file a team maintains.
- **Delivery belongs to the integrator.** The project picks the bot, the schedule and the approvers.

**How it behaves when things are uncertain**

- **Safety over automatic application.** An upgrade that cannot be applied safely and predictably is
  reported, not guessed at.
- **Determinism over cleverness.** A developer and an unattended run reach the same result from the
  same inputs, and both can reproduce it.
- **Give divergence a declared route.** Without one, a project edits a managed file and says nothing.
- **State what you cannot do.** Never invent a substitute, and never report an unrun check as a pass.

**What it optimises for**

- **Lifecycle completeness over feature breadth.** A few conventions that install, check and update
  beat a catalogue that only installs.
- **Simplicity over universality.** Serve the Gears workflow well before serving every repository
  shape.
- **A check is worth more than a generator.** A generator acts once; a check acts always, including
  on what already exists and is wrong.
- **Useful defaults over mandatory standards.** Conventions are a choice the project declares, and
  silence is never consent.
- **Extensible at the edges, fixed at the core.** Exit codes, report schema and lifecycle stay fixed.

## 7. What success looks like

Success is visible in what stops happening, and each outcome has somewhere it can be seen. The
signal is where to look, not a target to hit.

| Outcome | Where it shows |
|---|---|
| Nobody assembles a repository by reading `gears-rust` and copying from it | New repositories start from a declared selection rather than a copied tree, and the first commit is not a dump of configuration |
| A maintainer learns about drift from a report, not an incident | Drift is found by a check on a pull request rather than by a build that broke for an unrelated reason |
| A developer learns what a gear is missing before pushing | The check runs locally as a habit, not only in CI — if it only ever runs in CI, the value arrived too late |
| An upgrade lands in a week when nobody touched the repository | Repositories do not sit on pins that upstream moved past long ago, and the update pull request is a small diff rather than an afternoon |
| A convention improves in one place and reaches everyone | Fixing a shared rule means one release, not one pull request per repository that copied it |
| The set of files nobody manages keeps shrinking | The list of copied files gets shorter between releases, and each addition to it has a recorded reason |

None of that is the same as a healthy repository, and the tool should never claim otherwise. It
makes the state of a repository **legible** — what it follows, how far it has drifted, what it was
never told to care about. Teams still decide and still do the work.

Two outcomes would be failure even if every feature shipped. If teams keep copying `gears-rust` by
hand while the tool exists, it did not solve its problem. And if a green check comes to mean
"nothing ran", it is worse than what came before, because it replaces a known absence with a false
assurance. The measurable form of these belongs in the requirements, not here.

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
| A pass stops meaning anything | Unrun or unreachable checks are folded into a green result, and the report no longer separates "checked and passed" from "not checked" | Somebody asks what a green check actually covered, and the answer takes investigation |
| The menu fragments | Every project selects a unique set, so one piece of advice helps no second repository | No two projects share a selection |
| The tool lives only in CI | Developers meet it after a push, at the slowest point, and learn to work around it | The check command is absent from local use |
