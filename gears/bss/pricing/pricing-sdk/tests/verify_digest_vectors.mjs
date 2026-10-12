import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';

function jcs(value) {
  if (value === null || typeof value === 'boolean' || typeof value === 'string') {
    if (typeof value === 'string') assert(value.isWellFormed(), 'malformed Unicode');
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return '[' + value.map(jcs).join(',') + ']';
  if (typeof value === 'object') {
    return '{' + Object.keys(value).sort().map(k => jcs(k) + ':' + jcs(value[k])).join(',') + '}';
  }
  throw new Error('digest profile excludes JSON numbers and undefined');
}
function decimal(value) {
  assert.match(value, /^-?[0-9]+(?:\.[0-9]+)?$/);
  const negative = value.startsWith('-');
  const [integer, fraction = ''] = value.replace(/^-/, '').split('.');
  const whole = integer.replace(/^0+(?=\d)/, '');
  const part = fraction.replace(/0+$/, '');
  const normalized = whole + (part ? '.' + part : '');
  return negative && normalized !== '0' ? '-' + normalized : normalized;
}
const fixture = JSON.parse(readFileSync(new URL('./fixtures/digests.json', import.meta.url), 'utf8'));
for (const vector of fixture.vectors) {
  const canonical = jcs({ domain: vector.domain, payload: vector.payload });
  assert.equal(canonical, vector.canonical_text);
  assert.equal(createHash('sha256').update(canonical, 'utf8').digest('hex'), vector.sha256);
}
for (const test of fixture.semantic_projections) {
  let payload;
  if (test.kind === 'money') payload = { currency: 'EUR', model: { kind: 'per_unit', unit_amount: decimal(test.input) }, minimum_fee: null };
  else if (test.kind === 'u64') {
    const value = BigInt(test.input);
    assert(value >= 0n && value <= 18446744073709551615n);
    payload = { version: value.toString() };
  } else if (test.kind === 'instant') {
    payload = { at: new Date(test.input).toISOString().replace('.000Z', '.000000000Z') };
  } else throw new Error('unknown semantic projection');
  assert.deepEqual(payload, fixture.vectors[test.vector].payload);
}
assert.throws(() => jcs({ number: 0.047 }));
assert.throws(() => jcs('\ud800'));
assert.throws(() => decimal('1e-3'));
console.log(`${fixture.vectors.length} canonical/hash vectors and ${fixture.semantic_projections.length} semantic projections passed`);
