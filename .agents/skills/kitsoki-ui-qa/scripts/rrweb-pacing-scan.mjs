#!/usr/bin/env node
// rrweb-pacing-scan.mjs — DETERMINISTIC (no-LLM) pacing detector for EMBEDDED
// rrweb tour clips (the `.rrweb.json` logs a slidey `video` scene replays).
//
// The vision QA gate and the MP4 `pacing-scan.sh` are both blind to a tour whose
// CONTENT is crammed: a frame sampler sees each end-state frame looking correct,
// and there is no chapter sidecar for an rrweb embed. The classic defect this
// catches: a captured conversation plays fine for most of its length, then the
// last few messages / the final artifact all flush in under a second — "the last
// 3-5 messages are super-rushed." That is invisible to interpretation; it is only
// visible in the EVENT TIMELINE, which is exactly what this reads.
//
// How it works (pure structural parse of the rrweb log — deterministic, same log
// in → same flags out):
//   • A "content reveal" = an incremental DOM mutation (type 3, source 0) whose
//     `adds` introduce a substantial block — i.e. the number of added nodes (or a
//     single added text node long enough to be a message) clears --sig-min-adds.
//     Tiny streaming/char-append mutations are below the floor and ignored.
//   • Reveals within --coalesce ms are ONE group (a single logical render that
//     rrweb emitted as several adjacent mutations), so a multi-part message is
//     not mistaken for several rushed messages.
//   • Each group's DWELL = time until the next group (the last group's dwell runs
//     to the end of the clip). A group whose dwell is below --min-dwell ms was on
//     screen too briefly to read before the next content replaced/scrolled it.
//   • The clip FAILS the readable bar when any group is rushed; the "rushed tail"
//     count (rushed groups inside the final --tail-window ms) is reported
//     separately because that is the most common and worst-felt case.
//
// Usage:
//   rrweb-pacing-scan.mjs <clip.rrweb.json | dir> [--out scan.json]
//     [--min-dwell N] [--coalesce N] [--sig-min-adds N] [--sig-min-text N]
//     [--tail-window N] [--fail-on-find]
// Defaults: --min-dwell 1200 --coalesce 150 --sig-min-adds 4 --sig-min-text 24
//           --tail-window 4000
// Exit: 0 = scanned OK (no flags, or flags but advisory);
//       3 = flags found AND --fail-on-find; 2 = usage/parse error.
//
// Advisory by default (surface in qa-report.md); qa.sh promotes to a hard gate
// under --rrweb-strict — same advisory/strict shape as blank-scan / pacing-scan.

