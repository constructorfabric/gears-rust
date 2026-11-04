// Builds the "Nightly status" issue: one table with the state of the nightly
// workflows. Used by .github/workflows/nightly-status.yml.
//
// Set NIGHTLY_STATUS_DRY_RUN=1 to print the body and make no write calls.
// This is for local testing only.

const WORKFLOWS = [
  { file: 'e2e.yml', label: 'E2E' },
  { file: 'clippy-nightly.yml', label: 'Clippy (full feature matrix)' },
  { file: 'macos-windows-tests.yml', label: 'macOS and Windows tests' },
  { file: 'shear-nightly.yml', label: 'Unused deps (cargo-shear)' },
];
const TITLE = 'Nightly status';
const NIGHTS = 14;

// Cancelled and timed out count as red on purpose: a nightly that never
// finished gave no signal.
const RED = new Set(['failure', 'cancelled', 'timed_out', 'startup_failure']);

function mark(run) {
  if (run.status !== 'completed') return '⚪';
  if (run.conclusion === 'success') return '✅';
  if (RED.has(run.conclusion)) return '❌';
  return '⚪';
}

function row(label, url, runs) {
  const name = `[${label}](${url})`;
  if (runs.length === 0) return `| ${name} | — | — | — | — | — |`;

  const newest = runs[0];
  const last = `${mark(newest)} ${newest.status === 'completed' ? newest.conclusion : 'running'}`;
  const date = new Date(newest.run_started_at || newest.created_at).toISOString().slice(0, 10);

  // Count red nights from the newest finished run. A run still going has no
  // result yet, so it is skipped.
  const done = runs.filter((x) => x.status === 'completed');
  let streak = 0;
  for (const r of done) {
    if (mark(r) !== '❌') break;
    streak += 1;
  }
  // Only NIGHTS runs are fetched. If all of them are red, the streak may be
  // longer than that, so show it as "14+".
  if (streak > 0 && streak === done.length && runs.length === NIGHTS) streak = `${streak}+`;

  const history = runs.map(mark).reverse().join('');
  return `| ${name} | ${last} | ${date} | [#${newest.run_number}](${newest.html_url}) | ${streak} | ${history} |`;
}

module.exports = async ({ github, context, core }) => {
  const { owner, repo } = context.repo;
  const repoUrl = `${context.serverUrl}/${owner}/${repo}`;
  const dryRun = process.env.NIGHTLY_STATUS_DRY_RUN === '1';

  const rows = [];
  for (const { file, label } of WORKFLOWS) {
    const res = await github.rest.actions.listWorkflowRuns({
      owner, repo, workflow_id: file, event: 'schedule', per_page: NIGHTS,
    });
    // Runs come newest first.
    const runs = res.data.workflow_runs.slice(0, NIGHTS);
    rows.push(row(label, `${repoUrl}/actions/workflows/${file}?query=event%3Aschedule`, runs));
  }

  const body = [
    '<!-- nightly-status: generated, edits are overwritten -->',
    'State of the scheduled (nightly) workflows on `main`. Updated automatically after each nightly run.',
    '',
    '| Workflow | Last night | Date | Run | Red nights in a row | Last 14 nights (old → new) |',
    '|---|---|---|---|---|---|',
    ...rows,
    '',
    '✅ passed · ❌ failed, cancelled or timed out · ⚪ skipped or still running',
    '',
    `All nightly runs: ${repoUrl}/actions?query=event%3Aschedule`,
    `Updated ${new Date().toISOString()} by [this run](${repoUrl}/actions/runs/${context.runId}).`,
  ].join('\n');

  // Same lookup as .github/actions/nightly-issue: exact title, no label,
  // because creating a label needs rights we don't assume. PRs are skipped.
  const candidates = await github.paginate(github.rest.issues.listForRepo, {
    owner, repo, state: 'open', creator: 'github-actions[bot]', per_page: 100,
  });
  const existing = candidates
    .filter((i) => !i.pull_request && i.title === TITLE)
    .sort((a, b) => a.number - b.number)[0];

  if (dryRun) {
    core.info(body);
    core.info(existing
      ? `Dry run: would update #${existing.number}`
      : `Dry run: would create "${TITLE}" and pin it`);
    return;
  }

  if (existing) {
    await github.rest.issues.update({ owner, repo, issue_number: existing.number, body });
    core.info(`Updated #${existing.number}`);
    return;
  }

  const created = await github.rest.issues.create({
    owner, repo, title: TITLE, body, labels: ['ci', 'nightly'],
  });
  const number = created.data.number;
  core.info(`Created #${number}`);

  // Pinning may need more than GITHUB_TOKEN can do. It must never fail the job.
  try {
    await github.graphql(
      'mutation($id: ID!) { pinIssue(input: { issueId: $id }) { issue { number } } }',
      { id: created.data.node_id },
    );
    core.info(`Pinned #${number}`);
  } catch (e) {
    core.warning(`Could not pin #${number}: ${e.message}; pin it by hand`);
  }
};
