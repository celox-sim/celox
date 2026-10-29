#!/usr/bin/env python3
"""Invoke the pinned, unmodified sv-tests tools/runner and compare its verdicts."""

import argparse
from collections import Counter
from concurrent.futures import CancelledError, ThreadPoolExecutor, as_completed
import csv
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys

import reporting

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
    for relative in ("tools/runner", "tools/BaseRunner.py", "tests", "conf/runners/libs.json", "conf/meta-tags.conf"):
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
        "CELOX_SV_TESTS_CASE": test,
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
    input_issues = json.loads((REPO / "conformance/sv-tests/input-issues.json").read_text())["cases"]
    return reporting.annotate({
        "test": test, "state_mode": state_mode, "result": verdict,
        "mode": meta.get("mode", ""), "tags": meta.get("tags", ""),
        "group": (meta.get("results_group") or "main") if meta else "unclassified",
        "input_issue": (input_issues.get(test, {}).get("reason", "")
                        if meta.get("rc") == "1" and "include directory is unavailable:" in log else ""),
        "rc": meta.get("rc", ""), "tool_success": meta.get("tool_success", ""),
        "log": str(log_path),
    }, log)


def read_meta_tags(path):
    meta_tags = {}
    for line in path.read_text().splitlines():
        line = line.strip()
        if line.startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) not in (2, 3):
            raise ValueError(f"invalid upstream meta-tag entry: {line}")
        meta_tags[fields[0]] = set(fields[1].split())
    return meta_tags


def summarize(rows, meta_tags=None):
    """Match upstream totals: count each executed case once and only fully passing tags."""
    groups = {}
    for group in sorted({row["group"] for row in rows}):
        groups[group] = {}
        for state in sorted({row["state_mode"] for row in rows if row["group"] == group}):
            selected = [row for row in rows if row["group"] == group and row["state_mode"] == state]
            counts = Counter(row["result"] for row in selected)
            executed = counts["pass"] + counts["fail"]
            tags = {}
            modes = {}
            for row in selected:
                if row["result"] == "skip":
                    continue
                modes.setdefault(row["mode"], Counter())[row["result"]] += 1
                case_tags = set(row["tags"].split())
                for meta_tag, dependencies in (meta_tags or {}).items():
                    if not dependencies.isdisjoint(case_tags):
                        case_tags.add(meta_tag)
                for tag in case_tags:
                    tags.setdefault(tag, Counter())[row["result"]] += 1
            groups[group][state] = {
                "passed": counts["pass"], "failed": counts["fail"], "skipped": counts["skip"],
                "executed": executed,
                "upstream_input_issues": sum(bool(row.get("input_issue")) for row in selected),
                "pass_percent": round(100 * counts["pass"] / executed, 2) if executed else None,
                "tags_passed": sum(count["fail"] == 0 for count in tags.values()),
                "tags_tested": len(tags),
                "by_tag": {tag: {"passed": count["pass"], "executed": sum(count.values())}
                           for tag, count in sorted(tags.items())},
                "by_execution_mode": {mode: dict(count) for mode, count in sorted(modes.items())},
                **reporting.breakdown(selected),
            }
    return groups


