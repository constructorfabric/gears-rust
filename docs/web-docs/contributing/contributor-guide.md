---
title: Contributor guide
description: Three ways to contribute to the Gears docs, the git workflow, local setup, and the writing style guide.
sidebar:
  label: Contributor guide
  order: 2
---

Thank you for helping improve the Gears documentation. This site is built with
[Astro](https://astro.build) and [Starlight](https://starlight.astro.build), and
all content is plain Markdown/MDX — so contributing is mostly editing text files.

## Ways to contribute

There are three paths, from quickest to most powerful.

### 1. Edit on GitHub (quickest)

Every page has an **Edit page** link at the bottom. It opens the source file in
GitHub's web editor. Make your change, then **Propose changes** — GitHub forks the
repo and opens a pull request for you. Best for typos and small fixes.

### 2. GitHub Codespaces (cloud)

For larger changes without installing anything locally, open the repo in a
[Codespace](https://github.com/constructorfabric/gears-rust-web-docs). The
[dev container](https://github.com/constructorfabric/gears-rust-web-docs/tree/main/.devcontainer)
comes preconfigured with Node, pnpm, and the recommended VS Code extensions. The
dev server is forwarded automatically.

### 3. Local development

For substantial work, clone and run the site locally.

## Prerequisites

- **Node.js** ≥ 22.13 (LTS)
- **pnpm** 11.8 (`corepack enable` will pick the pinned version up automatically)
- **Git** and a GitHub account

## Local setup

```bash
# Fork the repo on GitHub first, then clone your fork:
git clone https://github.com/<your-username>/gears-rust-web-docs.git
cd gears-rust-web-docs

pnpm install
pnpm dev
```

The dev server runs at `http://localhost:4321`.

:::note
Full-text search is disabled in `dev`. Run `pnpm build && pnpm preview` to exercise
search and the production output (including the [/i18n/](../../i18n/) dashboard).
:::

## Git workflow

The flow mirrors a standard fork-and-PR model:

1. **Start from an issue** (or open one describing the change).
2. **Fork** [constructorfabric/gears-rust-web-docs](https://github.com/constructorfabric/gears-rust-web-docs)
   to your account.
3. **Branch** off `main`:

   ```bash
   git checkout -b feature/short-description
   ```

4. **Edit** the relevant files under `src/content/docs/`.
5. **Commit** with a clear message and **push** to your fork.
6. **Open a pull request** against `main` of the upstream repo. CI runs the checks
   below; address any failures and a maintainer will review.

## Quality gates

CI runs these on every pull request. Run them locally before pushing:

```bash
pnpm check     # Astro type & content validation
pnpm lint:md   # markdownlint
pnpm build     # production build (also emits the /i18n dashboard)
pnpm links     # dead-link check against the built site
```

## Writing style

- Use clear, concise language; prefer active voice.
- Use **sentence case** for headings.
- Keep examples runnable and minimal.
- Cross-link related pages with root-relative links (e.g. `/concepts/runtime-and-lifecycle/`).

## Markdown & MDX reference

Standard Markdown works everywhere. Starlight adds a few extensions:

**Asides** — for notes, tips, and warnings:

```md
:::note
Useful context.
:::

:::tip
A helpful suggestion.
:::

:::caution
Something to be careful about.
:::

:::danger
A serious warning.
:::
```

**Code blocks** support titles and line highlighting:

````md
```rust title="src/main.rs" {2}
fn main() {
    println!("highlighted line");
}
```
````

For interactive components (tabs, custom Astro components), use an `.mdx` file
instead of `.md` and import the component at the top of the page.

:::note
By contributing you agree to follow the project's Code of Conduct. See the
repository's `CONTRIBUTING.md` for the canonical quickstart.
:::