import { readFileSync, writeFileSync, statSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

const argv = process.argv.slice(2);
if (!argv.length) { usage('missing <clip.rrweb.json | dir>'); }
const src = argv[0];
const opt = {
  out: '', minDwell: 1200, maxDwell: 2600, msPerChar: 16,
  coalesce: 150, sigMinAdds: 4, sigMinText: 24,
  tailWindow: 4000, failOnFind: false,
};
for (let i = 1; i < argv.length; i++) {
  const a = argv[i];
  const num = () => { const v = Number(argv[++i]); if (!Number.isFinite(v)) usage(`bad number for ${a}`); return v; };
  switch (a) {
    case '--out': opt.out = argv[++i]; break;
    case '--min-dwell': opt.minDwell = num(); break;
    case '--max-dwell': opt.maxDwell = num(); break;
    case '--per-char': opt.msPerChar = num(); break;
    case '--coalesce': opt.coalesce = num(); break;
    case '--sig-min-adds': opt.sigMinAdds = num(); break;
    case '--sig-min-text': opt.sigMinText = num(); break;
    case '--tail-window': opt.tailWindow = num(); break;
    case '--fail-on-find': opt.failOnFind = true; break;
    default: usage(`unknown arg: ${a}`);
  }
}

function usage(msg) {
  process.stderr.write(`rrweb-pacing-scan: ${msg}\n` +
    'usage: rrweb-pacing-scan.mjs <clip.rrweb.json | dir> [--out f] [--min-dwell N] ' +
    '[--coalesce N] [--sig-min-adds N] [--sig-min-text N] [--tail-window N] [--fail-on-find]\n');
  process.exit(2);
}

// Collect clip paths: a single file, or every *.rrweb.json under a directory.
function clipPaths(p) {
  let st;
  try { st = statSync(p); } catch { usage(`no such path: ${p}`); }
  if (st.isDirectory()) {
    return readdirSync(p).filter(f => f.endsWith('.rrweb.json')).sort().map(f => join(p, f));
  }
  return [p];
}

function loadEvents(path) {
  const raw = JSON.parse(readFileSync(path, 'utf8'));
  const events = Array.isArray(raw) ? raw : (raw && raw.events);
  if (!Array.isArray(events) || events.length < 2) throw new Error('not an rrweb event array');
  return events;
}

// Count the "content weight" a mutation adds: element nodes plus text nodes that
// carry real text. type 2 = element, type 3 = text (rrweb serialized node types).
function contentAdds(adds) {
  let nodes = 0, maxText = 0;
  for (const a of adds || []) {
    const n = a && a.node;
    if (!n) continue;
    if (n.type === 2) nodes++;
    else if (n.type === 3) { nodes++; maxText = Math.max(maxText, String(n.textContent || '').trim().length); }
  }
  return { nodes, maxText };
}

function scanClip(path) {
  const events = loadEvents(path);
  const t0 = events[0].timestamp;
  const tEnd = events[events.length - 1].timestamp;
  const durationMs = tEnd - t0;

  // Significant content reveals, with the text length they put on screen.
  const reveals = [];
  for (const e of events) {
    if (e.type !== 3 || !e.data || e.data.source !== 0) continue;
    const adds = e.data.adds;
    if (!Array.isArray(adds) || !adds.length) continue;
    const { nodes, maxText } = contentAdds(adds);
    if (nodes >= opt.sigMinAdds || maxText >= opt.sigMinText) {
      reveals.push({ ts: e.timestamp - t0, textLen: maxText });
    }
  }

  // Coalesce reveals within --coalesce ms into groups (one logical render); a
  // group's text weight is the longest text block it revealed.
  const groups = [];
  for (const r of reveals) {
    if (groups.length && r.ts - groups[groups.length - 1].endMs <= opt.coalesce) {
      const g = groups[groups.length - 1];
      g.endMs = r.ts; g.count++; g.textLen = Math.max(g.textLen, r.textLen);
    } else {
      groups.push({ atMs: r.ts, endMs: r.ts, count: 1, textLen: r.textLen });
    }
  }

  // Required dwell scales with the text shown — a long typed answer needs real
  // reading time, or the transcript scrolls past it before it can be read.
  const requiredDwell = (textLen) => Math.min(opt.maxDwell, opt.minDwell + Math.max(0, textLen) * opt.msPerChar);

  // Dwell per group = gap to the next group's start; last group runs to clip end.
  const flagged = [];
  for (let i = 0; i < groups.length; i++) {
    const start = groups[i].atMs;
    const nextStart = i + 1 < groups.length ? groups[i + 1].atMs : durationMs;
    const dwellMs = Math.round(nextStart - start);
    const need = Math.round(requiredDwell(groups[i].textLen));
    groups[i].dwellMs = dwellMs;
    groups[i].requiredMs = need;
    if (dwellMs < need) {
      flagged.push({
        index: i, atMs: Math.round(start), dwellMs, requiredMs: need, textLen: groups[i].textLen,
        inTail: start >= durationMs - opt.tailWindow,
        issue: `content reveal on screen ${dwellMs}ms < ${need}ms readable dwell` +
          (groups[i].textLen >= opt.sigMinText ? ` (${groups[i].textLen} chars of text)` : '') +
          ' — flashes by before the next message/scroll',
      });
    }
  }
  const rushedTail = flagged.filter(f => f.inTail).length;
  const dwells = groups.map(g => g.dwellMs).filter(d => d != null);

  return {
    clip: path,
    duration_ms: durationMs,
    reveal_groups: groups.length,
    min_dwell_ms: opt.minDwell,
    median_dwell_ms: dwells.length ? dwells.slice().sort((a, b) => a - b)[Math.floor(dwells.length / 2)] : 0,
    shortest_dwell_ms: dwells.length ? Math.min(...dwells) : 0,
    rushed_total: flagged.length,
    rushed_tail: rushedTail,
    tail_window_ms: opt.tailWindow,
    flagged,
  };
}

const results = [];
for (const p of clipPaths(src)) {
  try { results.push(scanClip(p)); }
  catch (e) { results.push({ clip: p, error: String(e.message || e), flagged: [] }); }
}

const report = {
  min_dwell_ms: opt.minDwell,
  coalesce_ms: opt.coalesce,
  clips_scanned: results.length,
  rushed_clips: results.filter(r => (r.rushed_total || 0) > 0).length,
  clips: results,
};

const json = JSON.stringify(report, null, 2);
if (opt.out) writeFileSync(opt.out, json + '\n');
else process.stdout.write(json + '\n');

let total = 0;
for (const r of results) {
  if (r.error) { process.stderr.write(`rrweb-pacing-scan: ${r.clip}: ${r.error}\n`); continue; }
  total += r.rushed_total;
  if (r.rushed_total > 0) {
    process.stderr.write(`rrweb-pacing-scan: ${r.clip} — ${r.rushed_total} rushed reveal(s) ` +
      `(${r.rushed_tail} in the final ${r.tail_window_ms}ms), shortest dwell ${r.shortest_dwell_ms}ms:\n`);
    for (const f of r.flagged) {
      process.stderr.write(`    @${(f.atMs / 1000).toFixed(1)}s dwell=${f.dwellMs}ms${f.inTail ? ' [tail]' : ''}\n`);
    }
  } else {
    process.stderr.write(`rrweb-pacing-scan: ${r.clip} — all ${r.reveal_groups} reveals comfortably paced ` +
      `(median dwell ${r.median_dwell_ms}ms)\n`);
  }
}

if (total > 0 && opt.failOnFind) process.exit(3);
process.exit(0);
