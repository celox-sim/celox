#!/usr/bin/env python3
"""Invoke the pinned, unmodified sv-tests tools/runner and compare its verdicts."""

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor, as_completed
import csv
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys

MODES = ("2state", "4state")
HEADER = ("test", "state_mode", "result")
REPO = Path(__file__).resolve().parents[2]


def read_expected(path):
    with path.open(newline="") as stream:
        reader = csv.DictReader(stream, delimiter="\t")
        if reader.fieldnames != list(HEADER):
            raise ValueError(f"{path}: expected TSV columns {HEADER}")
        results = {}
        for row in reader:
            key = (row["test"], row["state_mode"])
            if (None in row or not all(row.values()) or key in results
                    or key[1] not in MODES or row["result"] not in {"pass", "fail", "skip"}):
                raise ValueError(f"{path}: invalid or duplicate row {row}")
            results[key] = row["result"]
        return results


def write_expected(path, results):
    with path.open("w", newline="") as stream:
        writer = csv.writer(stream, delimiter="\t", lineterminator="\n")
        writer.writerow(HEADER)
        for (test, mode), result in sorted(results.items()):
            writer.writerow((test, mode, result))


def official_verdict(stderr, log, test):
    """Read the verdict emitted by tools/runner, without repeating its rules."""
    verdicts = re.findall(
        rf"^(?:INFO|WARNING)\s+\| (PASS|FAIL): celox/{re.escape(test)}$",
        stderr, re.MULTILINE,
    )
    if len(verdicts) == 1 and log:
        return verdicts[0].lower()
    if not log and re.search(
        rf"^INFO\s+\| Skipping celox/{re.escape(test)}$", stderr, re.MULTILINE
    ):
        return "skip"
    raise ValueError(f"official runner did not emit a unique verdict for {test}")


def log_metadata(log):
    metadata = {}
    for line in log.split("\n\n", 1)[0].splitlines():
        key, separator, value = line.partition(": ")
        if not separator:
            raise ValueError(f"invalid official log metadata: {line}")
        metadata[key] = value
    return metadata


def preflight(root, driver):
    if importlib.util.find_spec("psutil") is None:
        raise ValueError("psutil is required by sv-tests BaseRunner; use the locked development shell")
    if not driver.is_file() or not os.access(driver, os.X_OK):
        raise ValueError(f"Celox driver is not executable: {driver}")
    for relative in ("tools/runner", "tools/BaseRunner.py", "tests", "conf/runners/libs.json"):
        if not (root / relative).exists():
            raise ValueError(f"incomplete sv-tests checkout: missing {relative}")
    libraries = json.loads((root / "conf/runners/libs.json").read_text())
    for library in libraries.values():
        for source in library["files"]:
            path = root / "third_party" / source
            if not path.is_file():
                raise ValueError(f"missing sv-tests library: {path}; initialize the pinned library submodules")
        for directory in library["incdirs"]:
            if not (root / "third_party" / directory).is_dir():
                raise ValueError(f"missing sv-tests library include directory: {directory}")


def run_case(root, driver, out_dir, test, state_mode):
    case_dir = out_dir / state_mode / test
    case_dir.mkdir(parents=True, exist_ok=True)
    log_path = case_dir / "official.log"
    # Remove stale evidence: a skipped/failed invocation must not reuse a prior log.
    log_path.unlink(missing_ok=True)
    environment = os.environ.copy()
    environment.update({
        "OUT_DIR": str(out_dir),
        "CONF_DIR": str(root / "conf"),
        "TESTS_DIR": str(root / "tests"),
        "RUNNERS_DIR": str(REPO / "conformance/sv-tests/runners"),
        "THIRD_PARTY_DIR": str(root / "third_party"),
        "CELOX_SV_TESTS_DRIVER": str(driver),
        "CELOX_SV_STATE_MODE": state_mode,
    })
    result = subprocess.run(
        [sys.executable, str(root / "tools/runner"), "--runner", "celox",
         "--test", test, "--out", str(log_path)],
        cwd=root, env=environment, capture_output=True, text=True,
    )
    (case_dir / "runner.log").write_text(result.stdout + result.stderr)
    if result.returncode != 0:
        raise ValueError(f"official runner infrastructure error for {test} [{state_mode}]; see {case_dir / 'runner.log'}")
    if not log_path.is_file():
        raise ValueError(f"official runner did not write {log_path}")
    log = log_path.read_text()
    verdict = official_verdict(result.stderr, log, test)
    meta = log_metadata(log) if log else {}
    return {
        "test": test, "state_mode": state_mode, "result": verdict,
        "mode": meta.get("mode", ""), "tags": meta.get("tags", ""),
        "rc": meta.get("rc", ""), "tool_success": meta.get("tool_success", ""),
        "log": str(log_path),
    }


