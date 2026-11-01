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

    def test_core_case_details_keep_driver_revision_and_omit_local_log_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            source, destination = Path(directory) / 'report.summary.json', Path(directory) / 'cores.json'
            source.write_text(json.dumps({'complete': True, 'groups': {'cores': {}}}))
            source.with_name('report.tsv').write_text(
                'test\tstate_mode\tgroup\tresult\tlog\n'
                'generated/core.sv\t2state\tcores\tfail\t/private/official.log\n'
                'generated/imported.sv\t2state\timported\tpass\t/private/other.log\n')
            publish(source, destination, 'a' * 40)
            report = json.loads(destination.read_text())
            self.assertEqual(report['celox_revision'], 'a' * 40)
            self.assertEqual(len(report['cases']), 1)
            self.assertEqual(report['cases'][0]['test'], 'generated/core.sv')
            self.assertNotIn('/private', destination.read_text())
