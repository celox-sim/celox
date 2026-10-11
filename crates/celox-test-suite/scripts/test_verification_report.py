import json
import tempfile
import unittest
from pathlib import Path

from verification_report import read_report


class VerificationReportTest(unittest.TestCase):
    def test_index_filenames_and_hashed_directories(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            row = {"name": "counter::increment", "status": "passed", "detail": ""}
            report = {"schema_version": 3, "cases": [row]}
            for namespace in ["icarus", ".report-" + "a" * 64]:
                (directory / namespace).mkdir()
                (directory / namespace / "counter.json").write_text(json.dumps({"cases": [row]}))
                index = {"schema_version": 4, "case_files": [f"{namespace}/counter.json"]}
                for filename in ["nightly.icarus.json", "nightly report.json", "検証.json", "extensionless"]:
                    path = directory / filename
                    path.write_text(json.dumps(index))
                    self.assertEqual(read_report(path), report)
            (directory / "icarus/operators.json").write_text(json.dumps({"cases": [{"name": "operators::negative"}]}))
            for files in [[".report-bad/counter.json"], [".report-../counter.json"],
                          [f".report-{'a' * 64}/counter.json", "icarus/operators.json"]]:
                path.write_text(json.dumps({"schema_version": 4, "case_files": files}))
                with self.assertRaises(ValueError):
                    read_report(path)

    def test_split_and_legacy_reports_preserve_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "icarus.json"
            row = {"name": "counter::increment", "status": "compile_error", "detail": "diagnostic"}
            report = {"schema_version": 3, "version": "fixture", "cases": [row]}
            path.write_text(json.dumps(report))
            self.assertEqual(read_report(path), report)
            index = {"schema_version": 4, "version": "fixture", "case_files": ["icarus/counter.json"]}
            path.write_text(json.dumps(index))
            with self.assertRaises(FileNotFoundError):
                read_report(path)
            shard = Path(directory) / "icarus/counter.json"
            shard.parent.mkdir()
            shard.write_text(json.dumps({"cases": [row]}))
            self.assertEqual(read_report(path), report)
            for cases in [[], [row, row], [{"name": "operators::negative"}]]:
                shard.write_text(json.dumps({"cases": cases}))
                with self.assertRaises(ValueError):
                    read_report(path)
            for files in [[], ["icarus/../outside.json"], ["icarus/counter.json"] * 2]:
                path.write_text(json.dumps(dict(index, case_files=files)))
                with self.assertRaises(ValueError):
                    read_report(path)
            path.write_text(json.dumps(dict(index, cases=[row])))
            with self.assertRaises(ValueError):
                read_report(path)


if __name__ == "__main__":
    unittest.main()