def positive_int(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return number


def collect_cases(root, driver, out_dir, tests, modes, jobs):
    # Each official runner owns a unique case/state directory. BaseRunner also
    # uses mkdtemp for its work directory below OUT_DIR/tmp.
    executor = ThreadPoolExecutor(max_workers=jobs)
    futures = []
    rows = []
    try:
        futures = [executor.submit(run_case, root, driver, out_dir, test, mode)
                   for test in tests for mode in modes]
        for future in as_completed(futures):
            rows.append(future.result())
            if len(rows) % (100 * len(modes)) == 0:
                print(f"sv-tests: {len(rows)}/{len(tests) * len(modes)} mode runs", file=sys.stderr)
    finally:
        # A preparation/infrastructure error cannot accept a partial baseline.
        # Cancel queued work and let already running official processes clean up.
        executor.shutdown(wait=True, cancel_futures=True)
    return sorted(rows, key=lambda row: (row["test"], row["state_mode"]))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sv_tests", type=Path, help="pinned sv-tests checkout root")
    parser.add_argument("--driver", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, required=True, help="official logs and temporary directories")
    parser.add_argument("--jobs", type=positive_int, default=1, help="concurrent official runner processes (default: 1)")
    parser.add_argument("--state-mode", choices=(*MODES, "both"), default="both")
    parser.add_argument("--expected", type=Path)
    parser.add_argument("--update", action="store_true")
    parser.add_argument("--report", type=Path)
    options = parser.parse_args(argv)
    if options.update and not options.expected:
        parser.error("--update requires --expected")
    root, driver, out_dir = options.sv_tests.resolve(), options.driver.resolve(), options.out_dir.resolve()
    try:
        preflight(root, driver)
        modes = MODES if options.state_mode == "both" else (options.state_mode,)
        expected = read_expected(options.expected) if options.expected and options.expected.exists() else {}
        if options.expected and not options.expected.exists() and not options.update:
            raise ValueError(f"expectations do not exist: {options.expected}")
        tests = sorted(p.relative_to(root / "tests").as_posix()
                       for p in (root / "tests").rglob("*") if p.is_file() and p.suffix.lower() == ".sv")
        if not tests:
            raise ValueError("sv-tests checkout contains no .sv tests")
        rows = collect_cases(root, driver, out_dir, tests, modes, options.jobs)
        results = {(row["test"], row["state_mode"]): row["result"] for row in rows}
        report = options.report or out_dir / "report.tsv"
        report.parent.mkdir(parents=True, exist_ok=True)
        with report.open("w", newline="") as stream:
            writer = csv.DictWriter(stream, fieldnames=list(rows[0]), delimiter="\t", lineterminator="\n")
            writer.writeheader()
            writer.writerows(rows)
        for mode in modes:
            print(f"{mode}: {dict(Counter(v for (_, m), v in results.items() if m == mode))}", file=sys.stderr)
        if options.update:
            expected = {key: value for key, value in expected.items() if key[1] not in modes}
            expected.update(results)
            write_expected(options.expected, expected)
            return 0
        if options.expected:
            selected = {key: value for key, value in expected.items() if key[1] in modes}
            changes = [(key, selected.get(key, "missing"), results.get(key, "missing"))
                       for key in sorted(selected.keys() | results.keys()) if selected.get(key) != results.get(key)]
            for (test, mode), before, after in changes:
                print(f"{test} [{mode}]: {before} -> {after}", file=sys.stderr)
            if changes:
                print("Review changes before accepting them with --update", file=sys.stderr)
                return 1
        return 0
    except (OSError, ValueError) as error:
        print(f"sv-tests: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