def positive_int(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return number


def shard(value):
    """`INDEX/COUNT`, selecting the INDEXth of COUNT disjoint shards (1-based)."""
    index, separator, count = value.partition("/")
    try:
        index, count = int(index), int(count)
    except ValueError:
        index = count = 0
    if not separator or count < 1 or not 1 <= index <= count:
        raise argparse.ArgumentTypeError("must be INDEX/COUNT with 1 <= INDEX <= COUNT")
    return index, count


def shard_members(tests, selection):
    """The tests of one shard, dealt round-robin from the sorted names so that
    neighbouring tests, which tend to cost alike, spread over the shards."""
    if selection is None:
        return set(tests)
    index, count = selection
    return set(sorted(tests)[index - 1::count])


class CollectionError(ValueError):
    def __init__(self, rows, errors):
        super().__init__("official runner infrastructure failure; collection is incomplete")
        self.rows = sorted(rows, key=lambda row: (row["test"], row["state_mode"]))
        self.errors = sorted(errors, key=lambda error: (error["test"], error["state_mode"]))


def collect_cases(root, driver, out_dir, tests, modes, jobs):
    # Preserve completed evidence, but never update a baseline after an error.
    rows, errors = [], []
    executor = ThreadPoolExecutor(max_workers=jobs)
    try:
        futures = {executor.submit(run_case, root, driver, out_dir, test, mode): (test, mode)
                   for test in tests for mode in modes}
        for future in as_completed(futures):
            test, mode = futures[future]
            try:
                rows.append(future.result())
            except CancelledError:
                continue
            except (OSError, ValueError) as error:
                errors.append({"category": "infrastructure", "stage": "collection", "test": test,
                               "state_mode": mode, "message": str(error),
                               "log": str(out_dir / mode / test / "runner.log")})
                for pending in futures:
                    pending.cancel()
            if len(rows) and len(rows) % (100 * len(modes)) == 0:
                print(f"sv-tests: {len(rows)}/{len(tests) * len(modes)} mode runs", file=sys.stderr)
    finally:
        executor.shutdown(wait=True, cancel_futures=True)
    if errors:
        raise CollectionError(rows, errors)
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
    parser.add_argument("--shard", type=shard,
                        help="INDEX/COUNT: run and compare only that shard of the tests and expectations")
    options = parser.parse_args(argv)
    if options.update and not options.expected:
        parser.error("--update requires --expected")
    root, driver, out_dir = options.sv_tests.resolve(), options.driver.resolve(), options.out_dir.resolve()
    report = options.report or out_dir / "report.tsv"
    rows, meta_tags, requested = [], {}, None
    stage = "preflight"
    revision = None
    try:
        result = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"], capture_output=True, text=True)
        revision = result.stdout.strip() if result.returncode == 0 else None
        preflight(root, driver)
        meta_tags = read_meta_tags(root / "conf/meta-tags.conf")
        modes = MODES if options.state_mode == "both" else (options.state_mode,)
        expected = read_expected(options.expected) if options.expected and options.expected.exists() else {}
        if options.expected and not options.expected.exists() and not options.update:
            raise ValueError(f"expectations do not exist: {options.expected}")
        stage = "corpus"
        tests = sorted(p.relative_to(root / "tests").as_posix()
                       for p in (root / "tests").rglob("*") if p.is_file() and p.suffix.lower() == ".sv")
        if not tests:
            raise ValueError("sv-tests checkout contains no .sv tests")
        # Expected tests that the corpus lost belong to a shard too, so that
        # exactly one shard reports them missing.
        members = shard_members(set(tests) | {test for test, _ in expected}, options.shard)
        tests = [test for test in tests if test in members]
        expected = {key: value for key, value in expected.items() if key[0] in members}
        requested = len(tests) * len(modes)
        stage = "collection"
        rows = collect_cases(root, driver, out_dir, tests, modes, options.jobs)
        results = {(row["test"], row["state_mode"]): row["result"] for row in rows}
        stage = "report"
        groups = summarize(rows, meta_tags)
        reporting.write_reports(report, rows, groups, revision, requested=requested)
        for group, states in groups.items():
            for mode, counts in states.items():
                print(f"{group} [{mode}]: {counts['passed']}/{counts['executed']} passed "
                      f"({counts['pass_percent']}%), {counts['skipped']} skipped, "
                      f"{counts['tags_passed']}/{counts['tags_tested']} tags passed", file=sys.stderr)
        if options.update:
            retained = read_expected(options.expected) if options.expected.exists() else {}
            expected = {key: value for key, value in retained.items()
                        if key[1] not in modes or key[0] not in members}
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
        errors = [{"category": "infrastructure", "stage": stage, "message": str(error)}]
        if isinstance(error, CollectionError):
            rows, errors = error.rows, error.errors
        try:
            reporting.write_reports(report, rows, summarize(rows, meta_tags), revision, errors, requested)
        except OSError as report_error:
            print(f"sv-tests: cannot write incomplete report: {report_error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
