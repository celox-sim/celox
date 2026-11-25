"""Only the reviewed malformed dashboard input may reach the driver with a missing include."""

import importlib.util
import json
import os
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("celox_adapter", HERE / "runners/celox.py")
adapter = importlib.util.module_from_spec(SPEC)
with patch.dict("sys.modules", {"BaseRunner": types.SimpleNamespace(BaseRunner=object)}):
    SPEC.loader.exec_module(adapter)


class InputIssueTests(unittest.TestCase):
    def test_exception_is_limited_to_case_path_revision_and_positive_test(self):
        issues = json.loads((HERE / "input-issues.json").read_text())
        case, issue = next(iter(issues["cases"].items()))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            env = {"TESTS_DIR": str(root / "tests"), "CELOX_SV_TESTS_CASE": case}
            with patch.dict(os.environ, env), patch.object(adapter.subprocess, "check_output", return_value=issues["revision"]):
                path = root / issue["missing_include"]
                self.assertTrue(adapter.known_missing_include(path, {"should_fail": "0"}))
                self.assertFalse(adapter.known_missing_include(path, {"should_fail": "1"}))
                self.assertFalse(adapter.known_missing_include(root / "missing", {"should_fail": "0"}))
                with patch.dict(os.environ, {"CELOX_SV_TESTS_CASE": "unrelated.sv"}):
                    self.assertFalse(adapter.known_missing_include(path, {"should_fail": "0"}))
                with patch.object(adapter.subprocess, "check_output", return_value="different revision"):
                    self.assertFalse(adapter.known_missing_include(path, {"should_fail": "0"}))

    def test_core_include_exception_is_exact_and_only_for_positive_tests(self):
        issues = json.loads((HERE / "input-issues.json").read_text())
        case = "generated/ariane/cva6_tb_cv64a6_imafdc_sv39.sv"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            env = {"TESTS_DIR": str(root / "tests"), "CELOX_SV_TESTS_CASE": case}
            with patch.dict(os.environ, env), patch.object(adapter.subprocess, "check_output", return_value=issues["revision"]):
                for relative in issues["cases"][case]["missing_includes"]:
                    self.assertTrue(adapter.known_missing_include(root / relative, {"should_fail": "0"}))
                    self.assertFalse(adapter.known_missing_include(root / relative, {"should_fail": "1"}))
                self.assertFalse(adapter.known_missing_include(root / "unreviewed", {"should_fail": "0"}))
                with patch.object(adapter.subprocess, "check_output", return_value="other"):
                    self.assertFalse(adapter.known_missing_include(root / issues["cases"][case]["missing_includes"][0], {"should_fail": "0"}))


class NoModuleIssueTests(unittest.TestCase):
    """A reviewed case whose files declare no module is reported as an input
    issue only when its run fails of that, with the driver's own message."""

    RUN = importlib.util.spec_from_file_location("sv_tests_run_issues", HERE / "run.py")

    def setUp(self):
        self.run = importlib.util.module_from_spec(self.RUN)
        self.RUN.loader.exec_module(self.run)
        self.issues = json.loads((HERE / "input-issues.json").read_text())["cases"]

    def test_reason_needs_the_failed_run_and_the_driver_message(self):
        case, issue = next((case, issue) for case, issue in self.issues.items() if issue.get("no_module"))
        log = f"rc: 1\n{self.run.NO_MODULE}\n"
        self.assertEqual(self.run.input_issue(issue, "1", log), issue["reason"])
        self.assertEqual(self.run.input_issue(issue, "0", log), "")
        self.assertEqual(self.run.input_issue(issue, "1", "unsupported construct\n"), "")
        self.assertEqual(self.run.input_issue(None, "1", log), "")
        # A missing include does not stand for a missing module.
        self.assertEqual(self.run.input_issue(issue, "1", "include directory is unavailable: x\n"), "")
        # Nor does the adapter forward a missing include for the case.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            env = {"TESTS_DIR": str(root / "tests"), "CELOX_SV_TESTS_CASE": case}
            with patch.dict(os.environ, env):
                self.assertFalse(adapter.known_missing_include(root / "tb", {"should_fail": "0"}))

    def test_marker_is_the_driver_message(self):
        driver = (HERE.parents[1] / "crates/celox/examples/sv_tests.rs").read_text()
        self.assertIn(self.run.NO_MODULE, driver)

    @unittest.skipUnless(os.environ.get("SV_TESTS_ROOT"), "set SV_TESTS_ROOT to the pinned checkout")
    def test_listed_files_declare_no_module(self):
        import re
        root = Path(os.environ["SV_TESTS_ROOT"]).resolve()
        checked = 0
        for case, issue in self.issues.items():
            if not issue.get("no_module"):
                continue
            # Each daily job generates only its own group (cores jobs never
            # generate this main-group case), so an absent case is not this
            # checkout's to check; a present case's listed files must exist.
            if not (root / "tests" / case).is_file():
                continue
            checked += 1
            files = self.run.case_metadata(root / "tests" / case)["files"].split()
            for source in files:
                text = (root / source).read_text() if not Path(source).is_absolute() else Path(source).read_text()
                self.assertIsNone(re.search(r"^\s*module\b", text, re.M), f"{case}: {source} declares a module")
        if not checked:
            self.skipTest("no reviewed no-module case is generated in this checkout")
