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

function dateOf(run) {
  return new Date(run.run_started_at || run.created_at).toISOString().slice(0, 10);
}

// The UTC date `n` days before the given YYYY-MM-DD date.
function daysBefore(date, n) {
  return new Date(Date.parse(date) - n * 86400000).toISOString().slice(0, 10);
}

// `byDate` maps a UTC date to the newest run of that date. `window` lists the
// NIGHTS dates to show, oldest first.
function row(label, url, byDate, window) {
  const name = `[${label}](${url})`;
  const history = window.map((d) => (byDate.has(d) ? mark(byDate.get(d)) : '➖')).join('');
  const newestDate = [...window].reverse().find((d) => byDate.has(d));
  if (!newestDate) return `| ${name} | — | — | — | — | ${history} |`;

  const newest = byDate.get(newestDate);
  const last = `${mark(newest)} ${newest.status === 'completed' ? newest.conclusion : 'running'}`;

  // Walk the window from the newest night. Nights with no run are skipped: a
  // dropped night is no evidence of a fix. A run still going has no result
  // yet, so it is skipped too. Count red nights until the first finished
  // non-red run. If the walk reaches the window start without one, the streak
  // may be longer, so show it as "14+".
  let streak = 0;
  let broken = false;
  for (const d of [...window].reverse()) {
    const r = byDate.get(d);
    if (!r || r.status !== 'completed') continue;
    if (mark(r) !== '❌') {
      broken = true;
      break;
    }
    streak += 1;
  }
  if (streak > 0 && !broken) streak = `${streak}+`;

  return `| ${name} | ${last} | ${newestDate} | [#${newest.run_number}](${newest.html_url}) | ${streak} | ${history} |`;
}

module.exports = async ({ github, context, core }) => {
  const { owner, repo } = context.repo;
  const repoUrl = `${context.serverUrl}/${owner}/${repo}`;
  const dryRun = process.env.NIGHTLY_STATUS_DRY_RUN === '1';

  const rows = [];
  for (const { file, label } of WORKFLOWS) {
    const today = new Date().toISOString().slice(0, 10);
    const res = await github.rest.actions.listWorkflowRuns({
      owner, repo, workflow_id: file, event: 'schedule',
      created: `>=${daysBefore(today, NIGHTS + 1)}`, per_page: 100,
    });
    // Runs come newest first, so the first run seen for a date is the newest.
    const byDate = new Map();
    for (const run of res.data.workflow_runs) {
      const d = dateOf(run);
      if (!byDate.has(d)) byDate.set(d, run);
    }
    // The nightly fires late morning UTC, so "no run yet today" is not a gap.
    const end = byDate.has(today) ? today : daysBefore(today, 1);
    const window = [];
    for (let i = NIGHTS - 1; i >= 0; i--) window.push(daysBefore(end, i));
    rows.push(row(label, `${repoUrl}/actions/workflows/${file}?query=event%3Aschedule`, byDate, window));
  }

  const body = [
    '<!-- nightly-status: generated, edits are overwritten -->',
    'State of the scheduled (nightly) workflows on `main`. Updated automatically after each nightly run.',
    '',
    '| Workflow | Last night | Date | Run | Red nights in a row | Last 14 nights (old → new) |',
    '|---|---|---|---|---|---|',
    ...rows,
    '',
    '✅ passed · ❌ failed, cancelled or timed out · ⚪ skipped or still running · ➖ no scheduled run that night',
    '',
    `Open nightly issues (failures and this one): ${repoUrl}/issues?q=is%3Aissue%20is%3Aopen%20label%3Anightly`,
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
