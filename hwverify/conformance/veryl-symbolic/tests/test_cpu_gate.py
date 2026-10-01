import importlib.util
import json
import pathlib
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = pathlib.Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location('cpu_capacity_test', ROOT/'audit/veryl_scaling/cpu_capacity.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

class CpuCapacityGateTests(unittest.TestCase):
    def test_normal_counterexample_cannot_pass_the_capacity_gate(self):
        with tempfile.TemporaryDirectory() as temp:
            out = pathlib.Path(temp)/'case'
            def rejected(width, fault, destination, *args):
                destination.mkdir()
                (destination/'report.json').write_text(json.dumps({'status':'counterexample','obligations':[]}))
                raise RuntimeError('correct CPU failed')
            with patch.object(sys, 'argv', ['cpu_capacity.py','--out',str(out),'--sizes','4','--faults','--require-success']), patch.object(module.runner,'run_case',side_effect=rejected):
                with self.assertRaises(SystemExit): module.main()
            result=json.loads((out/'summary.json').read_text())[0]
            self.assertEqual(result['status'],'counterexample')
            self.assertFalse(result['correct_outcome'])
