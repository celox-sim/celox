from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import snapshot_metadata


class MetadataSnapshotTests(unittest.TestCase):
    def test_uses_pinned_case_tags_and_expands_aggregate_tags(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'tests').mkdir()
            (root / 'conf').mkdir()
            (root / 'tests/a.sv').write_text(':defines: \n:tags: a\n:results_group: main\n')
            (root / 'conf/meta-tags.conf').write_text('combo\ta b\n')
            (root / 'conf/lrm.conf').write_text('a\tA feature\t\ncombo\tCombined feature\n')
            expected = root / 'expected.tsv'
            expected.write_text('test\tstate_mode\tresult\na.sv\t2state\tpass\na.sv\t4state\tfail\n')
            revision = (snapshot_metadata.HERE / 'revision').read_text().strip()
            with patch.object(snapshot_metadata, 'git', return_value=revision):
                metadata = snapshot_metadata.capture(root, expected)
            self.assertEqual(metadata['cases'], {'a.sv': ['a', 'combo']})
            self.assertEqual(metadata['labels']['combo']['title'], 'Combined feature')

    def test_rejects_metadata_from_a_different_revision(self):
        with patch.object(snapshot_metadata, 'git', return_value='unreviewed'):
            with self.assertRaisesRegex(ValueError, 'pinned sv-tests revision'):
                snapshot_metadata.capture(Path('/unused'), Path('/unused'))
