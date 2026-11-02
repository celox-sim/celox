#!/usr/bin/env python3
"""Capture tag metadata from the pinned main corpus for offline docs builds."""
import argparse
import json
from pathlib import Path
import re

from prepare import HERE, git
from run import read_expected, read_meta_tags


def capture(root, expected):
    revision = (HERE / 'revision').read_text().strip()
    if git(root, 'rev-parse', 'HEAD') != revision:
        raise ValueError('metadata must match the pinned sv-tests revision')
    meta_tags = read_meta_tags(root / 'conf/meta-tags.conf')
    cases = {}
    for test in sorted({name for name, _ in read_expected(expected)}):
        fields = {}
        for line in (root / 'tests' / test).read_text().splitlines():
            match = re.match(r'^:([a-zA-Z_-]+):\s*(.+)', line)
            if match:
                fields[match[1].lower()] = match[2]
        tags = set(fields.get('tags', '').split())
        for tag, dependencies in meta_tags.items():
            if not tags.isdisjoint(dependencies):
                tags.add(tag)
        cases[test] = sorted(tags)
    labels = {}
    for line in (root / 'conf/lrm.conf').read_text().splitlines():
        if line.startswith('#') or not line.strip():
            continue
        fields = line.split('\t')
        labels[fields[0]] = {'title': fields[1], 'url': fields[2] if len(fields) > 2 else None}
    return {'revision': revision, 'cases': cases, 'labels': labels}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('sv_tests', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    args.output.write_text(json.dumps(capture(args.sv_tests.resolve(), HERE / 'expected.tsv'), indent=2) + '\n')
