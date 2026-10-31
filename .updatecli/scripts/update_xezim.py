#!/usr/bin/env python3
"""Update the released xezim source and both locks before Updatecli opens a PR."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

FILES = ('flake.nix', 'flake.lock', 'nix/xezim/Cargo.lock')
RELEASE = re.compile(r'v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\Z')
SOURCE = re.compile(r'github:aionhw/xezim/[^"\s]+')


def consistent(root: Path, tag: str) -> bool:
    locked = json.loads((root / 'flake.lock').read_text())['nodes']['xezim']
    packages = tomllib.loads((root / 'nix/xezim/Cargo.lock').read_text())['package']
    version = tag.removeprefix('v')
    return (locked['original'].get('ref') == tag
            and re.fullmatch(r'[a-f0-9]{40}', locked['locked'].get('rev', '')) is not None
            and locked['locked'].get('narHash', '').startswith('sha256-')
            and any(p['name'] == 'xezim' and p['version'] == version for p in packages))


def update(root: Path, tag: str, dry_run: bool = False) -> bool:
    if not RELEASE.fullmatch(tag):
        raise ValueError('xezim must be a stable release tag')
    before = {name: (root / name).read_bytes() for name in FILES}
    source = before['flake.nix'].decode()
    matches = list(SOURCE.finditer(source))
    if len(matches) != 1:
        raise ValueError('Expected exactly one xezim flake input')
    replacement = f'github:aionhw/xezim/{tag}'
    if matches[0][0] == replacement and consistent(root, tag):
        return False
    if dry_run:
        print(f'Would update xezim to {tag}: release tag, flake.lock and Cargo.lock')
        return True

    def run(*command: str, capture: bool = False):
        return subprocess.run(command, cwd=root, check=True, text=True,
                              stdout=subprocess.PIPE if capture else sys.stderr,
                              stderr=sys.stderr)

    try:
        (root / 'flake.nix').write_text(SOURCE.sub(replacement, source))
        run('nix', 'flake', 'update', 'xezim')
        run('bash', 'nix/xezim/update-lock.sh')
        if not consistent(root, tag):
            raise ValueError('Generated xezim locks do not match the selected release')
        run('nix', 'flake', 'check', '--no-build', '--all-systems', '--no-update-lock-file')
        built = run('nix', 'build', '--no-link', '--print-out-paths', '--no-update-lock-file', '.#xezim', capture=True)
        binary = Path(built.stdout.strip()) / 'bin/xezim'
        version = run(str(binary), '-V', capture=True).stdout.splitlines()[0]
        if version != f'xezim version {tag.removeprefix("v")}':
            raise ValueError(f'Built xezim has an unexpected version: {version}')
    except BaseException:
        for name, content in before.items():
            (root / name).write_bytes(content)
        raise
    changed = any((root / name).read_bytes() != content for name, content in before.items())
    if changed:
        print(f'Updated xezim to {tag}: release tag and both locks; Nix build/version verified')
    return changed


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('release')
    args = parser.parse_args()
    update(Path.cwd(), args.release, os.environ.get('DRY_RUN', 'false').lower() == 'true')


if __name__ == '__main__':
    main()
