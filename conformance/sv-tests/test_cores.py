import tempfile
from pathlib import Path
import unittest

from prepare_cores import fields, validate_inputs


class CoreInputTests(unittest.TestCase):
    def test_empty_annotation_does_not_consume_the_next_metadata_line(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / 'case.sv'
            source.write_text('/*\n:defines: \n:tags: ariane cva6\n:results_group: cores\n*/')
            metadata = fields(source)
            self.assertEqual(metadata['tags'], 'ariane cva6')
            self.assertEqual(metadata['results_group'], 'cores')

    def test_missing_sources_and_unreviewed_includes_abort_preparation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / 'tests/generated/core/case.sv'
            path.parent.mkdir(parents=True)
            path.write_text(':files: absent.sv\n:results_group: cores\n')
            with self.assertRaisesRegex(ValueError, 'missing core files input'):
                validate_inputs(root, [path])
            (root / 'tests/present.sv').write_text('module t; endmodule')
            path.write_text(':files: present.sv\n:incdirs: missing\n:results_group: cores\n')
            with self.assertRaisesRegex(ValueError, 'missing core incdirs input'):
                validate_inputs(root, [path])
