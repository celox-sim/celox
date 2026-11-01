import json
from pathlib import Path
import tempfile
import unittest

from website_report import publish


class WebsiteReportTests(unittest.TestCase):
    def test_incomplete_report_cannot_replace_published_measurement(self):
        with tempfile.TemporaryDirectory() as directory:
            source, destination = Path(directory) / 'report.json', Path(directory) / 'latest.json'
            source.write_text(json.dumps({'complete': False}))
            destination.write_text('retained measurement')
            with self.assertRaises(ValueError):
                publish(source, destination)
            self.assertEqual(destination.read_text(), 'retained measurement')

    def test_complete_report_preserves_groups_and_adds_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            source, destination = Path(directory) / 'report.json', Path(directory) / 'latest.json'
            source.write_text(json.dumps({'complete': True, 'groups': {'imported': {}}}))
            publish(source, destination)
            report = json.loads(destination.read_text())
            self.assertEqual(report['groups'], {'imported': {}})
            self.assertEqual(len(report['celox_revision']), 40)
            self.assertIn('measured_at', report)
