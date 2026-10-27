"""Diagnostic classification and offline report contracts (no verdict recomputation)."""

import csv
import json
from pathlib import Path
import tempfile
import unittest

import reporting
import run as runner


def fixture(body, result="fail", rc="1", mode="elaboration", negative="0"):
    row = {"test": "chapter-18/test.sv", "state_mode": "2state", "group": "main", "tags": "18 a",
           "result": result, "rc": rc, "mode": mode, "tool_success": "1" if rc == "0" else "0",
           "log": "official.log", "input_issue": ""}
    log = f"mode: {mode}\nrc: {rc}\ntool_success: {row['tool_success']}\nshould_fail: {negative}\n\n/driver -- source.sv\n{body}\n"
    return row, log


class ClassificationTests(unittest.TestCase):
    def test_observed_failures_are_distinct(self):
        examples = [
            ("unsupported_simulator_parser\n\n × Unsupported SystemVerilog\n │ construct: constrained randomize)", "1", "elaboration", "0", "unsupported"),
            ("SystemVerilog parse error: /root/a.sv:2:3: expected `;`", "1", "simulation", "0", "compile_error"),
            ("illegal_context\n × invalid argument", "1", "elaboration", "0", "compile_error"),
            ("elaboration requires a module root; none found", "1", "elaboration", "0", "unsupported"),
            ("$fatal: bad data", "1", "simulation", "0", "runtime_error"),
            ("Timeout: > 30s", "71", "simulation", "0", "timeout"),
            ("thread 'main' panicked at x.rs:1", "101", "elaboration", "0", "crash"),
            ("", "-11", "simulation", "1", "crash"),
            ("", "139", "simulation", "0", "crash"),
            (":assert: (1 == 2)", "0", "simulation", "0", "assertion_failure"),
            ("", "0", "elaboration", "1", "unexpected_success"),
            ("include directory is unavailable: /missing", "1", "elaboration", "0", "environment"),
            ("some new diagnostic", "1", "simulation", "0", "unclassified"),
            ("unsupported file name only", "1", "simulation", "0", "unclassified"),
        ]
        for body, rc, mode, negative, category in examples:
            with self.subTest(body=body, rc=rc):
                row, log = fixture(body, rc=rc, mode=mode, negative=negative)
                actual = reporting.annotate(row, log)
                self.assertEqual(actual["failure_category"], category)
                self.assertEqual(actual["result"], "fail")
        row, log = fixture(examples[0][0])
        self.assertEqual(reporting.annotate(row, log)["evidence"], "constrained randomize")

    def test_passes_and_negative_diagnostic_warnings_do_not_become_failures(self):
        row, log = fixture("unsupported_simulator_parser", result="pass", negative="1")
        annotated = reporting.annotate(row, log)
        self.assertEqual(annotated["failure_category"], "")
        summary = runner.summarize([annotated])["main"]["2state"]
        self.assertEqual((summary["passed"], summary["failed"], summary["executed"]), (1, 0, 1))
        self.assertEqual(summary["negative_pass_diagnostics"], {"unsupported_construct": 1})
        row, log = fixture(":assert: (1 == 1)", result="pass", rc="0", mode="simulation")
        self.assertEqual(reporting.annotate(row, log)["diagnostic"], "")
        row["result"] = "skip"
        self.assertEqual(reporting.annotate(row, "")["failure_category"], "")

    def test_missing_stale_and_ambiguous_evidence_is_not_guessed(self):
        row, log = fixture("unsupported_simulator_parser")
        self.assertEqual(reporting.annotate(row, "")["diagnostic"], "missing_log")
        self.assertEqual(reporting.annotate(row, log.replace("rc: 1", "rc: 0"))["diagnostic"], "metadata_mismatch")
        row, log = fixture("driver returned an ordinary error", rc="71")
        self.assertEqual(reporting.annotate(row, log)["failure_category"], "unclassified")
        row["input_issue"] = "reviewed bad include"
        self.assertEqual(reporting.annotate(row, log)["failure_category"], "input_issue")

    def test_summaries_partition_failures_without_changing_dashboard_totals(self):
        rows = []
        for state in ("2state", "4state"):
            for source in ("/checkout/a.sv:2:3:", "/other/b.sv:90:12:"):
                row, log = fixture(f"SystemVerilog parse error: {source} expected `;`")
                row["state_mode"] = state
                rows.append(reporting.annotate(row, log))
        row, log = fixture("", result="pass", rc="0")
        rows.append(reporting.annotate(row, log))
        groups = runner.summarize(rows)
        for counts in groups["main"].values():
            self.assertEqual(sum(counts["by_failure_category"].values()), counts["failed"])
            self.assertEqual(len(counts["by_failure_reason"]), 1)
            self.assertEqual(counts["by_failure_reason"][0]["count"], 2)
        self.assertEqual(groups["main"]["2state"]["pass_percent"], 33.33)
        self.assertEqual(groups["main"]["4state"]["pass_percent"], 0)

    def test_offline_report_relocates_logs_and_preserves_incomplete_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            row, log = fixture("unsupported_simulator_parser")
            source = root / "old.tsv"
            with source.open("w", newline="") as stream:
                writer = csv.DictWriter(stream, fieldnames=list(row), delimiter="\t")
                writer.writeheader()
                writer.writerow(row)
            source.with_suffix(".summary.json").write_text(json.dumps({
                "revision": "pinned", "complete": False, "requested": 2,
                "collection_errors": [{"stage": "collection", "message": "broken runner"}],
            }))
            logs = root / "logs"
            path = logs / row["state_mode"] / row["test"] / "official.log"
            path.parent.mkdir(parents=True)
            path.write_text(log)
            output = root / "classified.tsv"
            before = source.read_bytes()
            self.assertEqual(reporting.main([str(source), "--output", str(output), "--log-root", str(logs)]), 0)
            summary = json.loads(output.with_suffix(".summary.json").read_text())
            failures = json.loads(output.with_suffix(".failures.json").read_text())
            self.assertFalse(summary["complete"])
            self.assertFalse(failures["complete"])
            self.assertEqual(summary["revision"], "pinned")
            self.assertEqual(failures["failures"][0]["failure_category"], "unsupported")
            self.assertIn("INCOMPLETE", output.with_suffix(".summary.md").read_text())
            self.assertEqual(source.read_bytes(), before)
            self.assertEqual(reporting.main([str(source), "--output", str(source)]), 2)
            source.with_suffix(".summary.json").unlink()
            self.assertEqual(reporting.main([str(source), "--output", str(output), "--log-root", str(logs)]), 0)
            self.assertIsNone(json.loads(output.with_suffix(".summary.json").read_text())["complete"])
            self.assertIsNone(json.loads(output.with_suffix(".failures.json").read_text())["complete"])

    def test_offline_rejects_baselines_and_duplicate_rows(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, output = root / "input.tsv", root / "output.tsv"
            source.write_text("test\tstate_mode\tresult\na.sv\t2state\tfail\n")
            self.assertEqual(reporting.main([str(source), "--output", str(output)]), 2)
            row, _ = fixture("")
            with source.open("w", newline="") as stream:
                writer = csv.DictWriter(stream, fieldnames=list(row), delimiter="\t")
                writer.writeheader()
                writer.writerows([row, row])
            self.assertEqual(reporting.main([str(source), "--output", str(output)]), 2)
