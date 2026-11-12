"""Tests for scripts/run-vtest.py: manifest expansion, selection and reports."""
import importlib.util
import json
import os
import re
import stat
import tempfile
import textwrap
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("run_vtest", ROOT / "scripts/run-vtest.py")
rv = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(rv)


def manifest(text):
    return tomllib.loads(textwrap.dedent(text))


BASE = """
[veryl]
backends = ["native", "interp"]
[sv]
backends = ["native"]
"""
CASES = {"veryl": ["a::x", "a::y", "b::z"], "sv": ["t::u"]}


def ids(pairs):
    return [rv.pair_id(p) + ("!" if p["ignored"] else "") for p in pairs]


class Expand(unittest.TestCase):
    def test_defaults_groups_and_cases(self):
        pairs = rv.expand(manifest(BASE + """
            [veryl.groups.a]
            add = ["native-baseline"]
            [veryl.cases."a::y"]
            omit = ["interp"]
            ignore = ["native-baseline"]
            reason = "not yet"
            [sv.cases."t::u"]
            add = ["native-image"]
        """), CASES)
        self.assertEqual(ids(pairs), [
            "veryl/a::x@native", "veryl/a::x@interp", "veryl/a::x@native-baseline",
            "veryl/a::y@native", "veryl/a::y@native-baseline!",
            "veryl/b::z@native", "veryl/b::z@interp",
            "sv/t::u@native", "sv/t::u@native-image",
        ])
        self.assertEqual(pairs[4]["reason"], "not yet")

    def test_rejects_stale_entries(self):
        for extra, message in [
            ('[veryl.cases."a::gone"]\nomit = ["interp"]', "no such case"),
            ('[veryl.groups.gone]\nadd = ["sv"]', "no such group"),
            ('[veryl.cases."a::x"]\nomit = ["wasm"]', "is not one of its backends"),
            ('[veryl.cases."a::x"]\nadd = ["interp"]', "already one of its backends"),
            ('[veryl.cases."a::x"]\nignore = ["wasm"]', "is not one of its backends"),
            ('[veryl.cases."a::x"]\nadd = ["interpreter"]', "unknown veryl backend"),
            ('[veryl.cases."a::x"]\nskip = true', "unknown keys"),
            ('[veryl.groups.a]\nignore = ["interp"]', "one by one"),
        ]:
            with self.subTest(extra=extra), self.assertRaisesRegex(rv.UsageError, message):
                rv.expand(manifest(BASE + extra), CASES)

    def test_repository_manifest_names_known_backends(self):
        data = rv.load_manifest()
        names = {}
        for language in rv.LANGUAGES:
            vtest = ROOT / f"crates/celox-test-suite/src/{language}/cases"
            names[language] = [
                f"{path.stem}::{m.group(1)}"
                for path in sorted(vtest.glob("*.vtest"))
                for m in re.finditer(r"^\(case\s+([\w-]+)", path.read_text(), re.M)
            ]
        pairs = rv.expand(data, names)
        # Every case runs somewhere unless it is explicitly ignored.
        runs = {(p["language"], p["case"]) for p in pairs}
        self.assertEqual(runs, {(l, c) for l, cs in names.items() for c in cs})


class Select(unittest.TestCase):
    pairs = rv.expand(manifest(BASE + """
        [veryl.cases."a::y"]
        ignore = ["interp"]
    """), CASES)

    def select(self, **options):
        return ids(rv.select(self.pairs, **options))

    def test_patterns(self):
        self.assertEqual(self.select(patterns=["a::"]), ["veryl/a::x@native", "veryl/a::x@interp", "veryl/a::y@native"])
        self.assertEqual(self.select(patterns=["*/b::*"]), ["veryl/b::z@native", "veryl/b::z@interp"])
        self.assertEqual(self.select(patterns=["veryl/a::x@interp"]), ["veryl/a::x@interp"])
        self.assertEqual(self.select(patterns=["sv/"]), ["sv/t::u@native"])

    def test_backends_languages_and_ignores(self):
        self.assertEqual(self.select(backends=["interp"]), ["veryl/a::x@interp", "veryl/b::z@interp"])
        self.assertEqual(self.select(skip_backends=["native"]), ["veryl/a::x@interp", "veryl/b::z@interp"])
        self.assertEqual(self.select(languages=["sv"]), ["sv/t::u@native"])
        self.assertEqual(self.select(ignored="only"), ["veryl/a::y@interp!"])
        self.assertIn("veryl/a::y@interp!", self.select(ignored="include"))

    def test_scope(self):
        scope = rv.parse_scope(json.dumps({
            "select": [{"groups": ["veryl/b"]}, {"groups": ["sv"]}, {"backends": ["interp"], "groups": ["veryl/a"]}],
            "exclude_backends": ["native"],
        }))
        self.assertEqual(self.select(scope=scope), ["veryl/a::x@interp", "veryl/b::z@interp"])
        self.assertEqual(len(self.select(scope=rv.parse_scope("{}"))), len(self.select()))
        self.assertEqual(self.select(scope=rv.parse_scope('{"select": []}')), [])
        for bad in ['[]', '{"groups": []}', '{"select": [{"group": []}]}', '{"exclude_backends": "wasm"}']:
            with self.subTest(bad=bad), self.assertRaises(rv.UsageError):
                rv.parse_scope(bad)


