#!/usr/bin/env python3
"""Run the celox-test-suite cases on Celox backends.

Builds the `vtest-exec` example with the `vtest` Cargo profile, expands
`crates/celox/vtest.toml` into case and backend pairs, and runs the executor
once per pair, in parallel, each with a timeout. No Rust test registers the
cases.
"""
import argparse
import concurrent.futures
import fnmatch
import json
import os
import signal
import subprocess
import sys
import threading
import time
import re
import tomllib
from pathlib import Path
from xml.sax.saxutils import escape, quoteattr

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "crates/celox/vtest.toml"
LANGUAGES = ("veryl", "sv")
# Every backend name the executor's adapters accept; the manifest may name
# only these, whether or not this build includes them.
KNOWN_BACKENDS = {
    "veryl": {
        "native", "native-baseline", "native-parallel", "hotspot", "cranelift",
        "cranelift-parallel", "wasm", "interp", "sv", "veryl",
    },
    "sv": {
        "native", "native-image", "native-parallel", "interpreter", "hotspot",
        "cranelift", "cranelift-parallel", "wasm",
    },
}
TEST_PROFILE = "vtest"
RUNTIME_CASES = "celox-test-suite/runtime-cases"
# CI's full Rust test build: the workspace members and features of the
# "Run workspace tests with CI features" step. The executor builds with the
# same scope there, so the workspace crates compile once for both.
WORKSPACE_FEATURES = (
    "celox/cranelift,celox/wasm-runtime,celox/wasm-component,celox-vpi/cranelift,"
    "celox-vpi/wasm-runtime,celox-bench/cranelift,celox-test-suite/verilator,"
    "celox-test-suite/icarus,celox-test-suite/xezim,celox-test-suite/runtime-cases"
)
# The features of the optional backends, which CI's scoped builds enable.
CI_BACKEND_FEATURES = "celox/cranelift,celox/wasm-runtime"
SCOPE_NAME = r"^[a-zA-Z0-9_][a-zA-Z0-9_./-]*$"
OUTPUT_LIMIT = 64 * 1024


class UsageError(Exception):
    pass


# ── manifest ────────────────────────────────────────────────────────────


def load_manifest(path=MANIFEST):
    with open(path, "rb") as file:
        return tomllib.load(file)


def _names(value, where):
    if not isinstance(value, list) or not all(isinstance(name, str) for name in value):
        raise UsageError(f"{where}: expected a list of backend names")
    return value


def _entry(entry, language, where):
    if not isinstance(entry, dict):
        raise UsageError(f"{where}: expected a table")
    unknown = set(entry) - {"add", "omit", "ignore", "reason"}
    if unknown:
        raise UsageError(f"{where}: unknown keys {sorted(unknown)}")
    for key in ("add", "omit", "ignore"):
        for name in _names(entry.get(key, []), f"{where}.{key}"):
            if name not in KNOWN_BACKENDS[language]:
                raise UsageError(f"{where}.{key}: unknown {language} backend {name!r}")
    if "reason" in entry and not isinstance(entry["reason"], str):
        raise UsageError(f"{where}.reason: expected a string")
    return entry


def _apply(backends, entry, where):
    for name in entry.get("omit", []):
        if name not in backends:
            raise UsageError(f"{where}.omit: {name!r} is not one of its backends")
        backends.remove(name)
    for name in entry.get("add", []):
        if name in backends:
            raise UsageError(f"{where}.add: {name!r} is already one of its backends")
        backends.append(name)
    return backends


