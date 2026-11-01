#!/usr/bin/env python3
"""Generate all pinned upstream cores, preserving their source and metadata."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys

from prepare import HERE, git

SUBMODULES = ('ariane', 'black-parrot', 'fx68k', 'ibex', 'rggen-sample',
              'rggen-sv-rtl', 'rsd', 'scr1', 'tnoc', 'veer-eh1', 'veer-el2')
GENERATORS = ('ariane', 'black-parrot', 'fx68k', 'rggen', 'rsd', 'scr1', 'tnoc',
              'veer-config', 'fusesoc')


def fields(path):
    metadata = {}
    for line in path.read_text().splitlines():
        match = re.match(r'^:([a-zA-Z_-]+):\s*(.+)', line)
        if match:
            metadata[match[1].lower()] = match[2]
    return metadata


def validate_inputs(root, cases):
    issues = json.loads((HERE / 'input-issues.json').read_text())['cases']
    for path in cases:
        test = path.relative_to(root / 'tests').as_posix()
        metadata = fields(path)
        if not metadata.get('files'):
            raise ValueError(f'generated core has no sources: {test}')
        for key in ('files', 'incdirs'):
            for relative in metadata.get(key, '').split():
                candidate = Path(relative)
                if not candidate.is_absolute():
                    candidate = root / 'tests' / candidate
                if candidate.exists():
                    continue
                issue = issues.get(test, {})
                reviewed = {root / name for name in issue.get('missing_includes', [])}
                if key == 'incdirs' and candidate in reviewed:
                    continue
                raise ValueError(f'missing core {key} input: {candidate}')


def prepare_cores(root):
    revision = (HERE / 'revision').read_text().strip()
    if git(root, 'rev-parse', 'HEAD') != revision:
        raise ValueError(f'sv-tests must be checked out at {revision}')
    for name in SUBMODULES:
        relative = f'third_party/cores/{name}'
        directory = root / relative
        if (not directory.is_dir()
                or Path(git(directory, 'rev-parse', '--show-toplevel')).resolve() != directory
                or git(directory, 'rev-parse', 'HEAD') != git(root, 'rev-parse', f'HEAD:{relative}')):
            raise ValueError(f'initialize the pinned submodule: {relative}')
    cva6 = root / 'third_party/cores/ariane'
    nested = subprocess.check_output(['git', '-C', str(cva6), 'config', '-f', '.gitmodules', '--get-regexp', 'path'], text=True)
    # RTL and testbench dependencies only; omit unrelated software/docs trees.
    paths = [line.split(None, 1)[1] for line in nested.splitlines()
             if line.split(None, 1)[1].startswith(('core/', 'corev_apu/'))]
    paths.append('verif/core-v-verif')
    subprocess.run(['git', '-C', str(cva6), 'submodule', 'update', '--init', '--depth', '1', '--recursive', *paths], check=True)
    subprocess.run(['git', '-C', str(root / 'third_party/cores/tnoc'), 'submodule', 'update', '--init', '--depth', '1', 'rtl/bcm'], check=True)
    environment = dict(os.environ, TESTS_DIR=str(root / 'tests'), CONF_DIR=str(root / 'conf'),
                       THIRD_PARTY_DIR=str(root / 'third_party'))
    for generator in GENERATORS:
        subprocess.run([sys.executable, str(root / 'generators' / generator), generator],
                       cwd=root, env=environment, check=True)
    cases = [path for path in (root / 'tests/generated').rglob('*.sv')
             if fields(path).get('results_group') == 'cores']
    if len(cases) != 21:
        raise ValueError(f'incomplete cores corpus: {len(cases)} cases, expected 21')
    validate_inputs(root, cases)
    print(f'sv-tests cores: {len(cases)} cases, revision {revision}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('sv_tests', type=Path)
    args = parser.parse_args()
    try:
        prepare_cores(args.sv_tests.resolve())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(2, f'{error}\n')
