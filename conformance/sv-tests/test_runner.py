"""Contract checks against the actual pinned tools/runner (SV_TESTS_ROOT)."""

import csv
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("sv_tests_run", Path(__file__).with_name("run.py"))
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class AggregateTests(unittest.TestCase):
    def test_reads_official_verdict_instead_of_driver_exit_code(self):
        self.assertEqual(runner.official_verdict("INFO    | PASS: celox/a.sv\n", "rc: 1\n", "a.sv"), "pass")
        self.assertEqual(runner.official_verdict("WARNING | FAIL: celox/a.sv\n", "rc: 0\n", "a.sv"), "fail")
        self.assertEqual(runner.official_verdict("INFO     | Skipping celox/a.sv\n", "", "a.sv"), "skip")
        for stderr, log in [("", "rc: 0\n"), ("INFO | PASS: celox/a.sv\n", ""),
                            ("INFO | PASS: celox/a.sv\nINFO | PASS: celox/a.sv\n", "rc: 0\n")]:
            with self.assertRaises(ValueError):
                runner.official_verdict(stderr, log, "a.sv")

    def test_dashboard_summary_excludes_skips_and_counts_tags_without_double_counting_cases(self):
        rows = [
            {"group": "main", "state_mode": "2state", "mode": "simulation", "result": "pass", "tags": "a b a"},
            {"group": "main", "state_mode": "2state", "mode": "elaboration", "result": "fail", "tags": "b"},
            {"group": "main", "state_mode": "2state", "mode": "", "result": "skip", "tags": "c"},
            {"group": "main", "state_mode": "4state", "mode": "simulation", "result": "pass", "tags": "a"},
            {"group": "cores", "state_mode": "2state", "mode": "elaboration", "result": "fail", "tags": "core"},
        ]
        summary = runner.summarize(rows)
        main = summary["main"]["2state"]
        self.assertEqual((main["passed"], main["executed"], main["skipped"]), (1, 2, 1))
        self.assertEqual(main["pass_percent"], 50)
        self.assertEqual((main["tags_passed"], main["tags_tested"]), (1, 2))
        self.assertEqual(main["by_tag"], {"a": {"passed": 1, "executed": 1},
                                         "b": {"passed": 1, "executed": 2}})
        self.assertEqual(summary["main"]["4state"]["pass_percent"], 100)
        self.assertEqual(summary["cores"]["2state"]["pass_percent"], 0)
        expanded = runner.summarize(rows, {"meta": {"a", "b"}, "chained": {"meta"}})["main"]["2state"]
        self.assertEqual(expanded["tags_tested"], 4)
        self.assertEqual(expanded["by_tag"]["meta"], {"passed": 1, "executed": 2})
        self.assertEqual(expanded["by_tag"]["chained"], {"passed": 1, "executed": 2})

    def test_reads_upstream_meta_tags_and_rejects_malformed_entries(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "meta-tags.conf"
            path.write_text("# comment\nmeta\ta b\nchained\tmeta\thttps://example.com\n")
            self.assertEqual(runner.read_meta_tags(path), {"meta": {"a", "b"}, "chained": {"meta"}})
            path.write_text("meta without a tab\n")
            with self.assertRaises(ValueError):
                runner.read_meta_tags(path)


    def test_jobs_require_a_positive_integer(self):
        self.assertEqual(runner.positive_int("4"), 4)
        for value in ("0", "-1", "invalid"):
            with self.assertRaises((ValueError, runner.argparse.ArgumentTypeError)):
                runner.positive_int(value)

    def test_expected_schema_and_duplicates(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "expected.tsv"
            results = {("a.sv", "2state"): "pass", ("a.sv", "4state"): "skip"}
            runner.write_expected(path, results)
            self.assertEqual(runner.read_expected(path), results)
            path.write_text(path.read_text() + "a.sv\t2state\tpass\n")
            with self.assertRaises(ValueError):
                runner.read_expected(path)
            path.write_text("test\tstate_mode\tresult\na.sv\t2state\tno_top\n")
            with self.assertRaises(ValueError):
                runner.read_expected(path)


@unittest.skipUnless(os.environ.get("SV_TESTS_ROOT"), "set SV_TESTS_ROOT to the pinned checkout")
class OfficialRunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "sv-tests"
        self.root.mkdir()
        self.tests = self.root / "tests"
        self.tests.mkdir()
        (self.root / "conf/runners").mkdir(parents=True)
        (self.root / "conf/runners/libs.json").write_text("{}")
        (self.root / "conf/meta-tags.conf").write_text("# no meta-tags in fixtures\n")
        (self.root / "tools").symlink_to(Path(os.environ["SV_TESTS_ROOT"]).resolve() / "tools", target_is_directory=True)
        self.driver = self.root / "driver"
        self.driver.write_text('''#!/usr/bin/env python3
import json, pathlib, sys, time
print("ARGS: " + json.dumps(sys.argv[1:]))
print("CWD: " + str(pathlib.Path.cwd()))
source = pathlib.Path(sys.argv[-1]).read_text()
code = 0
for line in source.splitlines():
    if line.startswith("// OUTPUT: "): print(line[len("// OUTPUT: "):])
    if line.startswith("// EXIT: "): code = int(line[len("// EXIT: "):])
    if line.startswith("// SLEEP"): time.sleep(3)
sys.exit(code)
''')
        self.driver.chmod(0o755)
        self.out = Path(self.temp.name) / "out"

    def case(self, metadata="", body="", mode="2state"):
        (self.tests / "fixture.sv").write_text(f"/*\n:tags: fixture\n:description: fixture\n{metadata}*/\n{body}\n")
        return runner.run_case(self.root, self.driver, self.out, "fixture.sv", mode)

    def test_official_metadata_defaults_priority_negative_and_assertions(self):
        self.assertEqual(self.case()["mode"], "elaboration")
        self.assertEqual(self.case(":type: parsing elaboration\n")["mode"], "elaboration")
        self.assertEqual(self.case(":type: elaboration simulation\n", "// OUTPUT: :assert: 1 == 1")["result"], "pass")
        row = self.case(":type: simulation\n", "// OUTPUT: :assert: 1 == 2")
        self.assertEqual((row["result"], row["failure_category"]), ("fail", "assertion_failure"))
        row = self.case(":should_fail_because: invalid\n")
        self.assertEqual((row["result"], row["failure_category"]), ("fail", "unexpected_success"))
        row = self.case(":should_fail_because: invalid\n", "// OUTPUT: unsupported_simulator_parser\n// EXIT: 1")
        self.assertEqual((row["result"], row["diagnostic"]), ("pass", "unsupported_construct"))
        row = self.case(":type: simulation\n:should_fail_because: invalid\n", "// EXIT: 1")
        self.assertEqual((row["result"], row["rc"]), ("pass", "1"))
        self.assertEqual(self.case(":type: simulation\n:should_fail_because: invalid\n", "// EXIT: 1\n// OUTPUT: :assert: False")["result"], "fail")
        self.assertEqual(self.case(":should_fail_because: invalid\n", "// EXIT: 126")["result"], "fail")

    def test_official_skips_and_no_module_is_not_preemptively_skipped(self):
        row = self.case(":type: parsing\n")
        self.assertEqual((row["mode"], row["result"]), ("parsing", "pass"))
        self.assertEqual(self.case(":type: parsing\n:should_fail_because: syntax\n", "// EXIT: 1")["result"], "pass")
        row = self.case(":type: preprocessing\n")
        self.assertEqual((row["mode"], row["result"]), ("preprocessing", "pass"))
        self.assertEqual(self.case(":type: preprocessing\n", "// EXIT: 1")["result"], "fail")
        self.assertEqual(self.case(":type: preprocessing\n:should_fail_because: invalid\n", "// EXIT: 1")["result"], "pass")
        self.assertEqual(self.case(":compatible-runners: other\n")["result"], "skip")
        # The source has no module. Its diagnostic still reaches official judgement.
        row = self.case(body="// OUTPUT: no module diagnostic\n// EXIT: 1")
        self.assertEqual((row["result"], row["rc"]), ("fail", "1"))
        self.assertIn("no module diagnostic", Path(row["log"]).read_text())

    def test_files_defines_includes_top_state_and_library_injection(self):
        (self.tests / "headers").mkdir()
        (self.tests / "extra.sv").write_text("module extra; endmodule")
        (self.root / "third_party/lib").mkdir(parents=True)
        (self.root / "third_party/lib/pkg.sv").write_text("package pkg; endpackage")
        (self.root / "conf/runners/libs.json").write_text(json.dumps({
            "fixture": {"files": ["lib/pkg.sv"], "incdirs": ["lib"]},
        }))
        row = self.case(":files: tests/extra.sv\n:incdirs: headers\n:defines: FLAG VALUE=42\n:top_module: extra\n", mode="4state")
        log = Path(row["log"]).read_text()
        args = json.loads(next(line[len("ARGS: "):] for line in log.splitlines() if line.startswith("ARGS: ")))
        self.assertEqual(args[:6], ["--mode", "elaboration", "--state-mode", "4state", "--top", "extra"])
        self.assertIn("VALUE=42", args)
        self.assertIn(str(self.tests / "headers"), args)
        self.assertIn(str(self.root / "third_party/lib"), args)
        self.assertEqual(args[-3:], ["--", str(self.root / "third_party/lib/pkg.sv"), str(self.tests / "extra.sv")])

    def test_missing_environment_is_not_a_negative_pass(self):
        with self.assertRaisesRegex(ValueError, "infrastructure"):
            self.case(":files: tests/missing.sv\n:should_fail_because: invalid\n")
        (self.root / "conf/runners/libs.json").write_text(json.dumps({
            "uvm": {"files": ["missing.sv"], "incdirs": []},
        }))
        with self.assertRaisesRegex(ValueError, "missing sv-tests library"):
            runner.preflight(self.root, self.driver)

    def test_variants_are_forwarded_separately_only_for_simulation(self):
        for mode in ("simulation", "elaboration"):
            row = self.case(f":type: {mode}\n:simvariants: case=1 case=2\n")
            log = Path(row["log"]).read_text()
            args = json.loads(next(line[6:] for line in log.splitlines() if line.startswith("ARGS: ")))
            if mode == "simulation":
                indices = [i for i, arg in enumerate(args) if arg == "--simvariant"]
                self.assertEqual([args[i + 1] for i in indices], ["case=1", "case=2"])
            else:
                self.assertNotIn("--simvariant", args)

    def test_official_timeout(self):
        row = self.case(":type: simulation\n:timeout: 1\n", "// SLEEP")
        self.assertEqual((row["result"], row["rc"]), ("fail", "71"))
        self.assertEqual(row["failure_category"], "timeout")

    def test_partial_update_and_comparison(self):
        self.case()
        expected = self.root / "expected.tsv"
        runner.write_expected(expected, {("fixture.sv", "4state"): "fail"})
        args = [str(self.root), "--driver", str(self.driver), "--out-dir", str(self.out),
                "--expected", str(expected), "--state-mode", "2state"]
        self.assertEqual(runner.main(args + ["--update"]), 0)
        self.assertEqual(runner.read_expected(expected), {
            ("fixture.sv", "2state"): "pass", ("fixture.sv", "4state"): "fail",
        })
        self.assertEqual(runner.main(args), 0)
        self.assertEqual(runner.main(args[:-1] + ["both"]), 1)
        (self.tests / "fixture.sv").unlink()
        (self.tests / "replacement.sv").write_text("/*\n:tags: fixture\n:description: fixture\n*/")
        self.assertEqual(runner.main(args), 1)


    def test_shards_partition_the_tests_and_their_expectations(self):
        names = [f"case{index}.sv" for index in range(5)]
        for name in names:
            (self.tests / name).write_text("/*\n:tags: fixture\n:description: fixture\n*/")
        expected = self.root / "expected.tsv"
        # A test the corpus lost is reported missing by exactly one shard.
        runner.write_expected(expected, {
            **{(name, mode): "pass" for name in names for mode in runner.MODES},
            ("lost.sv", "2state"): "pass",
        })
        args = [str(self.root), "--driver", str(self.driver), "--out-dir", str(self.out),
                "--expected", str(expected), "--jobs", "2"]
        members = [runner.shard_members(names + ["lost.sv"], (index, 2)) for index in (1, 2)]
        self.assertFalse(members[0] & members[1])
        self.assertEqual(members[0] | members[1], set(names) | {"lost.sv"})
        verdicts = [runner.main(args + ["--shard", f"{index}/2"]) for index in (1, 2)]
        self.assertEqual(sorted(verdicts), [0, 1])
        self.assertEqual(verdicts[0 if "lost.sv" in members[0] else 1], 1)
        # An update replaces only the rows of its shard.
        runner.write_expected(expected, {(name, mode): "fail" for name in names for mode in runner.MODES})
        self.assertEqual(runner.main(args + ["--shard", "1/2", "--update"]), 0)
        self.assertEqual(runner.read_expected(expected), {
            (name, mode): "pass" if name in members[0] else "fail"
            for name in names for mode in runner.MODES
        })
        for value in ("0/2", "3/2", "1/0", "1", "a/b"):
            with self.assertRaises(runner.argparse.ArgumentTypeError):
                runner.shard(value)

    def test_parallel_collection_is_deterministic_and_isolated(self):
        self.case(":type: simulation\n", "// OUTPUT: :assert: True")
        for name, mode, body in [
            ("a.sv", "elaboration", ""),
            ("b.sv", "simulation", "// OUTPUT: :assert: False"),
            ("c.sv", "parsing", ""),
        ]:
            (self.tests / name).write_text(f"/*\n:tags: fixture\n:description: fixture\n:type: {mode}\n*/\n{body}\n")
        tests = sorted(path.name for path in self.tests.glob("*.sv"))
        sequential = runner.collect_cases(self.root, self.driver, self.out / "sequential", tests, runner.MODES, 1)
        parallel = runner.collect_cases(self.root, self.driver, self.out / "parallel", tests, runner.MODES, 4)
        canonical = lambda rows: [{key: value for key, value in row.items() if key != "log"} for row in rows]
        self.assertEqual(canonical(parallel), canonical(sequential))
        self.assertEqual([(r["test"], r["state_mode"]) for r in parallel],
                         sorted((r["test"], r["state_mode"]) for r in parallel))
        directories = []
        logs = []
        for row in parallel:
            logs.append(row["log"])
            if row["result"] == "skip":
                continue
            lines = Path(row["log"]).read_text().splitlines()
            args = json.loads(next(line[6:] for line in lines if line.startswith("ARGS: ")))
            self.assertEqual(args[args.index("--mode") + 1], row["mode"])
            self.assertEqual(args[args.index("--state-mode") + 1], row["state_mode"])
            directories.append(next(line[5:] for line in lines if line.startswith("CWD: ")))
        self.assertEqual(len(logs), len(set(logs)))
        self.assertEqual(len(directories), len(set(directories)))
        self.assertFalse(any((self.out / "parallel/tmp").iterdir()))

    def test_parallel_infrastructure_error_preserves_expectations(self):
        (self.tests / "broken.sv").write_text("/*\n:tags: fixture\n:description: fixture\n:files: tests/missing.sv\n:should_fail_because: invalid\n*/")
        (self.tests / "valid.sv").write_text("/*\n:tags: fixture\n:description: fixture\n*/")
        expected = self.root / "expected.tsv"
        runner.write_expected(expected, {("valid.sv", "2state"): "fail"})
        original = expected.read_bytes()
        code = runner.main([str(self.root), "--driver", str(self.driver), "--out-dir", str(self.out),
                            "--jobs", "4", "--expected", str(expected), "--update"])
        self.assertEqual(code, 2)
        self.assertEqual(expected.read_bytes(), original)
        summary = json.loads((self.out / "report.summary.json").read_text())
        self.assertFalse(summary["complete"])
        self.assertEqual(summary["requested"], 4)
        self.assertTrue(summary["collection_errors"])
        self.assertTrue(all(error["test"] == "broken.sv" for error in summary["collection_errors"]))
        with (self.out / "report.tsv").open() as stream:
            rows = list(csv.DictReader(stream, delimiter="\t"))
        self.assertTrue(all(row["result"] == "pass" for row in rows))
        self.assertIn("INCOMPLETE", (self.out / "report.summary.md").read_text())

    def test_preflight_failure_replaces_stale_reports(self):
        self.case()
        args = [str(self.root), "--driver", str(self.driver), "--out-dir", str(self.out)]
        self.assertEqual(runner.main(args), 0)
        self.driver.unlink()
        self.assertEqual(runner.main(args), 2)
        summary = json.loads((self.out / "report.summary.json").read_text())
        self.assertFalse(summary["complete"])
        self.assertEqual(summary["collected"], 0)
        self.assertEqual(summary["collection_errors"][0]["stage"], "preflight")
