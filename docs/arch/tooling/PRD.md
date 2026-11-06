<!-- Created: 2026-11-04 by Constructor Tech -->
<!-- Draft. Related: ./VISION.md, ./INVENTORY.md, ./DECISIONS.md -->

# Gears CLI — Requirements (draft)

[VISION.md](VISION.md) gives the direction. This document holds the requirements that the vision
deliberately keeps out of itself. It is a working draft: the items below were moved here so that the
vision stays an intent document, and they need review against the kit's PRD template before they
become the real PRD.

Terms are the ones in [VISION.md section 2](VISION.md#2-terms).

## Table of Contents

- [1. Conformance checks](#1-conformance-checks)
- [2. The command-line contract](#2-the-command-line-contract)
- [3. Update wiring](#3-update-wiring)
- [4. Acceptance criteria](#4-acceptance-criteria)

## 1. Conformance checks

The check covers the footprint of the tool and nothing else. Within that boundary it verifies that:

- every managed file is unmodified;
- every pin is current against its upstream;
- every reference resolves, so the submodule is initialised and the workflow tag exists;
- the repository layout agrees with the conventions that the project declared;
- the tools that those conventions need are installed, at the version that they need;
- each declared convention has its enforcement in place, so the CI job that applies it exists and
  runs.

## 2. The command-line contract

The tool exposes three actions: check, report and apply. The contract around them:

- the output is deterministic — the same inputs always produce the same bytes;
- the exit codes separate the outcomes that an automation must act on;
- the report is machine-readable;
- the caller selects the target version; the tool never chooses it;
- the tool reads upstream to learn what a pin could move to, and changes nothing outside the working
  tree;
- the tool never creates a branch, a commit or a pull request.

## 3. Update wiring

Setup installs the wiring that lets a bot drive the tool. It has three parts:

- a scheduled job that runs the update command, and that opens a pull request when an upgrade
  exists;
- a job that runs the check command on every pull request;
- the configuration that tells the updater of the project about the pins that setup created, so that
  Dependabot or Renovate can move them.

The project owns these files after setup writes them.

## 4. Acceptance criteria

- A new repository reaches a passing check with no hand edits, before it holds any product code.
- The list of copied files shrinks between releases. It grows only with a recorded reason.
- An upgrade arrives as a pull request in a week when nobody touched that repository.
- A repository outside this organization states which conventions it follows and passes against that
  statement.
- A developer reads one report and can name the convention that failed and the file to change.
- Declared exceptions stay few, and each one names a reason. Today the same debt hides as 37
  `[[ignore]]` blocks in a configuration file that nobody reads for that purpose.
- `gears-rust` runs the tool on itself, selects its own conventions, and passes.
