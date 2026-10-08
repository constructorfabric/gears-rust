#!/usr/bin/env python3
"""Validate local Orders design links, explicit anchors and baseline inventories."""
from pathlib import Path
import collections
import re

DOCS = Path(__file__).resolve().parent.parent
cache = {}
def anchors(path):
    if path in cache:
        return cache[path]
    text = path.read_text()
    result = set(re.findall(r'<a\s+(?:id|name)=["\']([^"\']+)', text))
    seen = collections.Counter()
    for heading in re.findall(r'^#{1,6}\s+(.+)', text, re.M):
        heading = re.sub(r'\[([^\]]+)\]\([^)]+\)', r'\1', heading)
        heading = re.sub('<[^>]+>', '', heading).lower()
        slug = ''.join(c for c in heading if c.isalnum() or c in '_- ').replace(' ', '-')
        suffix = seen[slug]
        seen[slug] += 1
        result.add(slug + (f'-{suffix}' if suffix else ''))
    cache[path] = result
    return result

errors = []
links = 0
for path in DOCS.rglob('*.md'):
    text = path.read_text()
    explicit = collections.Counter(re.findall(r'<a\s+(?:id|name)=["\']([^"\']+)', text))
    for anchor, count in explicit.items():
        if count > 1:
            errors.append(f'{path.relative_to(DOCS)} duplicate anchor {anchor}')
    for target in re.findall(r'\]\(([^)]+)\)', text):
        if '://' in target or target.startswith('mailto:'):
            continue
        location, _, anchor = target.partition('#')
        destination = (path.parent / location).resolve() if location else path.resolve()
        links += 1
        if not destination.exists():
            errors.append(f'{path.relative_to(DOCS)} missing path {target}')
        elif anchor and destination.is_file() and destination.suffix == '.md' and anchor not in anchors(destination):
            errors.append(f'{path.relative_to(DOCS)} missing anchor {target}')

design = (DOCS / 'DESIGN.md').read_text()
foundation = (DOCS / 'features/01-foundation.md').read_text()
tables = re.findall(r'^\| `(orders_[^`]+)` \| `bss_orders__[^`]+`', design, re.M)
events = re.findall(r'^\| `(Order[A-Z][A-Za-z]+)` \| [\d, ]+ \|', design, re.M)
rows = re.findall(r'^(\d+)\. \[ \].*?\*\*FROM\*\*.*?\*\*WHEN\*\* `([^`]+)`.*?`(inst-tr-[^`]+)`$', foundation, re.M)
states = re.findall(r'`([^`]+)`', foundation.rsplit('**States**:', 1)[1].split('**Terminal states**:', 1)[0])
counts = (len(tables), len(events), len(rows), len(set(row[1] for row in rows)), len(states))
if counts != (24, 11, 29, 21, 11):
    errors.append(f'Baseline inventory changed: tables/events/rows/triggers/states = {counts}; reconcile manifest')
if [int(row[0]) for row in rows] != list(range(1, 30)):
    errors.append('Transition row numbering is not exactly 1–29')
if errors:
    raise SystemExit('\n'.join(errors))
print(f'Validated {links} local links, unique explicit anchors and 24 tables / 11 events / 29 rows / 21 triggers / 11 states')
