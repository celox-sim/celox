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
