"""The upstream gate must reject missing, empty, failed and skipped-only results."""

from pathlib import Path
import argparse
import json
import os
import tempfile
import unittest
from unittest import mock
import xml.etree.ElementTree as ET

from run import result_counts
import run


class ResultTests(unittest.TestCase):
    def test_coverage_inventory_matches_enabled_and_pending_groups(self):
        inventory = json.loads(Path(run.__file__).with_name("coverage.json").read_text())
        self.assertEqual(inventory["revision"], run.REVISION)
        rows = {row["group"]: row for row in inventory["groups"]}
        self.assertEqual(len(rows), len(inventory["groups"]))
        enabled = {run.CASE_DIRECTORIES.get(name, name) for name in run.DEFAULT_CASES}
        self.assertEqual(enabled, {name for name, row in rows.items() if row["status"] == "enabled"})
        for name in run.PENDING_CASES:
            self.assertEqual(rows[name]["status"], "pending")
            self.assertNotIn(name, run.DEFAULT_CASES)
        self.assertTrue(all(row["reason"] for row in rows.values()))

    def check_xml(self, xml):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "results.xml"
            path.write_text(xml)
            return result_counts(path)

    def test_counts_upstream_language_skips_separately(self):
        self.assertEqual(self.check_xml(
            '<testsuites><testsuite><testcase name="ok"/>'
            '<testcase name="vhdl"><skipped/></testcase></testsuite></testsuites>'
        ), {"passed": 1, "failed": 0, "skipped": 1})

    def test_rejects_failures_and_empty_selections(self):
        for body in ("", '<testcase><skipped/></testcase>',
                     '<testcase/><testcase><failure/></testcase>',
                     '<testcase/><testcase><error/></testcase>'):
            with self.subTest(body=body), self.assertRaises(ValueError):
                self.check_xml(f"<testsuite>{body}</testsuite>")

    def test_rejects_missing_or_malformed_results(self):
        with self.assertRaises(ET.ParseError):
            self.check_xml("bad xml")
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(FileNotFoundError):
                result_counts(Path(directory) / "missing.xml")

    def test_group_configuration_isolated_from_callers_test_selection(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            args = argparse.Namespace(source=root / "source", celox=root / "celox",
                                      python=root / "python", output=root / "results",
                                      case=["test_force_release", "test_test_filter"], timeout=1)
            commands = []

            def execute(command, **kwargs):
                commands.append((command, kwargs))
                if "--results-file" in command:
                    Path(command[command.index("--results-file") + 1]).write_text(
                        '<testsuite><testcase name="passed"/></testsuite>')

            poisoned = {"COCOTB_TEST_FILTER": "must_not_leak", "COCOTB_TESTCASE": "wrong",
                        "COCOTB_RANDOM_TEST_ORDER": "1", "GPI_EXTRA": "invalid"}
            with mock.patch.dict(os.environ, poisoned), \
                    mock.patch.object(run.subprocess, "check_output",
                                      side_effect=[run.REVISION, "2.1.0"]), \
                    mock.patch.object(run.subprocess, "run", side_effect=execute):
                self.assertEqual(run.run(args), 0)
            simulations = [(cmd, opts) for cmd, opts in commands if "--results-file" in cmd]
            first = simulations[0][1]["env"]
            for key in poisoned:
                self.assertNotIn(key, first)
            second = simulations[1][1]["env"]
            self.assertEqual(second["COCOTB_TEST_FILTER"], "y_test")
            build = next(cmd for cmd, _ in commands if "build" in cmd)
            self.assertEqual(build.count("--force-signal"), 2)


if __name__ == "__main__":
    unittest.main()