def expand(manifest, cases):
    """The case and backend pairs of `manifest` for the corpus `cases`.

    `cases` maps each language to its case names. Returns a list of dicts
    {language, case, backend, ignored, reason}, in corpus order.
    """
    unknown = set(manifest) - set(LANGUAGES)
    if unknown:
        raise UsageError(f"vtest.toml: unknown languages {sorted(unknown)}")
    pairs = []
    for language in LANGUAGES:
        table = manifest.get(language)
        if table is None:
            raise UsageError(f"vtest.toml: missing [{language}]")
        extra = set(table) - {"backends", "groups", "cases"}
        if extra:
            raise UsageError(f"vtest.toml [{language}]: unknown keys {sorted(extra)}")
        defaults = _names(table.get("backends"), f"[{language}].backends")
        for name in defaults:
            if name not in KNOWN_BACKENDS[language]:
                raise UsageError(f"[{language}].backends: unknown backend {name!r}")
        names = cases[language]
        groups = {name.split("::", 1)[0] for name in names}
        group_entries = table.get("groups", {})
        case_entries = table.get("cases", {})
        for group, entry in group_entries.items():
            if group not in groups:
                raise UsageError(f"[{language}.groups.{group}]: no such group")
            _entry(entry, language, f"[{language}.groups.{group}]")
            if "ignore" in entry or "reason" in entry:
                raise UsageError(f"[{language}.groups.{group}]: ignore cases one by one")
        known = set(names)
        for case, entry in case_entries.items():
            if case not in known:
                raise UsageError(f'[{language}.cases."{case}"]: no such case')
            _entry(entry, language, f'[{language}.cases."{case}"]')
        for case in names:
            group = case.split("::", 1)[0]
            backends = list(defaults)
            if group in group_entries:
                _apply(backends, group_entries[group], f"[{language}.groups.{group}]")
            entry = case_entries.get(case, {})
            where = f'[{language}.cases."{case}"]'
            _apply(backends, entry, where)
            ignored = entry.get("ignore", [])
            for name in ignored:
                if name not in backends:
                    raise UsageError(f"{where}.ignore: {name!r} is not one of its backends")
            for backend in backends:
                pairs.append({
                    "language": language,
                    "case": case,
                    "backend": backend,
                    "ignored": backend in ignored,
                    "reason": entry.get("reason", "") if backend in ignored else "",
                })
    return pairs


# ── selection ───────────────────────────────────────────────────────────


def pair_id(pair):
    return f"{pair['language']}/{pair['case']}@{pair['backend']}"


def matches(pattern, pair):
    """A pattern selects `language/group::case`; without a glob it is a substring."""
    name = f"{pair['language']}/{pair['case']}"
    if "@" in pattern:
        pattern, backend = pattern.rsplit("@", 1)
        if backend != pair["backend"]:
            return False
    if any(c in pattern for c in "*?["):
        return fnmatch.fnmatchcase(name, pattern)
    return pattern in name


def parse_scope(text):
    """CI's selection, from `scripts/ci-rust-scope.mjs`:

    {"select": [{"groups": [...], "backends": [...]}, ...], "exclude_backends": [...]}

    A pair is selected when one entry of `select` selects it, and an entry
    without `groups` or `backends` keeps every group or backend; a `groups`
    item is a language (`sv`) or a group (`sv/timing`). Without `select`
    every pair is. `exclude_backends` then drops backends.
    """
    scope = json.loads(text)
    if not isinstance(scope, dict) or set(scope) - {"select", "exclude_backends"}:
        raise UsageError("--scope expects an object with select or exclude_backends")
    names = lambda value: isinstance(value, list) and all(isinstance(v, str) for v in value)
    if "exclude_backends" in scope and not names(scope["exclude_backends"]):
        raise UsageError("--scope exclude_backends: expected a list of backend names")
    if "select" in scope:
        if not isinstance(scope["select"], list):
            raise UsageError("--scope select: expected a list")
        for entry in scope["select"]:
            if (not isinstance(entry, dict) or set(entry) - {"groups", "backends"}
                    or not all(names(v) for v in entry.values())):
                raise UsageError("--scope select: expected objects with groups or backends lists")
    return scope


