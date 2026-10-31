"""The upstream gate must reject missing, empty, failed and skipped-only results."""

from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET

from run import result_counts


class ResultTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
