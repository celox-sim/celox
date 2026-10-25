#!/usr/bin/env python3
"""Generate the pinned upstream dashboard's main-tests corpus, without patching it."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
GENERATORS = ("template_generator", "easyUVM", "uvm-examples")
SUBMODULES = ("uvm", "uvm-1.2", "easyUVM", "uvm-examples")


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def test_paths(directory):
    return {p.relative_to(directory).as_posix() for p in directory.rglob("*")
            if p.is_file() and p.suffix.lower() == ".sv"}


def prepare(root):
    revision = (HERE / "revision").read_text().strip()
    if git(root, "rev-parse", "HEAD") != revision:
        raise ValueError(f"sv-tests must be checked out at {revision}")
    # A missing submodule must not let an upstream glob generate zero tests.
    for name in SUBMODULES:
        relative = f"third_party/tests/{name}"
        expected = git(root, "rev-parse", f"HEAD:{relative}")
        directory = root / relative
        if (not directory.is_dir()
                or Path(git(directory, "rev-parse", "--show-toplevel")).resolve() != directory
                or git(directory, "rev-parse", "HEAD") != expected):
            raise ValueError(f"initialize the pinned submodule: {relative}")
    counts = json.loads((HERE / "corpus.json").read_text())
    # Generate into a fresh tree, so stale outputs cannot hide a failed generator.
    with tempfile.TemporaryDirectory(prefix="sv-tests-main-") as temporary:
        staging = Path(temporary)
        environment = os.environ.copy()
        environment.update({"TESTS_DIR": str(staging), "CONF_DIR": str(root / "conf"),
                            "THIRD_PARTY_DIR": str(root / "third_party")})
        for generator in GENERATORS:
            subprocess.run([sys.executable, str(root / "generators" / generator), generator],
                           cwd=root, env=environment, check=True)
        generated = staging / "generated"
        fresh = test_paths(generated)
        if len(fresh) != counts["generated_cases"]:
            raise ValueError(f"incomplete main-test generation: {len(fresh)} cases")
        destination = root / "tests/generated"
        stale = test_paths(destination) - fresh
        if stale:
            raise ValueError(f"unexpected generated cases; use a clean checkout: {sorted(stale)}")
        shutil.copytree(generated, destination, dirs_exist_ok=True)
    actual = len(test_paths(root / "tests"))
    if actual != counts["main_cases"]:
        raise ValueError(f"main-test corpus has {actual} cases, expected {counts['main_cases']}")
    print(f"sv-tests main: {actual} cases ({len(fresh)} generated), revision {revision}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sv_tests", type=Path)
    options = parser.parse_args()
    try:
        prepare(options.sv_tests.resolve())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"sv-tests preparation: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
