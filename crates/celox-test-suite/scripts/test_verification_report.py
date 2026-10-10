import json
import tempfile
import unittest
from pathlib import Path

from verification_report import read_report


class VerificationReportTest(unittest.TestCase):
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
