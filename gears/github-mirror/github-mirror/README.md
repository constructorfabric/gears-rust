# GitHub Mirror

The `github-mirror` gear keeps a synchronized local replica of GitHub repository metadata
and serves it back through a GitHub-compatible REST API plus a small native API, so
platform services can read issues, pull requests, commits and CI results without spending
GitHub rate limit on every call.

This is the first increment of the product described in [docs/PRD.md](../docs/PRD.md):
on-demand synchronization into tenant-scoped storage and a read-only serving surface.
Background scheduling, sync sessions, write-back and per-tenant token pools are tracked as
follow-up work — see the PRD's "Increment scope" section.

**What's implemented:**

- Sync engine: `POST .../sync` fetches a repository and 25 related entity families
  (issues, pulls, commits, comments, reviews, labels, milestones, releases, branches,
  tags, contributors, workflow runs/jobs, deployments, check runs, statuses, timelines, …)
  from the GitHub REST + GraphQL APIs in one transaction, with rate-limit retry/backoff
  and completeness-gated deletion reconciliation for rows removed upstream
- Tenant-scoped storage behind SecureORM: 26 tables, 31 migrations, every row stamped
  with `extracted_at`; two tenants can mirror the same repository without collision
- GitHub-compatible surface: 29 read endpoints under `/repos/{owner}/{name}/…` that
  answer in GitHub's JSON shape with GitHub-style `page`/`per_page` + `Link` pagination,
  entirely from the local store
- Native surface under `/github-mirror/v1/…`: health, mirrored-repository listing
  (keyset cursor), sync trigger, and analytics reads that GitHub's own API doesn't offer
  (commit files, review threads)
- `github-mirror-sdk` crate: typed client + models for other gears, exercised by
  integration tests over the full HTTP stack

## Quickstart

Run the example server with the gear's dev config (set `GITHUB_TOKEN` to sync private
repositories or to get authenticated rate limits):

```bash
cargo run --bin cf-gears-example-server -- --config config/github-mirror-dev.yaml run
```

That config sets no gateway `prefix_path`, so routes are served at the root
(`config/quickstart.yaml` is the one that adds a `/cf` prefix). Interactive API docs:
<http://127.0.0.1:8087/docs>

### Health

```bash
curl -s http://127.0.0.1:8087/github-mirror/v1/health
```

```json
{
  "gear": "github-mirror",
  "version": "0.1.0",
  "api_base_url": "https://api.github.com"
}
```

### Mirror a repository, then read it back

```bash
curl -s -X POST http://127.0.0.1:8087/github-mirror/v1/repos/octocat/hello-world/sync
```

```bash
curl -s http://127.0.0.1:8087/repos/octocat/hello-world/issues?state=open&per_page=10
```

## Configuration

```yaml
gears:
  github-mirror:
    config:
      api_base_url: https://api.github.com
      github_token: "${GITHUB_TOKEN}"
```

All keys are optional — `api_base_url` falls back to the public GitHub API and an unset
token syncs unauthenticated (public repositories only, at GitHub's anonymous rate limit).

`github_token` is a single gear-wide credential: every tenant's sync currently
authenticates to GitHub with it. This is an interim shortcut until credstore-backed
per-tenant credentials land (gears-rust#4534); don't point one deployment's token at
repositories whose visibility should differ per tenant.

## Documentation

- [PRD](../docs/PRD.md) — product requirements, including the current increment's scope
