#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import path from 'node:path';

const dir = new URL('.', import.meta.url).pathname;
const diagrams = [
  ['gear_architecture.drawio.xml', 'Gear architecture'],
  ['planned_gears_map.drawio.xml', 'Planned Gears map'],
  ['gear_categories.drawio.xml', 'Gear categories'],
];

for (const [file, label] of diagrams) {
  const result = spawnSync('slidey', [
    'drawio',
    path.join(dir, 'reference', file),
    '--out-dir',
    path.join(dir, 'themed-svg'),
    '--label',
    label,
  ], { stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status || 1);
}