def in_scope(scope, pair):
    if pair["backend"] in scope.get("exclude_backends", []):
        return False
    if "select" not in scope:
        return True
    group = f"{pair['language']}/{pair['case'].split('::', 1)[0]}"
    return any(
        ("groups" not in entry or group in entry["groups"] or pair["language"] in entry["groups"])
        and ("backends" not in entry or pair["backend"] in entry["backends"])
        for entry in scope["select"]
    )


def select(pairs, *, patterns=(), languages=(), backends=(), skip_backends=(),
           ignored="default", scope=None):
    selected = []
    for pair in pairs:
        if languages and pair["language"] not in languages:
            continue
        if backends and pair["backend"] not in backends:
            continue
        if pair["backend"] in skip_backends:
            continue
        if patterns and not any(matches(p, pair) for p in patterns):
            continue
        if scope is not None and not in_scope(scope, pair):
            continue
        if ignored == "default" and pair["ignored"]:
            continue
        if ignored == "only" and not pair["ignored"]:
            continue
        selected.append(pair)
    return selected


# ── executor ────────────────────────────────────────────────────────────


def executor_scope(env):
    """The packages, features and test targets the executor builds with: CI's
    full workspace build, the scoped build that tests Celox, or the local
    default. Selecting the same test targets also selects the same
    dev-dependencies, so the two builds resolve the same features."""
    full = env.get("CELOX_RUST_FULL")
    if full is not None and full != "false":
        return ["--workspace", "--exclude", "lydite*", "--features", WORKSPACE_FEATURES, "--tests"]
    if full == "false":
        builds = json.loads(env.get("BUILDS") or "[]")
        celox = next((b for b in builds if isinstance(b, dict) and "celox" in (b.get("packages") or [])), None)
        if celox is None:
            # A scope that builds no Celox tests, such as a change to case
            # scripts only: build the executor alone, with every backend.
            return ["-p", "celox", "--features", f"{CI_BACKEND_FEATURES},{RUNTIME_CASES}"]
        names = [*celox["packages"], *celox["features"], *(celox.get("targets") or [])]

        if not all(isinstance(n, str) and re.match(SCOPE_NAME, n) for n in names):
            raise UsageError("invalid Cargo scope argument")
        features = list(dict.fromkeys([*celox["features"], RUNTIME_CASES]))
        targets = [a for t in celox.get("targets") or [] for a in ("--test", t)] or ["--tests"]
        return [*[a for p in celox["packages"] for a in ("-p", p)], "--features", ",".join(features), *targets]
    features = [RUNTIME_CASES]
    extra = env.get("CELOX_TEST_FEATURES")
    if extra:
        features += [f if "/" in f else f"celox/{f}" for f in extra.split(",") if f]
    return ["-p", "celox", "--features", ",".join(features)]


def executor_path(output):
    paths = []
    for line in output.splitlines():
        if not line.strip():
            continue
        message = json.loads(line)
        target = message.get("target") or {}
        if (message.get("reason") == "compiler-artifact" and target.get("name") == "vtest-exec"
                and "example" in target.get("kind", []) and message.get("executable")):
            paths.append(message["executable"])
    if len(paths) != 1:
        raise UsageError("Cargo did not report one vtest executor")
    return paths[0]


def build_executor(env, features=None, profile=TEST_PROFILE):
    scope = executor_scope(env)
    if features:
        scope = [*scope, "--features", ",".join(f if "/" in f else f"celox/{f}" for f in features)]
    command = ["cargo", "build", "--locked", *scope, "--example", "vtest-exec",
               "--profile", profile, "--message-format=json-render-diagnostics"]
    print("+", " ".join(command), file=sys.stderr, flush=True)
    result = subprocess.run(command, cwd=ROOT, env=env, stdout=subprocess.PIPE, text=True)
    if result.returncode != 0:
        raise SystemExit(result.returncode)
    return executor_path(result.stdout)


