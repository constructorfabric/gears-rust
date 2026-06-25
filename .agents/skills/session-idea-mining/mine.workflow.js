// mine.workflow.js — focused idea-mining over distilled chat traces.
//
// Invoke from the `session-idea-mining` skill AFTER running prep.py:
//   Workflow({ scriptPath: "<skill-dir>/mine.workflow.js", args: {
//     focus:       "the kitsoki-dev story (dev-story hub + bugfix/feature pipelines)",
//     context:     "<paragraph orienting the readers: what the focus IS, its parts/vocab>",
//     categories:  ["feature","pain","design","abandoned"],   // optional; these are the default
//     batchDir:    "/tmp/sm-<tag>/batches",                    // from prep.py BATCHDIR=
//     batchCount:  19,                                          // from prep.py BATCHES=
//     title:       "kitsoki-dev — ideas mined from chats"      // optional; for the brief
//   }})
//
// One reader agent per batch extracts focus-relevant findings (4 categories),
// then a barrier into one synthesis agent that dedups/clusters/ranks them.
// Returns { tracesRead, rawFindingCount, headline, themes, rawFindings } — pipe
// the result through focus_brief.py to render the ranked Markdown brief.

export const meta = {
  name: 'session-idea-mining',
  description: 'Mine distilled chat traces for ideas about a focus topic; synthesize a ranked themed brief',
  phases: [
    { title: 'Extract', detail: 'one reader agent per batch of distilled traces' },
    { title: 'Synthesize', detail: 'dedup + cluster all findings into a ranked themed brief' },
  ],
}

const focus = (args && args.focus) || 'the focus topic'
const context = (args && args.context) || ''
const categories = (args && args.categories) || ['feature', 'pain', 'design', 'abandoned']
const batchDir = (args && args.batchDir)
const batchCount = (args && args.batchCount)
if (!batchDir || !batchCount) {
  throw new Error('mine.workflow.js requires args.batchDir and args.batchCount (run prep.py first)')
}

const CAT_GLOSS = {
  feature: '**feature**: explicit "it would be nice if" / "we should add" / new capabilities or extensions.',
  pain: '**pain**: bugs, friction, "this broke / bounced / dropped my input" — things that misbehaved and imply a fix.',
  design: '**design**: architectural musings, half-finished proposals, how the thing should evolve.',
  abandoned: '**abandoned**: threads started but never built or filed — dropped TODOs worth resurfacing.',
}
const catList = categories.map(c => '- ' + (CAT_GLOSS[c] || ('**' + c + '**'))).join('\n')
const catEnum = categories

const READER_BRIEF = `You are mining the user's REAL Claude Code chat history (distilled to compact action traces) for ideas to **improve, extend, or implement** ${focus}.

${context ? '## Focus context\n' + context + '\n' : ''}
## Harvest these kinds of signal
${catList}

## Discipline
- Only capture signal genuinely about the focus — NOT generic chatter, unrelated tooling, or one-off edits with no bearing on it.
- Prefer signal from the USER's own words or genuine design discussion over your own inference; set the \`signal\` field honestly.
- Be concrete and actionable, not "improve X".
- Attribute every finding to the source trace basename(s) (the .txt filename minus extension = the session id). One finding may cite multiple traces if it recurs.
- A trace with nothing relevant contributes zero findings — don't manufacture.

## Trace format
Each line is \`USER:\` (human prompt), \`AI:\` (assistant narration), or \`  > Tool: arg\` (a tool call — the most reliable signal of what happened).`

const FINDINGS_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    traces_read: { type: 'integer' },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        properties: {
          title: { type: 'string', description: 'short imperative title' },
          category: { type: 'string', enum: catEnum },
          target: { type: 'string', description: 'which part of the focus this touches' },
          description: { type: 'string', description: '1-3 concrete sentences: the idea/pain and what to do' },
          signal: { type: 'string', enum: ['explicit', 'repeated', 'aside', 'inferred'] },
          sessions: { type: 'array', items: { type: 'string' }, description: 'source trace basenames' },
        },
        required: ['title', 'category', 'target', 'description', 'signal', 'sessions'],
      },
    },
  },
  required: ['traces_read', 'findings'],
}

phase('Extract')
const batchNums = Array.from({ length: batchCount }, (_, i) => i + 1)
const batchResults = await parallel(batchNums.map(n => () => {
  const manifest = `${batchDir}/batch-${String(n).padStart(2, '0')}.txt`
  return agent(
    `${READER_BRIEF}

Your batch manifest is at: ${manifest}
Read that file to get the list of distilled trace file paths. Then Read EACH trace in full and mine it per the discipline above. Return structured findings.`,
    { label: `read:batch-${n}`, phase: 'Extract', schema: FINDINGS_SCHEMA }
  )
}))

const allFindings = batchResults.filter(Boolean).flatMap(r => r.findings || [])
const tracesRead = batchResults.filter(Boolean).reduce((a, r) => a + (r.traces_read || 0), 0)
log(`Extracted ${allFindings.length} raw findings from ${tracesRead} traces across ${batchResults.filter(Boolean).length}/${batchCount} batches`)

phase('Synthesize')
const THEMES_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    headline: { type: 'string', description: 'one-paragraph executive summary of what the chat history most says the focus needs' },
    themes: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        properties: {
          theme: { type: 'string' },
          priority: { type: 'string', enum: ['now', 'soon', 'later'] },
          rationale: { type: 'string', description: 'why this priority — frequency, pain severity, leverage' },
          categories: { type: 'array', items: { type: 'string', enum: catEnum } },
          target: { type: 'string' },
          summary: { type: 'string', description: '2-4 sentences: the consolidated idea + concrete next step' },
          supporting_ideas: { type: 'array', items: { type: 'string' } },
          session_count: { type: 'integer', description: 'distinct sessions touching this theme' },
          sessions: { type: 'array', items: { type: 'string' } },
        },
        required: ['theme', 'priority', 'rationale', 'categories', 'target', 'summary', 'supporting_ideas', 'session_count', 'sessions'],
      },
    },
  },
  required: ['headline', 'themes'],
}

const synth = await agent(
  `You are synthesizing a chat-history mining pass for ideas to improve/extend **${focus}**.
${context ? '\n## Focus context\n' + context + '\n' : ''}
Below is a raw JSON array of ${allFindings.length} findings extracted by ${batchCount} reader agents, each mining a batch of the user's real chat traces. They overlap and repeat. Your job:
1. **Dedup and cluster** findings that are the same idea (even if worded differently) into consolidated THEMES.
2. For each theme, count DISTINCT sessions (union the \`sessions\` arrays of its members) — recurrence across many sessions is the strongest priority signal.
3. **Rank** each theme now/soon/later by recurrence (session_count), pain severity, and leverage.
4. Preserve distinct sub-points as supporting_ideas so nothing concrete is lost.
5. Write a \`headline\`: what the chat history most loudly says the focus needs.

Bias toward themes backed by explicit user signal and multiple sessions. Keep weak single-session inferences as low-priority \`later\` themes rather than dropping them.

Raw findings JSON:
${JSON.stringify(allFindings)}

Return structured output.`,
  { label: 'synthesize', phase: 'Synthesize', schema: THEMES_SCHEMA }
)

return {
  tracesRead,
  rawFindingCount: allFindings.length,
  headline: synth.headline,
  themes: synth.themes,
  rawFindings: allFindings,
}
