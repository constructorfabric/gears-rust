"""Locate canonical Orders contracts in either supported documentation layout.

The map assigns logical contract sections to DESIGN.md or a feature. The invariant
checker reads those live sections, never a snapshot. Its older semantic assertions
can consequently keep their contract-local section addresses after a physical move.
"""
from __future__ import annotations

from functools import lru_cache

import fnmatch
import json
import re
from dataclasses import dataclass
from pathlib import Path

MAP_PATH = Path(__file__).with_name('orders-lifecycle-doc-map.json')


@lru_cache(maxsize=1)
def section_map() -> list[dict]:
    return json.loads(MAP_PATH.read_text())['sections']


@lru_cache(maxsize=None)
def migrated(docs: Path) -> bool:
    path = docs / 'DESIGN.md'
    return path.is_file() and '<!-- contract:' in path.read_text()


@dataclass(frozen=True)
class ContractSource:
    docs: Path
    stem: str

    @property
    def name(self) -> str:
        return self.stem + '.md'

    def __lt__(self, other):
        return self.name < other.name

    def __str__(self):
        return f'{self.docs} [contract {self.stem}]'

    def is_file(self) -> bool:
        return bool(self.records()) and all((self.docs / x['target']).is_file() for x in self.records())

    def records(self):
        return [x for x in section_map() if x['slice'] == self.stem]

    @lru_cache(maxsize=None)
    def read_text(self, encoding='utf-8') -> str:
        parts = []
        majors = set()
        for x in self.records():
            path = self.docs / x['target']
            text = path.read_text(encoding=encoding) if path.is_file() else ''
            marker = f'<!-- contract:{self.stem}:{x["section"]} -->'
            m = re.search(re.escape(marker) + r'\n### [^\n]+\n(.*?)\n<!-- /contract -->', text, re.S)
            if not m:
                raise ValueError(f'Missing canonical contract {self.stem}:{x["section"]} in {path}')
            major = x['section'].split('.')[0]
            if '.' in x['section'] and major not in majors:
                parts.append(f'## {major}. Contract sections\n')
            majors.add(major)
            heading = f'### {x["section"]} ' if '.' in x['section'] else f'## {x["section"]}. '
            parts.append(heading + x['title'] + '\n' + m[1])
        return normalize_references('\n'.join(parts))

    def write_text(self, text: str, encoding='utf-8') -> None:
        """Write a mutated logical source back to its actual canonical sections.

        Used by negative-test fixtures so a mutation changes the very text the
        migrated checker reads. Production authoring edits the real files.
        """
        sections = list(re.finditer(r'^(#{2,3}) (\d+(?:\.\d+)?)\.? [^\n]+\n', text, re.M))
        bodies = {m[2]: text[m.end():sections[n+1].start() if n+1<len(sections) else len(text)].rstrip('\n') for n,m in enumerate(sections)}
        for x in self.records():
            if x['section'] not in bodies:
                raise ValueError(f'Test mutation removed section {self.stem}:{x["section"]}')
            path = self.docs / x['target']; original = path.read_text(encoding=encoding)
            pattern = r'(<!-- contract:' + re.escape(self.stem+':'+x['section']) + r' -->\n### [^\n]+\n).*?(\n<!-- /contract -->)'
            updated, count = re.subn(pattern, lambda m: m[1]+bodies[x['section']]+'\n'+m[2], original, count=1, flags=re.S)
            if count != 1: raise ValueError(f'Missing mutation target {path}: {x["section"]}')
            path.write_text(updated, encoding=encoding)


def slice_sources(docs: Path, pattern='*.md'):
    if migrated(docs):
        names = sorted({x['slice'] for x in section_map()})
        return [ContractSource(docs, name) for name in names if fnmatch.fnmatch(name+'.md', pattern)]
    return sorted((docs / 'design').glob(pattern))


def source_path(docs: Path, path: str):
    if migrated(docs) and path.startswith('design/'):
        if path == 'design/README.md': return docs / 'DECOMPOSITION.md'
        return ContractSource(docs, Path(path).stem)
    return docs / path


def normalize_references(text: str) -> str:
    return re.sub(r'\[([^\]\n]+)\]\([^\n)]*#contract-[^\n)]+\)', r'\1', text)


@lru_cache(maxsize=None)
def read_source(path) -> str:
    if not path.is_file(): return ''
    text = path.read_text(encoding='utf-8')
    if isinstance(path, Path) and path.name == 'DESIGN.md' and '<!-- contract:' in text:
        return normalize_references(re.sub(r'<!-- contract:.*?<!-- /contract -->', '', text, flags=re.S))
    return normalize_references(text)