def query(executor, *args):
    result = subprocess.run([executor, *args], cwd=ROOT, stdout=subprocess.PIPE, text=True, check=True)
    return [line for line in result.stdout.splitlines() if line]


# ── running ─────────────────────────────────────────────────────────────


def run_pair(executor, pair, timeout, env):
    start = time.monotonic()
    process = subprocess.Popen(
        [executor, "run", pair["language"], pair["case"], pair["backend"]],
        cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    try:
        output, _ = process.communicate(timeout=timeout)
        status = "pass" if process.returncode == 0 else "fail"
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        output, _ = process.communicate()
        status = "timeout"
    output = output.decode("utf-8", "replace")
    if len(output) > OUTPUT_LIMIT:
        output = "[... output truncated ...]\n" + output[-OUTPUT_LIMIT:]
    return {**pair, "status": status, "code": process.returncode,
            "seconds": time.monotonic() - start, "output": output}


def junit(results, path):
    failures = sum(1 for r in results if r["status"] != "pass")
    seconds = sum(r["seconds"] for r in results)
    lines = ['<?xml version="1.0" encoding="UTF-8"?>',
             f'<testsuites name="vtest" tests="{len(results)}" failures="{failures}" time="{seconds:.3f}">',
             f'<testsuite name="vtest" tests="{len(results)}" failures="{failures}" time="{seconds:.3f}">']
    for r in results:
        classname = quoteattr(f"vtest.{r['language']}.{r['case'].split('::', 1)[0]}")
        name = quoteattr(f"{r['case']}@{r['backend']}")
        head = f'<testcase classname={classname} name={name} time="{r["seconds"]:.3f}"'
        if r["status"] == "pass":
            lines.append(head + "/>")
        else:
            message = "timed out" if r["status"] == "timeout" else f"exit status {r['code']}"
            lines.append(head + ">")
            lines.append(f'<failure message={quoteattr(message)}>{escape(r["output"])}</failure>')
            lines.append("</testcase>")
    lines += ["</testsuite>", "</testsuites>"]
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    Path(path).write_text("\n".join(lines) + "\n")


def run(executor, pairs, *, jobs, timeout, fail_fast, verbose, env):
    results = []
    lock = threading.Lock()
    stop = threading.Event()
    total = len(pairs)

    def one(pair):
        if stop.is_set():
            return None
        result = run_pair(executor, pair, timeout, env)
        with lock:
            results.append(result)
            done = len(results)
            if result["status"] != "pass":
                label = "TIMEOUT" if result["status"] == "timeout" else "FAIL"
                print(f"{label} [{result['seconds']:.2f}s] ({done}/{total}) {pair_id(result)}\n"
                      f"{result['output'].rstrip()}\n", flush=True)
                if fail_fast:
                    stop.set()
            elif verbose:
                print(f"PASS [{result['seconds']:.2f}s] ({done}/{total}) {pair_id(result)}", flush=True)
            elif done % 500 == 0:
                print(f"... {done}/{total}", flush=True)
        return result

    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        list(pool.map(one, pairs))
    order = {pair_id(p): i for i, p in enumerate(pairs)}
    results.sort(key=lambda r: order[pair_id(r)])
    return results


# ── command line ────────────────────────────────────────────────────────


def parse_args(argv):
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""Patterns select `language/group::case`, optionally with `@backend`;
a pattern without `*`, `?` or `[` matches as a substring. Examples:

  python3 scripts/run-vtest.py counter::
  python3 scripts/run-vtest.py 'sv/timing::*' --backend native
  python3 scripts/run-vtest.py --features cranelift,wasm-runtime --language sv
  python3 scripts/run-vtest.py --list 'veryl/operators::*'""")
    parser.add_argument("patterns", nargs="*", metavar="PATTERN")
    parser.add_argument("--language", action="append", choices=LANGUAGES, default=[])
    parser.add_argument("--backend", action="append", default=[], help="run only these backends")
    parser.add_argument("--skip-backend", action="append", default=[], help="never run these backends")
    parser.add_argument("--ignored", choices=("default", "include", "only"), default="default",
                        help="whether to run the backends vtest.toml ignores (default: skip them)")
    parser.add_argument("--scope", help="CI's JSON selection (see parse_scope)")
    parser.add_argument("--list", action="store_true", help="print the selected pairs and exit")
    parser.add_argument("-j", "--jobs", type=int, default=os.cpu_count() or 1)
    parser.add_argument("--timeout", type=float, default=600, help="seconds per pair (default 600)")
    parser.add_argument("--fail-fast", action="store_true")
    parser.add_argument("-v", "--verbose", action="store_true", help="also print passing pairs")
    parser.add_argument("--junit", help="write a JUnit report here")
    parser.add_argument("--features", help="extra features for the executor, e.g. cranelift,wasm-runtime"
                        " (default: $CELOX_TEST_FEATURES)")
    parser.add_argument("--cargo-profile", default=TEST_PROFILE,
                        help=f"the Cargo profile of the executor (default {TEST_PROFILE})")
    parser.add_argument("--executor", help="a built vtest-exec; skips the Cargo build")
    parser.add_argument("--build-only", action="store_true", help="build the executor and exit")
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(sys.argv[1:] if argv is None else argv)
    env = dict(os.environ)
    try:
        features = [f for f in (args.features or "").split(",") if f]
        executor = args.executor or build_executor(env, features, args.cargo_profile)
        if args.build_only:
            print(executor)
            return 0
        cases = {language: query(executor, "cases", language) for language in LANGUAGES}
        available = {language: set(query(executor, "backends", language)) for language in LANGUAGES}
        for language in LANGUAGES:
            stale = available[language] - KNOWN_BACKENDS[language]
            if stale:
                raise UsageError(f"the executor runs {language} backends {sorted(stale)} "
                                 "that run-vtest.py does not know")
        pairs = expand(load_manifest(), cases)
        scope = parse_scope(args.scope) if args.scope else None
        selected = select(pairs, patterns=args.patterns, languages=args.language,
                          backends=args.backend, skip_backends=args.skip_backend,
                          ignored=args.ignored, scope=scope)
        unbuilt = [p for p in selected if p["backend"] not in available[p["language"]]]
        for name in args.backend:
            if any(p["backend"] == name for p in unbuilt):
                raise UsageError(f"this executor build does not include the {name} backend")
        runnable = [p for p in selected if p["backend"] in available[p["language"]]]
    except UsageError as error:
        print(f"run-vtest: {error}", file=sys.stderr)
        return 2
    if args.list:
        for pair in runnable:
            print(pair_id(pair) + (" (ignored)" if pair["ignored"] else ""))
        return 0
    if not runnable and not (args.scope and not args.patterns):
        print("run-vtest: no case matches the selection", file=sys.stderr)
        return 2
    skipped = {}
    for pair in unbuilt:
        skipped[pair["backend"]] = skipped.get(pair["backend"], 0) + 1
    started = time.monotonic()
    results = run(executor, runnable, jobs=args.jobs, timeout=args.timeout,
                  fail_fast=args.fail_fast, verbose=args.verbose, env=env)
    if args.junit:
        junit(results, args.junit)
    failed = [r for r in results if r["status"] != "pass"]
    print(f"\nvtest: {len(results) - len(failed)} passed, {len(failed)} failed"
          f", {len(runnable) - len(results)} not run"
          f" in {time.monotonic() - started:.1f}s")
    if skipped:
        print("not built into this executor: " + ", ".join(f"{b} ({n})" for b, n in sorted(skipped.items())))
    for r in failed:
        print(f"  {r['status'].upper()}: {pair_id(r)}")
    return 1 if failed or len(results) < len(runnable) else 0


if __name__ == "__main__":
    sys.exit(main())
