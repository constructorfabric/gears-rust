// Append `sccache --show-stats` to the job summary. Never fails the job.
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");

// Absolute path, exported by setup.sh when it started the server.
const sccache = process.env.CI_CACHE_SCCACHE;
let stats;
try {
  if (!sccache) throw new Error("CI_CACHE_SCCACHE is not set");
  stats = execFileSync(sccache, ["--show-stats"], { encoding: "utf8", timeout: 30000 });
} catch (e) {
  stats = `sccache --show-stats failed: ${e.message}`;
}
try {
  const body = stats.endsWith("\n") ? stats : stats + "\n";
  fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY, "### sccache\n\n```\n" + body + "```\n");
} catch (e) {
  console.log(`::warning::ci-cache: could not write the job summary: ${e.message}`);
}
console.log(stats);
