"""Preparation must not silently accept incomplete or stale generated corpora."""

import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("sv_prepare", Path(__file__).with_name("prepare.py"))
prepare = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(prepare)


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.config = self.root / "config"
        self.config.mkdir()
        (self.config / "revision").write_text("pinned")
        (self.config / "corpus.json").write_text(json.dumps({"main_cases": 4, "generated_cases": 3}))
        (self.root / "tests").mkdir()
        (self.root / "tests/hand.sv").write_text("module Top; endmodule")
        (self.root / "generators").mkdir()
        for name in prepare.SUBMODULES:
            (self.root / "third_party/tests" / name).mkdir(parents=True)
        for name in prepare.GENERATORS:
            (self.root / "generators" / name).write_text(
                "import os, pathlib, sys\n"
                "p = pathlib.Path(os.environ['TESTS_DIR']) / 'generated' / sys.argv[1]\n"
                "p.mkdir(parents=True)\n"
                "(p / 'case.sv').write_text('module Top; endmodule')\n")
        self.patch_here = patch.object(prepare, "HERE", self.config)
        self.patch_here.start()
        self.addCleanup(self.patch_here.stop)
        self.patch_git = patch.object(prepare, "git", side_effect=lambda root, *args:
                                      str(root) if args[-1] == "--show-toplevel" else "pinned")
        self.patch_git.start()
        self.addCleanup(self.patch_git.stop)

    def test_generation_is_complete_and_repeatable(self):
        prepare.prepare(self.root)
        first = {p: (self.root / "tests" / p).read_bytes() for p in prepare.test_paths(self.root / "tests")}
        prepare.prepare(self.root)
        second = {p: (self.root / "tests" / p).read_bytes() for p in prepare.test_paths(self.root / "tests")}
        self.assertEqual(first, second)
        self.assertEqual(len(first), 4)

    def test_wrong_revision_and_uninitialized_submodule_fail_before_generation(self):
        with patch.object(prepare, "git", return_value="wrong"):
            with self.assertRaisesRegex(ValueError, "checked out"):
                prepare.prepare(self.root)
        (self.root / "third_party/tests/uvm").rmdir()
        with self.assertRaisesRegex(ValueError, "submodule"):
            prepare.prepare(self.root)
        self.assertFalse((self.root / "tests/generated").exists())

    def test_missing_outputs_do_not_reuse_stale_success(self):
        prepare.prepare(self.root)
        (self.root / "generators/easyUVM").write_text("pass\n")
        with self.assertRaisesRegex(ValueError, "incomplete"):
            prepare.prepare(self.root)

    def test_failed_generator_does_not_install_partial_output(self):
        (self.root / "generators/easyUVM").write_text("raise SystemExit(1)\n")
        with self.assertRaises(subprocess.CalledProcessError):
            prepare.prepare(self.root)
        self.assertFalse((self.root / "tests/generated").exists())

    def test_foreign_generated_cases_are_not_deleted_or_counted_as_main(self):
        foreign = self.root / "tests/generated/core.sv"
        foreign.parent.mkdir()
        foreign.write_text("module Core; endmodule")
        with self.assertRaisesRegex(ValueError, "unexpected generated"):
            prepare.prepare(self.root)
        self.assertTrue(foreign.exists())
