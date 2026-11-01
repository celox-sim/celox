#!/usr/bin/env python3
"""Add upstream's imported group to an already prepared main corpus."""
import argparse
import os
from pathlib import Path
import subprocess
import sys

from prepare import git, HERE

SUBMODULES = ('tests/hdlconvertor', 'tests/utd-sv', 'tests/projf-explore',
              'cores/basejump_stl', 'tools/yosys', 'tools/icarus')


def prepare_imported(root):
    revision = (HERE / 'revision').read_text().strip()
    if git(root, 'rev-parse', 'HEAD') != revision:
        raise ValueError(f'sv-tests must be checked out at {revision}')
    for name in SUBMODULES:
        relative = f'third_party/{name}'
        directory = root / relative
        if (not directory.is_dir()
                or Path(git(directory, 'rev-parse', '--show-toplevel')).resolve() != directory
                or git(directory, 'rev-parse', 'HEAD') != git(root, 'rev-parse', f'HEAD:{relative}')):
            raise ValueError(f'initialize the pinned submodule: {relative}')
    # Require the pinned Icarus regression data before invoking its generator.
    if not any((root / 'third_party/tools/icarus/ivtest').glob('*.list')):
        raise ValueError('missing pinned Icarus ivtest regression lists')
    environment = dict(os.environ, TESTS_DIR=str(root / 'tests'),
                       CONF_DIR=str(root / 'conf'), THIRD_PARTY_DIR=str(root / 'third_party'))
    for generator in ('path_generator', 'ivtest', 'yosys_hana'):
        subprocess.run([sys.executable, str(root / 'generators' / generator), generator],
                       cwd=root, env=environment, check=True)
    imported = [p for p in (root / 'tests/generated').rglob('*.sv')
                if ':results_group: imported' in p.read_text()]
    if len(imported) != 3442:
        raise ValueError(f'incomplete imported corpus: {len(imported)} cases, expected 3442')
    print(f'sv-tests imported: {len(imported)} cases, revision {revision}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('sv_tests', type=Path)
    args = parser.parse_args()
    try:
        prepare_imported(args.sv_tests.resolve())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(2, f'{error}\n')
