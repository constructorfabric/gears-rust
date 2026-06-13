# PR Review with Constructor Studio

This project uses **Constructor Studio** for AI-powered PR reviews and status reports.

## Quick Start

Gears have integrated Constructor Studio automation for PR review assistance. Use any supported agent
(Windsurf, Cursor, Claude, Copilot) — each has thin stubs that redirect to
the canonical workflows via `/cf-gears-pr-review` and `/cf-gears-pr-status` commands.

You can use the following prompts in your IDE to review PRs or get status:

> cf list PRs
> review PR 100
> /cf-gears-pr-review 100
> review all PRs
> PR status 300
> /cf-gears-pr-status 300

See the `.prs/{ID}/` folder for the review results:

```bash
review.md
status.md
meta.json
diff.patch
review_comments.json
review_threads.json
```

## Configuration

### Configure GitHub API token

The `pr.py` script uses the [GitHub CLI (`gh`)](https://cli.github.com/) to fetch PR data. You need `gh` installed and authenticated:

1. **Install `gh`**

   ```bash
   # macOS
   brew install gh

   # Linux (Debian/Ubuntu)
   sudo apt install gh

   # Other: https://github.com/cli/cli#installation
   ```

2. **Authenticate with GitHub**

   ```bash
   gh auth login
   ```

   Follow the interactive prompts. Choose:
   - **GitHub.com** (or your GitHub Enterprise host)
   - **HTTPS** as the preferred protocol
   - **Login with a web browser** (recommended) or paste a personal access token

   The token needs these scopes: `repo`, `read:org` (for private repos).

3. **Verify authentication**

   ```bash
   gh auth status
   ```

   You should see `Logged in to GitHub.com as <your-username>`.

4. **(Optional) Use a personal access token directly**

   If you prefer not to use the browser flow:

   ```bash
   # Create a token at: https://github.com/settings/tokens
   # Required scopes: repo, read:org
   gh auth login --with-token < token.txt
   ```

   Or set the `GH_TOKEN` / `GITHUB_TOKEN` environment variable:

   ```bash
   export GH_TOKEN="ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
   ```

### Workflow Reference

1. Fetch PR metadata using `studio-kit-gears/scripts/pr.py` CLI tool
2. Select the most appropriate review prompt (code, design, ADR, or PRD)
3. Analyze changes against the corresponding checklist
4. Write a structured review to `.prs/{ID}/review.md` or status report to `.prs/{ID}/status.md`

### Excluding PRs

Edit `.prs/config.yaml` → `exclude_prs` to skip specific PRs during bulk operations, like review ALL

## Templates

Report templates define the expected output format for reviews and status reports.

| Template | Canonical location | Docs copy |
|----------|-------------------|-----------|
| Code review | `studio-kit-gears/artifacts/PR-CODE-REVIEW-TEMPLATE/template.md` | `docs/pr-review/code-review-template.md` |
| Status report | `studio-kit-gears/artifacts/PR-STATUS-REPORT-TEMPLATE/template.md` | `docs/pr-review/status-report-template.md` |

The canonical templates live inside `studio-kit-gears/artifacts/`. Kit updates
via `cfs kit update` will show a diff for any template changes.

## Review Prompts

Each review type has a dedicated prompt file and checklist:

| Review type | Prompt | Checklist |
|-------------|--------|-----------|
| Code Review | `studio-kit-gears/scripts/prompts/pr/code-review.md` | `docs/checklists/CODING.md` |
| Design Review | `studio-kit-gears/scripts/prompts/pr/design-review.md` | `docs/checklists/DESIGN.md` |
| ADR Review | `studio-kit-gears/scripts/prompts/pr/adr-review.md` | `docs/checklists/ADR.md` |
| PRD Review | `studio-kit-gears/scripts/prompts/pr/prd-review.md` | `docs/checklists/PRD.md` |
