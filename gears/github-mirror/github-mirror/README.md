# GitHub Mirror - Quickstart

The `github-mirror` gear serves a synchronized local replica of GitHub repository metadata
through a GitHub-compatible REST API and an analytics API, so platform services can read
issues, pull requests and commits without spending GitHub rate limit on every call.

This crate is currently a skeleton: it boots inside the gears runtime and answers a health
probe. Sync engine, storage and the GitHub-compatible surface are ported incrementally from
the `github-repotap` prototype.

**Features:**

- Gear lifecycle + REST capability wired into the platform runtime
- Health endpoint reporting gear name, version and configured GitHub API base URL

Full API documentation: <http://127.0.0.1:8087/cf/docs>

## Examples

### Health

```bash
curl -s http://127.0.0.1:8087/cf/github-mirror/v1/health
```

```json
{
  "gear": "github-mirror",
  "version": "0.1.0",
  "api_base_url": "https://api.github.com"
}
```

## Configuration

```yaml
gears:
  github-mirror:
    config:
      api_base_url: https://api.github.com
```

All keys are optional — the gear falls back to the defaults above.