class Executor(unittest.TestCase):
    def test_scope(self):
        self.assertEqual(rv.executor_scope({}), ["-p", "celox", "--features", rv.RUNTIME_CASES])
        self.assertEqual(rv.executor_scope({"CELOX_TEST_FEATURES": "cranelift,wasm-runtime"}),
                         ["-p", "celox", "--features", f"{rv.RUNTIME_CASES},celox/cranelift,celox/wasm-runtime"])
        for full in ["true", ""]:
            self.assertEqual(rv.executor_scope({"CELOX_RUST_FULL": full}),
                             ["--workspace", "--exclude", "lydite*", "--features", rv.WORKSPACE_FEATURES, "--tests"])
        builds = [{"packages": ["celox-test-suite"], "features": [], "targets": []},
                  {"packages": ["celox"], "features": ["celox/cranelift"], "targets": ["latch", "systemverilog"]}]
        self.assertEqual(rv.executor_scope({"CELOX_RUST_FULL": "false", "BUILDS": json.dumps(builds)}), [
            "-p", "celox", "--features", f"celox/cranelift,{rv.RUNTIME_CASES}",
            "--test", "latch", "--test", "systemverilog"])
        builds[1]["targets"] = []
        self.assertEqual(rv.executor_scope({"CELOX_RUST_FULL": "false", "BUILDS": json.dumps(builds)})[-1], "--tests")
        self.assertEqual(rv.executor_scope({"CELOX_RUST_FULL": "false", "BUILDS": json.dumps(builds[:1])}),
                         ["-p", "celox", "--features", f"celox/cranelift,celox/wasm-runtime,{rv.RUNTIME_CASES}"])
        builds[1]["features"] = ["celox/cranelift --evil"]
        with self.assertRaises(rv.UsageError):
            rv.executor_scope({"CELOX_RUST_FULL": "false", "BUILDS": json.dumps(builds)})

    def test_executor_path(self):
        artifact = {"reason": "compiler-artifact", "target": {"name": "vtest-exec", "kind": ["example"]},
                    "executable": "/managed target/vtest/examples/vtest-exec"}
        output = '{"reason":"build-finished"}\n' + json.dumps(artifact) + "\n"
        self.assertEqual(rv.executor_path(output), "/managed target/vtest/examples/vtest-exec")
        with self.assertRaises(rv.UsageError):
            rv.executor_path('{"reason":"build-finished"}\n')


class Running(unittest.TestCase):
    def executor(self, directory, body):
        path = Path(directory) / "vtest-exec"
        path.write_text("#!/bin/sh\n" + body)
        path.chmod(path.stat().st_mode | stat.S_IEXEC)
        return str(path)

    def test_results_timeouts_and_junit(self):
        with tempfile.TemporaryDirectory() as directory:
            executor = self.executor(directory, textwrap.dedent("""\
                case "$4" in
                  pass) exit 0 ;;
                  slow) sleep 30 ;;
                  *) echo "assertion <failed> & more"; exit 3 ;;
                esac
            """))
            pairs = [{"language": "veryl", "case": "g::c", "backend": b, "ignored": False, "reason": ""}
                     for b in ["pass", "fail", "slow"]]
            results = rv.run(executor, pairs, jobs=3, timeout=1, fail_fast=False, verbose=False, env=dict(os.environ))
            self.assertEqual([r["status"] for r in results], ["pass", "fail", "timeout"])
            self.assertIn("assertion <failed>", results[1]["output"])
            report = Path(directory) / "reports/vtest.xml"
            rv.junit(results, report)
            text = report.read_text()
            self.assertIn('<testcase classname="vtest.veryl.g" name="g::c@pass"', text)
            self.assertIn("assertion &lt;failed&gt; &amp; more", text)
            self.assertIn('failures="2"', text)
            self.assertIn('message="timed out"', text)


if __name__ == "__main__":
    unittest.main()
